//! Automatic encrypted backups (Settings → Your data): everything the
//! plaintext export (`crate::export`) can carry — conversations, projects,
//! artifacts, settings, skills, custom models, plugin records — plus the
//! agent audit log, sealed into one file in a folder the user picks, and
//! read back on any device with the recovery code. Model files and logs
//! never live in `app_db`, so they are never in a backup.
//!
//! **Why the recovery code unlocks it.** The only secret that exists off
//! this device is the recovery code: it seals a copy of the data key (DEK)
//! in `<data>/keys/recovery.wrap` (`commands::confirm_recovery_code`), and
//! the app never keeps the code itself. So a backup carries that wrap file's
//! bytes, and is encrypted under `HKDF(DEK, "openpal-backup-v1")`
//! (`crate::crypto::kdf::derive_subkey`). Restoring types the code, unseals
//! the DEK from the carried wrap, derives the same key, and decrypts —
//! nothing from the source machine's keychain is needed. A backup written
//! before the user replaced their recovery code carries the old wrap, so it
//! still opens with the code it was made under.
//!
//! File layout (`openpal-backup-<UTC stamp>.opbackup`):
//!
//! ```text
//! MAGIC (8) | version (1) | wrap length (u32 BE) | recovery.wrap bytes
//!   | nonce (12) | AES-256-GCM ciphertext || tag
//! ```
//!
//! Everything before the nonce is the AEAD's associated data, so a
//! truncated, bit-flipped or spliced file fails to open as a whole before
//! anything is written to the database. The plaintext is the export's own
//! zip ([`crate::export::archive_bytes`]) with an extra
//! `agent-audit.jsonl` entry — one format to read back, through
//! [`crate::import::import_archive`].
//!
//! The folder never holds more than [`MAX_BACKUPS`] of them: writing one
//! more removes the oldest. A file counts as ours only when its name has our
//! prefix and extension *and* it starts with [`MAGIC`] — anything else in
//! the folder is never touched.
//!
//! Same lock discipline as the export: [`read_snapshot`] is the short read
//! under `app_db`'s lock, [`write_backup`] the slow part, off it.
//!
//! The user's choices and the last run's outcome ([`BackupState`]) live in
//! their own `backup_config` table, never in an export or a backup — they
//! describe this device's disk.

use crate::crypto::key::DataKey;
use crate::crypto::{kdf, recovery, wrap};
use crate::export::{ExportCategories, ExportData};
use crate::import::ImportSummary;
use crate::{Error, Result};
use rand::rngs::OsRng;
use rand::RngCore;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use rusqlite::{params, Connection, OptionalExtension};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

/// How many backups a folder keeps.
pub const MAX_BACKUPS: usize = 7;

/// How long after a failed attempt the scheduler tries again.
pub const RETRY_AFTER_MS: u64 = 60 * 60 * 1000;

const MAGIC: &[u8; 8] = b"OPNPALBK";
const FORMAT_VERSION: u8 = 1;
const HEADER_FIXED_LEN: usize = MAGIC.len() + 1 + 4;
/// `recovery.wrap` is well under 100 bytes; anything claiming more than this
/// is a damaged length field, not a wrap file.
const MAX_WRAP_LEN: usize = 4096;
const TAG_LEN: usize = 16;
const SUBKEY_INFO: &[u8] = b"openpal-backup-v1";
const FILE_PREFIX: &str = "openpal-backup-";
const FILE_EXTENSION: &str = ".opbackup";
const AUDIT_ENTRY: &str = "agent-audit.jsonl";
const DAY_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupFrequency {
    #[default]
    Daily,
    Weekly,
}

impl BackupFrequency {
    pub fn interval_ms(self) -> u64 {
        match self {
            Self::Daily => DAY_MS,
            Self::Weekly => 7 * DAY_MS,
        }
    }
}

/// The user's choices and how the last run went — one JSON row in
/// `backup_config`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupState {
    pub enabled: bool,
    pub folder: Option<String>,
    pub frequency: BackupFrequency,
    pub last_success_ms: Option<u64>,
    pub last_attempt_ms: Option<u64>,
    /// Why the most recent attempt failed, safe to show as-is; `None` once
    /// an attempt succeeds.
    pub last_error: Option<String>,
}

