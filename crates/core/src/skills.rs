//! Skill persistence, in the app-wide SQLCipher database (`crate::app_db`) —
//! two tables: `skills` (slug/name/description/body) and `skill_files` (zero
//! or more supporting Markdown files per skill, keyed by `(skill_slug,
//! path)`). A skill used to be either a single `<slug>.md` file or a
//! `<slug>/` folder on disk, hand-editable in place; once storage moved into
//! the encrypted DB that in-place editing was no longer possible, so the
//! contract changed to an explicit round trip instead — export a skill to a
//! `.md` file (or a `.zip` for a folder skill), edit it, import it back. See
//! [`export_zip_bytes`]/`commands::export_skill` and
//! `docs/SPACES_AUTHORING.md` §9's "Decided: skills stay hand-editable, via
//! import/export".
//!
//! Typing `/<slug>` as the first word of a chat message invokes the skill —
//! the frontend folds the body (and any supporting files) in ahead of the
//! rest of the user's words (see `send` in `ui/src/state/store.ts`), the same
//! way a document attachment is folded in ahead of the user's own text. Rust
//! never sees the trigger; it only stores and lists these rows.
//!
//! Like conversations (`crate::store`), a corrupt row is skipped with a
//! `tracing::warn!` — listing skills must never be fatal.
//!
//! [`list_legacy_directory`] and [`migrate_legacy_skills`] are the one-time
//! on-disk-to-database migration for installs that had real `<slug>.md`/
//! `<slug>/` files before this module moved onto `app_db` — see
//! `crate::state::finish_unlock`'s three-way migrate/seed/no-op branch.

use crate::types::{Skill, SkillFile};
use crate::{Error, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Lowercase ascii letters/digits/dashes only — the charset every write goes
/// through ([`slugify`]) and every read of a caller-supplied slug checks
/// again, so a hand-crafted IPC payload can't smuggle something else through.
fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

// ---------------------------------------------------------------------------
// Steady-state reads/writes, against app_db
// ---------------------------------------------------------------------------

/// List every skill, sorted by name (case-insensitive — **not**
/// `ORDER BY ... COLLATE NOCASE`, whose collation is ASCII-only; see
/// `crate::store::search`'s own doc comment for the same Unicode trap).
/// Two queries (skills, then every `skill_files` row grouped by slug) rather
/// than a join: skill counts are tiny, and this keeps corrupt-row tolerance
/// at per-file granularity without an N+1 lookup per skill.
pub fn list(conn: &Connection) -> Vec<Skill> {
    let mut stmt = match conn.prepare("SELECT slug, name, description, body FROM skills") {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("couldn't prepare the skills list query: {e}");
            return Vec::new();
        }
    };
    // Raw columns first, always structurally valid — a row that fails to
    // extract is skipped in the second pass below, never fatal to the rest
    // of the list (mirrors `crate::store::list`'s own two-phase pattern).
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
            tracing::warn!("couldn't query skills: {e}");
            return Vec::new();
        }
    };

    let mut files_by_slug = files_grouped_by_skill(conn);
    let mut skills = Vec::new();
    for row in rows {
        let (slug, name, description, body) = match row {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!("skipping unreadable skill row: {e}");
                continue;
            }
        };
        let files = files_by_slug.remove(&slug).unwrap_or_default();
        skills.push(Skill {
            slug,
            name,
            description,
            body,
            files,
        });
    }
    skills.sort_by_key(|s| s.name.to_ascii_lowercase());
    skills
}

/// One skill by slug, with its supporting files — used by the export
/// command, which (unlike [`list`]) needs a single skill's full content.
pub fn get(conn: &Connection, slug: &str) -> Result<Skill> {
    if !is_valid_slug(slug) {
        return Err(Error::NotFound(format!("skill {slug}")));
    }
    let (name, description, body) = conn
        .query_row(
            "SELECT name, description, body FROM skills WHERE slug = ?1",
            params![slug],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("skill {slug}")),
            other => Error::Db(other),
        })?;
    Ok(Skill {
        slug: slug.to_string(),
        name,
        description,
        body,
        files: files_for_skill(conn, slug),
    })
}

/// Every `skill_files` row, grouped by `skill_slug` — used by [`list`], which
/// wants every skill's files in one pass rather than one query per skill.
fn files_grouped_by_skill(conn: &Connection) -> HashMap<String, Vec<SkillFile>> {
    let mut stmt =
        match conn.prepare("SELECT skill_slug, path, content FROM skill_files ORDER BY path") {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::warn!("couldn't prepare the skill_files query: {e}");
                return HashMap::new();
            }
        };
    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("couldn't query skill_files: {e}");
            return HashMap::new();
        }
    };
    let mut by_slug: HashMap<String, Vec<SkillFile>> = HashMap::new();
    for row in rows {
        let (skill_slug, path, content) = match row {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!("skipping unreadable skill_files row: {e}");
                continue;
            }
        };
        by_slug
            .entry(skill_slug)
            .or_default()
            .push(SkillFile { path, content });
    }
    by_slug
}

/// Every supporting file for one skill — used by [`get`], which only needs a
/// single skill's files. A corrupt row is skipped, not fatal to the rest.
fn files_for_skill(conn: &Connection, slug: &str) -> Vec<SkillFile> {
    let mut stmt = match conn
        .prepare("SELECT path, content FROM skill_files WHERE skill_slug = ?1 ORDER BY path")
    {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("couldn't prepare the skill_files query: {e}");
            return Vec::new();
        }
    };
    let rows = match stmt.query_map(params![slug], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("couldn't query skill_files for {slug}: {e}");
            return Vec::new();
        }
    };
    rows.filter_map(|row| match row {
        Ok((path, content)) => Some(SkillFile { path, content }),
        Err(e) => {
            tracing::warn!("skipping unreadable skill_files row for {slug}: {e}");
            None
        }
    })
    .collect()
}

