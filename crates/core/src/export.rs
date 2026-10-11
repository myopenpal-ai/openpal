//! Plaintext zip export of the user's local data: conversations, projects,
//! and artifacts.
//!
//! Conversations/projects/artifacts read from the app-wide SQLCipher
//! database (`crate::app_db`).
//!
//! Split into two phases so a caller never holds `app_db`'s lock for the
//! (much slower) zip-compression/disk-write work: [`read_export_data`] does
//! every SQL read and returns an owned, `Connection`-free [`ExportData`]
//! snapshot; [`write_export_zip`] takes that snapshot and does the actual
//! archive I/O. See `crates/app/src/commands.rs`'s `export_data` command for
//! how the two are meant to be used — read under the lock, then release it
//! before writing.
//!
//! Zip layout:
//! - `conversations/<id>.json` — the `data` column's bytes, copied verbatim
//!   (never re-serialized): this is exactly why `crate::store`'s `data`
//!   column stays pretty-printed JSON rather than switching to something
//!   more compact — a compactness win not worth losing "pure byte copy, no
//!   re-serialize, no new failure mode" for a rare, one-shot, small local
//!   export.
//! - `projects/<id>.json` — reconstructed and re-serialized at export time,
//!   the one unavoidable exception to "raw bytes only": `crate::projects`
//!   deliberately has no blob column to copy verbatim from (see its own doc
//!   comment for why).
//! - `artifacts/<conversation_id>/<artifact_id>__<filename>` — `content`
//!   copied verbatim.
//!
//! Never exports the raw `app.sqlite` file itself as "the export" — that
//! would defeat the whole point of this being a human-recoverable plaintext
//! escape hatch.
//!
//! One bad row never sinks the whole export — same "skip with a warning,
//! keep going" posture `store::list`/`projects::list` already use.
//! `write_export_zip` only returns `Err` if `dest` itself can't be created
//! (disk full, permissions) — everything else is counted in
//! [`ExportSummary::skipped`], never fatal.

use crate::types::Project;
use crate::{Error, Result};
use rusqlite::Connection;
use std::fs::File;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

/// How many zip entries were written per top-level category, plus how many
/// source rows existed but couldn't be read.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub conversations: usize,
    pub projects: usize,
    pub artifacts: usize,
    pub skipped: usize,
}

/// Everything [`write_export_zip`] needs from the database, read once under
/// a short-lived lock. Holds no `Connection` — safe to carry across an
/// `.await`/onto a blocking thread.
pub struct ExportData {
    conversations: Vec<(String, String)>,
    projects: Vec<Project>,
    artifacts: Vec<(String, String, String, String)>,
    /// Rows that existed but failed to extract at the SQL level (e.g. a
    /// type mismatch) — counted here since [`write_export_zip`] never
    /// touches the database and can't discover these itself.
    skipped: usize,
}

/// Read conversations/projects/artifacts. Fast (a handful of `SELECT`s) —
/// meant to run while `conn`'s lock is held, immediately followed by
/// releasing it before the caller moves on to [`write_export_zip`].
pub fn read_export_data(conn: &Connection) -> ExportData {
    let mut skipped = 0;
    let conversations = read_conversations(conn, &mut skipped);
    let projects = crate::projects::list(conn);
    let artifacts = read_artifacts(conn, &mut skipped);
    ExportData {
        conversations,
        projects,
        artifacts,
        skipped,
    }
}

pub fn write_export_zip(data: &ExportData, dest: &Path) -> Result<ExportSummary> {
    let file = File::create(dest)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    let mut summary = ExportSummary {
        skipped: data.skipped,
        ..Default::default()
    };

    for (id, blob) in &data.conversations {
        let zip_path = format!("conversations/{id}.json");
        if write_entry(&mut zip, options, &zip_path, blob.as_bytes()) {
            summary.conversations += 1;
        } else {
            summary.skipped += 1;
        }
    }

    for project in &data.projects {
        let Ok(json) = serde_json::to_string_pretty(project) else {
            summary.skipped += 1;
            continue;
        };
        let zip_path = format!("projects/{}.json", project.id);
        if write_entry(&mut zip, options, &zip_path, json.as_bytes()) {
            summary.projects += 1;
        } else {
            summary.skipped += 1;
        }
    }

    for (conversation_id, artifact_id, filename, content) in &data.artifacts {
        let zip_path = format!("artifacts/{conversation_id}/{artifact_id}__{filename}");
        if write_entry(&mut zip, options, &zip_path, content.as_bytes()) {
            summary.artifacts += 1;
        } else {
            summary.skipped += 1;
        }
    }

    zip.finish()
        .map_err(|e| Error::Export(format!("couldn't finish writing the export archive: {e}")))?;
    Ok(summary)
}

