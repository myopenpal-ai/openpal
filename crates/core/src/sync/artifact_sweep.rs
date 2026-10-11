//! Phase 4 artifact sync's trigger mechanism — a periodic reconciliation
//! sweep, not a write-time capture like every other synced table
//! (conversations/projects/settings/custom_models all route through
//! `crate::sync::capture::*_synced` directly from `crates/app/src/
//! commands.rs`, which has `AppState` access). The only artifact WRITE call
//! site in the whole app is the agent-mode `create_artifact` tool
//! (`crate::agent::builtin::artifact::CreateArtifactTool`), constructed with
//! just a raw `Connection` + conversation id — no `Settings`/`HlcClock`/
//! `SyncKey` access, and that module's own doc comment already explains why
//! widening its constructor for this one call site was rejected. Rather
//! than thread sync dependencies through agent-mode's consent-sensitive
//! plumbing, `crates/app/src/sync_loop.rs` (which already has full
//! `AppState` access for push/pull) periodically calls [`sweep_once`] here
//! to notice and push anything that changed since the last pass. This also
//! means any *future* artifact-write call site is covered automatically,
//! with no per-call-site retrofit needed.
//!
//! This module holds the logic, not `sync_loop.rs`, because `crates/core/
//! tests/sync_integration.rs` cannot depend on `crates/app` and needs to
//! call this directly — exactly like `capture::save_conversation_synced`
//! already is.

use crate::crypto::sync_key::SyncKey;
use crate::sync::hlc::HlcClock;
use crate::sync::{artifact_push_state, capture};
use crate::Result;
use rusqlite::Connection;

/// Fixed, non-user-configurable — far below `crate::artifacts::
/// MAX_ARTIFACT_BYTES` (5 MB), which remains the absolute local-storage
/// ceiling regardless of sync. An artifact over this cap simply stays
/// local-only; it never enters the sync pipeline at all. Checked first,
/// before any hash comparison or `Hlc` minting, so an oversized artifact
/// never reaches `sync_outbox` — this is what makes
/// `crates/app/src/sync_loop.rs`'s "leave a failed push queued, retry next
/// tick" posture safe: there is no artifact this loop would retry forever,
/// because a permanently-too-large one is never queued in the first place.
pub const ARTIFACT_SYNC_MAX_BYTES: usize = 1024 * 1024; // 1 MiB

/// One sweep pass's outcome, for logging/observability — not itself
/// load-bearing for correctness.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    pub pushed: usize,
    pub skipped_unchanged: usize,
    pub skipped_oversized: usize,
    pub failed: usize,
}

