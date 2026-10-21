//! Plaintext zip export of the user's local data: conversations, projects
//! and artifacts, plus — each only when the user ticks it, see
//! [`ExportCategories`] — settings, skills, the custom-model list and the
//! plugins' enable/setup records.
//!
//! Everything reads from the app-wide SQLCipher database (`crate::app_db`).
//!
//! Split into two phases so a caller never holds `app_db`'s lock for the
//! (much slower) zip-compression/disk-write work: [`read_export_data`] does
//! every SQL read and returns an owned, `Connection`-free [`ExportData`]
//! snapshot; [`write_export_zip`] takes that snapshot and does the actual
//! archive I/O. See `crates/app/src/commands.rs`'s `export_data` command for
//! how the two are meant to be used — read under the lock, then release it
//! before writing. [`estimate_export_size`] is the same second phase aimed
//! at a byte counter instead of a file, so the size the user is shown is
//! exactly what the export writes.
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
//! - `settings.json` — the `Settings` row, pretty-printed.
//! - `skills/<slug>/SKILL.md` plus `skills/<slug>/<path>` per supporting
//!   file — the same Markdown shape a single skill's own export uses
//!   (`crate::skills::export_zip_bytes`), so each skill opens in any editor.
//! - `custom-models.json` — the custom-model list (`ModelSpec` entries
//!   only; model files never go in the zip).
//! - `plugins.json` — each plugin's enabled/setup record
//!   (`crate::plugin::export_records_json`).
//!
//! The first three are the "chats" category and the only ones an export
//! made before the others existed contains; `crate::import` treats every
//! file as optional.
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

use crate::types::{ModelSpec, Project, Settings, Skill};
use crate::{Error, Result};
use rusqlite::Connection;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

/// Which parts of the user's data an export includes — one Settings →
/// Your data checkbox each. `chats` covers conversations, projects and
/// artifacts together (everything an export carried before the other
/// categories existed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCategories {
    pub chats: bool,
    pub settings: bool,
    pub skills: bool,
    pub custom_models: bool,
    pub plugins: bool,
}

impl ExportCategories {
    /// Every category — what the export row's checkboxes default to.
    pub const ALL: Self = Self {
        chats: true,
        settings: true,
        skills: true,
        custom_models: true,
        plugins: true,
    };
}

/// How much there is in each category right now — shown beside the export's
/// checkboxes so the user knows what each one would carry before ticking it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCounts {
    pub conversations: usize,
    pub projects: usize,
    pub artifacts: usize,
    /// Settings whose value differs from a fresh install's. Every export
    /// carries all of them; this is the part that's actually the user's own.
    pub changed_settings: usize,
    pub skills: usize,
    pub custom_models: usize,
    /// Plugins the records say are turned on.
    pub enabled_plugins: usize,
}

/// Count every category with a `COUNT(*)` or a small row read — fast enough
/// to run under `app_db`'s lock each time Settings → Your data opens.
pub fn count_export_contents(conn: &Connection) -> ExportCounts {
    let count = |table: &str| -> usize {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|n| n as usize)
        .unwrap_or_else(|e| {
            tracing::warn!("export: couldn't count {table}: {e}");
            0
        })
    };
    ExportCounts {
        conversations: count("conversations"),
        projects: count("projects"),
        artifacts: count("artifacts"),
        changed_settings: changed_settings_count(&crate::settings::load(conn)),
        skills: count("skills"),
        custom_models: crate::custom::load(conn).all().len(),
        enabled_plugins: crate::plugin::enabled_record_count(conn),
    }
}

/// How many of `settings`' fields differ from `Settings::default()`.
/// `onboardingCompleted` is left out: every install that reaches Settings
/// has it set, and it isn't something the user would recognise as a
/// setting of theirs.
fn changed_settings_count(settings: &Settings) -> usize {
    let (Ok(serde_json::Value::Object(ours)), Ok(serde_json::Value::Object(defaults))) = (
        serde_json::to_value(settings),
        serde_json::to_value(Settings::default()),
    ) else {
        return 0;
    };
    ours.iter()
        .filter(|(key, value)| {
            key.as_str() != "onboardingCompleted" && defaults.get(*key) != Some(value)
        })
        .count()
}

/// How many zip entries were written per top-level category, plus how many
/// source rows existed but couldn't be read.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub conversations: usize,
    pub projects: usize,
    pub artifacts: usize,
    pub skills: usize,
    pub custom_models: usize,
    /// Whether `settings.json` was written.
    pub settings: bool,
    /// How many plugin records `plugins.json` carries.
    pub plugins: usize,
    pub skipped: usize,
}

