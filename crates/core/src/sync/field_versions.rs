//! Generic last-writer-wins version tracking (`sync_field_versions`,
//! `SCHEMA_V6`), used by every synced table except conversations' message
//! list (which merges rather than replaces — see
//! `crate::sync::conversations`).
//!
//! `scope` names which "thing" is being versioned:
//! - `"projects"` / `"artifacts"` / `"settings"` / `"custom_models"` —
//!   whole-row LWW for those four existing tables, `row_key` is the row's
//!   own primary key (`"current"` for the singleton settings/
//!   custom_models tables, per `crate::settings`/`crate::custom`'s
//!   existing storage shape).
//! - `"conversation_title"` / `"conversation_project"` — two INDEPENDENT
//!   registers for two `conversations` columns, `row_key` is the
//!   conversation id. Kept as separate scopes (not one shared
//!   "conversation metadata" version) deliberately: an earlier draft of
//!   this design used one shared HLC for both fields and a design review
//!   found it silently drops one field's edit even on the "winning" side
//!   when both are edited concurrently on different devices — renaming a
//!   chat and moving it to a project are both common, unrelated edits.
//! - `"conversation_unread"` — the sidebar's "New reply" hint
//!   (`conversations.unread`, see `crate::app_db`'s `SCHEMA_V12`), a third
//!   independent register for the same reason, `row_key` is the
//!   conversation id. Unlike title/project it never rides on a
//!   `Conversation` save: `crate::sync::capture::set_conversation_unread_synced`
//!   is its only local writer.
//! - `"conversation_snapshot"` — the whole-conversation LWW fallback
//!   `crate::sync::conversations` uses when it detects a rewrite/
//!   truncation (an edited-and-regenerated message) rather than a pure
//!   append, `row_key` is the conversation id.

use crate::sync::hlc::Hlc;
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// The currently-recorded version for `(scope, row_key)`, if any has ever
/// been recorded (locally or via a synced write).
pub fn get(conn: &Connection, scope: &str, row_key: &str) -> Option<Hlc> {
    conn.query_row(
        "SELECT hlc_ms, hlc_counter, hlc_device_id FROM sync_field_versions WHERE scope = ?1 AND row_key = ?2",
        params![scope, row_key],
        |row| {
            Ok(Hlc {
                physical_ms: row.get(0)?,
                counter: row.get(1)?,
                device_id: row.get(2)?,
            })
        },
    )
    .optional()
    .unwrap_or(None)
}

fn set(conn: &Connection, scope: &str, row_key: &str, hlc: &Hlc) -> Result<()> {
    conn.execute(
        "INSERT INTO sync_field_versions (scope, row_key, hlc_ms, hlc_counter, hlc_device_id) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(scope, row_key) DO UPDATE SET \
            hlc_ms = excluded.hlc_ms, hlc_counter = excluded.hlc_counter, hlc_device_id = excluded.hlc_device_id",
        params![scope, row_key, hlc.physical_ms, hlc.counter, hlc.device_id],
    )?;
    Ok(())
}

/// A purely local write always "wins" trivially — there's nothing to
/// conflict with yet, it's this device's own new opinion. Mints a fresh
/// `Hlc` via `clock`, records it as the current version for `(scope,
/// row_key)`, and returns it — the caller includes this `Hlc` in the
/// outbox envelope it queues for the change (see
/// `crate::sync::conversations::apply`'s doc comment for why that queuing
/// happens in the same transaction as the version bump).
pub fn record_local(
    conn: &Connection,
    scope: &str,
    row_key: &str,
    clock: &crate::sync::hlc::HlcClock,
) -> Result<Hlc> {
    let hlc = clock.next();
    set(conn, scope, row_key, &hlc)?;
    Ok(hlc)
}

/// A remote write must be compared against whatever's already on record.
/// Returns `true` ("wins — the caller must apply it via the target table's
/// existing `save`/`delete` function, and this call already recorded the
/// new version") or `false` ("loses — the caller must NOT apply it, and
/// must instead write it to a conflict file; the stored version is
/// unchanged"). A `(scope, row_key)` with no recorded version yet always
/// wins (there is nothing to conflict with — this is the first anyone has
/// ever synced about this row).
pub fn try_apply_remote(
    conn: &Connection,
    scope: &str,
    row_key: &str,
    incoming: &Hlc,
) -> Result<bool> {
    let current = get(conn, scope, row_key);
    let wins = match &current {
        None => true,
        Some(existing) => incoming > existing,
    };
    if wins {
        set(conn, scope, row_key, incoming)?;
    }
    Ok(wins)
}

