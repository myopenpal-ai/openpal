//! Local roster persistence — the `sync_devices` table in the app-wide
//! SQLCipher database (`crate::app_db`, `SCHEMA_V5`). One row per device
//! this device has ever paired with (including itself, `is_self = true`),
//! covered by the same SQLCipher/DEK encryption as every other `app_db`
//! table — no separate wrap file needed, this is local metadata, not shared
//! secret material (`docs/SYNC_PAIRING_PROTOCOL.md`'s "Group creation" step
//! 5).
//!
//! Same idioms as `crate::projects`:
//! - `list` tolerates corrupt/unreadable rows (never fatal).
//! - `device_id`s come from the pairing protocol (should be UUID-like).
//!   [`validate_id`] keeps the same `[a-zA-Z0-9-]`-only charset check
//!   `crate::projects::validate_id` uses — input hygiene, not a
//!   path-traversal defense.
//! - `save` is an `INSERT OR REPLACE` upsert.
//!
//! Deliberately **not** here: any signature-verification logic (e.g.
//! "verify this signed message came from a device currently in the
//! roster"). That belongs to the pairing/rotation protocol implementation
//! (`crate::sync::pairing`, a sibling module), which calls `list`/`get`
//! here as a building block — this module is CRUD only.

use crate::{Error, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

/// One row of the local roster: a device this device has paired with (or,
/// for the `is_self` row, this device itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    pub device_id: String,
    /// This device's Ed25519 verifying-key bytes
    /// (`crate::crypto::device_identity::DeviceIdentityPublic::identity_sign_pub_bytes`).
    pub identity_sign_pub: [u8; 32],
    /// This device's X25519 static public-key bytes
    /// (`DeviceIdentityPublic::box_pub_bytes`).
    pub box_pub: [u8; 32],
    /// User-facing label chosen at pairing time (e.g. "Sarah's MacBook").
    pub label: String,
    /// The sync-group generation this device was added in
    /// (`docs/SYNC_PAIRING_PROTOCOL.md`'s `gen_n`).
    pub gen_added: u32,
    /// Whether this row describes the local device itself, rather than a
    /// peer.
    pub is_self: bool,
}

/// What the UI actually needs for a device list — deliberately excludes
/// `identity_sign_pub`/`box_pub` (raw key material with no reason to ever
/// reach the frontend; the webview has no network access and no use for
/// it, per `ARCHITECTURE.md`'s "Streaming via Tauri Channels" boundary).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub device_id: String,
    pub label: String,
    pub gen_added: u32,
    pub is_self: bool,
}

impl From<&RosterEntry> for DeviceSummary {
    fn from(entry: &RosterEntry) -> Self {
        Self {
            device_id: entry.device_id.clone(),
            label: entry.label.clone(),
            gen_added: entry.gen_added,
            is_self: entry.is_self,
        }
    }
}

/// [`list`], mapped to the UI-facing [`DeviceSummary`] shape. Used by the
/// `sync_device_list` Tauri command.
pub fn list_summaries(conn: &Connection) -> Vec<DeviceSummary> {
    list(conn).iter().map(DeviceSummary::from).collect()
}

pub fn list(conn: &Connection) -> Vec<RosterEntry> {
    let mut stmt = match conn.prepare(
        "SELECT device_id, identity_sign_pub, box_pub, label, gen_added, is_self \
         FROM sync_devices ORDER BY gen_added, device_id",
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("couldn't prepare the sync_devices list query: {e}");
            return Vec::new();
        }
    };
    let rows = match stmt.query_map([], row_to_entry) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("couldn't query sync_devices: {e}");
            return Vec::new();
        }
    };

    let mut entries = Vec::new();
    for row in rows {
        match row {
            Ok(entry) => entries.push(entry),
            Err(e) => tracing::warn!("skipping unreadable sync_devices row: {e}"),
        }
    }
    entries
}

pub fn get(conn: &Connection, device_id: &str) -> Result<RosterEntry> {
    validate_id(device_id)?;
    conn.query_row(
        "SELECT device_id, identity_sign_pub, box_pub, label, gen_added, is_self \
         FROM sync_devices WHERE device_id = ?1",
        params![device_id],
        row_to_entry,
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("device {device_id}")),
        other => Error::Db(other),
    })
}

