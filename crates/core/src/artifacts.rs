//! Persistence for files the model creates via the `create_artifact`
//! agent-mode tool, in the app-wide SQLCipher database (`crate::app_db`) —
//! one row per artifact in the `artifacts` table, keyed by
//! `(conversation_id, id)` (confirmed by this module's own tests: an
//! `artifact_id` is only unique *within* one conversation, not globally, so
//! the composite key matches that exactly). `filename`/`content`/
//! `size_bytes` are real columns now — `get` is a plain primary-key lookup,
//! not the old directory-scan-for-a-`<artifact_id>__`-prefix trick a
//! filesystem needed to recover a filename the caller never provided.
//!
//! - `artifact_id` is provided by the caller (the tool passes its own
//!   `ToolContext::call_id`, already unique per tool call) rather than
//!   generated here.
//! - Both `conversation_id` and `artifact_id` are validated with the same
//!   `[a-zA-Z0-9-]`-only charset `store.rs` uses for conversation ids —
//!   input hygiene now, not a path-traversal defense (see `crate::store`'s
//!   own doc comment for the same reasoning; there's no filesystem path
//!   built from either id anymore).
//! - `content` is `TEXT`, not `BLOB`: artifacts are always UTF-8 (capped at
//!   [`MAX_ARTIFACT_BYTES`], never streamed, `Artifact.content: String`
//!   everywhere) — never truly arbitrary binary, so `TEXT` is the simpler,
//!   behavior-equivalent choice with identical at-rest encryption (SQLCipher
//!   encrypts pages regardless of column affinity).

use crate::types::Artifact;
use crate::{Error, Result};
use rusqlite::{params, Connection};

/// Content size cap (5 MB) — generous for a report/script/HTML page while
/// keeping one runaway artifact from silently blowing up the database.
pub const MAX_ARTIFACT_BYTES: usize = 5 * 1024 * 1024;

/// Windows-reserved device names (case-insensitive) — this app ships on
/// Windows too (see ARCHITECTURE.md "Adding a platform"), and a downloaded
/// file named e.g. `CON` or `NUL.txt` can't be created there.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Longest filename this app will write, in characters.
const MAX_FILENAME_CHARS: usize = 150;

/// Conservative sanitizer for a model-supplied filename: takes only the last
/// path component (defeating `../../etc/passwd`-style traversal and any
/// embedded directory structure), strips control characters/null bytes,
/// strips leading dots (avoid dotfiles), caps length, and — since this app
/// ships on Windows too — prefixes a Windows-reserved device name (`CON`,
/// `NUL`, `COM1`, …) with `_` so a downloaded copy can actually be created
/// there. Falls back to `"artifact.txt"` if nothing usable survives. Pure —
/// no I/O, unaffected by this module's storage backend.
pub fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();

    // Keep only the last path component, so any `/`, `\`, or `..` segments
    // in front of it are dropped along with everything else in them.
    let last_component = trimmed.rsplit(['/', '\\']).next().unwrap_or("").to_string();

    // Belt-and-suspenders: strip any remaining separators, control
    // characters, and null bytes (`char::is_control` covers NUL). Taking
    // the last path component above already removes every `..` *segment*
    // (there's no separator left for one to hide behind); the only residual
    // case is a last component that's *itself* `..`, which the leading-dot
    // strip below reduces to empty, hitting the `artifact.txt` fallback.
    let cleaned: String = last_component
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && !c.is_control())
        .collect();

    // Strip leading dots (avoid dotfiles like `.bashrc`).
    let no_leading_dots = cleaned.trim_start_matches('.');

    let capped: String = no_leading_dots.chars().take(MAX_FILENAME_CHARS).collect();
    let capped = capped.trim();

    if capped.is_empty() {
        return "artifact.txt".to_string();
    }

    let stem = capped.split('.').next().unwrap_or(capped);
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
    {
        return format!("_{capped}");
    }

    capped.to_string()
}

/// Input hygiene, not a path-traversal defense (see this module's header).
/// Used for both `conversation_id` and `artifact_id` (a `call_id` is
/// expected to already be safe, but is never trusted blindly here).
fn validate_id(id: &str, what: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Error::NotFound(format!("{what} {id}")));
    }
    Ok(())
}

/// Save `content` as a new artifact, sanitizing `filename` first. Rejects
/// content over [`MAX_ARTIFACT_BYTES`].
pub fn save(
    conn: &Connection,
    conversation_id: &str,
    artifact_id: &str,
    filename: &str,
    content: &str,
) -> Result<Artifact> {
    validate_id(conversation_id, "conversation")?;
    validate_id(artifact_id, "artifact")?;
    if content.len() > MAX_ARTIFACT_BYTES {
        return Err(Error::Rejected(format!(
            "That file is too large ({} bytes; the limit is {} bytes).",
            content.len(),
            MAX_ARTIFACT_BYTES
        )));
    }
    let sanitized = sanitize_filename(filename);
    let size_bytes = content.len() as i64;
    conn.execute(
        "INSERT OR REPLACE INTO artifacts (conversation_id, id, filename, content, size_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![conversation_id, artifact_id, sanitized, content, size_bytes],
    )?;

    Ok(Artifact {
        id: artifact_id.to_string(),
        filename: sanitized,
        content: content.to_string(),
        size_bytes: size_bytes as u64,
    })
}