/// Every conversation's `(id, data)` — `data` is the raw blob, copied
/// byte-for-byte later, never parsed or re-serialized.
fn read_conversations(conn: &Connection, skipped: &mut usize) -> Vec<(String, String)> {
    let mut stmt = match conn.prepare("SELECT id, data FROM conversations") {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("export: couldn't prepare the conversations query: {e}");
            return Vec::new();
        }
    };
    let rows = match stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("export: couldn't query conversations: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for row in rows {
        match row {
            Ok(pair) => out.push(pair),
            Err(e) => {
                tracing::warn!("export: skipping unreadable conversation row: {e}");
                *skipped += 1;
            }
        }
    }
    out
}

/// Every artifact's `(conversation_id, id, filename, content)` — `content`
/// is copied byte-for-byte later, same on-disk naming the old file-based
/// layout used.
fn read_artifacts(conn: &Connection, skipped: &mut usize) -> Vec<(String, String, String, String)> {
    let mut stmt =
        match conn.prepare("SELECT conversation_id, id, filename, content FROM artifacts") {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::warn!("export: couldn't prepare the artifacts query: {e}");
                return Vec::new();
            }
        };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("export: couldn't query artifacts: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for row in rows {
        match row {
            Ok(tuple) => out.push(tuple),
            Err(e) => {
                tracing::warn!("export: skipping unreadable artifact row: {e}");
                *skipped += 1;
            }
        }
    }
    out
}

/// Write one file's bytes into the archive; `false` (never `Err`) on
/// failure so the caller just bumps its skip counter.
fn write_entry(
    zip: &mut ZipWriter<File>,
    options: SimpleFileOptions,
    zip_path: &str,
    bytes: &[u8],
) -> bool {
    if zip.start_file(zip_path, options).is_err() {
        tracing::warn!("export: couldn't start zip entry {zip_path}");
        return false;
    }
    if std::io::Write::write_all(zip, bytes).is_err() {
        tracing::warn!("export: couldn't write zip entry {zip_path}");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Conversation, SamplingParams, TaskType};

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn conversation(id: &str) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: format!("Chat {id}"),
            created_at_ms: 1,
            updated_at_ms: 1,
            messages: Vec::new(),
            sampling: SamplingParams::default(),
            context_length: 8192,
            task_type: TaskType::General,
            project_id: None,
            pinned: false,
            branches: std::collections::HashMap::new(),
            forked_from: None,
        }
    }

    #[test]
    fn export_zip_contains_everything_with_correct_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        crate::store::save(&conn, &conversation("conv-1")).unwrap();
        crate::projects::save(
            &conn,
            &Project {
                id: "proj-1".to_string(),
                name: "Research".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        crate::artifacts::save(&conn, "conv-1", "art-1", "report.md", "# Report").unwrap();

        let data = read_export_data(&conn);
        let dest = tmp.path().join("export.zip");
        let summary = write_export_zip(&data, &dest).unwrap();

        assert_eq!(
            (
                summary.conversations,
                summary.projects,
                summary.artifacts,
                summary.skipped
            ),
            (1, 1, 1, 0)
        );

        let file = File::open(&dest).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "artifacts/conv-1/art-1__report.md",
                "conversations/conv-1.json",
                "projects/proj-1.json",
            ]
        );

        let mut content = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("conversations/conv-1.json").unwrap(),
            &mut content,
        )
        .unwrap();
        assert_eq!(
            content,
            serde_json::to_string_pretty(&conversation("conv-1")).unwrap(),
            "the exported bytes must be the stored `data` blob verbatim"
        );
    }

    #[test]
    fn export_with_nothing_stored_produces_an_empty_summary_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();

        let data = read_export_data(&conn);
        let summary = write_export_zip(&data, &tmp.path().join("export.zip")).unwrap();
        assert_eq!(
            (
                summary.conversations,
                summary.projects,
                summary.artifacts,
                summary.skipped
            ),
            (0, 0, 0, 0)
        );
    }

    /// A row that exists but fails to extract at the SQL level (as opposed
    /// to a structurally-fine row whose JSON *content* is corrupt — see
    /// `store::tests` for that case) must be skipped, not abort the whole
    /// read. A `BLOB` literal written into a `TEXT`-affinity column is the
    /// concrete way to construct this: SQLite does not coerce it to `TEXT`,
    /// so `row.get::<_, String>(..)` genuinely fails.
    #[test]
    fn a_row_that_fails_to_extract_at_the_sql_level_is_skipped_not_fatal() {
        let conn = test_conn();
        crate::store::save(&conn, &conversation("conv-1")).unwrap();
        conn.execute(
            "INSERT INTO conversations \
             (id, title, created_at_ms, updated_at_ms, message_count, task_type, data) \
             VALUES ('bad', 'Bad', 1, 1, 0, 'general', X'0102')",
            [],
        )
        .unwrap();

        let data = read_export_data(&conn);
        assert_eq!(
            data.conversations.len(),
            1,
            "the well-formed row still reads"
        );
        assert_eq!(
            data.skipped, 1,
            "the BLOB-in-a-TEXT-column row is counted as skipped"
        );

        let tmp = tempfile::tempdir().unwrap();
        let summary = write_export_zip(&data, &tmp.path().join("export.zip")).unwrap();
        assert_eq!(summary.conversations, 1);
        assert_eq!(summary.skipped, 1);
    }
}
