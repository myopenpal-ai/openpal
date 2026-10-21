//! Read-back counterpart to `crate::export`: reads a zip produced by
//! [`crate::export::write_export_zip`] and brings back whatever it contains —
//! conversations, projects, artifacts, settings, skills, custom models and
//! plugin records — into the app-wide SQLCipher database
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
//! - `settings.json` — parsed as `Settings` and saved over this device's,
//!   machine-specific fields included (the user chose full replacement over
//!   keeping Force CPU, sync and the like). A relay URL `update_settings`
//!   would refuse makes it skipped instead.
//! - `skills/<slug>/SKILL.md` plus that folder's supporting files — each
//!   skill replaces the local one with the same slug
//!   (`crate::skills::replace_from_export`); other local skills stay.
//! - `custom-models.json` — each entry replaces the local one with the same
//!   id, or is added; other local entries stay. A local-file entry whose
//!   file isn't on this device just reads as not installed.
//! - `plugins.json` — each plugin's `enabled` is taken; setup state stays
//!   this device's own (`crate::plugin::import_records_json`).
//!
//! An export made before the last four existed simply has none of them, and
//! leaves those parts of this device untouched. The app layer must reload
//! whatever it keeps in memory of the last four afterwards — see
//! `crates/app/src/commands.rs`'s `import_data`.
//! - Anything else: skipped silently, not counted as an error — forward
//!   compatible with a newer export format adding entries this older import
//!   doesn't understand yet.
//!
//! One bad entry never sinks the whole import — same "skip with a warning,
//! keep going" posture `crate::export` (and `store::list`/`projects::list`)
//! already use. [`read_import_zip`] only returns `Err` if `src` itself can't
//! be opened as a zip — everything else is counted in
//! [`ImportSummary::skipped`], never fatal.

use crate::types::{Conversation, ModelSpec, Project, Settings, SkillFile};
use crate::{Error, Result};
use rusqlite::Connection;
use std::collections::BTreeMap;
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
    pub skills: usize,
    pub custom_models: usize,
    /// Whether `settings.json` replaced this device's settings.
    pub settings: bool,
    /// How many plugin records `plugins.json` carried.
    pub plugins: usize,
    pub skipped: usize,
}

/// One skill's entries, gathered from anywhere in the archive before it's
/// written — zip order doesn't promise `SKILL.md` comes first.
#[derive(Default)]
struct PendingSkill {
    skill_md: Option<String>,
    files: Vec<SkillFile>,
}

/// Read the zip at `src` (as written by
/// [`crate::export::write_export_zip`]) and bring every entry it contains
/// into `conn`.
pub fn read_import_zip(conn: &mut Connection, src: &Path) -> Result<ImportSummary> {
    let file = std::fs::File::open(src)
        .map_err(|e| Error::DataImport(format!("couldn't open the import archive: {e}")))?;
    let mut archive = ZipArchive::new(file).map_err(|e| {
        Error::DataImport(format!("that doesn't look like a valid export .zip: {e}"))
    })?;
    Ok(import_archive(conn, &mut archive).0)
}

/// The body of [`read_import_zip`], for an archive from anywhere — a file,
/// or a backup `crate::backup` has just decrypted in memory. Also returns
/// how many writes the database itself refused (disk full, say), which
/// [`ImportSummary::skipped`] can't tell apart from an entry that didn't
/// parse: a restore that must be all-or-nothing rolls back on those.
pub(crate) fn import_archive<R: std::io::Read + std::io::Seek>(
    conn: &mut Connection,
    archive: &mut ZipArchive<R>,
) -> (ImportSummary, usize) {
    let mut summary = ImportSummary::default();
    let mut failed_writes = 0;
    let mut note = |e: &Error| {
        if matches!(e, Error::Db(_)) {
            failed_writes += 1;
        }
    };
    let mut skills: BTreeMap<String, PendingSkill> = BTreeMap::new();

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
                        note(&e);
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
                        note(&e);
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
                            note(&e);
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

        if let Some((slug, path)) = name
            .strip_prefix("skills/")
            .and_then(|rest| rest.split_once('/'))
        {
            if entry.is_dir() {
                continue;
            }
            let pending = skills.entry(slug.to_string()).or_default();
            match read_entry_to_string(&mut entry) {
                Ok(contents) if path == "SKILL.md" => pending.skill_md = Some(contents),
                Ok(content) => pending.files.push(SkillFile {
                    path: path.to_string(),
                    content,
                }),
                Err(e) => {
                    tracing::warn!("import: couldn't read skill entry {name}: {e}");
                    summary.skipped += 1;
                }
            }
            continue;
        }

        type ImportOne = fn(&Connection, &str, &mut ImportSummary) -> Option<Error>;
        let single_file = match name.as_str() {
            "settings.json" => Some(import_settings as ImportOne),
            "custom-models.json" => Some(import_custom_models as _),
            "plugins.json" => Some(import_plugins as _),
            _ => None,
        };
        if let Some(import) = single_file {
            match read_entry_to_string(&mut entry) {
                Ok(contents) => {
                    if let Some(e) = import(conn, &contents, &mut summary) {
                        note(&e);
                    }
                }
                Err(e) => {
                    tracing::warn!("import: couldn't read {name}: {e}");
                    summary.skipped += 1;
                }
            }
            continue;
        }

        // Any other/unexpected path: forward-compat, not an error.
    }

    for (slug, mut pending) in skills {
        let Some(skill_md) = pending.skill_md else {
            tracing::warn!("import: skipping skill {slug}: no SKILL.md");
            summary.skipped += 1;
            continue;
        };
        pending.files.sort_by(|a, b| a.path.cmp(&b.path));
        match crate::skills::replace_from_export(conn, &slug, &skill_md, &pending.files) {
            Ok(_) => summary.skills += 1,
            Err(e) => {
                tracing::warn!("import: skipping skill {slug}: {e}");
                note(&e);
                summary.skipped += 1;
            }
        }
    }

    (summary, failed_writes)
}