/// Everything [`write_export_zip`] needs from the database, read once under
/// a short-lived lock. Holds no `Connection` — safe to carry across an
/// `.await`/onto a blocking thread. A category left out of the
/// [`ExportCategories`] it was read with is simply empty/`None` here.
pub struct ExportData {
    conversations: Vec<(String, String)>,
    projects: Vec<Project>,
    artifacts: Vec<(String, String, String, String)>,
    settings: Option<Settings>,
    skills: Vec<Skill>,
    custom_models: Option<Vec<ModelSpec>>,
    /// `plugins.json`'s finished text, with how many records it holds.
    plugins: Option<(String, usize)>,
    /// Rows that existed but failed to extract at the SQL level (e.g. a
    /// type mismatch) — counted here since [`write_export_zip`] never
    /// touches the database and can't discover these itself.
    skipped: usize,
}

/// Read every ticked category. Fast (a handful of `SELECT`s) — meant to run
/// while `conn`'s lock is held, immediately followed by releasing it before
/// the caller moves on to [`write_export_zip`]/[`estimate_export_size`].
pub fn read_export_data(conn: &Connection, include: &ExportCategories) -> ExportData {
    let mut skipped = 0;
    let (conversations, projects, artifacts) = if include.chats {
        (
            read_conversations(conn, &mut skipped),
            crate::projects::list(conn),
            read_artifacts(conn, &mut skipped),
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let plugins = include
        .plugins
        .then(|| match crate::plugin::export_records_json(conn) {
            Ok(exported) => Some(exported),
            Err(e) => {
                tracing::warn!("export: couldn't serialize plugin records: {e}");
                skipped += 1;
                None
            }
        })
        .flatten();
    ExportData {
        conversations,
        projects,
        artifacts,
        settings: include.settings.then(|| crate::settings::load(conn)),
        skills: if include.skills {
            crate::skills::list(conn)
        } else {
            Vec::new()
        },
        custom_models: include
            .custom_models
            .then(|| crate::custom::load(conn).all().to_vec()),
        plugins,
        skipped,
    }
}

pub fn write_export_zip(data: &ExportData, dest: &Path) -> Result<ExportSummary> {
    let file = File::create(dest)?;
    write_archive(data, &[], file).map(|(summary, _)| summary)
}

/// The same archive [`write_export_zip`] writes, built in memory with
/// `extra` `(path, bytes)` entries appended after the usual ones — for
/// `crate::backup`, which encrypts the whole archive before it touches disk
/// and adds the agent audit log the plaintext export leaves out.
pub(crate) fn archive_bytes(
    data: &ExportData,
    extra: &[(&str, &[u8])],
) -> Result<(ExportSummary, Vec<u8>)> {
    write_archive(data, extra, std::io::Cursor::new(Vec::new()))
        .map(|(summary, out)| (summary, out.into_inner()))
}

/// The exact size in bytes of the zip [`write_export_zip`] would write for
/// `data`: builds the same archive into a counter that keeps no bytes. As
/// slow as the export's compression itself, so — like it — run it off
/// `app_db`'s lock and off the async executor.
pub fn estimate_export_size(data: &ExportData) -> Result<u64> {
    write_archive(data, &[], ByteCounter::default()).map(|(_, counter)| counter.len)
}

/// The shared body of [`write_export_zip`], [`estimate_export_size`] and
/// [`archive_bytes`]: every entry, in one fixed order, then `extra`, into
/// whatever `out` is.
fn write_archive<W: Write + Seek>(
    data: &ExportData,
    extra: &[(&str, &[u8])],
    out: W,
) -> Result<(ExportSummary, W)> {
    let mut zip = ZipWriter::new(out);
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

    if let Some(settings) = &data.settings {
        match serde_json::to_string_pretty(settings) {
            Ok(json) if write_entry(&mut zip, options, "settings.json", json.as_bytes()) => {
                summary.settings = true;
            }
            _ => summary.skipped += 1,
        }
    }

    for skill in &data.skills {
        let skill_md = crate::skills::export_markdown(skill);
        let zip_path = format!("skills/{}/SKILL.md", skill.slug);
        if !write_entry(&mut zip, options, &zip_path, skill_md.as_bytes()) {
            summary.skipped += 1;
            continue;
        }
        summary.skills += 1;
        for file in &skill.files {
            let zip_path = format!("skills/{}/{}", skill.slug, file.path);
            if !write_entry(&mut zip, options, &zip_path, file.content.as_bytes()) {
                summary.skipped += 1;
            }
        }
    }

    if let Some(models) = &data.custom_models {
        match serde_json::to_string_pretty(models) {
            Ok(json) if write_entry(&mut zip, options, "custom-models.json", json.as_bytes()) => {
                summary.custom_models = models.len();
            }
            _ => summary.skipped += 1,
        }
    }

    if let Some((json, count)) = &data.plugins {
        if write_entry(&mut zip, options, "plugins.json", json.as_bytes()) {
            summary.plugins = *count;
        } else {
            summary.skipped += 1;
        }
    }

    for (zip_path, bytes) in extra {
        if !write_entry(&mut zip, options, zip_path, bytes) {
            summary.skipped += 1;
        }
    }

    let out = zip
        .finish()
        .map_err(|e| Error::Export(format!("couldn't finish writing the export archive: {e}")))?;
    Ok((summary, out))
}

/// A seekable sink that only tracks how far anything was written — the zip
/// writer seeks back to patch each entry's header once it knows the
/// compressed size, so a plain `io::sink()` isn't enough.
#[derive(Default)]
struct ByteCounter {
    pos: u64,
    len: u64,
}

impl Write for ByteCounter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pos += buf.len() as u64;
        self.len = self.len.max(self.pos);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Seek for ByteCounter {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        };
        self.pos = target.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start")
        })?;
        Ok(self.pos)
    }
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
fn write_entry<W: Write + Seek>(
    zip: &mut ZipWriter<W>,
    options: SimpleFileOptions,
    zip_path: &str,
    bytes: &[u8],
) -> bool {
    if zip.start_file(zip_path, options).is_err() {
        tracing::warn!("export: couldn't start zip entry {zip_path}");
        return false;
    }
    if zip.write_all(bytes).is_err() {
        tracing::warn!("export: couldn't write zip entry {zip_path}");
        return false;
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::types::{Conversation, SamplingParams, SkillFile, TaskType};

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    /// Something stored in every category, so a test can tell which ones an
    /// archive carries: one chat (with its project and artifact), settings
    /// with a recognisable name, a skill with a supporting file, a
    /// local-file custom model whose file doesn't exist, and a plugin record
    /// that says enabled and set up.
    pub(crate) fn populate_every_category(conn: &mut Connection) {
        crate::store::save(conn, &conversation("conv-1")).unwrap();
        crate::projects::save(
            conn,
            &Project {
                id: "proj-1".to_string(),
                name: "Research".to_string(),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        crate::artifacts::save(conn, "conv-1", "art-1", "report.md", "# Report").unwrap();

        let settings = Settings {
            user_name: Some("Ada".to_string()),
            force_cpu: true,
            theme: "dark".to_string(),
            ..Settings::default()
        };
        crate::settings::save(conn, &settings).unwrap();

        crate::skills::replace_from_export(
            conn,
            "summarize",
            "---\nname: Summarize\ndescription: Make it short\n---\n\nSummarize the text.\n",
            &[SkillFile {
                path: "examples/one.md".to_string(),
                content: "An example.".to_string(),
            }],
        )
        .unwrap();

        let mut custom = crate::custom::CustomModels::default();
        custom.add(crate::custom::local_spec(
            Path::new("/nowhere/on/this/device/my-model-Q4_K_M.gguf"),
            1_000_000,
            &[],
        ));
        crate::custom::save(conn, &custom).unwrap();

        conn.execute(
            "INSERT INTO plugin_records (key, json) VALUES ('current', ?1)",
            [r#"{"voice":{"enabled":true,"setupVersion":"1.0.0"}}"#],
        )
        .unwrap();
    }

    fn entry_names(zip_path: &Path) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
        let mut names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        names
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

        let data = read_export_data(&conn, &ExportCategories::ALL);
        let dest = tmp.path().join("export.zip");
        let summary = write_export_zip(&data, &dest).unwrap();

        // Nothing but chats stored: the other categories add only an empty
        // custom-model list, an empty plugin map and the default settings.
        assert_eq!(
            entry_names(&dest),
            vec![
                "artifacts/conv-1/art-1__report.md",
                "conversations/conv-1.json",
                "custom-models.json",
                "plugins.json",
                "projects/proj-1.json",
                "settings.json",
            ]
        );
        assert_eq!(
            (
                summary.conversations,
                summary.projects,
                summary.artifacts,
                summary.skipped
            ),
            (1, 1, 1, 0)
        );

        let mut archive = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
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

        let data = read_export_data(&conn, &ExportCategories::ALL);
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

        let data = read_export_data(&conn, &ExportCategories::ALL);
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

    #[test]
    fn everything_ticked_carries_every_category() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = test_conn();
        populate_every_category(&mut conn);

        let data = read_export_data(&conn, &ExportCategories::ALL);
        let dest = tmp.path().join("export.zip");
        let summary = write_export_zip(&data, &dest).unwrap();

        assert_eq!(
            entry_names(&dest),
            vec![
                "artifacts/conv-1/art-1__report.md",
                "conversations/conv-1.json",
                "custom-models.json",
                "plugins.json",
                "projects/proj-1.json",
                "settings.json",
                "skills/summarize/SKILL.md",
                "skills/summarize/examples/one.md",
            ]
        );
        assert_eq!(
            (
                summary.skills,
                summary.custom_models,
                summary.settings,
                summary.plugins,
                summary.skipped
            ),
            (1, 1, true, 1, 0)
        );

        let mut archive = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
        let mut read = |name: &str| {
            let mut out = String::new();
            std::io::Read::read_to_string(&mut archive.by_name(name).unwrap(), &mut out).unwrap();
            out
        };
        let models: Vec<ModelSpec> = serde_json::from_str(&read("custom-models.json")).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0].local_path.as_deref(),
            Some(Path::new("/nowhere/on/this/device/my-model-Q4_K_M.gguf")),
            "the entry only — no model file rides along"
        );
        let plugins: serde_json::Value = serde_json::from_str(&read("plugins.json")).unwrap();
        assert_eq!(
            plugins["voice"],
            serde_json::json!({"enabled": true, "setupVersion": "1.0.0"})
        );
        let settings: Settings = serde_json::from_str(&read("settings.json")).unwrap();
        assert_eq!(settings.user_name.as_deref(), Some("Ada"));
        assert!(settings.force_cpu, "machine-specific settings go too");
    }

    #[test]
    fn an_unticked_category_contributes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = test_conn();
        populate_every_category(&mut conn);

        let cases: [(ExportCategories, &[&str]); 5] = [
            (
                ExportCategories {
                    chats: false,
                    ..ExportCategories::ALL
                },
                &["conversations/", "projects/", "artifacts/"],
            ),
            (
                ExportCategories {
                    settings: false,
                    ..ExportCategories::ALL
                },
                &["settings.json"],
            ),
            (
                ExportCategories {
                    skills: false,
                    ..ExportCategories::ALL
                },
                &["skills/"],
            ),
            (
                ExportCategories {
                    custom_models: false,
                    ..ExportCategories::ALL
                },
                &["custom-models.json"],
            ),
            (
                ExportCategories {
                    plugins: false,
                    ..ExportCategories::ALL
                },
                &["plugins.json"],
            ),
        ];
        for (include, absent_prefixes) in cases {
            let dest = tmp.path().join("export.zip");
            write_export_zip(&read_export_data(&conn, &include), &dest).unwrap();
            let names = entry_names(&dest);
            for prefix in absent_prefixes {
                assert!(
                    !names.iter().any(|n| n.starts_with(prefix)),
                    "{include:?} must leave out {prefix}, got {names:?}"
                );
            }
            assert!(
                names.len() >= 2,
                "every other category is still there: {names:?}"
            );
        }
    }

    #[test]
    fn the_estimate_is_exactly_the_size_written() {
        let tmp = tempfile::tempdir().unwrap();
        let mut conn = test_conn();
        populate_every_category(&mut conn);
        let big = "lorem ipsum dolor sit amet ".repeat(20_000);
        crate::artifacts::save(&conn, "conv-1", "art-2", "big.md", &big).unwrap();

        for include in [
            ExportCategories::ALL,
            ExportCategories {
                chats: false,
                ..ExportCategories::ALL
            },
            ExportCategories {
                chats: true,
                settings: false,
                skills: false,
                custom_models: false,
                plugins: false,
            },
        ] {
            let data = read_export_data(&conn, &include);
            let estimate = estimate_export_size(&data).unwrap();
            let dest = tmp.path().join("export.zip");
            write_export_zip(&data, &dest).unwrap();
            assert_eq!(
                estimate,
                std::fs::metadata(&dest).unwrap().len(),
                "{include:?}"
            );
        }
    }

    #[test]
    fn nothing_ticked_estimates_an_empty_archive() {
        let conn = test_conn();
        let none = ExportCategories {
            chats: false,
            settings: false,
            skills: false,
            custom_models: false,
            plugins: false,
        };
        // An empty zip is just its 22-byte end-of-central-directory record.
        assert_eq!(
            estimate_export_size(&read_export_data(&conn, &none)).unwrap(),
            22
        );
    }

    #[test]
    fn counts_every_category_and_only_settings_the_user_changed() {
        let mut conn = test_conn();
        assert_eq!(count_export_contents(&conn), ExportCounts::default());

        populate_every_category(&mut conn);
        crate::artifacts::save(&conn, "conv-1", "art-2", "notes.md", "Notes").unwrap();
        let mut settings = crate::settings::load(&conn);
        settings.onboarding_completed = true;
        crate::settings::save(&conn, &settings).unwrap();

        assert_eq!(
            count_export_contents(&conn),
            ExportCounts {
                conversations: 1,
                projects: 1,
                artifacts: 2,
                // userName, forceCpu and theme; never onboardingCompleted.
                changed_settings: 3,
                skills: 1,
                custom_models: 1,
                enabled_plugins: 1,
            }
        );
    }
}
