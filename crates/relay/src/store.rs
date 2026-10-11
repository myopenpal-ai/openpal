//! SQLite-backed mailbox store. One table, no crypto: rows hold whatever
//! opaque `ciphertext` bytes the caller pushed (Phase 1: synthetic/test data
//! only — see `main.rs`'s module doc).

use rusqlite::{params, Connection};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// A pending envelope as stored (ciphertext still raw bytes — base64 is a
/// wire-layer concern handled in `routes.rs`, not persisted here).
pub struct StoredRow {
    pub envelope_id: i64,
    pub version: u32,
    pub ciphertext: Vec<u8>,
    pub created_at_ms: i64,
}

/// Current time in epoch milliseconds. Shared by `pull`'s expiry filter,
/// `pending_count`, and `sweep_expired` so "now" is computed the same way
/// everywhere in this file.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Cheaply cloneable handle to the mailbox table. `rusqlite::Connection` is
/// `!Sync`, so the connection is guarded by a `Mutex` and shared via `Arc`
/// for use as axum `State`. This is a skeleton store (single connection, no
/// pool) — fine for Phase 1's plumbing-proving purpose, not a production
/// concurrency design.
#[derive(Clone)]
pub struct MailboxStore {
    conn: Arc<Mutex<Connection>>,
}

