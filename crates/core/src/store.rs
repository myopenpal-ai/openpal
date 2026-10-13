//! Conversation persistence, in the app-wide SQLCipher database
//! (`crate::app_db`) — one row per conversation in the `conversations`
//! table. `title`/`created_at_ms`/`updated_at_ms`/`message_count`/
//! `task_type`/`project_id`/`pinned` are a read-optimization cache,
//! re-derived from the `Conversation` on every [`save`]; the full
//! `serde_json::to_string_pretty(&conversation)` (byte-identical to what
//! used to be the file's contents) lives in the `data` column. [`save`] is
//! the only function allowed to write either the cache columns or `data` —
//! never patch one without the other, or [`list`]/[`get`]/an export would
//! silently disagree. [`backfill_message_ids`] is a deliberate, narrow
//! exception: it patches `data` directly (never the cache columns, which a
//! bare id backfill can't change) because going through [`save`]'s typed
//! `Conversation` would itself trigger the exact id-churn hazard the
//! backfill exists to close — see its own doc comment.
//!
//! - [`list`] never touches `data` at all — a real improvement over the old
//!   file-based version, which had to fully parse every conversation just
//!   for lightweight metadata.
//! - A row that fails to deserialize (an unparseable `task_type` value, or
//!   in [`search`], an unparseable `data` blob) is skipped with a
//!   `tracing::warn!`, never propagated — chat history must never block the
//!   app, the same invariant the old per-file version held.
//! - Conversation ids come from the frontend (UUIDs). [`validate_id`] keeps
//!   the same `[a-zA-Z0-9-]`-only charset check the old file-path version
//!   used, though the reason has changed: there's no filesystem path built
//!   from an id anymore (SQL parameter binding already rules out
//!   injection), so this is input hygiene now, not a path-traversal
//!   defense — kept anyway so a UUID-shaped id violating this charset is
//!   still rejected early, and the observable `NotFound`-on-garbage-id
//!   contract stays stable for any caller that depends on it.
//! - [`search`] is a plain case-insensitive scan of title + message content —
//!   no embeddings/second model, consistent with AGENTS.md's mono-model-first
//!   rule (see `agent::builtin::rerank`'s doc comment for the same call made
//!   for web-page reranking).

use crate::types::{Conversation, ConversationMeta, ConversationSearchResult};
use crate::{Error, Result};
use rusqlite::{params, Connection};

pub fn list(conn: &Connection) -> Vec<ConversationMeta> {
    let mut stmt = match conn.prepare(
        "SELECT id, title, created_at_ms, updated_at_ms, message_count, task_type, \
         project_id, pinned FROM conversations ORDER BY updated_at_ms DESC",
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("couldn't prepare the conversation list query: {e}");
            return Vec::new();
        }
    };
    // Raw columns first, always structurally valid — the JSON-decode step
    // (only `task_type` here; `list` never touches `data`) happens in a
    // second pass below, per row, so one bad row can never abort the whole
    // query the way `.collect::<rusqlite::Result<Vec<_>>>()` would.
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, bool>(7)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("couldn't query conversations: {e}");
            return Vec::new();
        }
    };

    let mut metas = Vec::new();
    for row in rows {
        let (
            id,
            title,
            created_at_ms,
            updated_at_ms,
            message_count,
            task_type_json,
            project_id,
            pinned,
        ) = match row {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!("skipping unreadable conversation row: {e}");
                continue;
            }
        };
        let task_type = match serde_json::from_str(&task_type_json) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("skipping conversation {id} with an unparseable task_type: {e}");
                continue;
            }
        };
        metas.push(ConversationMeta {
            id,
            title,
            created_at_ms: created_at_ms as u64,
            updated_at_ms: updated_at_ms as u64,
            message_count: message_count as usize,
            task_type,
            project_id,
            pinned,
        });
    }
    metas
}