/// `settings.json`: replaces this device's settings outright. Like the two
/// below, returns the database's own error when saving failed, for
/// [`import_archive`]'s failed-write count.
fn import_settings(
    conn: &Connection,
    contents: &str,
    summary: &mut ImportSummary,
) -> Option<Error> {
    let settings = serde_json::from_str::<Settings>(contents)
        .map_err(|e| e.to_string())
        .and_then(|settings| {
            if let Some(url) = &settings.sync_relay_url {
                crate::sync::mailbox_client::validate_relay_url(url)?;
            }
            Ok(settings)
        });
    let result = match settings {
        Ok(settings) => crate::settings::save(conn, &settings),
        Err(e) => {
            tracing::warn!("import: skipping settings: {e}");
            summary.skipped += 1;
            return None;
        }
    };
    match result {
        Ok(()) => {
            summary.settings = true;
            None
        }
        Err(e) => {
            tracing::warn!("import: skipping settings: {e}");
            summary.skipped += 1;
            Some(e)
        }
    }
}

/// `custom-models.json`: merged into this device's list by id. Each entry
/// is parsed on its own, so one that no longer fits `ModelSpec` is skipped
/// without losing the rest.
fn import_custom_models(
    conn: &Connection,
    contents: &str,
    summary: &mut ImportSummary,
) -> Option<Error> {
    let entries: Vec<serde_json::Value> = match serde_json::from_str(contents) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("import: skipping custom models: {e}");
            summary.skipped += 1;
            return None;
        }
    };
    let mut custom = crate::custom::load(conn);
    let mut imported = 0;
    for entry in entries {
        match serde_json::from_value::<ModelSpec>(entry) {
            Ok(spec) => {
                custom.upsert(spec);
                imported += 1;
            }
            Err(e) => {
                tracing::warn!("import: skipping a custom model: {e}");
                summary.skipped += 1;
            }
        }
    }
    match crate::custom::save(conn, &custom) {
        Ok(()) => {
            summary.custom_models += imported;
            None
        }
        Err(e) => {
            tracing::warn!("import: couldn't save custom models: {e}");
            summary.skipped += imported;
            Some(e)
        }
    }
}

