//! `app_db`-backed persistence for Remote Access: the gateway's own
//! enabled/port/cert config (`remote_access_config`, a fourth singleton-row
//! store — see `crate::app_db`'s `SCHEMA_V4`) and the paired-device roster
//! (`paired_devices`, a real multi-row table).
//!
//! Token hashing duplicates `crate::applock`'s `hash_pin`/`new_salt` shape
//! rather than sharing it — this crate's established preference for small,
//! locally-owned logic over cross-cutting abstraction (see that module's
//! own doc comment, and `crate::crypto::recovery`'s hand-rolled base32 for
//! the same call). Unlike a user-typed PIN, a device token is 256 bits of
//! `rand`, so there's no lockout/rate-limit logic to duplicate — just the
//! salted-hash shape, so a stolen `app.sqlite` backup can't be replayed as a
//! bearer token.

use crate::Result;
use ring::hmac;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// How long a paired device's bearer token stays valid before the gateway
/// starts rejecting it (`gateway::authenticate` checks this, this module
/// only stores/computes the timestamp). 90 days, not Signal-Protocol-short —
/// this is a LAN/overlay convenience feature for a beginner's own devices,
/// not a high-security admin surface, so forced frequent re-pairing would be
/// beginner-hostile friction for little added benefit. There is no in-band
/// refresh: no client exists yet to push a rotated token to, so recovery
/// past expiry is re-pairing via the existing desktop paired-device UI,
/// exactly like first-time pairing.
pub const TOKEN_LIFETIME_MS: i64 = 90 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteAccessConfig {
    pub enabled: bool,
    /// Persisted (unlike the engine's per-start random port) so a paired
    /// device has a stable address to reconnect to across desktop
    /// restarts/wakes — picked once via `crate::remote::gateway::free_port`
    /// the first time Remote Access is enabled, then reused.
    pub port: Option<u16>,
    /// The gateway's self-signed TLS certificate/key, DER-encoded and
    /// base64'd for JSON storage. Generated once and reused across
    /// restarts, so a paired device's pinned fingerprint (captured at
    /// pairing time) keeps matching. `app.sqlite` is already encrypted at
    /// rest (SQLCipher) — no additional per-field encryption needed here,
    /// consistent with every other JSON blob this database holds.
    pub cert_der_b64: Option<String>,
    pub key_der_b64: Option<String>,
    /// A stable random id for this install, generated once — used as the
    /// mDNS instance name/hostname (`crate::remote::discovery`) so it stays
    /// consistent across restarts.
    pub device_id: Option<String>,
}

pub fn load_config(conn: &Connection) -> RemoteAccessConfig {
    let row: Option<String> = conn
        .query_row(
            "SELECT json FROM remote_access_config WHERE key = 'current'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);
    match row {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("remote access config row is corrupt ({e}); using defaults");
            RemoteAccessConfig::default()
        }),
        None => RemoteAccessConfig::default(),
    }
}

pub fn save_config(conn: &Connection, cfg: &RemoteAccessConfig) -> Result<()> {
    let json = serde_json::to_string(cfg)?;
    conn.execute(
        "INSERT INTO remote_access_config (key, json) VALUES ('current', ?1)
         ON CONFLICT(key) DO UPDATE SET json = excluded.json",
        params![json],
    )?;
    Ok(())
}

/// Flat view of one paired device for the UI's revoke list. The bearer
/// token itself is never readable back once issued — only its salted hash
/// is stored, so there's nothing here to leak beyond a name and timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDeviceInfo {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub last_seen_ms: Option<i64>,
    /// When this device's bearer token stops being accepted — see
    /// [`TOKEN_LIFETIME_MS`]. The UI surfaces this so a beginner sees
    /// "expires in N days" / "expired — pair again" rather than an
    /// unexplained sudden disconnect.
    pub expires_at_ms: i64,
}

