//! Append-only, hash-chained tool-call lifecycle log — rows in the app-wide
//! SQLCipher database's `agent_audit` table (`crate::app_db`), not a
//! standalone file. This needs no key of its own: `PRAGMA key` is applied
//! once when `app_db::open` runs, so every read/write through that
//! connection — this one included — is transparently encrypted. Per-tool-call
//! volume is orders of magnitude below the app log's per-tracing-event
//! volume, so contending for `app_db`'s single `Mutex<Connection>` here is a
//! non-issue the way it would be for `crate::engine`/`crates::app::logging`.
//!
//! Each row's `prevHash` (inside its JSON `line`) is the SHA-256 of the
//! previous row's exact `line` bytes (genesis: 64 zeros), so a retroactive
//! edit to an already-written row breaks every row's chain after it. This
//! is honest, limited tamper-evidence, and — now that the table lives
//! inside an encrypted, page-integrity-checked database — a narrower,
//! *complementary* one: SQLCipher's per-page HMAC already catches a bit-flip
//! or truncation (and does so earlier, on read, rather than only on an
//! explicit `verify()` walk), but only says "this page is corrupt," not
//! *which* event was altered. The chain still pinpoints the exact row where
//! history diverges, and — against an attacker who has the DEK itself, where
//! SQLCipher's HMAC gives no protection at all — still forces a forger to
//! recompute every `prevHash` after the tampered row, not just re-seal one
//! page. Neither mechanism catches wholesale rollback to a stale-but-valid
//! backup; that gap is store-wide (see `docs/SPACES_AUTHORING.md` §9's
//! `keycheck`/`write_counter` discussion) and isn't specific to this table.
//! It also doesn't carry full argument/output content (that stays in the —
//! deletable — conversation transcript, capped at a short preview here):
//! this is a lifecycle/authorization trail, not a full recorder.

use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub ts_ms: u64,
    pub turn_id: String,
    pub call_id: String,
    pub tool_id: String,
    /// One of: "consentRequested" | "decided" | "started" | "finished" |
    /// "rejected" | "injectionReview" (the user's answer to a
    /// prompt-injection review of the call's result: `decision` is
    /// "continued" or "stopped").
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    /// Populated whenever this event was an auto-approved grant hit, so a
    /// later audit can tell which standing grant authorized it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl AuditEvent {
    pub fn new(turn_id: &str, call_id: &str, tool_id: &str, phase: &str) -> Self {
        Self {
            ts_ms: super::consent::now_ms(),
            turn_id: turn_id.to_string(),
            call_id: call_id.to_string(),
            tool_id: tool_id.to_string(),
            phase: phase.to_string(),
            decision: None,
            ok: None,
            grant_key: None,
            reason: None,
        }
    }
}

const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub struct AuditLog {
    conn: Arc<Mutex<Connection>>,
    prev_hash: Mutex<String>,
}

impl AuditLog {
    pub fn open(conn: Arc<Mutex<Connection>>) -> Self {
        let prev_hash =
            last_line_hash(&conn.lock().unwrap()).unwrap_or_else(|| GENESIS.to_string());
        Self {
            conn,
            prev_hash: Mutex::new(prev_hash),
        }
    }

    pub fn append(&self, event: &AuditEvent) {
        let mut prev = self.prev_hash.lock().unwrap();
        let mut record = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(_) => return,
        };
        if let serde_json::Value::Object(map) = &mut record {
            map.insert(
                "prevHash".to_string(),
                serde_json::Value::String(prev.clone()),
            );
        }
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        let conn = self.conn.lock().unwrap();
        let inserted = conn.execute(
            "INSERT INTO agent_audit (ts_ms, line) VALUES (?1, ?2)",
            params![event.ts_ms as i64, line],
        );
        if inserted.is_ok() {
            *prev = hash_line(&line);
        }
    }

    /// Verify the chain from the oldest row. Returns `(valid_row_count,
    /// intact)` — `intact` is `false` as soon as a row's recorded `prevHash`
    /// doesn't match the hash of the row before it.
    pub fn verify(&self) -> (usize, bool) {
        verify_chain(&self.conn.lock().unwrap())
    }

    /// Every entry so far, oldest first.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<serde_json::Value> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT line FROM agent_audit ORDER BY id ASC")
            .unwrap();
        let lines = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
            .collect();
        lines
    }
}

fn hash_line(line: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(line.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn last_line_hash(conn: &Connection) -> Option<String> {
    let last: String = conn
        .query_row(
            "SELECT line FROM agent_audit ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()?;
    Some(hash_line(&last))
}

fn verify_chain(conn: &Connection) -> (usize, bool) {
    let mut stmt = match conn.prepare("SELECT line FROM agent_audit ORDER BY id ASC") {
        Ok(stmt) => stmt,
        Err(_) => return (0, true),
    };
    let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(_) => return (0, true),
    };

    let mut expected_prev = GENESIS.to_string();
    let mut count = 0;
    for row in rows {
        let Ok(line) = row else {
            return (count, false);
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            return (count, false);
        };
        let recorded_prev = value.get("prevHash").and_then(|v| v.as_str()).unwrap_or("");
        if recorded_prev != expected_prev {
            return (count, false);
        }
        expected_prev = hash_line(&line);
        count += 1;
    }
    (count, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Arc<Mutex<Connection>> {
        let dir = tempfile::tempdir().unwrap().keep();
        Arc::new(Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ))
    }

    #[test]
    fn appended_events_form_a_valid_chain() {
        let log = AuditLog::open(test_conn());
        for i in 0..5 {
            log.append(&AuditEvent::new(
                "turn1",
                &format!("call{i}"),
                "run_shell",
                "started",
            ));
        }
        let (count, intact) = log.verify();
        assert_eq!(count, 5);
        assert!(intact);
    }

    #[test]
    fn a_corrupted_line_breaks_the_chain_from_that_point() {
        let conn = test_conn();
        let log = AuditLog::open(conn.clone());
        for i in 0..3 {
            log.append(&AuditEvent::new(
                "turn1",
                &format!("call{i}"),
                "run_shell",
                "started",
            ));
        }
        // Tamper with the middle row's content without touching its
        // recorded prevHash — its own now-wrong hash breaks the *next*
        // row's chain link, not this row's own recorded prevHash.
        {
            let c = conn.lock().unwrap();
            let line: String = c
                .query_row("SELECT line FROM agent_audit WHERE id = 2", [], |r| {
                    r.get(0)
                })
                .unwrap();
            let tampered = line.replace("run_shell", "run_shell_TAMPERED");
            c.execute(
                "UPDATE agent_audit SET line = ?1 WHERE id = 2",
                params![tampered],
            )
            .unwrap();
        }

        let (count, intact) = AuditLog::open(conn).verify();
        assert!(!intact);
        assert_eq!(count, 2, "the first two rows (0 and the tampered one) still parse; the break shows up validating row 2 against the tampered row's now-different hash");
    }

    #[test]
    fn a_fresh_process_continues_the_chain_from_the_db() {
        let conn = test_conn();
        AuditLog::open(conn.clone()).append(&AuditEvent::new("t", "c1", "run_shell", "started"));
        let log2 = AuditLog::open(conn);
        log2.append(&AuditEvent::new("t", "c2", "run_shell", "finished"));
        let (count, intact) = log2.verify();
        assert_eq!(count, 2);
        assert!(intact);
    }
}
