//! The authoritative, persisted "current sync-key generation" register
//! (`sync_key_state`, `crate::app_db`'s `SCHEMA_V8`). Before this existed,
//! `crate::sync::pairing`'s `next_gen_n` recomputed `max(sync_devices.gen_added)`
//! — write-once pairing metadata (the generation a device was *added* in),
//! not a live counter — so it never actually advanced after a
//! device-removal key rotation. `pairing::next_gen_n` now delegates to
//! [`get`] here.
//!
//! Singleton row (`id = 1`, enforced by a `CHECK` constraint in the
//! migration) — same idiom as `crate::remote::store`'s `remote_access_config`
//! (`key = 'current'`), but typed integer columns since this needs
//! arithmetic comparison (`gen_n >= current_gen_n`), not just blob storage.
//!
//! `envelopes_since_rotation` backs the count-based half of the
//! periodic-rotation trigger (`pairing::rotate_key_periodically`) —
//! `docs/SYNC_PAIRING_PROTOCOL.md`'s "Revocation and rotation".

use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncKeyState {
    pub current_gen_n: u32,
    pub key_installed_at_ms: i64,
    pub envelopes_since_rotation: u32,
}

/// Reads the current state. If no row exists yet — an install that paired
/// before `SCHEMA_V8` existed — seeds `current_gen_n` from `roster::list`'s
/// max `gen_added` rather than defaulting to 0, so an upgrading install
/// doesn't regress its generation number. Not persisted by this read alone;
/// the first call to [`set_current_gen`]/[`record_envelopes`] is what
/// actually creates the row.
pub fn get(conn: &Connection) -> SyncKeyState {
    conn.query_row(
        "SELECT current_gen_n, key_installed_at_ms, envelopes_since_rotation \
         FROM sync_key_state WHERE id = 1",
        [],
        |row| {
            Ok(SyncKeyState {
                current_gen_n: row.get(0)?,
                key_installed_at_ms: row.get(1)?,
                envelopes_since_rotation: row.get(2)?,
            })
        },
    )
    .optional()
    .unwrap_or(None)
    .unwrap_or_else(|| {
        let current_gen_n = crate::sync::roster::list(conn)
            .iter()
            .map(|r| r.gen_added)
            .max()
            .unwrap_or(0);
        SyncKeyState {
            current_gen_n,
            key_installed_at_ms: 0,
            envelopes_since_rotation: 0,
        }
    })
}

/// Installs `gen_n` as current. Monotonic ONCE A ROW EXISTS: a no-op if
/// `gen_n` isn't strictly newer than what's already recorded — the same
/// idempotency/tie-break rule `pairing::apply_incoming_rotate` already
/// relies on for a duplicate or stale rotation message, applied here too so
/// two callers racing to record the same (or an out-of-order) generation
/// can't move the counter backwards. Resets `envelopes_since_rotation` to 0
/// whenever it actually advances — a freshly-installed key starts its own
/// count from zero.
///
/// The very FIRST call for a device (no row yet — called from
/// `pairing::create_group`/the pairing command handlers when a device
/// creates or joins a group) always writes, even though [`get`]'s seeded
/// default might equal `gen_n` exactly (a fresh group starts at generation
/// 0). Without this, a device's `key_installed_at_ms` would stay stuck at
/// [`get`]'s in-memory-only default of `0` forever (until a rotation
/// happens to touch it) — read as "the Unix epoch," which would make the
/// periodic-rotation age trigger fire on this device's very next tick.
pub fn set_current_gen(conn: &Connection, gen_n: u32, now_ms: i64) -> Result<()> {
    if row_exists(conn) && gen_n <= get(conn).current_gen_n {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO sync_key_state (id, current_gen_n, key_installed_at_ms, envelopes_since_rotation) \
         VALUES (1, ?1, ?2, 0) \
         ON CONFLICT(id) DO UPDATE SET \
            current_gen_n = excluded.current_gen_n, \
            key_installed_at_ms = excluded.key_installed_at_ms, \
            envelopes_since_rotation = 0",
        params![gen_n, now_ms],
    )?;
    Ok(())
}

fn row_exists(conn: &Connection) -> bool {
    conn.query_row("SELECT 1 FROM sync_key_state WHERE id = 1", [], |_| Ok(()))
        .optional()
        .unwrap_or(None)
        .is_some()
}