pub fn list_paired_devices(conn: &Connection) -> Result<Vec<PairedDeviceInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at_ms, last_seen_ms, expires_at_ms FROM paired_devices \
         ORDER BY created_at_ms DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(PairedDeviceInfo {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at_ms: row.get(2)?,
                last_seen_ms: row.get(3)?,
                expires_at_ms: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[allow(clippy::too_many_arguments)]
pub fn insert_paired_device(
    conn: &Connection,
    id: &str,
    name: &str,
    token_hash: &str,
    token_salt: &str,
    created_at_ms: i64,
) -> Result<()> {
    let expires_at_ms = created_at_ms + TOKEN_LIFETIME_MS;
    conn.execute(
        "INSERT INTO paired_devices (id, name, token_hash, token_salt, created_at_ms, last_seen_ms, expires_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
        params![id, name, token_hash, token_salt, created_at_ms, expires_at_ms],
    )?;
    Ok(())
}

pub fn revoke_paired_device(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM paired_devices WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn rename_paired_device(conn: &Connection, id: &str, name: &str) -> Result<()> {
    conn.execute(
        "UPDATE paired_devices SET name = ?1 WHERE id = ?2",
        params![name, id],
    )?;
    Ok(())
}

pub fn touch_last_seen(conn: &Connection, id: &str, now_ms: i64) -> Result<()> {
    conn.execute(
        "UPDATE paired_devices SET last_seen_ms = ?1 WHERE id = ?2",
        params![now_ms, id],
    )?;
    Ok(())
}

/// Check `token` against every paired device's salted hash, returning the
/// matching device (and bumping its `last_seen_ms`) or `None`. There's no
/// way to index-lookup a device by bearer token directly — each row's hash
/// is salted independently, the same tradeoff `applock`'s per-PIN salted
/// hash accepts — but this table's realistic size (a handful of paired
/// devices per user) makes the linear scan a non-issue.
pub fn authenticate(
    conn: &Connection,
    token: &str,
    now_ms: i64,
) -> Result<Option<PairedDeviceInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at_ms, last_seen_ms, expires_at_ms, token_hash, token_salt \
         FROM paired_devices",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                PairedDeviceInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at_ms: row.get(2)?,
                    last_seen_ms: row.get(3)?,
                    expires_at_ms: row.get(4)?,
                },
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    for (info, token_hash, token_salt) in rows {
        if verify_token(token, &token_salt, &token_hash) {
            touch_last_seen(conn, &info.id, now_ms)?;
            return Ok(Some(info));
        }
    }
    Ok(None)
}

/// A fresh random salt, hex-encoded. Mirrors `crate::applock::new_salt`.
pub fn new_salt() -> String {
    let bytes: [u8; 16] = rand::random();
    to_hex(&bytes)
}

/// HMAC-SHA256 keyed by `salt`, applied to `token` — mirrors
/// `crate::applock::hash_pin`'s salted-hash shape, but HMAC rather than a
/// salt-prepended plain hash so [`verify_token`] can check it via
/// `ring::hmac::verify` (constant-time by construction) instead of ever
/// comparing hash bytes directly.
pub fn hash_token(token: &str, salt: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, salt.as_bytes());
    to_hex(hmac::sign(&key, token.as_bytes()).as_ref())
}

/// Constant-time token check via `ring::hmac::verify` — never `==` (or any
/// other byte-comparison) on secret material, matching
/// `crate::sync::pairing::verify_mac`'s established pattern in this
/// codebase for the same class of problem. `false` for a malformed
/// (non-hex) stored hash, same as a genuine mismatch — never panics on
/// corrupt data.
fn verify_token(token: &str, salt: &str, expected_hash_hex: &str) -> bool {
    let Ok(expected) = hex_decode(expected_hash_hex) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, salt.as_bytes());
    hmac::verify(&key, token.as_bytes(), &expected).is_ok()
}

/// A fresh 256-bit bearer token, hex-encoded — shown to the user exactly
/// once at pairing time, never stored or retrievable again (only its salted
/// hash is).
pub fn new_device_token() -> String {
    let bytes: [u8; 32] = rand::random();
    to_hex(&bytes)
}

/// A fresh random device id, hex-encoded.
pub fn new_device_id() -> String {
    let bytes: [u8; 16] = rand::random();
    to_hex(&bytes)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| ()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    #[test]
    fn config_roundtrip_and_corrupt_fallback() {
        let conn = test_conn();
        let cfg = load_config(&conn);
        assert!(!cfg.enabled);
        assert_eq!(cfg.port, None);

        let mut cfg2 = cfg.clone();
        cfg2.enabled = true;
        cfg2.port = Some(4433);
        save_config(&conn, &cfg2).unwrap();
        let cfg3 = load_config(&conn);
        assert!(cfg3.enabled);
        assert_eq!(cfg3.port, Some(4433));

        conn.execute(
            "INSERT INTO remote_access_config (key, json) VALUES ('current', '{not json') \
             ON CONFLICT(key) DO UPDATE SET json = excluded.json",
            [],
        )
        .unwrap();
        let cfg4 = load_config(&conn);
        assert!(!cfg4.enabled);
    }

    #[test]
    fn paired_device_crud_and_authenticate() {
        let conn = test_conn();
        let token = new_device_token();
        let salt = new_salt();
        let hash = hash_token(&token, &salt);
        insert_paired_device(&conn, "dev-1", "Sarah's Phone", &hash, &salt, 1000).unwrap();

        let devices = list_paired_devices(&conn).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "Sarah's Phone");
        assert_eq!(devices[0].expires_at_ms, 1000 + TOKEN_LIFETIME_MS);

        let matched = authenticate(&conn, &token, 2000).unwrap().unwrap();
        assert_eq!(matched.id, "dev-1");
        assert_eq!(matched.expires_at_ms, 1000 + TOKEN_LIFETIME_MS);
        let devices = list_paired_devices(&conn).unwrap();
        assert_eq!(devices[0].last_seen_ms, Some(2000));

        assert!(authenticate(&conn, "wrong-token", 3000).unwrap().is_none());

        rename_paired_device(&conn, "dev-1", "Sarah's iPhone").unwrap();
        assert_eq!(
            list_paired_devices(&conn).unwrap()[0].name,
            "Sarah's iPhone"
        );

        revoke_paired_device(&conn, "dev-1").unwrap();
        assert!(list_paired_devices(&conn).unwrap().is_empty());
        assert!(authenticate(&conn, &token, 4000).unwrap().is_none());
    }

    #[test]
    fn token_hash_is_deterministic_and_salt_sensitive() {
        let token = "abc123";
        let salt = "salt1";
        assert_eq!(hash_token(token, salt), hash_token(token, salt));
        assert_ne!(hash_token(token, salt), hash_token(token, "salt2"));
    }
}