/// Save `skill`. A new skill (from the editor) arrives with an empty `slug`;
/// this generates one from `name` — unique against what's already in the
/// DB — and returns the saved `Skill` with it filled in. An existing slug is
/// overwritten in place and **never touches `skill_files`** — a save from
/// the simple in-app editor only ever changes name/description/body, never
/// creates or deletes supporting files (those only ever change via
/// [`import_folder`]/[`import_zip`], which always mint a fresh slug). Renaming
/// a skill only ever changes `name`, never the slug/trigger. `skill.files` is
/// never itself written — it's populated by re-reading whatever's in
/// `skill_files` after the write, since it's a read-derived view, not a save
/// input.
pub fn save(conn: &mut Connection, skill: Skill) -> Result<Skill> {
    if skill.slug.is_empty() {
        return insert_new_skill(conn, &skill.name, &skill.description, &skill.body, &[]);
    }
    if !is_valid_slug(&skill.slug) {
        return Err(Error::NotFound(format!("skill {}", skill.slug)));
    }
    conn.execute(
        "INSERT INTO skills (slug, name, description, body) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (slug) DO UPDATE SET name = ?2, description = ?3, body = ?4",
        params![skill.slug, skill.name, skill.description, skill.body],
    )?;
    let files = files_for_skill(conn, &skill.slug);
    Ok(Skill { files, ..skill })
}

/// Remove the skill with `slug` and every one of its supporting files.
/// Idempotent. Wrapped in a transaction so a crash between the two deletes
/// can't orphan `skill_files` rows.
pub fn delete(conn: &mut Connection, slug: &str) -> Result<()> {
    if !is_valid_slug(slug) {
        return Err(Error::NotFound(format!("skill {slug}")));
    }
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM skill_files WHERE skill_slug = ?1",
        params![slug],
    )?;
    tx.execute("DELETE FROM skills WHERE slug = ?1", params![slug])?;
    tx.commit()?;
    Ok(())
}

/// Import a skill from somewhere else on disk — a single `.md` file, or a
/// folder containing a `SKILL.md` (case-insensitive) plus any supporting
/// `.md` files, the same shape [`list`] reads back. Always creates a *new*
/// skill: the slug is generated fresh from the imported name, unique against
/// what's currently in the DB, exactly like [`save`]'s empty-slug case —
/// importing the same source twice yields two separate skills rather than
/// silently overwriting one edited since.
pub fn import(conn: &mut Connection, src: &Path) -> Result<Skill> {
    if src.is_dir() {
        import_folder(conn, src)
    } else {
        import_file(conn, src)
    }
}

/// A skill is a handful of small Markdown files — refuse anything wildly
/// past that shape rather than let a zip bomb (one tiny compressed file that
/// unpacks to gigabytes, or thousands of tiny entries) fill the user's disk
/// or hang the import.
const MAX_ZIP_ENTRIES: usize = 2_000;
const MAX_ZIP_UNCOMPRESSED_BYTES: u64 = 50 * 1024 * 1024;

fn import_file(conn: &mut Connection, src: &Path) -> Result<Skill> {
    let raw = std::fs::read_to_string(src)
        .map_err(|_| Error::Import("that file couldn't be read as text".to_string()))?;
    let fallback_slug = fallback_slug_from(src.file_stem().and_then(|s| s.to_str()));
    let (name, description, body) = parse(&raw, &fallback_slug);
    insert_new_skill(conn, &name, &description, &body, &[])
}

fn import_folder(conn: &mut Connection, src: &Path) -> Result<Skill> {
    let Some(entry) = find_entry_file(src) else {
        return Err(Error::Import(
            "that folder doesn't look like a skill (it needs a SKILL.md file)".to_string(),
        ));
    };
    let raw = std::fs::read_to_string(&entry)?;
    let fallback_slug = fallback_slug_from(src.file_name().and_then(|s| s.to_str()));
    let (name, description, body) = parse(&raw, &fallback_slug);
    let files = collect_supporting_files(src, &entry);
    insert_new_skill(conn, &name, &description, &body, &files)
}

/// Same as [`import`], but `src` is a `.zip` archive of a skill folder (or a
/// folder skill wrapped in one extra top-level directory, the shape most
/// "download as zip" tools produce). Every entry is read straight from the
/// open archive into memory — **never extracted to disk** — so untrusted zip
/// content never transits an unencrypted OS temp directory the way it used
/// to (the fix for the independent bug this module used to have: extracting
/// to `std::env::temp_dir()` before reading it back in). As a side effect
/// this also closes the path-traversal/symlink classes of attack a real
/// `archive.extract()` would have to guard against — entry names are only
/// ever used as in-memory map keys here, and a "symlink" entry is just read
/// as whatever bytes it declares, never turned into a real symlink.
pub fn import_zip(conn: &mut Connection, src: &Path) -> Result<Skill> {
    let file = std::fs::File::open(src)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|_| Error::Import("that file isn't a valid .zip archive".to_string()))?;

    if archive.len() > MAX_ZIP_ENTRIES {
        return Err(Error::Import(
            "that .zip archive has too many files to be a skill".to_string(),
        ));
    }
    let mut total_bytes: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|_| Error::Import("that .zip archive looks corrupted".to_string()))?;
        total_bytes = total_bytes.saturating_add(entry.size());
        if total_bytes > MAX_ZIP_UNCOMPRESSED_BYTES {
            return Err(Error::Import(
                "that .zip archive is too large to be a skill".to_string(),
            ));
        }
    }

    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|_| Error::Import("that .zip archive looks corrupted".to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().replace('\\', "/");
        let mut buf = Vec::with_capacity(entry.size() as usize);
        std::io::Read::read_to_end(&mut entry, &mut buf)
            .map_err(|_| Error::Import("that .zip file couldn't be extracted".to_string()))?;
        entries.push((name, buf));
    }

    let (name, description, body, files) = find_skill_in_memory(&entries).ok_or_else(|| {
        Error::Import(
            "that folder doesn't look like a skill (it needs a SKILL.md file)".to_string(),
        )
    })?;
    insert_new_skill(conn, &name, &description, &body, &files)
}