/// Look up a previously saved artifact by `(conversation_id, artifact_id)` —
/// a plain composite-primary-key lookup.
pub fn get(conn: &Connection, conversation_id: &str, artifact_id: &str) -> Result<Artifact> {
    validate_id(conversation_id, "conversation")?;
    validate_id(artifact_id, "artifact")?;
    conn.query_row(
        "SELECT filename, content, size_bytes FROM artifacts WHERE conversation_id = ?1 AND id = ?2",
        params![conversation_id, artifact_id],
        |row| {
            Ok(Artifact {
                id: artifact_id.to_string(),
                filename: row.get(0)?,
                content: row.get(1)?,
                size_bytes: row.get::<_, i64>(2)? as u64,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("artifact {artifact_id}")),
        other => Error::Db(other),
    })
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

    #[test]
    fn sanitize_filename_strips_path_traversal() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("a/b/c.txt"), "c.txt");
        assert_eq!(sanitize_filename("..\\..\\windows\\system32"), "system32");
    }

    #[test]
    fn sanitize_filename_empty_falls_back() {
        assert_eq!(sanitize_filename(""), "artifact.txt");
        assert_eq!(sanitize_filename("   "), "artifact.txt");
    }

    #[test]
    fn sanitize_filename_only_dots_falls_back() {
        assert_eq!(sanitize_filename("."), "artifact.txt");
        assert_eq!(sanitize_filename(".."), "artifact.txt");
        assert_eq!(sanitize_filename("...."), "artifact.txt");
    }

    #[test]
    fn sanitize_filename_windows_reserved_name_is_prefixed() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("Lpt1"), "_Lpt1");
        assert_eq!(sanitize_filename("nul"), "_nul");
        // Not a reserved name: only the stem must match exactly.
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
    }

    #[test]
    fn sanitize_filename_caps_length() {
        let long_name = "a".repeat(300) + ".txt";
        let sanitized = sanitize_filename(&long_name);
        assert!(sanitized.chars().count() <= MAX_FILENAME_CHARS);
    }

    #[test]
    fn sanitize_filename_strips_control_characters() {
        let sanitized = sanitize_filename("evil\0name\n.txt");
        assert!(!sanitized.contains('\0'));
        assert!(!sanitized.contains('\n'));
    }

    #[test]
    fn sanitize_filename_passes_normal_names_through() {
        assert_eq!(sanitize_filename("weekly-report.md"), "weekly-report.md");
        assert_eq!(sanitize_filename("landing-page.html"), "landing-page.html");
    }

    #[test]
    fn save_then_get_round_trips() {
        let conn = test_conn();
        let artifact = save(&conn, "conv-1", "call-1", "report.md", "# Hello").unwrap();
        assert_eq!(artifact.filename, "report.md");
        assert_eq!(artifact.content, "# Hello");
        assert_eq!(artifact.size_bytes, 7);

        let got = get(&conn, "conv-1", "call-1").unwrap();
        assert_eq!(got.filename, "report.md");
        assert_eq!(got.content, "# Hello");
        assert_eq!(got.id, "call-1");
    }

    #[test]
    fn different_conversations_do_not_collide() {
        let conn = test_conn();
        save(&conn, "conv-1", "call-1", "f.txt", "from conv 1").unwrap();
        save(&conn, "conv-2", "call-1", "f.txt", "from conv 2").unwrap();

        let a = get(&conn, "conv-1", "call-1").unwrap();
        let b = get(&conn, "conv-2", "call-1").unwrap();
        assert_eq!(a.content, "from conv 1");
        assert_eq!(b.content, "from conv 2");
    }

    #[test]
    fn hostile_ids_are_rejected() {
        let conn = test_conn();
        for id in ["", "../evil", "a/b", "a.b", "id with spaces", "café"] {
            assert!(
                matches!(
                    save(&conn, id, "call-1", "f.txt", "x"),
                    Err(Error::NotFound(_))
                ),
                "save conversation_id={id:?}"
            );
            assert!(
                matches!(
                    save(&conn, "conv-1", id, "f.txt", "x"),
                    Err(Error::NotFound(_))
                ),
                "save artifact_id={id:?}"
            );
            assert!(
                matches!(get(&conn, id, "call-1"), Err(Error::NotFound(_))),
                "get conversation_id={id:?}"
            );
            assert!(
                matches!(get(&conn, "conv-1", id), Err(Error::NotFound(_))),
                "get artifact_id={id:?}"
            );
        }
    }

    #[test]
    fn oversized_content_is_rejected() {
        let conn = test_conn();
        let too_big = "a".repeat(MAX_ARTIFACT_BYTES + 1);
        let err = save(&conn, "conv-1", "call-1", "f.txt", &too_big).unwrap_err();
        assert!(matches!(err, Error::Rejected(_)));
    }

    #[test]
    fn get_missing_artifact_is_not_found() {
        let conn = test_conn();
        save(&conn, "conv-1", "call-1", "f.txt", "x").unwrap();
        assert!(matches!(
            get(&conn, "conv-1", "call-2"),
            Err(Error::NotFound(_))
        ));
    }
}
