//! `sync_artifact_push_state` (`SCHEMA_V9`) — a local-only dedup cache for
//! Phase 4 artifact sync: one row per `(conversation_id, artifact_id)`
//! recording the SHA-256 hex digest of the content that was last *enqueued*
//! for sync. `crate::sync::artifact_sweep`'s periodic reconciliation pass
//! uses this to tell "already pushed, content unchanged" from "new or
//! edited since the last sweep" without re-transferring identical content
//! every pass.
//!
//! Deliberately not `crate::sync::field_versions`: that table's `Hlc`
//! answers "who wins a cross-device conflict" — a fundamentally different
//! question from this one's "is the current content the same thing I
//! already queued." This cache is never itself synced and never compared
//! against a remote value; contaminating `field_versions`' scope/row_key →
//! `Hlc` contract with an unrelated hash-comparison use case would blur
//! what that table means.

use crate::Result;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

/// SHA-256 hex digest of `content` — same primitive already used by
/// `crate::agent::builtin::artifact::CreateArtifactTool`'s `grant_key`
/// computation, applied here to a different (dedup, not consent) purpose.
pub fn hash_content(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The content hash last recorded as enqueued for `(conversation_id,
/// artifact_id)`, or `None` if this artifact has never been swept.
pub fn get(conn: &Connection, conversation_id: &str, artifact_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT content_hash FROM sync_artifact_push_state WHERE conversation_id = ?1 AND artifact_id = ?2",
        params![conversation_id, artifact_id],
        |row| row.get(0),
    )
    .ok()
}

/// Upserts the recorded hash. Called from `crate::sync::capture::
/// save_artifact_synced`'s own transaction, right after enqueueing — see
/// that function's doc comment for why "recorded at enqueue time" (not
/// relay-ack time) is the correct choice.
pub fn set(
    conn: &Connection,
    conversation_id: &str,
    artifact_id: &str,
    content_hash: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_artifact_push_state (conversation_id, artifact_id, content_hash) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(conversation_id, artifact_id) DO UPDATE SET content_hash = excluded.content_hash",
        params![conversation_id, artifact_id, content_hash],
    )?;
    Ok(())
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
    fn get_returns_none_when_nothing_recorded() {
        let conn = test_conn();
        assert_eq!(get(&conn, "conv-1", "art-1"), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let conn = test_conn();
        set(&conn, "conv-1", "art-1", "abc123").unwrap();
        assert_eq!(get(&conn, "conv-1", "art-1"), Some("abc123".to_string()));
    }

    #[test]
    fn set_upserts_overwriting_a_previous_hash() {
        let conn = test_conn();
        set(&conn, "conv-1", "art-1", "old-hash").unwrap();
        set(&conn, "conv-1", "art-1", "new-hash").unwrap();
        assert_eq!(
            get(&conn, "conv-1", "art-1"),
            Some("new-hash".to_string()),
            "a second set must replace, not duplicate"
        );
    }

    #[test]
    fn hash_content_is_deterministic_and_content_sensitive() {
        assert_eq!(
            hash_content("hello"),
            hash_content("hello"),
            "same input must hash identically"
        );
        assert_ne!(
            hash_content("hello"),
            hash_content("hello!"),
            "different input must hash differently"
        );
    }

    #[test]
    fn different_artifacts_are_tracked_independently() {
        let conn = test_conn();
        set(&conn, "conv-1", "art-1", "hash-a").unwrap();
        set(&conn, "conv-1", "art-2", "hash-b").unwrap();
        assert_eq!(get(&conn, "conv-1", "art-1"), Some("hash-a".to_string()));
        assert_eq!(get(&conn, "conv-1", "art-2"), Some("hash-b".to_string()));
    }
}