pub fn get(conn: &Connection, id: &str) -> Result<Conversation> {
    validate_id(id)?;
    let data: String = conn
        .query_row(
            "SELECT data FROM conversations WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("conversation {id}")),
            other => Error::Db(other),
        })?;
    Ok(serde_json::from_str(&data)?)
}

pub fn save(conn: &Connection, conversation: &Conversation) -> Result<()> {
    validate_id(&conversation.id)?;
    let data = serde_json::to_string_pretty(conversation)?;
    let task_type = serde_json::to_string(&conversation.task_type)?;
    conn.execute(
        "INSERT OR REPLACE INTO conversations
            (id, title, created_at_ms, updated_at_ms, message_count, task_type, project_id, pinned, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        // SQLite integers are signed 64-bit; `u64` has no `ToSql` impl in
        // rusqlite, so timestamps round-trip through `i64` — safe for
        // epoch-ms values for the next ~292 million years.
        params![
            conversation.id,
            conversation.title,
            conversation.created_at_ms as i64,
            conversation.updated_at_ms as i64,
            conversation.messages.len() as i64,
            task_type,
            conversation.project_id,
            conversation.pinned,
            data,
        ],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    validate_id(id)?;
    // Idempotent already: 0 rows affected is not an error.
    conn.execute("DELETE FROM conversations WHERE id = ?1", params![id])?;
    Ok(())
}

/// One-time migration companion to `crate::skills::migrate_legacy_skills` —
/// mints and persists a stable `id` for every message in every conversation
/// whose on-disk data predates `ChatMessage.id`. Called once by
/// `crate::state::finish_unlock` (in `crates/app`), gated on the schema
/// version crossing 11 — see `crate::app_db`'s `SCHEMA_V11` doc comment.
///
/// **Patches two separate representations, not just `conversations.data`**:
/// a conversation sync has ever touched also has its messages recorded in
/// `sync_message_ops` (see `crate::sync::conversations`'s top doc comment),
/// which `load_ordered_messages`/`recompute_and_save` deserialize back into
/// a fresh `.messages` array on *every remote merge* — an unbackfilled op
/// row there would keep re-minting a new random id on every such merge
/// (`ChatMessage`'s `#[serde(default = "new_message_id")]` firing on every
/// deserialize) and silently overwrite whatever stable id `conversations.data`
/// just got below, reintroducing the exact churn hazard this migration
/// exists to close. The two representations are always the same message
/// sequence in the same order for an already-seeded conversation (the CRDT
/// convergence invariant `crate::sync::conversations` maintains throughout),
/// so the ids minted for `conversations.data` are reused for
/// `sync_message_ops` by matching position — not re-minted independently —
/// keeping both representations pointing at the same logical message with
/// the same id. Only live (non-tombstoned) ops are touched: a tombstoned row
/// is dead history a rewrite already superseded and is never deserialized
/// again by anything in this codebase.
///
/// Deliberately reads/writes both as raw [`serde_json::Value`] rather than
/// round-tripping through the typed `Conversation`/`ChatMessage` structs:
/// going through the typed structs would apply the same
/// `#[serde(default = "new_message_id")]` on *every* plain deserialize,
/// which is precisely the non-idempotent behavior being closed here.
/// Idempotent overall: a row whose messages already all have `"id"` is left
/// completely untouched (not even rewritten), so a second call is a no-op.
pub fn backfill_message_ids(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;

    // Pass 1: `conversations.data` — remember the id assigned to each
    // (conversation_id, message index) so pass 2 can reuse it rather than
    // minting its own.
    let mut assigned: std::collections::HashMap<(String, usize), String> =
        std::collections::HashMap::new();
    let rows: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT id, data FROM conversations")?;
        let mapped = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row?);
        }
        out
    };
    for (conversation_id, data) in rows {
        let mut value: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "skipping conversation {conversation_id} with unparseable data \
                     during message-id backfill: {e}"
                );
                continue;
            }
        };
        let mut changed = false;
        if let Some(messages) = value.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for (i, message) in messages.iter_mut().enumerate() {
                if let Some(obj) = message.as_object_mut() {
                    match obj.get("id").and_then(|v| v.as_str()) {
                        Some(existing) => {
                            assigned.insert((conversation_id.clone(), i), existing.to_string());
                        }
                        None => {
                            let id = crate::types::new_message_id();
                            obj.insert("id".to_string(), serde_json::Value::String(id.clone()));
                            assigned.insert((conversation_id.clone(), i), id);
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            let new_data = serde_json::to_string_pretty(&value)?;
            tx.execute(
                "UPDATE conversations SET data = ?1 WHERE id = ?2",
                params![new_data, conversation_id],
            )?;
        }
    }

    // Pass 2: `sync_message_ops` — reuse the ids from pass 1, correlated by
    // position (see this function's doc comment for why that's safe).
    let conversation_ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT DISTINCT conversation_id FROM sync_message_ops")?;
        let mapped = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in mapped {
            out.push(row?);
        }
        out
    };
    for conversation_id in conversation_ids {
        let op_rows: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT op_id, message_json FROM sync_message_ops \
                 WHERE conversation_id = ?1 AND tombstoned = 0 \
                 ORDER BY hlc_ms ASC, hlc_counter ASC, hlc_device_id ASC",
            )?;
            let mapped = stmt.query_map(params![conversation_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for row in mapped {
                out.push(row?);
            }
            out
        };
        for (i, (op_id, message_json)) in op_rows.into_iter().enumerate() {
            let mut value: serde_json::Value = match serde_json::from_str(&message_json) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "skipping op {op_id} with unparseable message_json during \
                         message-id backfill: {e}"
                    );
                    continue;
                }
            };
            let Some(obj) = value.as_object_mut() else {
                continue;
            };
            if obj.contains_key("id") {
                continue;
            }
            // Falls back to a fresh id only if pass 1 has no entry at this
            // exact position (shouldn't happen for a healthy, converged
            // conversation — defensive, not the expected path).
            let id = assigned
                .get(&(conversation_id.clone(), i))
                .cloned()
                .unwrap_or_else(crate::types::new_message_id);
            obj.insert("id".to_string(), serde_json::Value::String(id));
            let new_json = serde_json::to_string(&value)?;
            tx.execute(
                "UPDATE sync_message_ops SET message_json = ?1 WHERE op_id = ?2",
                params![new_json, op_id],
            )?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// How many characters of context to keep on each side of a content match in