/// `plugins.json`: each plugin's enabled switch, merged by id.
fn import_plugins(conn: &Connection, contents: &str, summary: &mut ImportSummary) -> Option<Error> {
    match crate::plugin::import_records_json(conn, contents) {
        Ok(count) => summary.plugins = count,
        Err(e) => {
            tracing::warn!("import: skipping plugin records: {e}");
            summary.skipped += 1;
        }
    }
    None
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
    use crate::export::tests::populate_every_category as populate;
    use crate::export::ExportCategories;
    use crate::types::{SamplingParams, Skill, TaskType};

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

        let data =
            crate::export::read_export_data(&src_conn, &crate::export::ExportCategories::ALL);
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
        let mut dst_conn = test_conn();
        let import_summary = read_import_zip(&mut dst_conn, &dest).unwrap();
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

        let mut conn = test_conn();
        let summary = read_import_zip(&mut conn, &dest).unwrap();
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

        let mut conn = test_conn();
        let summary = read_import_zip(&mut conn, &dest).unwrap();
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

        let mut conn = test_conn();
        assert!(matches!(
            read_import_zip(&mut conn, &dest),
            Err(Error::DataImport(_))
        ));
    }

    fn export(conn: &Connection, include: &ExportCategories, dest: &Path) {
        let data = crate::export::read_export_data(conn, include);
        crate::export::write_export_zip(&data, dest).unwrap();
    }

    fn skill(conn: &Connection, slug: &str) -> Skill {
        crate::skills::get(conn, slug).unwrap()
    }

    fn write_zip(dest: &Path, entries: &[(&str, &str)]) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(dest).unwrap());
        for (name, contents) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut zip, contents.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    /// Export everything on one device, import it on a fresh one with a
    /// different data directory and some data of its own: every new
    /// category comes back, and what the other device didn't have is kept.
    #[test]
    fn every_new_category_round_trips_onto_a_fresh_install() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        let mut src = test_conn();
        populate(&mut src);
        export(&src, &ExportCategories::ALL, &dest);

        let mut dst = test_conn();
        crate::skills::replace_from_export(&mut dst, "local-only", "Mine.", &[]).unwrap();
        let mut local = crate::custom::CustomModels::default();
        local.add(crate::custom::local_spec(
            Path::new("/here/kept.gguf"),
            1,
            &[],
        ));
        crate::custom::save(&dst, &local).unwrap();
        dst.execute(
            "INSERT INTO plugin_records (key, json) VALUES ('current', ?1)",
            [r#"{"writing-tools":{"enabled":true,"setupVersion":"2.0.0"}}"#],
        )
        .unwrap();

        let summary = read_import_zip(&mut dst, &dest).unwrap();
        assert_eq!(
            (
                summary.conversations,
                summary.projects,
                summary.artifacts,
                summary.skills,
                summary.custom_models,
                summary.settings,
                summary.plugins,
                summary.skipped
            ),
            (1, 1, 1, 1, 1, true, 1, 0)
        );

        assert_eq!(
            serde_json::to_value(crate::settings::load(&dst)).unwrap(),
            serde_json::to_value(crate::settings::load(&src)).unwrap(),
            "settings are replaced wholesale, machine-specific ones included"
        );

        let summarize = skill(&dst, "summarize");
        assert_eq!(
            (
                summarize.name.as_str(),
                summarize.description.as_str(),
                summarize.body.as_str()
            ),
            ("Summarize", "Make it short", "Summarize the text.")
        );
        assert_eq!(summarize.files.len(), 1);
        assert_eq!(summarize.files[0].path, "examples/one.md");
        assert_eq!(summarize.files[0].content, "An example.");
        assert_eq!(skill(&dst, "local-only").body, "Mine.");

        let custom = crate::custom::load(&dst);
        let src_custom = crate::custom::load(&src);
        let imported_id = &src_custom.all()[0].id;
        assert_eq!(custom.all().len(), 2, "the local entry is kept");
        assert_eq!(
            serde_json::to_value(custom.get(imported_id).unwrap()).unwrap(),
            serde_json::to_value(&src_custom.all()[0]).unwrap()
        );

        let (plugins, _) = crate::plugin::export_records_json(&dst).unwrap();
        let plugins: serde_json::Value = serde_json::from_str(&plugins).unwrap();
        assert_eq!(
            plugins["voice"],
            serde_json::json!({"enabled": true, "setupVersion": null}),
            "enabled comes over; setup stays this device's own (never set up here)"
        );
        assert_eq!(
            plugins["writing-tools"],
            serde_json::json!({"enabled": true, "setupVersion": "2.0.0"}),
            "a plugin the export doesn't mention is left alone"
        );
    }

    #[test]
    fn an_existing_skill_or_custom_model_is_replaced_not_duplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        let mut src = test_conn();
        populate(&mut src);
        export(&src, &ExportCategories::ALL, &dest);
        let imported_spec = crate::custom::load(&src).all()[0].clone();

        let mut dst = test_conn();
        crate::skills::replace_from_export(
            &mut dst,
            "summarize",
            "Old local body.",
            &[SkillFile {
                path: "stale.md".to_string(),
                content: "Stale.".to_string(),
            }],
        )
        .unwrap();
        let mut local = crate::custom::CustomModels::default();
        local.add(ModelSpec {
            name: "Local name".to_string(),
            ..imported_spec.clone()
        });
        crate::custom::save(&dst, &local).unwrap();

        read_import_zip(&mut dst, &dest).unwrap();

        let summarize = skill(&dst, "summarize");
        assert_eq!(summarize.body, "Summarize the text.");
        assert_eq!(
            summarize
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["examples/one.md"],
            "the local copy's supporting files go with it"
        );
        assert_eq!(crate::skills::list(&dst).len(), 1);

        let custom = crate::custom::load(&dst);
        assert_eq!(custom.all().len(), 1);
        assert_eq!(custom.all()[0].name, imported_spec.name);
    }

    /// A zip from before the new categories existed — exactly what today's
    /// chats-only export writes — still imports its chats and touches
    /// nothing else.
    #[test]
    fn a_chats_only_export_leaves_the_new_categories_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        let mut src = test_conn();
        populate(&mut src);
        let chats_only = ExportCategories {
            chats: true,
            settings: false,
            skills: false,
            custom_models: false,
            plugins: false,
        };
        export(&src, &chats_only, &dest);

        let mut dst = test_conn();
        let dst_settings = Settings {
            user_name: Some("Grace".to_string()),
            ..Settings::default()
        };
        crate::settings::save(&dst, &dst_settings).unwrap();
        crate::skills::replace_from_export(&mut dst, "local-only", "Mine.", &[]).unwrap();
        let mut local = crate::custom::CustomModels::default();
        local.add(crate::custom::local_spec(
            Path::new("/here/kept.gguf"),
            1,
            &[],
        ));
        crate::custom::save(&dst, &local).unwrap();
        crate::plugin::import_records_json(&dst, r#"{"voice":{"enabled":false}}"#).unwrap();
        let before = (
            serde_json::to_value(crate::settings::load(&dst)).unwrap(),
            serde_json::to_value(crate::skills::list(&dst)).unwrap(),
            serde_json::to_value(crate::custom::load(&dst).all()).unwrap(),
            crate::plugin::export_records_json(&dst).unwrap().0,
        );

        let summary = read_import_zip(&mut dst, &dest).unwrap();
        assert_eq!(
            (
                summary.conversations,
                summary.projects,
                summary.artifacts,
                summary.skills,
                summary.custom_models,
                summary.settings,
                summary.plugins,
                summary.skipped
            ),
            (1, 1, 1, 0, 0, false, 0, 0)
        );
        let after = (
            serde_json::to_value(crate::settings::load(&dst)).unwrap(),
            serde_json::to_value(crate::skills::list(&dst)).unwrap(),
            serde_json::to_value(crate::custom::load(&dst).all()).unwrap(),
            crate::plugin::export_records_json(&dst).unwrap().0,
        );
        assert_eq!(before, after);
    }

    #[test]
    fn a_bad_entry_in_a_new_category_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        let good_spec = crate::custom::local_spec(Path::new("/x/good.gguf"), 1, &[]);
        let custom_models = format!(
            "[{}, {{\"not\": \"a model\"}}]",
            serde_json::to_string(&good_spec).unwrap()
        );
        write_zip(
            &dest,
            &[
                (
                    "conversations/conv-1.json",
                    &serde_json::to_string_pretty(&conversation("conv-1")).unwrap(),
                ),
                ("settings.json", "{not json"),
                ("custom-models.json", &custom_models),
                ("plugins.json", "[]"),
                ("skills/no-skill-md/notes.md", "Orphan."),
                ("skills/Bad Slug/SKILL.md", "Body."),
                ("skills/fine/SKILL.md", "Fine."),
            ],
        );

        let mut conn = test_conn();
        let summary = read_import_zip(&mut conn, &dest).unwrap();
        assert_eq!(
            (
                summary.conversations,
                summary.skills,
                summary.custom_models,
                summary.settings,
                summary.plugins
            ),
            (1, 1, 1, false, 0)
        );
        assert_eq!(
            summary.skipped, 5,
            "settings, one model entry, plugins, and two skills"
        );
        assert_eq!(skill(&conn, "fine").body, "Fine.");
        assert!(crate::custom::load(&conn).get(&good_spec.id).is_some());
        assert!(crate::settings::load(&conn).user_name.is_none());
    }

    /// The same refusal `update_settings` gives a plain-HTTP relay applies
    /// to an imported one.
    #[test]
    fn imported_settings_with_a_refused_relay_url_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("export.zip");
        let settings = Settings {
            user_name: Some("Ada".to_string()),
            sync_relay_url: Some("http://relay.example.com".to_string()),
            ..Settings::default()
        };
        write_zip(
            &dest,
            &[("settings.json", &serde_json::to_string(&settings).unwrap())],
        );

        let mut conn = test_conn();
        let summary = read_import_zip(&mut conn, &dest).unwrap();
        assert!(!summary.settings);
        assert_eq!(summary.skipped, 1);
        assert!(crate::settings::load(&conn).user_name.is_none());
    }
}
