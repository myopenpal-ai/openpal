//! Read-back counterpart to `crate::export`: reads a zip produced by
//! [`crate::export::write_export_zip`] and upserts its conversations,
//! projects, and artifacts into the app-wide SQLCipher database
//! (`crate::app_db`). This is Phase 0 of multi-device sync (see
//! `docs/design/MULTI_DEVICE_SYNC.md`) — the lowest-risk slice: a user can manually
//! move their data to another device today by exporting on one and importing
//! on the other, before any network sync exists.
//!
//! Zip layout expected (must match `crate::export`'s own doc comment
//! exactly — see there for why each shape was chosen):
//! - `conversations/<id>.json` — parsed as a `Conversation`, then
//!   `crate::store::save`d (an upsert: `INSERT OR REPLACE`).
//! - `projects/<id>.json` — parsed as a `Project`, then `crate::projects::save`d
//!   (also an upsert).
//! - `artifacts/<conversation_id>/<artifact_id>__<filename>` — the entry's
//!   bytes become `content`; `conversation_id`/`artifact_id`/`filename` are
//!   recovered from the path itself and handed to `crate::artifacts::save`
//!   (also an upsert — it validates size and sanitizes `filename` itself, so
//!   this module doesn't duplicate that).
//! - Anything else: skipped silently, not counted as an error — forward
//!   compatible with a newer export format adding entries this older import
//!   doesn't understand yet.
//!
//! One bad entry never sinks the whole import — same "skip with a warning,
//! keep going" posture `crate::export` (and `store::list`/`projects::list`)
//! already use. [`read_import_zip`] only returns `Err` if `src` itself can't
//! be opened as a zip — everything else is counted in
//! [`ImportSummary::skipped`], never fatal.

use crate::types::{Conversation, Project};
use crate::{Error, Result};
use rusqlite::Connection;
use std::path::Path;
use zip::ZipArchive;

/// How many entries were successfully imported per top-level category, plus
/// how many entries existed but couldn't be read/parsed/saved.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub conversations: usize,
    pub projects: usize,
    pub artifacts: usize,
    pub skipped: usize,
}