/// [`search`]'s snippet.
const SNIPPET_CONTEXT_CHARS: usize = 60;

/// Case-insensitive search across every conversation's title and message
/// content, for the sidebar's search box. One query reading every row's
/// title and `data` — a local desktop app's conversation count doesn't
/// warrant a search index — but note this is `SELECT` once, page-cache
/// backed, not N file opens the way the old version worked. The matching
/// logic itself ([`find_match_char_index`]/[`excerpt_around`]) is
/// deliberately unchanged from the file-based version: it does per-`char`
/// Unicode-aware case folding (built to handle characters like `İ` that
/// expand into more than one character when lowercased), which SQLite's
/// built-in `LIKE`/`NOCASE` (ASCII-only) cannot reproduce — pushing the
/// actual matching into SQL would risk silent false negatives on exactly
/// the inputs this matcher exists to handle, which the ranking behavior
/// this function must preserve explicitly forbids. Title matches sort first
/// (then by recency), since a title match is almost always what the user
/// meant; content-only matches follow, also by recency. An empty (after
/// trimming) query returns nothing rather than every conversation.
pub fn search(conn: &Connection, query: &str) -> Vec<ConversationSearchResult> {
    let query_chars: Vec<char> = query.trim().chars().collect();
    if query_chars.is_empty() {
        return Vec::new();
    }

    let mut stmt = match conn
        .prepare("SELECT id, title, updated_at_ms, project_id, data FROM conversations")
    {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("couldn't prepare the conversation search query: {e}");
            return Vec::new();
        }
    };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("couldn't query conversations for search: {e}");
            return Vec::new();
        }
    };

    let mut title_matches = Vec::new();
    let mut content_matches = Vec::new();
    for row in rows {
        let (id, title, updated_at_ms, project_id, data) = match row {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!("skipping unreadable conversation row during search: {e}");
                continue;
            }
        };
        if find_match_char_index(&title, &query_chars).is_some() {
            title_matches.push(ConversationSearchResult {
                id,
                updated_at_ms: updated_at_ms as u64,
                project_id,
                snippet: title.clone(),
                title,
            });
            continue;
        }
        // Only reached once the cheap title check has already missed —
        // matches `list`'s posture of not paying for `data` unless needed.
        let conversation: Conversation = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "skipping conversation {id} with unparseable data during search: {e}"
                );
                continue;
            }
        };
        let snippet = conversation.messages.iter().find_map(|m| {
            let idx = find_match_char_index(&m.content, &query_chars)?;
            Some(excerpt_around(&m.content, idx, query_chars.len()))
        });
        if let Some(snippet) = snippet {
            content_matches.push(ConversationSearchResult {
                id,
                title,
                updated_at_ms: updated_at_ms as u64,
                project_id,
                snippet,
            });
        }
    }
    title_matches.sort_by_key(|m| std::cmp::Reverse(m.updated_at_ms));
    content_matches.sort_by_key(|m| std::cmp::Reverse(m.updated_at_ms));
    title_matches.extend(content_matches);
    title_matches
}