pub fn load_state(conn: &Connection) -> BackupState {
    let row: Option<String> = conn
        .query_row(
            "SELECT json FROM backup_config WHERE key = 'current'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or_else(|e| {
            tracing::warn!("backup: couldn't read its settings: {e}");
            None
        });
    match row {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("backup settings row is corrupt ({e}); starting from defaults");
            BackupState::default()
        }),
        None => BackupState::default(),
    }
}

/// Read, change and save the state in one go — run it under one hold of
/// `app_db`'s lock, so the scheduler recording a result and the user
/// changing a choice can't overwrite each other.
pub fn update_state(
    conn: &Connection,
    change: impl FnOnce(&mut BackupState),
) -> Result<BackupState> {
    let mut state = load_state(conn);
    change(&mut state);
    let json = serde_json::to_string(&state)?;
    conn.execute(
        "INSERT INTO backup_config (key, json) VALUES ('current', ?1)
         ON CONFLICT(key) DO UPDATE SET json = excluded.json",
        params![json],
    )?;
    Ok(state)
}

/// Whether a backup should be written now: switched on with a folder, and
/// the chosen interval has passed since the last good one (or there never
/// was one). After a failure, waits [`RETRY_AFTER_MS`] before trying again.
/// A clock that went backwards past the last backup counts as due, so a
/// bad clock can't stop backups until it catches up.
pub fn is_due(state: &BackupState, now_ms: u64) -> bool {
    if !state.enabled || state.folder.is_none() {
        return false;
    }
    if let (Some(_), Some(attempt)) = (&state.last_error, state.last_attempt_ms) {
        if now_ms >= attempt && now_ms - attempt < RETRY_AFTER_MS {
            return false;
        }
    }
    match state.last_success_ms {
        None => true,
        Some(last) if now_ms < last => true,
        Some(last) => now_ms - last >= state.frequency.interval_ms(),
    }
}

/// Everything a backup holds, read from the database in one short go.
/// Holds no `Connection` — safe to move onto a blocking thread.
pub struct Snapshot {
    export: ExportData,
    audit_lines: Vec<String>,
}

/// The fast part, under `app_db`'s lock: a handful of `SELECT`s.
pub fn read_snapshot(conn: &Connection) -> Result<Snapshot> {
    Ok(Snapshot {
        export: crate::export::read_export_data(conn, &ExportCategories::ALL),
        audit_lines: crate::agent::audit::read_lines(conn)?,
    })
}

/// The slow part, off the lock: build the archive, seal it under the DEK's
/// backup subkey with `recovery_wrap` (the bytes of `recovery.wrap`)
/// alongside, write it into `folder`, then trim the folder to
/// [`MAX_BACKUPS`]. Never creates `folder` — a missing one means an
/// unplugged drive, and creating it would quietly fill the internal disk
/// instead. Returns the new file's path.
pub fn write_backup(
    snapshot: &Snapshot,
    recovery_wrap: &[u8],
    dek: &DataKey,
    folder: &Path,
    now_ms: u64,
) -> Result<PathBuf> {
    if !folder.is_dir() {
        return Err(Error::Backup(
            "The backup folder isn't available. Check that its drive is connected.".to_string(),
        ));
    }
    let audit = snapshot.audit_lines.join("\n");
    let (summary, payload) =
        crate::export::archive_bytes(&snapshot.export, &[(AUDIT_ENTRY, audit.as_bytes())])?;
    if summary.skipped > 0 {
        tracing::warn!(
            "backup: {} item(s) couldn't be read and were left out",
            summary.skipped
        );
    }
    let sealed = seal(&payload, recovery_wrap, dek)?;

    let name = file_name(now_ms);
    let dest = folder.join(&name);
    let tmp = folder.join(format!(".{name}.tmp"));
    if let Err(e) = write_synced(&tmp, &sealed).and_then(|()| fs::rename(&tmp, &dest)) {
        let _ = fs::remove_file(&tmp);
        return Err(io_failure(e));
    }
    prune(folder, &dest);
    Ok(dest)
}

/// What went wrong opening a backup.
#[derive(Debug)]
pub enum OpenError {
    /// The recovery code doesn't unseal the key this backup carries.
    WrongCode,
    /// Not an OpenPal backup, or a damaged/truncated one. Safe to show.
    Unreadable(String),
    Io(std::io::Error),
}