/// Read the zip at `src` (as written by
/// [`crate::export::write_export_zip`]) and upsert every conversation,
/// project, and artifact it contains into `conn`.
pub fn read_import_zip(conn: &Connection, src: &Path) -> Result<ImportSummary> {
    let file = std::fs::File::open(src)
        .map_err(|e| Error::DataImport(format!("couldn't open the import archive: {e}")))?;
    let mut archive = ZipArchive::new(file).map_err(|e| {
        Error::DataImport(format!("that doesn't look like a valid export .zip: {e}"))
    })?;

    let mut summary = ImportSummary::default();

    for i in 0..archive.len() {
        let mut entry = match archive.by_index(i) {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!("import: couldn't read zip entry {i}: {e}");
                summary.skipped += 1;
                continue;
            }
        };
        let name = entry.name().to_string();

        if let Some(id) = name
            .strip_prefix("conversations/")
            .and_then(|rest| rest.strip_suffix(".json"))
        {
            let id = id.to_string();
            match read_entry_to_string(&mut entry) {
                Ok(contents) => match import_conversation(conn, &id, &contents) {
                    Ok(()) => summary.conversations += 1,
                    Err(e) => {
                        tracing::warn!("import: skipping conversation {id}: {e}");
                        summary.skipped += 1;
                    }
                },
                Err(e) => {
                    tracing::warn!("import: couldn't read conversation entry {name}: {e}");
                    summary.skipped += 1;
                }
            }
            continue;
        }

        if let Some(id) = name
            .strip_prefix("projects/")
            .and_then(|rest| rest.strip_suffix(".json"))
        {
            let id = id.to_string();
            match read_entry_to_string(&mut entry) {
                Ok(contents) => match import_project(conn, &contents) {
                    Ok(()) => summary.projects += 1,
                    Err(e) => {
                        tracing::warn!("import: skipping project {id}: {e}");
                        summary.skipped += 1;
                    }
                },
                Err(e) => {
                    tracing::warn!("import: couldn't read project entry {name}: {e}");
                    summary.skipped += 1;
                }
            }
            continue;
        }

        if let Some(rest) = name.strip_prefix("artifacts/") {
            let Some((conversation_id, artifact_id, filename)) = parse_artifact_path(rest) else {
                // Doesn't parse into the expected shape — skip, don't panic.
                tracing::warn!("import: skipping unrecognized artifact path {name}");
                summary.skipped += 1;
                continue;
            };
            match read_entry_to_string(&mut entry) {
                Ok(content) => {
                    match crate::artifacts::save(
                        conn,
                        conversation_id,
                        artifact_id,
                        filename,
                        &content,
                    ) {
                        Ok(_) => summary.artifacts += 1,
                        Err(e) => {
                            tracing::warn!("import: skipping artifact {name}: {e}");
                            summary.skipped += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("import: couldn't read artifact entry {name}: {e}");
                    summary.skipped += 1;
                }
            }
            continue;
        }

        // Any other/unexpected path: forward-compat, not an error.
    }

    Ok(summary)
}

/// `<id>.json`'s bytes, parsed as a `Conversation` and upserted. The `id` in
/// the path is informational only (for log messages) — the id that actually
/// gets saved is whatever's embedded in the JSON itself, same as the rest of
/// `Conversation`'s fields.
fn import_conversation(conn: &Connection, _id: &str, contents: &str) -> Result<()> {
    let conversation: Conversation = serde_json::from_str(contents)?;
    crate::store::save(conn, &conversation)
}

fn import_project(conn: &Connection, contents: &str) -> Result<()> {
    let project: Project = serde_json::from_str(contents)?;
    crate::projects::save(conn, &project)
}

/// Recover `(conversation_id, artifact_id, filename)` from an
/// `artifacts/<conversation_id>/<artifact_id>__<filename>` path's tail
/// (everything after `artifacts/`) — the exact inverse of the
/// `format!("artifacts/{conversation_id}/{artifact_id}__{filename}")` used
/// in `crate::export::write_export_zip`. `None` if `rest` doesn't split into
/// that shape (e.g. missing the `/` or the `__` separator).
fn parse_artifact_path(rest: &str) -> Option<(&str, &str, &str)> {
    let (conversation_id, tail) = rest.split_once('/')?;
    let (artifact_id, filename) = tail.split_once("__")?;
    if conversation_id.is_empty() || artifact_id.is_empty() || filename.is_empty() {
        return None;
    }
    Some((conversation_id, artifact_id, filename))
}

/// Read one zip entry's bytes as a UTF-8 string. Generic over the concrete
/// `zip` crate's per-entry reader type so this doesn't have to name it.
fn read_entry_to_string<R: std::io::Read>(entry: &mut R) -> Result<String> {
    let mut out = String::new();
    entry
        .read_to_string(&mut out)
        .map_err(|e| Error::DataImport(format!("couldn't read a zip entry: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{SamplingParams, TaskType};

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
    fn round_trips_through_export_and_import() {
        let tmp = tempfile::tempdir().unwrap();
        let src_conn = test_conn();
        crate::store::save(&src_conn, &conversation("conv-1")).unwrap();
        crate::projects::save(
            &src_conn,
            &Project {
                id: "proj-1".to_string(),
                name: "Research".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        crate::artifacts::save(&src_conn, "conv-1", "art-1", "report.md", "# Report").unwrap();

        let data = crate::export::read_export_data(&src_conn);
        let dest = tmp.path().join("export.zip");
        let export_summary = crate::export::write_export_zip(&data, &dest).unwrap();
        assert_eq!(
            (
                export_summary.conversations,
                export_summary.projects,
                export_summary.artifacts,
                export_summary.skipped
            ),
            (1, 1, 1, 0)
        );

        // Import into a second, fresh, empty database.
        let dst_conn = test_conn();
        let import_summary = read_import_zip(&dst_conn, &dest).unwrap();
        assert_eq!(
            (
                import_summary.conversations,
                import_summary.projects,
                import_summary.artifacts,
                import_summary.skipped
            ),
            (1, 1, 1, 0)
        );

        let imported_conversation = crate::store::get(&dst_conn, "conv-1").unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&imported_conversation).unwrap(),
            serde_json::to_string_pretty(&conversation("conv-1")).unwrap(),
            "the imported conversation must match what was exported"
        );
        assert_eq!(
            crate::projects::get(&dst_conn, "proj-1").unwrap().name,
            "Research"
        );
        let artifact = crate::artifacts::get(&dst_conn, "conv-1", "art-1").unwrap();
        assert_eq!(artifact.filename, "report.md");
        assert_eq!(artifact.content, "# Report");
    }

    /// A corrupt/unparseable entry must be skipped, never abort the whole
    /// import — same posture `crate::export` upholds on the write side.
    #[test]
    fn a_corrupt_entry_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        {
            let file = std::fs::File::create(&dest).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();

            zip.start_file("conversations/conv-1.json", options)
                .unwrap();
            std::io::Write::write_all(
                &mut zip,
                serde_json::to_string_pretty(&conversation("conv-1"))
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();

            zip.start_file("conversations/garbage.json", options)
                .unwrap();
            std::io::Write::write_all(&mut zip, b"not valid json at all").unwrap();

            zip.finish().unwrap();
        }

        let conn = test_conn();
        let summary = read_import_zip(&conn, &dest).unwrap();
        assert_eq!(
            summary.conversations, 1,
            "the well-formed entry still imports"
        );
        assert_eq!(
            summary.skipped, 1,
            "the garbage entry is skipped, not fatal"
        );

        assert_eq!(crate::store::get(&conn, "conv-1").unwrap().id, "conv-1");
    }

    #[test]
    fn unrecognized_paths_are_skipped_silently_not_counted_as_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        {
            let file = std::fs::File::create(&dest).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();

            zip.start_file("future/thing.json", options).unwrap();
            std::io::Write::write_all(&mut zip, b"{}").unwrap();

            // An artifact path that doesn't have the `id__filename` shape.
            zip.start_file("artifacts/conv-1/no-separator-here", options)
                .unwrap();
            std::io::Write::write_all(&mut zip, b"content").unwrap();

            zip.finish().unwrap();
        }

        let conn = test_conn();
        let summary = read_import_zip(&conn, &dest).unwrap();
        assert_eq!(summary.conversations, 0);
        assert_eq!(summary.projects, 0);
        assert_eq!(summary.artifacts, 0);
        // `future/thing.json` is silently skipped (forward-compat, not an
        // error); the malformed artifact path is counted as skipped.
        assert_eq!(summary.skipped, 1);
    }

    #[test]
    fn importing_a_file_that_is_not_a_zip_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("not-a-zip.zip");
        std::fs::write(&dest, b"definitely not a zip archive").unwrap();

        let conn = test_conn();
        assert!(matches!(
            read_import_zip(&conn, &dest),
            Err(Error::DataImport(_))
        ));
    }
}