/// The char index in `text` where `query_chars` first occurs, matched
/// character-by-character via [`char::to_lowercase`] rather than lowercasing
/// the whole string and using [`str::find`] — a handful of characters (e.g.
/// `İ`) expand into more than one character when lowercased, which would
/// desync a byte offset found in a lowercased copy from the original text.
fn find_match_char_index(text: &str, query_chars: &[char]) -> Option<usize> {
    if query_chars.is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    if query_chars.len() > chars.len() {
        return None;
    }
    (0..=chars.len() - query_chars.len()).find(|&start| {
        chars[start..start + query_chars.len()]
            .iter()
            .zip(query_chars)
            .all(|(c, q)| c.to_lowercase().eq(q.to_lowercase()))
    })
}

/// A short excerpt of `text` centered on the match at `char_idx` (which is
/// `query_len` characters long), with an ellipsis on whichever side(s) got
/// cut off.
fn excerpt_around(text: &str, char_idx: usize, query_len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = char_idx.saturating_sub(SNIPPET_CONTEXT_CHARS);
    let end = (char_idx + query_len + SNIPPET_CONTEXT_CHARS).min(chars.len());
    let excerpt: String = chars[start..end].iter().collect();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        excerpt.trim(),
        if end < chars.len() { "…" } else { "" },
    )
}