/// Decrypt the backup at `path` with the recovery code the user typed,
/// returning the archive it holds. Nothing is written anywhere.
pub fn open_backup(path: &Path, code: &str) -> std::result::Result<Vec<u8>, OpenError> {
    let bytes = fs::read(path).map_err(OpenError::Io)?;
    open_backup_bytes(&bytes, code)
}

fn open_backup_bytes(bytes: &[u8], code: &str) -> std::result::Result<Vec<u8>, OpenError> {
    let damaged =
        || OpenError::Unreadable("This backup file is damaged or incomplete.".to_string());
    if bytes.len() < HEADER_FIXED_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return Err(OpenError::Unreadable(
            "That file isn't an OpenPal backup.".to_string(),
        ));
    }
    if bytes[MAGIC.len()] != FORMAT_VERSION {
        return Err(OpenError::Unreadable(
            "This backup was made by a newer version of OpenPal. Update OpenPal and try again."
                .to_string(),
        ));
    }
    let wrap_len = u32::from_be_bytes(
        bytes[MAGIC.len() + 1..HEADER_FIXED_LEN]
            .try_into()
            .map_err(|_| damaged())?,
    ) as usize;
    if wrap_len > MAX_WRAP_LEN || bytes.len() < HEADER_FIXED_LEN + wrap_len + NONCE_LEN + TAG_LEN {
        return Err(damaged());
    }
    let (aad, rest) = bytes.split_at(HEADER_FIXED_LEN + wrap_len);
    let wrap_bytes = &aad[HEADER_FIXED_LEN..];

    let header = wrap::header_from_bytes(wrap_bytes).map_err(|_| damaged())?;
    let normalized = recovery::normalize(code);
    let kek = kdf::derive(
        normalized.as_bytes(),
        &header.salt,
        header.m_cost_kib,
        header.t_cost,
    )
    .map_err(|e| {
        tracing::error!("backup: deriving the recovery key failed: {e}");
        damaged()
    })?;
    let dek = match wrap::open_wrap_bytes(wrap_bytes, &kek) {
        Ok(dek) => dek,
        Err(wrap::WrapError::WrongPassphrase) => return Err(OpenError::WrongCode),
        Err(_) => return Err(damaged()),
    };

    let (nonce, ciphertext) = rest.split_at(NONCE_LEN);
    let key = backup_key(&dek).map_err(|_| damaged())?;
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| damaged())?;
    let mut buf = ciphertext.to_vec();
    let len = key
        .open_in_place(nonce, Aad::from(aad), &mut buf)
        .map_err(|_| damaged())?
        .len();
    buf.truncate(len);
    Ok(buf)
}

/// Bring a decrypted backup's contents into `conn`, merged the same way
/// [`crate::import`] merges an export, all inside one transaction: if the
/// database refuses any write, nothing is kept. The app layer reloads what
/// it holds in memory afterwards, exactly as after an import, and calls
/// `AuditLog::resync`.
pub fn restore(conn: &mut Connection, archive: Vec<u8>) -> Result<ImportSummary> {
    let unreadable = || Error::Backup("This backup file is damaged or incomplete.".to_string());
    let mut archive = zip::ZipArchive::new(Cursor::new(archive)).map_err(|_| unreadable())?;
    let audit_lines = match archive.by_name(AUDIT_ENTRY) {
        Ok(mut entry) => {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|_| unreadable())?;
            text.lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        }
        Err(_) => Vec::new(),
    };

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let (summary, failed_writes) = crate::import::import_archive(conn, &mut archive);
    let result = if failed_writes > 0 {
        Err(Error::Backup(
            "OpenPal couldn't save the restored data, so nothing was changed. Check that you \
             have free disk space and try again."
                .to_string(),
        ))
    } else {
        crate::agent::audit::restore_lines(conn, &audit_lines)
            .map_err(Error::from)
            .and_then(|_| conn.execute_batch("COMMIT").map_err(Error::from))
    };
    if let Err(e) = result {
        if let Err(rollback) = conn.execute_batch("ROLLBACK") {
            tracing::error!("backup: rolling back a failed restore failed too: {rollback}");
        }
        return Err(e);
    }
    Ok(summary)
}

fn backup_key(dek: &DataKey) -> std::result::Result<LessSafeKey, ring::error::Unspecified> {
    let subkey = kdf::derive_subkey(dek, SUBKEY_INFO);
    Ok(LessSafeKey::new(UnboundKey::new(
        &AES_256_GCM,
        subkey.as_bytes(),
    )?))
}