/// Upsert `entry` — an `INSERT OR REPLACE`, so re-saving an existing
/// `device_id` (e.g. a label edit) overwrites the row in place.
pub fn save(conn: &Connection, entry: &RosterEntry) -> Result<()> {
    validate_id(&entry.device_id)?;
    conn.execute(
        "INSERT OR REPLACE INTO sync_devices \
         (device_id, identity_sign_pub, box_pub, label, gen_added, is_self) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.device_id,
            entry.identity_sign_pub.as_slice(),
            entry.box_pub.as_slice(),
            entry.label,
            entry.gen_added,
            entry.is_self,
        ],
    )?;
    Ok(())
}

/// Remove `device_id` from the roster. Idempotent, like
/// `crate::projects::delete` — removing an already-absent device is not an
/// error.
pub fn remove(conn: &Connection, device_id: &str) -> Result<()> {
    validate_id(device_id)?;
    conn.execute(
        "DELETE FROM sync_devices WHERE device_id = ?1",
        params![device_id],
    )?;
    Ok(())
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<RosterEntry> {
    let identity_sign_pub: Vec<u8> = row.get(1)?;
    let box_pub: Vec<u8> = row.get(2)?;
    Ok(RosterEntry {
        device_id: row.get(0)?,
        identity_sign_pub: blob32(identity_sign_pub)?,
        box_pub: blob32(box_pub)?,
        label: row.get(3)?,
        gen_added: row.get::<_, i64>(4)? as u32,
        is_self: row.get(5)?,
    })
}

/// Convert a `BLOB` column's bytes into a fixed 32-byte array, treating the
/// wrong length as a row-level extraction failure — same "a row that fails
/// to extract at the SQL level is skipped, not fatal" posture
/// `crate::projects::list` upholds via its own SQL-level failures.
fn blob32(bytes: Vec<u8>) -> rusqlite::Result<[u8; 32]> {
    let len = bytes.len();
    bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            len,
            rusqlite::types::Type::Blob,
            "expected exactly 32 bytes".into(),
        )
    })
}

/// Input hygiene, not a path-traversal defense — mirrors
/// `crate::projects::validate_id` exactly.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Error::NotFound(format!("device {id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn entry(device_id: &str, label: &str, gen_added: u32, is_self: bool) -> RosterEntry {
        RosterEntry {
            device_id: device_id.to_string(),
            identity_sign_pub: [1u8; 32],
            box_pub: [2u8; 32],
            label: label.to_string(),
            gen_added,
            is_self,
        }
    }

    #[test]
    fn save_get_list_remove_roundtrip() {
        let conn = test_conn();

        assert!(list(&conn).is_empty());

        save(&conn, &entry("device-a", "This Mac", 0, true)).unwrap();
        save(&conn, &entry("device-b", "Sarah's MacBook", 1, false)).unwrap();

        let all = list(&conn);
        assert_eq!(all.len(), 2);

        let got = get(&conn, "device-b").unwrap();
        assert_eq!(got.label, "Sarah's MacBook");
        assert_eq!(got.identity_sign_pub, [1u8; 32]);
        assert_eq!(got.box_pub, [2u8; 32]);
        assert_eq!(got.gen_added, 1);
        assert!(!got.is_self);

        remove(&conn, "device-b").unwrap();
        remove(&conn, "device-b").unwrap(); // idempotent
        assert!(matches!(get(&conn, "device-b"), Err(Error::NotFound(_))));
        assert_eq!(list(&conn).len(), 1);
        assert!(list(&conn)[0].is_self);
    }

    #[test]
    fn save_upserts_an_existing_device() {
        let conn = test_conn();
        save(&conn, &entry("device-a", "Old Label", 0, false)).unwrap();
        save(&conn, &entry("device-a", "New Label", 0, false)).unwrap();

        let all = list(&conn);
        assert_eq!(all.len(), 1, "upsert must not create a duplicate row");
        assert_eq!(get(&conn, "device-a").unwrap().label, "New Label");
    }

    #[test]
    fn get_on_a_missing_device_is_not_found() {
        let conn = test_conn();
        assert!(matches!(
            get(&conn, "no-such-device"),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn hostile_ids_are_rejected() {
        let conn = test_conn();
        for id in ["", "../evil", "a/b", "a.b", "id with spaces", "café"] {
            assert!(
                matches!(get(&conn, id), Err(Error::NotFound(_))),
                "get {id:?}"
            );
            assert!(
                matches!(remove(&conn, id), Err(Error::NotFound(_))),
                "remove {id:?}"
            );
            assert!(
                matches!(
                    save(&conn, &entry(id, "x", 0, false)),
                    Err(Error::NotFound(_))
                ),
                "save {id:?}"
            );
        }
    }
}