/// Adds `count` to `envelopes_since_rotation`, returns the new total.
/// `count == 0` is a no-op that still returns the current total (so a
/// caller can log/compare without special-casing an empty tick).
pub fn record_envelopes(conn: &Connection, count: u32) -> Result<u32> {
    let current = get(conn);
    if count == 0 {
        return Ok(current.envelopes_since_rotation);
    }
    let new_total = current.envelopes_since_rotation.saturating_add(count);
    conn.execute(
        "INSERT INTO sync_key_state (id, current_gen_n, key_installed_at_ms, envelopes_since_rotation) \
         VALUES (1, ?1, ?2, ?3) \
         ON CONFLICT(id) DO UPDATE SET envelopes_since_rotation = excluded.envelopes_since_rotation",
        params![current.current_gen_n, current.key_installed_at_ms, new_total],
    )?;
    Ok(new_total)
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
    fn get_defaults_to_zero_with_no_roster() {
        let conn = test_conn();
        let state = get(&conn);
        assert_eq!(state.current_gen_n, 0);
        assert_eq!(state.key_installed_at_ms, 0);
        assert_eq!(state.envelopes_since_rotation, 0);
    }

    #[test]
    fn get_seeds_from_roster_gen_added_when_no_row_exists_yet() {
        let conn = test_conn();
        crate::sync::roster::save(
            &conn,
            &crate::sync::roster::RosterEntry {
                device_id: "device-a".to_string(),
                identity_sign_pub: [1u8; 32],
                box_pub: [2u8; 32],
                label: "This Mac".to_string(),
                gen_added: 3,
                is_self: true,
            },
        )
        .unwrap();
        assert_eq!(
            get(&conn).current_gen_n,
            3,
            "must not regress an upgrading install's generation to 0"
        );
    }

    #[test]
    fn set_current_gen_round_trips_and_resets_envelope_count() {
        let conn = test_conn();
        record_envelopes(&conn, 5).unwrap();
        assert_eq!(get(&conn).envelopes_since_rotation, 5);

        set_current_gen(&conn, 1, 1_000).unwrap();
        let state = get(&conn);
        assert_eq!(state.current_gen_n, 1);
        assert_eq!(state.key_installed_at_ms, 1_000);
        assert_eq!(
            state.envelopes_since_rotation, 0,
            "a newly-installed generation resets the count"
        );
    }

    #[test]
    fn set_current_gen_writes_on_the_very_first_call_even_at_the_seeded_default() {
        let conn = test_conn();
        assert_eq!(
            get(&conn).current_gen_n,
            0,
            "sanity: a fresh install seeds at generation 0"
        );
        assert_eq!(
            get(&conn).key_installed_at_ms,
            0,
            "sanity: no row yet, so this is just get()'s in-memory default"
        );

        // A brand-new group also starts at generation 0 — `gen_n` here
        // isn't strictly greater than the seeded default, but this must
        // still persist a row (crucially, a real `key_installed_at_ms`),
        // or the periodic-rotation age trigger would see `0` (the Unix
        // epoch) as this key's install time forever.
        set_current_gen(&conn, 0, 5_000).unwrap();
        let state = get(&conn);
        assert_eq!(state.current_gen_n, 0);
        assert_eq!(
            state.key_installed_at_ms, 5_000,
            "the first call must always write, not no-op"
        );
    }

    #[test]
    fn set_current_gen_is_monotonic() {
        let conn = test_conn();
        set_current_gen(&conn, 5, 1_000).unwrap();
        record_envelopes(&conn, 3).unwrap();

        set_current_gen(&conn, 5, 2_000).unwrap();
        assert_eq!(
            get(&conn).key_installed_at_ms,
            1_000,
            "an equal gen_n must be a no-op"
        );
        assert_eq!(
            get(&conn).envelopes_since_rotation,
            3,
            "a no-op must not reset the count"
        );

        set_current_gen(&conn, 3, 3_000).unwrap();
        assert_eq!(
            get(&conn).current_gen_n,
            5,
            "an older gen_n must never move the counter backwards"
        );

        set_current_gen(&conn, 6, 4_000).unwrap();
        let state = get(&conn);
        assert_eq!(state.current_gen_n, 6);
        assert_eq!(state.key_installed_at_ms, 4_000);
        assert_eq!(state.envelopes_since_rotation, 0);
    }

    #[test]
    fn record_envelopes_accumulates() {
        let conn = test_conn();
        assert_eq!(record_envelopes(&conn, 0).unwrap(), 0);
        assert_eq!(record_envelopes(&conn, 4).unwrap(), 4);
        assert_eq!(record_envelopes(&conn, 6).unwrap(), 10);
        assert_eq!(get(&conn).envelopes_since_rotation, 10);
    }
}