/// The in-memory analog of `find_entry_file` + "wrapped in one extra
/// top-level directory" + `collect_supporting_files`, for [`import_zip`]:
/// find a `SKILL.md` (case-insensitive on its last path segment) either at
/// the entries' root or under exactly one shared top-level directory prefix,
/// parse it, and collect every other `.md` entry under that same root as a
/// [`SkillFile`] (path relative to that root). Invalid-UTF-8 entries are
/// skipped, same tolerance `walk_markdown` gives a real directory.
fn find_skill_in_memory(
    entries: &[(String, Vec<u8>)],
) -> Option<(String, String, String, Vec<SkillFile>)> {
    let is_skill_md = |name: &str| {
        name.rsplit('/')
            .next()
            .is_some_and(|last| last.eq_ignore_ascii_case("SKILL.md"))
    };

    // Root-level SKILL.md first; otherwise, if every entry shares exactly one
    // top-level directory prefix, look for SKILL.md directly inside it.
    let root_entry = entries
        .iter()
        .find(|(name, _)| !name.contains('/') && is_skill_md(name));
    let (prefix, entry_name, bytes) = if let Some((name, bytes)) = root_entry {
        (String::new(), name.clone(), bytes.clone())
    } else {
        let top_dirs: std::collections::BTreeSet<&str> = entries
            .iter()
            .filter_map(|(name, _)| name.split_once('/').map(|(dir, _)| dir))
            .collect();
        let [only_dir] = top_dirs.into_iter().collect::<Vec<_>>()[..] else {
            return None;
        };
        let want = format!("{only_dir}/");
        let (name, bytes) = entries.iter().find(|(name, _)| {
            name.strip_prefix(&want)
                .is_some_and(|rest| !rest.contains('/') && is_skill_md(rest))
        })?;
        (want, name.clone(), bytes.clone())
    };

    let raw = String::from_utf8(bytes).ok()?;
    let fallback_slug = fallback_slug_from(
        entry_name
            .strip_prefix(&prefix)
            .unwrap_or(&entry_name)
            .strip_suffix(".md")
            .or(Some("skill")),
    );
    let (name, description, body) = parse(&raw, &fallback_slug);

    let mut files = Vec::new();
    for (path, bytes) in entries {
        if *path == entry_name || !path.starts_with(&prefix) {
            continue;
        }
        let rel = &path[prefix.len()..];
        if !rel.ends_with(".md") || !is_safe_relative_path(rel) {
            continue;
        }
        let Ok(content) = String::from_utf8(bytes.clone()) else {
            continue; // non-UTF-8 — skip, don't break the import
        };
        files.push(SkillFile {
            path: rel.to_string(),
            content,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    Some((name, description, body, files))
}

/// `false` for anything that could escape the skill's own folder once
/// re-emitted by [`export_zip_bytes`] as `<slug>/<path>` — an empty
/// component, a `.`/`..` component, or a leading `/` (absolute). Unlike
/// [`walk_markdown`]'s real-directory walk, which derives a relative path
/// via `Path::strip_prefix` on paths it actually visited and so can
/// structurally never produce `..`, [`find_skill_in_memory`] builds `rel`
/// from an attacker-controlled zip entry name — a `research/../../../evil.md`
/// entry would otherwise round-trip verbatim into `skill_files.path` and
/// later into an exported zip's own entry name (zip-slip on export).
fn is_safe_relative_path(rel: &str) -> bool {
    !rel.starts_with('/')
        && rel
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Shared tail for [`save`]'s empty-slug case, [`import_file`],
/// [`import_folder`], and [`import_zip`]: generate a unique slug against
/// what's currently in the DB and write the new skill + its files in one
/// transaction. This is the one place skill-slug uniqueness is decided — and
/// because it runs under `conn`'s single connection (itself behind
/// `AppState.app_db`'s one `Mutex` in the running app), no separate
/// "hold a lock across read-taken-slugs-then-write" primitive is needed the
/// way the old filesystem version's `skills_lock` was for.
fn insert_new_skill(
    conn: &mut Connection,
    name: &str,
    description: &str,
    body: &str,
    files: &[SkillFile],
) -> Result<Skill> {
    let tx = conn.transaction()?;
    let taken: Vec<String> = {
        let mut stmt = tx.prepare("SELECT slug FROM skills")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.filter_map(std::result::Result::ok).collect()
    };
    let taken_refs: Vec<&str> = taken.iter().map(String::as_str).collect();
    let slug = unique_slug(&slugify(name), &taken_refs);
    tx.execute(
        "INSERT INTO skills (slug, name, description, body) VALUES (?1, ?2, ?3, ?4)",
        params![slug, name, description, body],
    )?;
    for f in files {
        tx.execute(
            "INSERT INTO skill_files (skill_slug, path, content) VALUES (?1, ?2, ?3)",
            params![slug, f.path, f.content],
        )?;
    }
    tx.commit()?;
    Ok(Skill {
        slug,
        name: name.to_string(),
        description: description.to_string(),
        body: body.to_string(),
        files: files.to_vec(),
    })
}

/// Bring back one skill from a full data export (`crate::import`), keeping
/// its slug: `raw` is its `SKILL.md` as [`export_markdown`] wrote it, and
/// `files` its supporting files. Unlike [`import`], which always mints a
/// fresh slug, a skill that already exists under `slug` is replaced — its
/// old supporting files included — so moving to a new device leaves exactly
/// one copy of each skill. A supporting file whose path could escape the
/// skill's folder is dropped, same rule [`find_skill_in_memory`] applies.
pub fn replace_from_export(
    conn: &mut Connection,
    slug: &str,
    raw: &str,
    files: &[SkillFile],
) -> Result<Skill> {
    if !is_valid_slug(slug) {
        return Err(Error::Import(format!("{slug} isn't a valid skill name")));
    }
    let (name, description, body) = parse(raw, slug);
    let files: Vec<SkillFile> = files
        .iter()
        .filter(|f| f.path.ends_with(".md") && is_safe_relative_path(&f.path))
        .cloned()
        .collect();
    // A savepoint, not a transaction: a backup restore (`crate::backup`)
    // runs every import inside one outer transaction, and SQLite refuses a
    // nested `BEGIN`.
    let tx = conn.savepoint()?;
    tx.execute(
        "INSERT INTO skills (slug, name, description, body) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (slug) DO UPDATE SET name = ?2, description = ?3, body = ?4",
        params![slug, name, description, body],
    )?;
    tx.execute(
        "DELETE FROM skill_files WHERE skill_slug = ?1",
        params![slug],
    )?;
    for f in &files {
        tx.execute(
            "INSERT INTO skill_files (skill_slug, path, content) VALUES (?1, ?2, ?3)",
            params![slug, f.path, f.content],
        )?;
    }
    tx.commit()?;
    Ok(Skill {
        slug: slug.to_string(),
        name,
        description,
        body,
        files,
    })
}

/// Seed a brand-new (genuinely first-run) skills store with one starter
/// skill that teaches the feature itself by using it: `/create-skill` walks
/// the user through designing their own, in whichever of the app's two UI
/// languages `language` names ("fr", else English — same fallback rule as
/// every other `Settings::language`-driven choice in this app). Best-effort
/// and silent on failure — callers should never let this block startup.
///
/// Writes a **fixed** slug (`"create-skill"`), not one derived from `name` —
/// the `/`-trigger itself isn't translated, so [`insert_new_skill`]'s
/// generate-from-name path isn't used here: `slugify` on the French name
/// (`"Créer une compétence"`) would strip the accented characters down to
/// something unrecognizable, and even the English name would slugify to
/// `"create-a-skill"`, not `"create-skill"`.
pub fn seed_starter_skill(conn: &mut Connection, language: &str) {
    let (name, description, body) = if language == "fr" {
        (
            "Créer une compétence",
            "Concevez une nouvelle compétence ensemble, étape par étape.",
            CREATE_SKILL_BODY_FR,
        )
    } else {
        (
            "Create a skill",
            "Design a new skill together, step by step.",
            CREATE_SKILL_BODY_EN,
        )
    };
    let result = conn.execute(
        "INSERT OR REPLACE INTO skills (slug, name, description, body) VALUES ('create-skill', ?1, ?2, ?3)",
        params![name, description, body.trim()],
    );
    if let Err(e) = result {
        tracing::warn!("couldn't seed the starter skill: {e}");
    }
}

/// One-time migration for an install that had real skill files on disk
/// before this module moved onto `app_db` — called from
/// `crate::state::finish_unlock` only when the database was just created
/// *and* a legacy `<data_dir>/skills/` directory already existed. Reuses
/// [`list_legacy_directory`] to read whatever's there, then writes each
/// result through the normal DB insert path. Best-effort per skill, same
/// posture as [`seed_starter_skill`]: one unreadable/malformed legacy file
/// must never block startup or the rest of the migration. The legacy
/// directory is deliberately left in place afterwards (a cheap recovery net
/// if this has a bug) — it becomes permanently inert since nothing reads it
/// again once migration has run.
pub fn migrate_legacy_skills(conn: &mut Connection, legacy_dir: &Path) {
    for skill in list_legacy_directory(legacy_dir) {
        if let Err(e) = insert_new_skill(
            conn,
            &skill.name,
            &skill.description,
            &skill.body,
            &skill.files,
        ) {
            tracing::warn!(slug = %skill.slug, "couldn't migrate legacy skill: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy on-disk reads — migration only, never the steady-state path
// ---------------------------------------------------------------------------

/// Every skill found in `dir` on a real filesystem — this is the module's
/// entire pre-encryption implementation, kept verbatim as the read side of
/// [`migrate_legacy_skills`]. Never called on the steady-state read path
/// again once a database exists.
fn list_legacy_directory(dir: &Path) -> Vec<Skill> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(), // no directory yet = no skills
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if let Some(skill) = read_folder(&path) {
                skills.push(skill);
            }
            continue;
        }
        if !file_type.is_file() {
            continue; // a symlink (or other special file) — never followed
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(file_slug) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let slug = file_slug.to_ascii_lowercase();
        if !is_valid_slug(&slug) {
            tracing::warn!("skipping skill with an unusable file name {path:?}");
            continue;
        }
        match read_file(&path, &slug) {
            Ok(skill) => skills.push(skill),
            Err(e) => tracing::warn!("skipping unreadable skill {path:?}: {e}"),
        }
    }
    skills.sort_by_key(|s| s.name.to_ascii_lowercase());
    skills
}

fn read_file(path: &Path, slug: &str) -> Result<Skill> {
    let raw = std::fs::read_to_string(path)?;
    let (name, description, body) = parse(&raw, slug);
    Ok(Skill {
        slug: slug.to_string(),
        name,
        description,
        body,
        files: Vec::new(),
    })
}

/// Read a folder-based skill at `dir`, or `None` if it isn't one (no
/// `SKILL.md`, an unusable folder name, or an unreadable entry file — logged
/// and skipped, same posture as a corrupt flat file).
fn read_folder(dir: &Path) -> Option<Skill> {
    let file_name = dir.file_name()?.to_str()?;
    let slug = file_name.to_ascii_lowercase();
    if !is_valid_slug(&slug) {
        tracing::warn!("skipping skill folder with an unusable name {dir:?}");
        return None;
    }
    let entry = find_entry_file(dir)?;
    let raw = match std::fs::read_to_string(&entry) {
        Ok(raw) => raw,
        Err(e) => {
            tracing::warn!("skipping unreadable skill folder {dir:?}: {e}");
            return None;
        }
    };
    let (name, description, body) = parse(&raw, &slug);
    let files = collect_supporting_files(dir, &entry);
    Some(Skill {
        slug,
        name,
        description,
        body,
        files,
    })
}

/// A `SKILL.md` directly inside `dir`, matched case-insensitively. Used by
/// [`import_folder`] (a real directory the user picked) and
/// [`list_legacy_directory`]'s [`read_folder`]. Uses `DirEntry::file_type`,
/// not `Path::is_file` — the latter follows symlinks, which would let a
/// `SKILL.md` that's actually a symlink read through to an arbitrary target
/// file elsewhere on disk (a real risk for `import_folder`, whose source
/// folder can come from anywhere the user points a picker at).
fn find_entry_file(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_real_file = entry.file_type().is_ok_and(|t| t.is_file());
        let is_entry = is_real_file
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("skill.md"));
        if is_entry {
            return Some(path);
        }
    }
    None
}

/// Every other `.md` file under `dir` (recursively), excluding `entry`
/// itself, as [`SkillFile`]s with paths relative to `dir` — sorted for a
/// deterministic order. Best-effort: an unreadable subtree or non-UTF-8 file
/// is skipped, never fatal. Symlinks (to a file or a directory) are never
/// followed — same reasoning as [`find_entry_file`].
fn collect_supporting_files(dir: &Path, entry: &Path) -> Vec<SkillFile> {
    let mut files = Vec::new();
    walk_markdown(dir, dir, entry, &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

fn walk_markdown(root: &Path, current: &Path, entry: &Path, out: &mut Vec<SkillFile>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for item in entries.flatten() {
        let path = item.path();
        if path == entry {
            continue;
        }
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk_markdown(root, &path, entry, out);
            continue;
        }
        if !file_type.is_file() {
            continue; // a symlink (or other special file) — never followed
        }
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue; // non-UTF-8 or unreadable — skip, don't break listing
        };
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let Some(rel_str) = rel.to_str() else {
            continue;
        };
        out.push(SkillFile {
            path: rel_str.replace('\\', "/"),
            content,
        });
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// `skill` rendered as the single-file `.md` form — for the export command's
/// flat-skill case. Thin `pub` wrapper so [`render`] itself can stay private
/// with its existing three-argument signature.
pub fn export_markdown(skill: &Skill) -> String {
    render(&skill.name, &skill.description, &skill.body)
}

/// `skill` as the bytes of a `.zip` archive shaped exactly like what
/// [`import_zip`] already knows how to read back in — every entry wrapped
/// under one top-level `<slug>/` directory — so **export → edit → import
/// round-trips without the user having to think about zip layout**. Used by
/// the export command for a skill that has supporting files.
pub fn export_zip_bytes(skill: &Skill) -> Result<Vec<u8>> {
    let zip_err = |e: zip::result::ZipError| {
        Error::Export(format!("couldn't build the skill export archive: {e}"))
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(format!("{}/SKILL.md", skill.slug), options)
            .map_err(zip_err)?;
        std::io::Write::write_all(&mut zip, export_markdown(skill).as_bytes())?;
        for f in &skill.files {
            zip.start_file(format!("{}/{}", skill.slug, f.path), options)
                .map_err(zip_err)?;
            std::io::Write::write_all(&mut zip, f.content.as_bytes())?;
        }
        zip.finish().map_err(zip_err)?;
    }
    Ok(buf.into_inner())
}

// ---------------------------------------------------------------------------
// Pure helpers — unchanged by the storage backend
// ---------------------------------------------------------------------------

pub const CREATE_SKILL_BODY_EN: &str = r#"
You are helping the user design a brand-new OpenPal skill: a reusable set of
instructions they save once and trigger later by typing `/<name>` as the
first word of a chat message.

Interview them first, one question at a time, in plain language:
1. What should happen when they use this skill? What's the task, the tone,
   the output format?
2. Do they usually want to add their own words after the trigger (like
   "/summarize this article"), or does the skill stand on its own?
3. Is one page of instructions enough, or does this need reference material,
   worked examples, or a checklist too long to keep in a single file?

Once you understand the goal, write the skill for them, clearly labeled so
it's easy to copy:

**Name:** a short, human title (2-4 words), e.g. "Summarize". (OpenPal turns
this into the actual typed trigger by lowercasing it and joining the words
with dashes: "Weekly Report" becomes /weekly-report, so a multi-word name
is fine.)
**Description:** one sentence for the `/`-autocomplete list.
**Instructions:** the actual instructions the model should follow every time
this skill runs, written directly to the model ("You are an expert
at…"), not to the user. Be specific and concrete: a vague instruction like
"be helpful" won't change how the model behaves, but one that says exactly
what to check, produce, and in what order will.

Tell them to paste Name/Description/Instructions into Skills → New skill.

If their answer to question 3 means this needs more than one file, explain
the folder version instead: a `SKILL.md` file with the same three parts (as
frontmatter + body) plus one or more supporting `.md` files alongside it,
for reference material, examples, or a checklist. The `SKILL.md`
instructions should say what the other files contain and when to use them.
Once they've saved these files together in one folder, they can add the
whole thing from Skills → Import → A folder (or zip it up first and use → A
.zip archive).

Keep iterating with the user: refine the instructions together until
they're happy with a full draft, rather than accepting the first pass.
"#;

pub const CREATE_SKILL_BODY_FR: &str = r#"
Vous aidez l'utilisateur à concevoir une nouvelle compétence OpenPal : un
ensemble d'instructions réutilisables, enregistrées une fois et déclenchées
ensuite en tapant « /<nom> » comme premier mot d'un message dans la
discussion.

Interrogez-le d'abord, une question à la fois, en langage simple :
1. Que doit-il se passer quand il utilise cette compétence ? Quelle est la
   tâche, le ton, le format de sortie attendu ?
2. Ajoute-t-il généralement son propre texte après le déclencheur (comme
   « /résumer cet article »), ou la compétence fonctionne-t-elle seule ?
3. Une page d'instructions suffit-elle, ou a-t-il besoin de documentation de
   référence, d'exemples ou d'une checklist trop longue pour un seul fichier ?

Une fois l'objectif compris, rédigez la compétence pour lui, clairement
présentée pour qu'il puisse la copier facilement :

**Nom :** un titre court et lisible (2 à 4 mots), par ex. « Résumer ».
(OpenPal transforme ce nom en déclencheur réel en le mettant en minuscules et
en reliant les mots par des tirets : « Rapport hebdomadaire » devient
/rapport-hebdomadaire, donc un nom à plusieurs mots ne pose aucun problème.)
**Description :** une phrase pour la liste d'autocomplétion du « / ».
**Instructions :** les instructions que le modèle doit suivre à chaque
exécution de cette compétence, rédigées directement à l'intention du modèle
(« Vous êtes un expert en… »), pas de l'utilisateur. Soyez précis et concret :
une instruction vague comme « sois utile » ne change rien au comportement du
modèle, alors qu'une instruction qui dit exactement quoi vérifier, quoi
produire, et dans quel ordre, le change vraiment.

Dites-lui de coller Nom/Description/Instructions dans Compétences → Nouvelle
compétence.

Si sa réponse à la question 3 indique qu'il faut plus d'un fichier, expliquez
plutôt la version dossier : un fichier `SKILL.md` avec les trois mêmes
parties (en en-tête + corps), plus un ou plusieurs fichiers `.md`
complémentaires à côté, pour de la documentation de référence, des exemples
ou une checklist. Les instructions du `SKILL.md` doivent préciser ce que
contiennent les autres fichiers et quand les utiliser. Une fois ces fichiers
enregistrés ensemble dans un même dossier, il peut les ajouter depuis
Compétences → Importer → Un dossier (ou les compresser d'abord et utiliser →
Une archive .zip).

Continuez à itérer avec l'utilisateur : affinez les instructions ensemble
jusqu'à ce qu'il soit satisfait d'une version complète, plutôt que d'accepter
le premier jet.
"#;

/// A lowercase, valid slug to fall back on when a source file/folder has no
/// usable name of its own (or an imported skill has no frontmatter `name`) —
/// [`parse`]'s own fallback humanizes this into a display name.
fn fallback_slug_from(name: Option<&str>) -> String {
    name.map(|s| s.to_ascii_lowercase())
        .filter(|s| is_valid_slug(s))
        .unwrap_or_else(|| "skill".to_string())
}

/// Split `raw` into `(name, description, body)`. No `---`-delimited header
/// (or an unterminated one) falls back to the whole file as the body, named
/// after `slug` — a hand-created `.md` file with no frontmatter still works.
fn parse(raw: &str, slug: &str) -> (String, String, String) {
    let fallback_name = humanize(slug);
    let Some(after_open) = raw.strip_prefix("---\n") else {
        return (fallback_name, String::new(), raw.trim().to_string());
    };
    let Some(header_end) = after_open.find("\n---\n") else {
        return (fallback_name, String::new(), raw.trim().to_string());
    };
    let header = &after_open[..header_end];
    let body = after_open[header_end + 5..].trim().to_string();

    let mut name = fallback_name;
    let mut description = String::new();
    for line in header.lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        }
    }
    (name, description, body)
}

/// Frontmatter values are single lines; strip newlines defensively so a
/// pasted multi-line name/description can never break the header shape.
fn escape_header(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// "custom-model-picker" -> "custom model picker" — a readable fallback name
/// when a file has no frontmatter to read one from.
fn humanize(slug: &str) -> String {
    slug.replace('-', " ")
}

/// Lowercase ascii letters/digits/dashes, e.g. "Summarize This!" -> "summarize-this".
fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "skill".to_string()
    } else {
        out
    }
}

/// `base`, appending "-2", "-3", … until free.
fn unique_slug(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken.contains(&s.as_str()))
        .expect("some suffix is always free")
}

fn render(name: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        escape_header(name),
        escape_header(description),
        body.trim(),
    )
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

    fn skill(slug: &str, name: &str, body: &str) -> Skill {
        Skill {
            slug: slug.to_string(),
            name: name.to_string(),
            description: "A test skill.".to_string(),
            body: body.to_string(),
            files: Vec::new(),
        }
    }

    #[test]
    fn save_get_list_delete_roundtrip() {
        let mut conn = test_conn();
        assert!(list(&conn).is_empty());

        let saved = save(&mut conn, skill("", "Summarize", "Summarize the text.")).unwrap();
        assert_eq!(saved.slug, "summarize");
        save(
            &mut conn,
            skill("brainstorm", "Brainstorm", "Brainstorm ideas."),
        )
        .unwrap();

        let all = list(&conn);
        let slugs: Vec<&str> = all.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, ["brainstorm", "summarize"], "alphabetical by name");

        assert_eq!(get(&conn, "summarize").unwrap().name, "Summarize");

        delete(&mut conn, "summarize").unwrap();
        delete(&mut conn, "summarize").unwrap(); // idempotent
        assert_eq!(list(&conn).len(), 1);
        assert!(matches!(get(&conn, "summarize"), Err(Error::NotFound(_))));
    }

    #[test]
    fn duplicate_names_get_a_unique_slug() {
        let mut conn = test_conn();
        let first = save(&mut conn, skill("", "Summarize", "v1")).unwrap();
        let second = save(&mut conn, skill("", "Summarize", "v2")).unwrap();
        assert_eq!(first.slug, "summarize");
        assert_eq!(second.slug, "summarize-2");
    }

    #[test]
    fn editing_keeps_the_same_slug_and_never_touches_files() {
        let mut conn = test_conn();
        let saved = insert_new_skill(
            &mut conn,
            "Research",
            "d",
            "v1",
            &[SkillFile {
                path: "reference.md".to_string(),
                content: "Reference content.".to_string(),
            }],
        )
        .unwrap();

        let edited = save(
            &mut conn,
            Skill {
                name: "Research, edited".to_string(),
                body: "v2".to_string(),
                ..saved.clone()
            },
        )
        .unwrap();
        assert_eq!(edited.slug, saved.slug);
        assert_eq!(edited.files.len(), 1, "reference.md must survive the edit");
        assert_eq!(list(&conn).len(), 1);
        assert_eq!(list(&conn)[0].name, "Research, edited");
        assert_eq!(list(&conn)[0].body, "v2");
    }

    #[test]
    fn invalid_slug_operations_are_rejected() {
        let mut conn = test_conn();
        for bad in ["../evil", "a/b", "a.b", "id with spaces", "café"] {
            assert!(
                matches!(delete(&mut conn, bad), Err(Error::NotFound(_))),
                "delete {bad:?}"
            );
            assert!(
                matches!(get(&conn, bad), Err(Error::NotFound(_))),
                "get {bad:?}"
            );
            assert!(
                matches!(
                    save(&mut conn, skill(bad, "x", "y")),
                    Err(Error::NotFound(_))
                ),
                "save {bad:?}"
            );
        }
    }

    #[test]
    fn slugs_are_clean() {
        assert_eq!(slugify("Summarize This!"), "summarize-this");
        assert_eq!(slugify("___"), "skill");
        assert_eq!(unique_slug("m", &["m", "m-2"]), "m-3");
    }

    #[test]
    fn a_corrupt_skills_row_is_skipped_not_fatal() {
        // A BLOB where TEXT is expected — the realistic corruption vector
        // now that nothing but this module's own validated code ever writes
        // a row (no more hand-edited files, no more non-UTF-8-on-disk case).
        // Mirrors the adversarial-review coverage store.rs/projects.rs got.
        let conn = test_conn();
        conn.execute(
            "INSERT INTO skills (slug, name, description, body) VALUES ('good', 'Good', 'd', 'body')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO skills (slug, name, description, body) VALUES ('bad', ?1, 'd', 'body')",
            params![vec![0xFFu8, 0xFE]],
        )
        .unwrap();

        let all = list(&conn);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].slug, "good");
    }

    #[test]
    fn a_corrupt_skill_files_row_only_drops_that_file() {
        let conn = test_conn();
        conn.execute(
            "INSERT INTO skills (slug, name, description, body) VALUES ('research', 'Research', 'd', 'body')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO skill_files (skill_slug, path, content) VALUES ('research', 'good.md', 'ok')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO skill_files (skill_slug, path, content) VALUES ('research', 'bad.md', ?1)",
            params![vec![0xFFu8, 0xFE]],
        )
        .unwrap();

        let all = list(&conn);
        assert_eq!(all.len(), 1, "the skill itself must still list");
        assert_eq!(all[0].files.len(), 1, "only the corrupt file is dropped");
        assert_eq!(all[0].files[0].path, "good.md");
    }

    // ------------------------- Import (real filesystem source) -------------------------

    #[test]
    fn import_file_creates_a_flat_skill() {
        let mut conn = test_conn();
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("shared.md");
        std::fs::write(
            &src,
            "---\nname: Shared skill\ndescription: d\n---\n\nImported body.",
        )
        .unwrap();

        let imported = import(&mut conn, &src).unwrap();
        assert_eq!(imported.slug, "shared-skill");
        assert_eq!(imported.body, "Imported body.");
        assert!(imported.files.is_empty());
        assert_eq!(list(&conn).len(), 1);
    }

    #[test]
    fn import_folder_creates_a_folder_skill_with_a_fresh_slug() {
        let mut conn = test_conn();
        save(
            &mut conn,
            skill("deep-research", "Deep research", "existing"),
        )
        .unwrap();

        let src_root = tempfile::tempdir().unwrap();
        let src = src_root.path().join("original-name");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: Deep research\ndescription: d\n---\n\nMain.",
        )
        .unwrap();
        std::fs::write(src.join("reference.md"), "Ref.").unwrap();

        let imported = import(&mut conn, &src).unwrap();
        assert_eq!(imported.slug, "deep-research-2");
        assert_eq!(imported.body, "Main.");
        assert_eq!(imported.files.len(), 1);
        assert_eq!(imported.files[0].path, "reference.md");
        assert_eq!(imported.files[0].content, "Ref.");
        assert!(
            src.join("SKILL.md").exists(),
            "the source folder is untouched (copied, not moved)"
        );
    }

    #[test]
    fn import_folder_without_skill_md_is_rejected() {
        let mut conn = test_conn();
        let src_root = tempfile::tempdir().unwrap();
        let src = src_root.path().join("not-a-skill");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("readme.md"), "irrelevant").unwrap();

        assert!(matches!(import(&mut conn, &src), Err(Error::Import(_))));
        assert!(list(&conn).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_files_are_not_followed_on_folder_import() {
        use std::os::unix::fs::symlink;

        let mut conn = test_conn();
        let secret_dir = tempfile::tempdir().unwrap();
        let secret = secret_dir.path().join("secret.txt");
        std::fs::write(&secret, "top secret contents").unwrap();

        let src_root = tempfile::tempdir().unwrap();
        let src = src_root.path().join("shared");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: Shared\ndescription: d\n---\n\nMain.",
        )
        .unwrap();
        symlink(&secret, src.join("leak.md")).unwrap();

        let imported = import(&mut conn, &src).unwrap();
        assert!(
            imported.files.is_empty(),
            "a symlinked file must never be read or copied in"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_symlinked_skill_md_is_not_treated_as_a_valid_entry_file() {
        use std::os::unix::fs::symlink;

        let mut conn = test_conn();
        let secret_dir = tempfile::tempdir().unwrap();
        let secret = secret_dir.path().join("secret.txt");
        std::fs::write(&secret, "top secret contents").unwrap();

        let src_root = tempfile::tempdir().unwrap();
        let src = src_root.path().join("shared");
        std::fs::create_dir_all(&src).unwrap();
        symlink(&secret, src.join("SKILL.md")).unwrap();

        assert!(matches!(import(&mut conn, &src), Err(Error::Import(_))));
    }

    // ------------------------- Import zip (in-memory) -------------------------

    fn write_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            for (name, content) in entries {
                writer.start_file(*name, options).unwrap();
                std::io::Write::write_all(&mut writer, content).unwrap();
            }
            writer.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn import_zip_flat_at_root() {
        let mut conn = test_conn();
        let zip_bytes = write_test_zip(&[(
            "SKILL.md",
            b"---\nname: Zipped\ndescription: d\n---\n\nBody.",
        )]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        let imported = import_zip(&mut conn, &path).unwrap();
        assert_eq!(imported.slug, "zipped");
        assert_eq!(imported.body, "Body.");
        assert!(imported.files.is_empty());
    }

    #[test]
    fn import_zip_wrapped_in_one_top_level_directory() {
        let mut conn = test_conn();
        let zip_bytes = write_test_zip(&[
            (
                "research/SKILL.md",
                b"---\nname: Research\ndescription: d\n---\n\nMain.",
            ),
            ("research/reference.md", b"Ref."),
            ("research/notes.txt", b"ignored"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        let imported = import_zip(&mut conn, &path).unwrap();
        assert_eq!(imported.slug, "research");
        assert_eq!(imported.body, "Main.");
        assert_eq!(imported.files.len(), 1);
        assert_eq!(imported.files[0].path, "reference.md");
        assert_eq!(imported.files[0].content, "Ref.");
    }

    #[test]
    fn import_zip_rejects_path_traversal_in_a_supporting_file_entry() {
        // A hostile entry name that would otherwise round-trip verbatim into
        // skill_files.path and later into an exported zip's own entry name
        // (zip-slip on export) — see is_safe_relative_path's doc comment.
        let mut conn = test_conn();
        let zip_bytes = write_test_zip(&[
            (
                "research/SKILL.md",
                b"---\nname: Research\ndescription: d\n---\n\nMain.",
            ),
            ("research/../../../../tmp/evil.md", b"malicious content"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        let imported = import_zip(&mut conn, &path).unwrap();
        assert!(
            imported.files.is_empty(),
            "a path-traversal entry must be dropped, not imported as a supporting file"
        );
    }

    #[test]
    fn is_safe_relative_path_rejects_traversal_and_absolute_paths() {
        assert!(is_safe_relative_path("reference.md"));
        assert!(is_safe_relative_path("examples/checklist.md"));
        assert!(!is_safe_relative_path("../evil.md"));
        assert!(!is_safe_relative_path("a/../../evil.md"));
        assert!(!is_safe_relative_path("/etc/passwd.md"));
        assert!(!is_safe_relative_path("a//b.md"));
        assert!(!is_safe_relative_path("./evil.md"));
    }

    #[test]
    fn import_zip_without_skill_md_is_rejected() {
        let mut conn = test_conn();
        let zip_bytes = write_test_zip(&[("readme.md", b"irrelevant")]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        assert!(matches!(
            import_zip(&mut conn, &path),
            Err(Error::Import(_))
        ));
    }

    #[test]
    fn import_zip_never_touches_the_os_temp_directory() {
        // The bug this module used to have: unique_temp_dir() unpacked to
        // std::env::temp_dir().join("openpal-skill-import-<pid>-<nanos>")
        // before reading it back in. Check specifically for that naming
        // pattern rather than diffing the whole shared OS temp directory —
        // the latter is racy under cargo test's default parallel execution,
        // since unrelated concurrent tests create/remove their own
        // `tempfile::tempdir()` entries there at the same time.
        let mut conn = test_conn();
        let zip_bytes = write_test_zip(&[(
            "SKILL.md",
            b"---\nname: Temp check\ndescription: d\n---\n\nBody.",
        )]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        import_zip(&mut conn, &path).unwrap();

        let leaked = std::fs::read_dir(std::env::temp_dir())
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("openpal-skill-import-")
            });
        assert!(
            !leaked,
            "import_zip must never create the old openpal-skill-import-* extraction directory"
        );
    }

    #[test]
    fn oversized_zip_entry_is_rejected() {
        let mut conn = test_conn();
        let huge = vec![0u8; (MAX_ZIP_UNCOMPRESSED_BYTES + 1) as usize];
        let zip_bytes = write_test_zip(&[
            ("SKILL.md", b"---\nname: Big\n---\n\nBody."),
            ("big.md", &huge),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        assert!(matches!(
            import_zip(&mut conn, &path),
            Err(Error::Import(_))
        ));
    }

    #[test]
    fn zip_with_too_many_entries_is_rejected() {
        let mut conn = test_conn();
        let entries: Vec<(String, &[u8])> = (0..MAX_ZIP_ENTRIES + 1)
            .map(|i| (format!("f{i}.md"), b"x" as &[u8]))
            .collect();
        let entries_ref: Vec<(&str, &[u8])> =
            entries.iter().map(|(n, c)| (n.as_str(), *c)).collect();
        let zip_bytes = write_test_zip(&entries_ref);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skill.zip");
        std::fs::write(&path, zip_bytes).unwrap();

        assert!(matches!(
            import_zip(&mut conn, &path),
            Err(Error::Import(_))
        ));
    }

    // ------------------------- Export -------------------------

    #[test]
    fn export_markdown_roundtrips_through_import_file() {
        let mut conn = test_conn();
        let saved = save(&mut conn, skill("", "Round Trip", "Body text.")).unwrap();
        let rendered = export_markdown(&saved);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exported.md");
        std::fs::write(&path, rendered).unwrap();

        let mut conn2 = test_conn();
        let reimported = import(&mut conn2, &path).unwrap();
        assert_eq!(reimported.name, "Round Trip");
        assert_eq!(reimported.body, "Body text.");
    }

    #[test]
    fn export_zip_bytes_roundtrips_through_import_zip() {
        let mut conn = test_conn();
        let saved = insert_new_skill(
            &mut conn,
            "Research",
            "d",
            "Main.",
            &[SkillFile {
                path: "reference.md".to_string(),
                content: "Ref.".to_string(),
            }],
        )
        .unwrap();
        let bytes = export_zip_bytes(&saved).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exported.zip");
        std::fs::write(&path, bytes).unwrap();

        let mut conn2 = test_conn();
        let reimported = import_zip(&mut conn2, &path).unwrap();
        assert_eq!(reimported.name, "Research");
        assert_eq!(reimported.body, "Main.");
        assert_eq!(reimported.files.len(), 1);
        assert_eq!(reimported.files[0].path, "reference.md");
        assert_eq!(reimported.files[0].content, "Ref.");
    }

    // ------------------------- Legacy migration -------------------------

    #[test]
    fn list_legacy_directory_reads_flat_and_folder_skills() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("summarize.md"),
            "---\nname: Summarize\ndescription: d\n---\n\nSummarize.",
        )
        .unwrap();
        let folder = dir.path().join("research");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            "---\nname: Research\ndescription: d\n---\n\nMain.",
        )
        .unwrap();
        std::fs::write(folder.join("reference.md"), "Ref.").unwrap();

        let all = list_legacy_directory(dir.path());
        assert_eq!(all.len(), 2);
        let slugs: Vec<&str> = all.iter().map(|s| s.slug.as_str()).collect();
        assert_eq!(slugs, ["research", "summarize"]);
    }

    #[test]
    fn migrate_legacy_skills_writes_every_legacy_skill_into_the_db() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("old-skill.md"),
            "---\nname: Old skill\ndescription: d\n---\n\nBody.",
        )
        .unwrap();

        let mut conn = test_conn();
        migrate_legacy_skills(&mut conn, dir.path());

        let all = list(&conn);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Old skill");
        assert_eq!(all[0].body, "Body.");
    }

    #[test]
    fn migrate_legacy_skills_on_a_missing_directory_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let mut conn = test_conn();
        migrate_legacy_skills(&mut conn, &missing);
        assert!(list(&conn).is_empty());
    }

    // ------------------------- Seeding -------------------------

    #[test]
    fn seed_starter_skill_writes_the_create_skill_trigger() {
        let mut conn = test_conn();
        seed_starter_skill(&mut conn, "en");

        let all = list(&conn);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].slug, "create-skill");
        assert_eq!(all[0].name, "Create a skill");
    }

    #[test]
    fn seed_starter_skill_uses_french_content_for_the_french_locale() {
        let mut conn = test_conn();
        seed_starter_skill(&mut conn, "fr");

        let all = list(&conn);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].slug, "create-skill");
        assert_eq!(all[0].name, "Créer une compétence");
        assert!(all[0].body.starts_with("Vous aidez l'utilisateur"));
    }
}
