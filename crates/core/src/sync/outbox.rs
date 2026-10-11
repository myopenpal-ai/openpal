//! `sync_outbox` (`SCHEMA_V6`) — locally-queued encrypted envelopes waiting
//! to be pushed to the relay. CRUD only; nothing here knows what's inside a
//! `ciphertext` blob (see `crate::sync::envelope` for that) — this module's
//! only job is "remember what needs sending until it's acked."
//!
//! Callers write here in the SAME transaction as the local change that
//! produced the envelope (`crate::sync::conversations::apply_local` and its
//! `crate::sync::field_versions`-based counterparts for the other four
//! tables) — see those modules' doc comments for why that matters for
//! crash-safety.

use crate::Result;
use rusqlite::{params, Connection};

pub struct OutboxItem {
    pub envelope_id: String,
    pub mailbox_id: String,
    pub ciphertext: Vec<u8>,
}

/// Queues one already-encrypted envelope. `envelope_id` is caller-chosen
/// (the sync engine uses the change's own `Hlc::op_id()`-shaped string, so
/// re-queuing the same change twice — e.g. a retried write — naturally
/// dedups instead of sending a duplicate).
pub fn enqueue(
    conn: &Connection,
    envelope_id: &str,
    mailbox_id: &str,
    ciphertext: &[u8],
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO sync_outbox (envelope_id, mailbox_id, ciphertext, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
        params![envelope_id, mailbox_id, ciphertext, now_ms()],
    )?;
    Ok(())
}

/// Every currently-queued item, oldest first, capped at `limit` (the sync
/// loop drains in bounded batches rather than risking one giant push after
/// a long time offline).
pub fn drain(conn: &Connection, limit: usize) -> Result<Vec<OutboxItem>> {
    let mut stmt = conn.prepare("SELECT envelope_id, mailbox_id, ciphertext FROM sync_outbox ORDER BY created_at_ms ASC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(OutboxItem {
            envelope_id: row.get(0)?,
            mailbox_id: row.get(1)?,
            ciphertext: row.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Removes a queued item once the relay has confirmed the push succeeded.
/// Idempotent — acking an already-gone item is not an error.
pub fn ack(conn: &Connection, envelope_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM sync_outbox WHERE envelope_id = ?1",
        params![envelope_id],
    )?;
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
    fn enqueue_drain_ack_round_trip() {
        let conn = test_conn();
        assert!(drain(&conn, 10).unwrap().is_empty());

        enqueue(&conn, "op-1", "mailbox-a", b"ciphertext-1").unwrap();
        enqueue(&conn, "op-2", "mailbox-a", b"ciphertext-2").unwrap();

        let items = drain(&conn, 10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].envelope_id, "op-1");

        ack(&conn, "op-1").unwrap();
        let remaining = drain(&conn, 10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].envelope_id, "op-2");
    }

    #[test]
    fn enqueue_is_idempotent_by_envelope_id() {
        let conn = test_conn();
        enqueue(&conn, "op-1", "mailbox-a", b"first").unwrap();
        enqueue(&conn, "op-1", "mailbox-a", b"second-attempt-same-id").unwrap();
        assert_eq!(
            drain(&conn, 10).unwrap().len(),
            1,
            "re-queuing the same envelope_id must not duplicate it"
        );
    }

    #[test]
    fn drain_respects_limit_and_ack_is_idempotent() {
        let conn = test_conn();
        for i in 0..5 {
            enqueue(&conn, &format!("op-{i}"), "mailbox-a", b"x").unwrap();
        }
        assert_eq!(drain(&conn, 3).unwrap().len(), 3);
        ack(&conn, "does-not-exist").unwrap(); // must not error
    }
}