/// Scans every artifact, pushing any whose content is new or has changed
/// since the last sweep. Returns `Err` only for a sweep-fatal issue (the
/// `artifacts` table itself couldn't be queried) — a single artifact's
/// failure to push is logged into `SweepStats::failed` and the sweep
/// continues with the rest, mirroring `crates/app/src/sync_loop.rs::
/// route_pulled_envelope`'s already-established "one bad item never aborts
/// the batch" posture.
pub fn sweep_once(
    conn: &mut Connection,
    clock: &HlcClock,
    sync_key: &SyncKey,
    mailbox_id: &str,
) -> Result<SweepStats> {
    // Collected into a Vec first — the statement can't stay open across the
    // `&mut Connection` borrow `capture::save_artifact_synced` needs per row.
    let rows: Vec<(String, String, String, String)> = {
        let mut stmt =
            conn.prepare("SELECT conversation_id, id, filename, content FROM artifacts")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        out
    };

    let mut stats = SweepStats::default();
    for (conversation_id, artifact_id, filename, content) in rows {
        if content.len() > ARTIFACT_SYNC_MAX_BYTES {
            stats.skipped_oversized += 1;
            continue;
        }

        let current_hash = artifact_push_state::hash_content(&content);
        if artifact_push_state::get(conn, &conversation_id, &artifact_id).as_deref()
            == Some(current_hash.as_str())
        {
            stats.skipped_unchanged += 1;
            continue;
        }

        match capture::save_artifact_synced(
            conn,
            &conversation_id,
            &artifact_id,
            &filename,
            &content,
            clock,
            sync_key,
            mailbox_id,
        ) {
            Ok(_) => stats.pushed += 1,
            Err(e) => {
                tracing::warn!(
                    "sync: artifact sweep failed to push {conversation_id}:{artifact_id}: {e}"
                );
                stats.failed += 1;
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::outbox;

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn seed_conversation(conn: &Connection, id: &str) {
        crate::store::save(
            conn,
            &crate::types::Conversation {
                id: id.to_string(),
                title: "Untitled".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
                messages: Vec::new(),
                sampling: crate::types::SamplingParams::default(),
                context_length: 8192,
                task_type: crate::types::TaskType::General,
                project_id: None,
                pinned: false,
                branches: std::collections::HashMap::new(),
                forked_from: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn sweep_once_pushes_a_new_artifact_and_records_its_hash() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        seed_conversation(&conn, "conv-1");
        crate::artifacts::save(&conn, "conv-1", "art-1", "report.md", "# Hello").unwrap();

        let stats = sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        assert_eq!(
            stats,
            SweepStats {
                pushed: 1,
                ..Default::default()
            }
        );
        assert_eq!(outbox::drain(&conn, 10).unwrap().len(), 1);
        assert_eq!(
            artifact_push_state::get(&conn, "conv-1", "art-1"),
            Some(artifact_push_state::hash_content("# Hello"))
        );
    }

    #[test]
    fn sweep_once_skips_an_artifact_whose_content_is_unchanged() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        seed_conversation(&conn, "conv-1");
        crate::artifacts::save(&conn, "conv-1", "art-1", "report.md", "# Hello").unwrap();

        sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        outbox::drain(&conn, 10)
            .unwrap()
            .into_iter()
            .for_each(|item| outbox::ack(&conn, &item.envelope_id).unwrap());

        let stats = sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        assert_eq!(
            stats,
            SweepStats {
                skipped_unchanged: 1,
                ..Default::default()
            }
        );
        assert!(
            outbox::drain(&conn, 10).unwrap().is_empty(),
            "unchanged content must not be re-queued"
        );
    }

    #[test]
    fn sweep_once_detects_a_changed_artifact_and_pushes_again() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        seed_conversation(&conn, "conv-1");
        crate::artifacts::save(&conn, "conv-1", "art-1", "report.md", "# Hello").unwrap();
        sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        outbox::drain(&conn, 10)
            .unwrap()
            .into_iter()
            .for_each(|item| outbox::ack(&conn, &item.envelope_id).unwrap());

        crate::artifacts::save(&conn, "conv-1", "art-1", "report.md", "# Hello, edited").unwrap();
        let stats = sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        assert_eq!(
            stats,
            SweepStats {
                pushed: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            artifact_push_state::get(&conn, "conv-1", "art-1"),
            Some(artifact_push_state::hash_content("# Hello, edited"))
        );
    }

    #[test]
    fn sweep_once_never_touches_the_outbox_or_push_state_for_an_oversized_artifact() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        seed_conversation(&conn, "conv-1");
        let oversized = "x".repeat(ARTIFACT_SYNC_MAX_BYTES + 1);
        crate::artifacts::save(&conn, "conv-1", "art-1", "big.txt", &oversized).unwrap();

        let stats = sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        assert_eq!(
            stats,
            SweepStats {
                skipped_oversized: 1,
                ..Default::default()
            }
        );
        assert!(
            outbox::drain(&conn, 10).unwrap().is_empty(),
            "an oversized artifact must never reach the outbox"
        );
        assert_eq!(
            artifact_push_state::get(&conn, "conv-1", "art-1"),
            None,
            "an oversized artifact's hash must never be recorded"
        );
    }

    #[test]
    fn sweep_once_processes_multiple_artifacts_independently() {
        let mut conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let sync_key = SyncKey::random();
        seed_conversation(&conn, "conv-1");
        crate::artifacts::save(&conn, "conv-1", "art-1", "a.txt", "content a").unwrap();
        crate::artifacts::save(&conn, "conv-1", "art-2", "b.txt", "content b").unwrap();
        sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        outbox::drain(&conn, 10)
            .unwrap()
            .into_iter()
            .for_each(|item| outbox::ack(&conn, &item.envelope_id).unwrap());

        // Only art-1 changes.
        crate::artifacts::save(&conn, "conv-1", "art-1", "a.txt", "content a, edited").unwrap();
        let stats = sweep_once(&mut conn, &clock, &sync_key, "mailbox-1").unwrap();
        assert_eq!(
            stats,
            SweepStats {
                pushed: 1,
                skipped_unchanged: 1,
                ..Default::default()
            }
        );
    }
}