fn seal(payload: &[u8], recovery_wrap: &[u8], dek: &DataKey) -> Result<Vec<u8>> {
    let failed = || Error::Backup("OpenPal couldn't encrypt the backup.".to_string());
    if recovery_wrap.len() > MAX_WRAP_LEN {
        return Err(failed());
    }
    let mut out = Vec::with_capacity(
        HEADER_FIXED_LEN + recovery_wrap.len() + NONCE_LEN + payload.len() + TAG_LEN,
    );
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.extend_from_slice(&(recovery_wrap.len() as u32).to_be_bytes());
    out.extend_from_slice(recovery_wrap);
    let aad_len = out.len();

    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let mut body = payload.to_vec();
    backup_key(dek)
        .and_then(|key| {
            key.seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(&out[..aad_len]),
                &mut body,
            )
        })
        .map_err(|_| failed())?;
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&body);
    Ok(out)
}

/// `openpal-backup-20261021-143005-123.opbackup` — UTC, so names sort in the
/// order they were written whatever the time zone.
fn file_name(now_ms: u64) -> String {
    let stamp = chrono::DateTime::from_timestamp_millis(now_ms as i64)
        .unwrap_or_default()
        .format("%Y%m%d-%H%M%S-%3f");
    format!("{FILE_PREFIX}{stamp}{FILE_EXTENSION}")
}

fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn io_failure(e: std::io::Error) -> Error {
    tracing::warn!("backup: writing the file failed: {e}");
    let reason = match e.kind() {
        std::io::ErrorKind::NotFound => {
            "The backup folder isn't available. Check that its drive is connected."
        }
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            "OpenPal isn't allowed to save files in the backup folder."
        }
        std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded => {
            "There isn't enough free space in the backup folder."
        }
        _ => "OpenPal couldn't save the backup file.",
    };
    Error::Backup(reason.to_string())
}

/// Whether `path` is a backup this module wrote: our name *and* our magic.
fn is_ours(path: &Path) -> bool {
    let named = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(FILE_PREFIX) && n.ends_with(FILE_EXTENSION));
    if !named || !path.is_file() {
        return false;
    }
    let mut magic = [0u8; MAGIC.len()];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == MAGIC
}