/// Input hygiene, not a path-traversal defense (see this module's header).
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Error::NotFound(format!("conversation {id}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, SamplingParams};

    /// A DB-backed test needs the directory to outlive this function (the
    /// returned `Connection` keeps the file open), so `.keep()` disowns the
    /// `TempDir` rather than letting it delete the directory on drop —
    /// harmless for a short-lived test process.
    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn conversation(id: &str, updated_at_ms: u64) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: format!("Chat {id}"),
            created_at_ms: 1,
            updated_at_ms,
            messages: vec![ChatMessage {
                id: "msg-1".to_string(),
                role: "user".to_string(),
                content: "hi".to_string(),
                created_at_ms: None,
                model: None,
                citations: None,
                thinking: None,
                steps: None,
                attachments: None,
                artifacts: None,
                stats: None,
                display_content: None,
            }],
            sampling: SamplingParams::default(),
            context_length: 8192,
            task_type: crate::types::TaskType::General,
            project_id: None,
            pinned: false,
            branches: std::collections::HashMap::new(),
            forked_from: None,
        }
    }

    #[test]
    fn save_get_list_delete_roundtrip() {
        let conn = test_conn();

        assert!(list(&conn).is_empty());

        save(&conn, &conversation("aaa-1", 100)).unwrap();
        save(&conn, &conversation("bbb-2", 300)).unwrap();
        save(&conn, &conversation("ccc-3", 200)).unwrap();

        let metas = list(&conn);
        let ids: Vec<&str> = metas.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["bbb-2", "ccc-3", "aaa-1"], "newest first");
        assert_eq!(metas[0].message_count, 1);

        let got = get(&conn, "aaa-1").unwrap();
        assert_eq!(got.title, "Chat aaa-1");

        delete(&conn, "aaa-1").unwrap();
        delete(&conn, "aaa-1").unwrap(); // idempotent
        assert!(matches!(get(&conn, "aaa-1"), Err(Error::NotFound(_))));
        assert_eq!(list(&conn).len(), 2);
    }

    /// `pinned` is a cache column (`SCHEMA_V10`) like `title`/`project_id`/
    /// `task_type`, all of which the roundtrip test above already exercises
    /// — but every fixture there hardcodes `pinned: false`, so a save->list
    /// roundtrip with `pinned: true` was never actually exercised. Guards
    /// against a future INSERT-column/SELECT-index mismatch silently making
    /// `pinned` always read back `false`.
    #[test]
    fn pinned_round_trips_through_save_and_list() {
        let conn = test_conn();

        let pinned = Conversation {
            pinned: true,
            ..conversation("pinned-1", 100)
        };
        save(&conn, &pinned).unwrap();
        save(&conn, &conversation("unpinned-1", 200)).unwrap();

        let metas = list(&conn);
        let pinned_meta = metas.iter().find(|m| m.id == "pinned-1").unwrap();
        let unpinned_meta = metas.iter().find(|m| m.id == "unpinned-1").unwrap();
        assert!(pinned_meta.pinned);
        assert!(!unpinned_meta.pinned);

        assert!(get(&conn, "pinned-1").unwrap().pinned);
    }

    #[test]
    fn a_conversation_with_a_corrupt_data_blob_is_skipped_by_list_but_never_touches_it() {
        let conn = test_conn();
        save(&conn, &conversation("good-1", 100)).unwrap();
        // Hand-corrupt just the `data` blob via raw SQL — the extracted
        // columns (title, task_type, etc.) stay well-formed, proving `list`
        // genuinely never reads `data` at all: if it did, this row's title
        // wouldn't be recoverable either.
        conn.execute(
            "INSERT INTO conversations (id, title, created_at_ms, updated_at_ms, message_count, \
             task_type, project_id, data) VALUES ('bad-1', 'Bad', 1, 50, 0, '\"general\"', \
             NULL, 'not json')",
            [],
        )
        .unwrap();

        let metas = list(&conn);
        assert_eq!(metas.len(), 2);
        assert!(metas.iter().any(|m| m.id == "bad-1" && m.title == "Bad"));

        // But `get`, which does read `data`, correctly fails on that row.
        assert!(get(&conn, "bad-1").is_err());
    }

    #[test]
    fn a_conversation_with_an_unparseable_task_type_is_skipped_by_list() {
        let conn = test_conn();
        save(&conn, &conversation("good-1", 100)).unwrap();
        conn.execute(
            "INSERT INTO conversations (id, title, created_at_ms, updated_at_ms, message_count, \
             task_type, project_id, data) VALUES ('bad-1', 'Bad', 1, 50, 0, 'not json', \
             NULL, '{}')",
            [],
        )
        .unwrap();

        let metas = list(&conn);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "good-1");
    }

    /// A row that fails to extract at the raw SQL level (as opposed to a
    /// structurally-fine row whose JSON *content* is corrupt — the two tests
    /// above) must be skipped too, not abort the whole list — the specific
    /// failure mode `.collect::<rusqlite::Result<Vec<_>>>()` would have
    /// caused and this module's own doc comment warns against. A `BLOB`
    /// literal written into `title` (`TEXT` affinity) is the concrete way to
    /// construct this: SQLite does not coerce it, so `row.get::<_,
    /// String>(..)` genuinely fails.
    #[test]
    fn a_row_that_fails_to_extract_at_the_sql_level_is_skipped_not_fatal() {
        let conn = test_conn();
        save(&conn, &conversation("good-1", 100)).unwrap();
        conn.execute(
            "INSERT INTO conversations (id, title, created_at_ms, updated_at_ms, message_count, \
             task_type, project_id, data) VALUES ('bad-1', X'0102', 1, 50, 0, '\"general\"', \
             NULL, '{}')",
            [],
        )
        .unwrap();

        let metas = list(&conn);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].id, "good-1");
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
                matches!(delete(&conn, id), Err(Error::NotFound(_))),
                "delete {id:?}"
            );
            assert!(
                matches!(save(&conn, &conversation(id, 1)), Err(Error::NotFound(_))),
                "save {id:?}"
            );
        }
    }

    fn conversation_with(id: &str, updated_at_ms: u64, title: &str, content: &str) -> Conversation {
        Conversation {
            title: title.to_string(),
            messages: vec![ChatMessage {
                id: "msg-1".to_string(),
                role: "user".to_string(),
                content: content.to_string(),
                created_at_ms: None,
                model: None,
                citations: None,
                thinking: None,
                steps: None,
                attachments: None,
                artifacts: None,
                stats: None,
                display_content: None,
            }],
            ..conversation(id, updated_at_ms)
        }
    }

    #[test]
    fn search_matches_title_case_insensitively() {
        let conn = test_conn();
        save(
            &conn,
            &conversation_with("a", 100, "Rust ownership notes", "irrelevant"),
        )
        .unwrap();
        save(
            &conn,
            &conversation_with("b", 200, "Banana bread", "irrelevant"),
        )
        .unwrap();

        let results = search(&conn, "RUST");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
        assert_eq!(results[0].snippet, "Rust ownership notes");
    }

    #[test]
    fn search_falls_back_to_message_content_with_a_snippet() {
        let conn = test_conn();
        save(
            &conn,
            &conversation_with(
                "a",
                100,
                "Untitled chat",
                "The quick brown fox jumps over the lazy dog.",
            ),
        )
        .unwrap();

        let results = search(&conn, "brown fox");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
        assert!(
            results[0].snippet.contains("brown fox"),
            "{}",
            results[0].snippet
        );
    }

    #[test]
    fn search_ranks_title_matches_before_content_matches() {
        let conn = test_conn();
        save(
            &conn,
            &conversation_with("older-title", 100, "widget factory", "n/a"),
        )
        .unwrap();
        save(
            &conn,
            &conversation_with(
                "newer-content",
                200,
                "unrelated",
                "we discussed a widget yesterday",
            ),
        )
        .unwrap();

        let results = search(&conn, "widget");
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            ["older-title", "newer-content"],
            "title match ranks first even though it's older"
        );
    }

    #[test]
    fn search_with_blank_query_returns_nothing() {
        let conn = test_conn();
        save(&conn, &conversation("a", 100)).unwrap();
        assert!(search(&conn, "   ").is_empty());
        assert!(search(&conn, "").is_empty());
    }

    #[test]
    fn search_carries_project_id_through() {
        let conn = test_conn();
        let mut c = conversation_with("a", 100, "Project chat", "n/a");
        c.project_id = Some("proj-1".to_string());
        save(&conn, &c).unwrap();

        let results = search(&conn, "project");
        assert_eq!(results[0].project_id.as_deref(), Some("proj-1"));
    }

    // -----------------------------------------------------------------
    // backfill_message_ids — see this function's own doc comment for the
    // id-churn hazard it exists to close (a bare `#[serde(default)]` alone
    // would mint a *different* random id on every plain read).
    // -----------------------------------------------------------------

    /// Writes a conversation row directly, with `data` JSON that has no
    /// `"id"` key on any message — simulating a conversation persisted
    /// before `ChatMessage.id` existed (bypassing `save`'s typed path,
    /// which would always produce an id via `#[serde(default)]`).
    fn insert_id_less_conversation(conn: &Connection, id: &str) {
        // `sampling`/`branches` are deliberately omitted — `Conversation`'s
        // own `#[serde(default)]` on those fields covers an absent key
        // (unlike `SamplingParams`, whose own fields aren't individually
        // defaulted, so a present-but-empty `"sampling": {}` would fail).
        let data = serde_json::json!({
            "id": id,
            "title": "Untitled",
            "createdAtMs": 1,
            "updatedAtMs": 1,
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
            ],
            "contextLength": 8192,
            "taskType": "general",
            "pinned": false,
        });
        conn.execute(
            "INSERT INTO conversations \
                (id, title, created_at_ms, updated_at_ms, message_count, task_type, project_id, pinned, data) \
             VALUES (?1, 'Untitled', 1, 1, 2, '\"general\"', NULL, 0, ?2)",
            params![id, data.to_string()],
        )
        .unwrap();
    }

    #[test]
    fn backfill_mints_stable_ids_and_is_idempotent() {
        let mut conn = test_conn();
        insert_id_less_conversation(&conn, "a");

        backfill_message_ids(&mut conn).unwrap();
        let first = get(&conn, "a").unwrap();
        assert_eq!(first.messages.len(), 2);
        let ids_first: Vec<String> = first.messages.iter().map(|m| m.id.clone()).collect();
        assert!(ids_first.iter().all(|id| !id.is_empty()));
        assert_ne!(ids_first[0], ids_first[1], "each message gets its own id");

        // A second backfill call must be a no-op: same ids, not re-minted.
        backfill_message_ids(&mut conn).unwrap();
        let second = get(&conn, "a").unwrap();
        let ids_second: Vec<String> = second.messages.iter().map(|m| m.id.clone()).collect();
        assert_eq!(
            ids_first, ids_second,
            "backfill must not re-mint ids on a second call"
        );

        // A plain read (no save/backfill in between) must also be stable —
        // this is the actual regression the bare `#[serde(default)]` alone
        // would fail: a fresh random default would mint a different id on
        // every deserialize of still-id-less JSON. Since backfill already
        // persisted real ids above, a third plain read must match too.
        let third = get(&conn, "a").unwrap();
        let ids_third: Vec<String> = third.messages.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids_first, ids_third, "a plain read must return stable ids");
    }

    #[test]
    fn backfill_leaves_already_migrated_conversations_untouched() {
        let mut conn = test_conn();
        // Written via the typed `save` path, so every message already has a
        // real `id` (`ChatMessage`'s `#[serde(default)]` only fires for
        // *missing* JSON keys, and `save` always serializes a populated
        // `Conversation` value, never a partial one).
        save(&conn, &conversation("a", 1)).unwrap();
        let before = get(&conn, "a").unwrap();
        let ids_before: Vec<String> = before.messages.iter().map(|m| m.id.clone()).collect();

        backfill_message_ids(&mut conn).unwrap();

        let after = get(&conn, "a").unwrap();
        let ids_after: Vec<String> = after.messages.iter().map(|m| m.id.clone()).collect();
        assert_eq!(
            ids_before, ids_after,
            "backfill must not touch messages that already have an id"
        );
    }
}