/// Writes a losing remote write's content to a conflict file — for any
/// `scope` in this module, never silently dropped. `content` is whatever
/// the caller would otherwise have applied (e.g. the `Project`/settings
/// JSON/etc. carried by the losing envelope); serialized as pretty JSON.
/// Mirrors `crate::sync::conversations`' own (private, conversation-
/// specific) conflict-file writer — kept as a separate small function here
/// rather than shared, since that module's version is tied to
/// `Conversation` specifically and this one is generic over any
/// `Serialize` payload.
pub fn write_conflict_file<T: serde::Serialize>(
    conflicts_dir: &std::path::Path,
    scope: &str,
    row_key: &str,
    hlc: &Hlc,
    content: &T,
) -> Result<()> {
    std::fs::create_dir_all(conflicts_dir).map_err(crate::Error::Io)?;
    let path = conflicts_dir.join(format!("{scope}-{row_key}-{}.json", hlc.op_id()));
    let json = serde_json::to_string_pretty(content)?;
    std::fs::write(path, json).map_err(crate::Error::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::hlc::HlcClock;

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    #[test]
    fn record_local_always_wins_and_persists() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        assert!(get(&conn, "projects", "proj-1").is_none());

        let hlc = record_local(&conn, "projects", "proj-1", &clock).unwrap();
        assert_eq!(get(&conn, "projects", "proj-1"), Some(hlc));
    }

    #[test]
    fn remote_write_wins_only_if_newer_than_the_recorded_version() {
        let conn = test_conn();
        let older = Hlc {
            physical_ms: 100,
            counter: 0,
            device_id: "device-a".to_string(),
        };
        let newer = Hlc {
            physical_ms: 200,
            counter: 0,
            device_id: "device-b".to_string(),
        };

        assert!(
            try_apply_remote(&conn, "projects", "proj-1", &older).unwrap(),
            "first write for a row always wins"
        );
        assert_eq!(get(&conn, "projects", "proj-1"), Some(older.clone()));

        assert!(
            !try_apply_remote(
                &conn,
                "projects",
                "proj-1",
                &Hlc {
                    physical_ms: 50,
                    ..older.clone()
                }
            )
            .unwrap(),
            "an older incoming write must lose"
        );
        assert_eq!(
            get(&conn, "projects", "proj-1"),
            Some(older),
            "a losing write must not change the recorded version"
        );

        assert!(
            try_apply_remote(&conn, "projects", "proj-1", &newer).unwrap(),
            "a newer incoming write must win"
        );
        assert_eq!(get(&conn, "projects", "proj-1"), Some(newer));
    }

    #[test]
    fn title_and_project_scopes_are_fully_independent() {
        let conn = test_conn();
        let clock = HlcClock::new("device-a".to_string());
        let title_hlc = record_local(&conn, "conversation_title", "conv-1", &clock).unwrap();
        let project_hlc = record_local(&conn, "conversation_project", "conv-1", &clock).unwrap();

        assert_ne!(
            title_hlc, project_hlc,
            "sanity: the clock actually advanced between the two calls"
        );
        assert_eq!(
            get(&conn, "conversation_title", "conv-1"),
            Some(title_hlc.clone())
        );
        assert_eq!(
            get(&conn, "conversation_project", "conv-1"),
            Some(project_hlc.clone())
        );

        // A remote write to ONLY the project scope must never affect the
        // title scope's recorded version — this is the exact bug an
        // earlier shared-HLC design had.
        let incoming_project_update = Hlc {
            physical_ms: title_hlc.physical_ms + 1000,
            counter: 0,
            device_id: "device-b".to_string(),
        };
        assert!(try_apply_remote(
            &conn,
            "conversation_project",
            "conv-1",
            &incoming_project_update
        )
        .unwrap());
        assert_eq!(
            get(&conn, "conversation_title", "conv-1"),
            Some(title_hlc),
            "the title scope's version must be untouched by a project-scope write"
        );
    }
}