/// Delete our oldest backups until [`MAX_BACKUPS`] are left, never the one
/// just written. Best-effort: a file that won't delete is logged and left.
fn prune(folder: &Path, just_written: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    let mut ours: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p != just_written && is_ours(p))
        .collect();
    // The names carry a UTC timestamp, so name order is age order.
    ours.sort();
    let excess = (ours.len() + 1).saturating_sub(MAX_BACKUPS);
    for old in ours.into_iter().take(excess) {
        if let Err(e) = fs::remove_file(&old) {
            tracing::warn!("backup: couldn't remove an old backup: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::tests::populate_every_category;

    const MARKER: &str = "zq-marker-7f3a";

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    /// A DEK and a `recovery.wrap` sealing it under `code` — what
    /// `confirm_recovery_code` writes. The KDF runs at its floor to keep
    /// tests quick; the header records it, and restore reads it from there.
    fn recovery_wrap(dek: &DataKey, code: &str) -> Vec<u8> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recovery.wrap");
        let salt = [9u8; 16];
        let header = wrap::WrapHeader {
            m_cost_kib: kdf::M_COST_FLOOR_KIB,
            t_cost: kdf::T_COST_FLOOR,
            ..wrap::WrapHeader::new(salt)
        };
        let kek = kdf::derive(
            recovery::normalize(code).as_bytes(),
            &salt,
            header.m_cost_kib,
            header.t_cost,
        )
        .unwrap();
        wrap::write_wrap(&path, dek, &kek, &header).unwrap();
        fs::read(&path).unwrap()
    }

    const CODE: &str = "ABCDE-FGHJK-MNPQR-STVWX-YZ012-3";

    /// Every category, each carrying [`MARKER`], plus two audit events.
    fn populated() -> Connection {
        let mut conn = test_conn();
        populate_every_category(&mut conn);
        let mut convo = crate::store::get(&conn, "conv-1").unwrap();
        convo.title = format!("Chat {MARKER}");
        crate::store::save(&conn, &convo).unwrap();
        crate::projects::save(
            &conn,
            &crate::types::Project {
                id: "proj-1".to_string(),
                name: format!("Project {MARKER}"),
                created_at_ms: 1,
                updated_at_ms: 1,
            },
        )
        .unwrap();
        crate::artifacts::save(
            &conn,
            "conv-1",
            "art-1",
            "report.md",
            &format!("# {MARKER}"),
        )
        .unwrap();
        let mut settings = crate::settings::load(&conn);
        settings.user_name = Some(MARKER.to_string());
        crate::settings::save(&conn, &settings).unwrap();
        conn
    }

    fn add_audit_events(conn: Connection, n: usize) -> Connection {
        let shared = std::sync::Arc::new(std::sync::Mutex::new(conn));
        let log = crate::agent::audit::AuditLog::open(shared.clone());
        for i in 0..n {
            log.append(&crate::agent::audit::AuditEvent::new(
                "turn",
                &format!("call{i}"),
                "run_shell",
                "started",
            ));
        }
        drop(log);
        std::sync::Arc::try_unwrap(shared)
            .ok()
            .unwrap()
            .into_inner()
            .unwrap()
    }

    fn backup_of(conn: &Connection, dek: &DataKey, folder: &Path, now_ms: u64) -> PathBuf {
        let snapshot = read_snapshot(conn).unwrap();
        write_backup(&snapshot, &recovery_wrap(dek, CODE), dek, folder, now_ms).unwrap()
    }

    fn json<T: serde::Serialize>(value: &T) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }

    fn row_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_backup_never_contains_its_contents_in_the_clear() {
        let conn = populated();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let path = backup_of(&conn, &dek, folder.path(), 1_000);

        let bytes = fs::read(&path).unwrap();
        let marker = MARKER.as_bytes();
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker),
            "the marker leaked into the backup file"
        );
        // And it really is there once decrypted — the test isn't vacuous.
        let archive = open_backup(&path, CODE).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
        let mut settings = String::new();
        zip.by_name("settings.json")
            .unwrap()
            .read_to_string(&mut settings)
            .unwrap();
        assert!(settings.contains(MARKER));
    }

    #[test]
    fn every_category_round_trips_onto_a_fresh_install() {
        let src = add_audit_events(populated(), 3);
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let path = backup_of(&src, &dek, folder.path(), 1_000);

        // A different data directory, with a key of its own.
        let mut dst = test_conn();
        let archive = open_backup(&path, &CODE.to_lowercase().replace('-', " ")).unwrap();
        let summary = restore(&mut dst, archive).unwrap();
        assert_eq!(
            (
                summary.conversations,
                summary.projects,
                summary.artifacts,
                summary.skipped
            ),
            (1, 1, 1, 0)
        );

        assert_eq!(
            crate::store::get(&dst, "conv-1").unwrap().title,
            format!("Chat {MARKER}")
        );
        assert_eq!(
            crate::projects::list(&dst)[0].name,
            format!("Project {MARKER}")
        );
        assert_eq!(
            crate::artifacts::get(&dst, "conv-1", "art-1")
                .unwrap()
                .content,
            format!("# {MARKER}")
        );
        assert_eq!(
            json(&crate::settings::load(&dst)),
            json(&crate::settings::load(&src)),
            "settings come back whole"
        );
        let skills = crate::skills::list(&dst);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].slug, "summarize");
        assert_eq!(skills[0].files.len(), 1);
        assert_eq!(
            json(&crate::custom::load(&dst).all().to_vec()),
            json(&crate::custom::load(&src).all().to_vec())
        );
        assert_eq!(crate::plugin::enabled_record_count(&dst), 1);
        assert_eq!(
            crate::agent::audit::read_lines(&dst).unwrap(),
            crate::agent::audit::read_lines(&src).unwrap(),
            "the audit log comes back row for row, chain intact"
        );

        // Restoring the same backup again adds nothing new.
        let again = restore(&mut dst, open_backup(&path, CODE).unwrap()).unwrap();
        assert_eq!(again.skipped, 0);
        assert_eq!(row_count(&dst, "agent_audit"), 3);
        assert_eq!(row_count(&dst, "conversations"), 1);
    }

    #[test]
    fn the_wrong_code_is_told_apart_and_changes_nothing() {
        let conn = populated();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let path = backup_of(&conn, &dek, folder.path(), 1_000);

        assert!(matches!(
            open_backup(&path, "ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ-Z"),
            Err(OpenError::WrongCode)
        ));
    }

    #[test]
    fn a_truncated_or_altered_file_is_refused_before_anything_is_written() {
        let conn = populated();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let path = backup_of(&conn, &dek, folder.path(), 1_000);
        let good = fs::read(&path).unwrap();

        let mut flipped = good.clone();
        let last = flipped.len() - 40;
        flipped[last] ^= 0x01;
        for bad in [
            good[..good.len() / 2].to_vec(),
            good[..good.len() - 1].to_vec(),
            flipped,
            b"not a backup at all".to_vec(),
        ] {
            assert!(
                matches!(open_backup_bytes(&bad, CODE), Err(OpenError::Unreadable(_))),
                "a damaged file must be refused as unreadable"
            );
        }
    }

    #[test]
    fn a_damaged_archive_inside_a_good_envelope_restores_nothing() {
        let mut dst = test_conn();
        assert!(restore(&mut dst, b"definitely not a zip".to_vec()).is_err());
        assert_eq!(row_count(&dst, "conversations"), 0);
    }

    #[test]
    fn a_write_the_database_refuses_rolls_the_whole_restore_back() {
        let src = populated();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let path = backup_of(&src, &dek, folder.path(), 1_000);

        let mut dst = test_conn();
        // Projects restore after conversations in archive order; make their
        // table refuse writes so the failure lands mid-restore.
        dst.execute_batch(
            "CREATE TRIGGER no_projects BEFORE INSERT ON projects
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )
        .unwrap();
        let err = restore(&mut dst, open_backup(&path, CODE).unwrap());
        assert!(err.is_err());
        assert_eq!(
            row_count(&dst, "conversations"),
            0,
            "nothing partially restored"
        );
        assert_eq!(row_count(&dst, "artifacts"), 0);
        assert_eq!(row_count(&dst, "skills"), 0);
        assert_eq!(crate::settings::load(&dst).user_name, None);
        // The connection is usable again afterwards.
        crate::store::save(&dst, &crate::store::get(&src, "conv-1").unwrap()).unwrap();
    }

    #[test]
    fn the_folder_keeps_the_newest_seven_and_never_touches_other_files() {
        let conn = populated();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let theirs = [
            "holiday.jpg",
            "openpal-backup-notes.txt",
            "openpal-backup-fake.opbackup",
        ];
        for name in theirs {
            fs::write(folder.path().join(name), b"the user's own file").unwrap();
        }

        let mut written = Vec::new();
        for i in 0..9u64 {
            written.push(backup_of(&conn, &dek, folder.path(), 1_000 + i * DAY_MS));
        }
        let mut ours: Vec<PathBuf> = fs::read_dir(folder.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| is_ours(p))
            .collect();
        ours.sort();
        assert_eq!(ours, written[2..].to_vec(), "the two oldest went");
        for name in theirs {
            assert_eq!(
                fs::read(folder.path().join(name)).unwrap(),
                b"the user's own file"
            );
        }
        let leftovers: Vec<_> = fs::read_dir(folder.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn the_database_stays_writable_while_a_backup_is_being_written() {
        let conn = populated();
        // Enough to make writing the file take a while.
        for i in 0..400 {
            crate::artifacts::save(
                &conn,
                "conv-1",
                &format!("big-{i}"),
                "big.md",
                &"x".repeat(60_000),
            )
            .unwrap();
        }
        let shared = std::sync::Arc::new(std::sync::Mutex::new(conn));
        let snapshot = read_snapshot(&shared.lock().unwrap()).unwrap();
        let dek = DataKey::random();
        let wrap = recovery_wrap(&dek, CODE);
        let folder = tempfile::tempdir().unwrap();
        let dir = folder.path().to_path_buf();
        let writer =
            std::thread::spawn(move || write_backup(&snapshot, &wrap, &dek, &dir, 1_000).unwrap());

        let mut saved_while_writing = 0;
        while !writer.is_finished() {
            let conn = shared.lock().unwrap();
            let mut convo = crate::store::get(&conn, "conv-1").unwrap();
            convo.title = format!("edited {saved_while_writing}");
            crate::store::save(&conn, &convo).unwrap();
            saved_while_writing += 1;
        }
        writer.join().unwrap();
        assert!(
            saved_while_writing > 0,
            "a chat save must go through while the backup file is written"
        );
    }

    #[test]
    fn an_unavailable_folder_is_an_error_and_is_never_created() {
        let conn = populated();
        let dek = DataKey::random();
        let parent = tempfile::tempdir().unwrap();
        let gone = parent.path().join("unplugged-drive");
        let snapshot = read_snapshot(&conn).unwrap();
        let err =
            write_backup(&snapshot, &recovery_wrap(&dek, CODE), &dek, &gone, 1_000).unwrap_err();
        assert!(matches!(err, Error::Backup(_)));
        assert!(!gone.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_without_write_permission_is_an_error_not_a_panic() {
        use std::os::unix::fs::PermissionsExt;
        let conn = populated();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let snapshot = read_snapshot(&conn).unwrap();
        let result = write_backup(
            &snapshot,
            &recovery_wrap(&dek, CODE),
            &dek,
            folder.path(),
            1,
        );
        fs::set_permissions(folder.path(), fs::Permissions::from_mode(0o700)).unwrap();
        // Root ignores the permission bits, so only assert when it applied.
        if let Err(e) = result {
            assert!(matches!(e, Error::Backup(_)));
        }
    }

    #[test]
    fn state_round_trips_and_defaults_to_daily_and_off() {
        let conn = test_conn();
        assert_eq!(load_state(&conn), BackupState::default());
        assert_eq!(load_state(&conn).frequency, BackupFrequency::Daily);
        update_state(&conn, |s| {
            s.enabled = true;
            s.folder = Some("/Volumes/Backup".to_string());
            s.frequency = BackupFrequency::Weekly;
        })
        .unwrap();
        let state = load_state(&conn);
        assert!(state.enabled);
        assert_eq!(state.folder.as_deref(), Some("/Volumes/Backup"));
        assert_eq!(state.frequency, BackupFrequency::Weekly);
    }

    #[test]
    fn backup_choices_never_travel_in_an_export_or_a_backup() {
        let conn = populated();
        update_state(&conn, |s| s.folder = Some(format!("/{MARKER}"))).unwrap();
        let dek = DataKey::random();
        let folder = tempfile::tempdir().unwrap();
        let path = backup_of(&conn, &dek, folder.path(), 1_000);
        let mut dst = test_conn();
        restore(&mut dst, open_backup(&path, CODE).unwrap()).unwrap();
        assert_eq!(load_state(&dst), BackupState::default());
    }

    fn on(frequency: BackupFrequency) -> BackupState {
        BackupState {
            enabled: true,
            folder: Some("/backups".to_string()),
            frequency,
            ..BackupState::default()
        }
    }

    #[test]
    fn due_when_on_with_a_folder_and_none_written_yet() {
        assert!(is_due(&on(BackupFrequency::Daily), 5));
        assert!(!is_due(&BackupState::default(), 5));
        let no_folder = BackupState {
            folder: None,
            ..on(BackupFrequency::Daily)
        };
        assert!(!is_due(&no_folder, 5));
    }

    #[test]
    fn due_once_per_chosen_interval() {
        let start = 10 * DAY_MS;
        for (frequency, interval) in [
            (BackupFrequency::Daily, DAY_MS),
            (BackupFrequency::Weekly, 7 * DAY_MS),
        ] {
            let state = BackupState {
                last_success_ms: Some(start),
                last_attempt_ms: Some(start),
                ..on(frequency)
            };
            assert!(!is_due(&state, start + interval - 1));
            assert!(is_due(&state, start + interval));
            // Closed for a month: one catch-up backup as soon as it's open.
            assert!(is_due(&state, start + 30 * DAY_MS));
        }
    }

    #[test]
    fn a_failure_is_retried_after_an_hour_not_before() {
        let start = 10 * DAY_MS;
        let state = BackupState {
            last_attempt_ms: Some(start),
            last_error: Some("unplugged".to_string()),
            ..on(BackupFrequency::Daily)
        };
        assert!(!is_due(&state, start + RETRY_AFTER_MS - 1));
        assert!(is_due(&state, start + RETRY_AFTER_MS));
    }

    #[test]
    fn a_clock_set_back_before_the_last_backup_still_backs_up() {
        let state = BackupState {
            last_success_ms: Some(10 * DAY_MS),
            last_attempt_ms: Some(10 * DAY_MS),
            ..on(BackupFrequency::Daily)
        };
        assert!(is_due(&state, DAY_MS));
    }
}