impl MailboxStore {
    /// Opens (creating if needed) the SQLite file at `path` and ensures the
    /// schema exists.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// In-memory store — used by tests and available for local experiments
    /// that don't want a file on disk. Not called from `main.rs`'s release
    /// build path, hence the `allow`.
    #[allow(dead_code)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn)
    }

    fn from_connection(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mailbox (
                envelope_id INTEGER PRIMARY KEY AUTOINCREMENT,
                mailbox_id TEXT NOT NULL,
                version INTEGER NOT NULL,
                ciphertext BLOB NOT NULL,
                created_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_mailbox_id ON mailbox(mailbox_id);
            CREATE INDEX IF NOT EXISTS idx_expires_at_ms ON mailbox(expires_at_ms);",
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Inserts a pending envelope and returns its assigned `envelope_id`.
    /// `ttl_seconds` is caller-supplied (already clamped by `routes.rs` to
    /// the relay's allowed range) and is turned into an absolute
    /// `expires_at_ms = created_at_ms + ttl_seconds * 1000` here.
    pub fn push(
        &self,
        mailbox_id: &str,
        version: u32,
        ciphertext: &[u8],
        created_at_ms: i64,
        ttl_seconds: u64,
    ) -> rusqlite::Result<i64> {
        let expires_at_ms = created_at_ms + (ttl_seconds as i64) * 1000;
        let conn = self.conn.lock().expect("mailbox store mutex poisoned");
        conn.execute(
            "INSERT INTO mailbox (mailbox_id, version, ciphertext, created_at_ms, expires_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mailbox_id, version, ciphertext, created_at_ms, expires_at_ms],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Returns all non-expired pending envelopes for `mailbox_id`, oldest
    /// first. A row past its `expires_at_ms` is never returned even if
    /// `sweep_expired` hasn't run yet — expiry is enforced at read time, the
    /// sweep is just housekeeping to reclaim storage.
    pub fn pull(&self, mailbox_id: &str) -> rusqlite::Result<Vec<StoredRow>> {
        let conn = self.conn.lock().expect("mailbox store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT envelope_id, version, ciphertext, created_at_ms
             FROM mailbox WHERE mailbox_id = ?1 AND expires_at_ms > ?2 ORDER BY envelope_id ASC",
        )?;
        let rows = stmt
            .query_map(params![mailbox_id, now_ms()], |row| {
                Ok(StoredRow {
                    envelope_id: row.get(0)?,
                    version: row.get(1)?,
                    ciphertext: row.get(2)?,
                    created_at_ms: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Deletes one envelope by id, scoped to `mailbox_id`. Deleting a row
    /// that doesn't exist (already acked, wrong id, wrong mailbox, or
    /// already expired) is not an error — the ack is idempotent by design.
    pub fn ack(&self, mailbox_id: &str, envelope_id: i64) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("mailbox store mutex poisoned");
        conn.execute(
            "DELETE FROM mailbox WHERE mailbox_id = ?1 AND envelope_id = ?2",
            params![mailbox_id, envelope_id],
        )?;
        Ok(())
    }

    /// Count of non-expired pending envelopes for `mailbox_id`, consistent
    /// with `pull`'s filtering — an expired backlog that hasn't been swept
    /// yet must not count against the per-mailbox cap. Used by
    /// `routes.rs::push_envelope` to enforce the 200-envelope flood cap.
    pub fn pending_count(&self, mailbox_id: &str) -> rusqlite::Result<i64> {
        let conn = self.conn.lock().expect("mailbox store mutex poisoned");
        conn.query_row(
            "SELECT COUNT(*) FROM mailbox WHERE mailbox_id = ?1 AND expires_at_ms > ?2",
            params![mailbox_id, now_ms()],
            |row| row.get(0),
        )
    }

    /// A real dependency check for `GET /health` — not just "the process is
    /// alive," which would miss the actual failure mode that matters for a
    /// small self-hosted service (the SQLite file becoming unwritable/
    /// disk-full/corrupted while the TCP listener is still happily
    /// accepting connections). Cheap: one mutex lock, no table scan.
    pub fn health_check(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().expect("mailbox store mutex poisoned");
        conn.query_row("SELECT 1", [], |_| Ok(()))
    }

    /// Global housekeeping sweep: deletes every row (across ALL mailboxes,
    /// not scoped to one) whose `expires_at_ms` has passed, regardless of
    /// ack state. Returns the number of rows deleted. Called periodically
    /// from `main.rs`; `pull`/`pending_count` already filter expired rows
    /// out on their own, so this is purely about reclaiming storage rather
    /// than correctness.
    pub fn sweep_expired(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().expect("mailbox store mutex poisoned");
        conn.execute(
            "DELETE FROM mailbox WHERE expires_at_ms <= ?1",
            params![now_ms()],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_pull_round_trips_a_non_expired_envelope() {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        let mailbox_id = "test-mailbox";

        store
            .push(mailbox_id, 1, b"hello", now_ms(), 600)
            .expect("push");

        let rows = store.pull(mailbox_id).expect("pull");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ciphertext, b"hello");
    }

    #[test]
    fn expired_envelope_is_not_returned_by_pull_even_before_sweep() {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        let mailbox_id = "test-mailbox";

        // created 10s ago with a 1s ttl: already expired, but sweep_expired
        // is never called in this test — pull must still filter it out.
        store
            .push(mailbox_id, 1, b"stale", now_ms() - 10_000, 1)
            .expect("push");

        let rows = store.pull(mailbox_id).expect("pull");
        assert!(
            rows.is_empty(),
            "expired row should not be returned by pull"
        );
    }

    #[test]
    fn sweep_expired_deletes_expired_rows_and_leaves_non_expired_ones() {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        let mailbox_id = "test-mailbox";

        // One already-expired row...
        store
            .push(mailbox_id, 1, b"stale", now_ms() - 10_000, 1)
            .expect("push expired");
        // ...and one that's still well within its ttl.
        store
            .push(mailbox_id, 1, b"fresh", now_ms(), 600)
            .expect("push fresh");

        let deleted = store.sweep_expired().expect("sweep");
        assert_eq!(deleted, 1, "sweep should delete exactly the expired row");

        let rows = store.pull(mailbox_id).expect("pull");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ciphertext, b"fresh");
    }

    #[test]
    fn health_check_succeeds_against_an_open_store() {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        assert!(store.health_check().is_ok());
    }

    #[test]
    fn pending_count_reflects_only_non_expired_rows() {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        let mailbox_id = "test-mailbox";

        assert_eq!(store.pending_count(mailbox_id).expect("count"), 0);

        // An expired row (not yet swept) must not count.
        store
            .push(mailbox_id, 1, b"stale", now_ms() - 10_000, 1)
            .expect("push expired");
        assert_eq!(
            store.pending_count(mailbox_id).expect("count"),
            0,
            "an expired-but-unswept row should not count as pending"
        );

        // Non-expired rows do count.
        store
            .push(mailbox_id, 1, b"fresh-1", now_ms(), 600)
            .expect("push fresh 1");
        store
            .push(mailbox_id, 1, b"fresh-2", now_ms(), 600)
            .expect("push fresh 2");
        assert_eq!(store.pending_count(mailbox_id).expect("count"), 2);
    }
}
