//! App-wide logging: a rolling, encrypted-at-rest file in the app-data
//! `logs/` folder, plus stdout in development.
//!
//! Why this exists: the app's `tracing` output used to go to stdout only, which
//! is thrown away in a packaged build (there's no console). Now every
//! `tracing::{info,warn,error,…}!` call is also written to a daily-rotated file
//! the user can find, view in Settings, and share when they need help.
//!
//! ## Behaviour
//! - One file per day: `openpal.<date>.enc`, rolled at midnight by
//!   `tracing-appender`. The newest [`MAX_LOG_FILES`] are kept; older ones are
//!   deleted automatically, so logs never grow without bound.
//! - Writes go through a non-blocking background worker, so logging never
//!   stalls the app. The returned [`WorkerGuard`] must be kept alive for the
//!   whole run (it flushes buffered lines on shutdown) — [`crate::state::BootState`]
//!   holds it.
//! - The level comes from the `RUST_LOG` env var when set (for developers),
//!   otherwise from the user's `logLevel` setting, otherwise `info`.
//! - Uncaught panics are routed into the log (with location + backtrace) so a
//!   crash leaves a trace instead of vanishing.
//!
//! ## Encryption and the boot-phase split
//!
//! Logging starts in `BootState::initialize` (Phase 1, pre-unlock, no
//! crypto — see that function's own doc comment), strictly before a DEK
//! exists. But `tracing_subscriber`'s global subscriber can only be
//! installed once per process (`try_init()`), so there's no reinstalling
//! it once `finish_unlock` (Phase 2) has a key. Instead, the *writer*
//! behind that one subscriber can be swapped live: [`LogWriter`] is a
//! `Clone`-able handle around a small state machine —
//!
//! - [`LogWriterState::PreUnlock`]: every line goes, in plaintext, to
//!   `<logs_dir>/boot.log` — a flagged, single-generation crash/boot log
//!   (mirrors `crate::engine`'s own `engine.log` → `engine.prev.log`
//!   pattern), deleted the moment the next successful unlock happens. This
//!   is what makes a boot failure — including the panic hook, which needs
//!   no special-casing at all, since it just calls `tracing::error!` like
//!   anything else — a logged, diagnosable event even though no DEK has
//!   ever existed yet this run.
//! - [`LogWriterState::PostUnlock`]: either [`LoggingBackend::Encrypted`]
//!   (a DEK was available — the normal case) or
//!   [`LoggingBackend::Plaintext`] (portable mode's `dek: None`, byte-
//!   identical to this file's pre-encryption behaviour).
//!
//! [`upgrade_to_encrypted`] performs that swap from `state::finish_unlock`,
//! once `app_db` is open and a `dek: Option<&DataKey>` is available.
//!
//! Each encrypted frame is one `tracing` line, sealed under a key re-
//! derived once per calendar day (`openpal_core::crypto::logframe`) — see
//! that module's own doc comment for the wire format and why the key
//! rotates daily rather than staying fixed for the process's life.

use openpal_core::crypto::key::DataKey;
use openpal_core::crypto::{kdf::derive_subkey, logframe};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

/// Prefix shared by every app log file (`openpal.<date>.enc`). Also used to
/// find and clean them up; the engine's own `engine.log.enc` is left alone.
const APP_LOG_PREFIX: &str = "openpal";

/// How many daily log files to keep. Two weeks is plenty to debug a problem a
/// user reports days later, without letting the folder grow unbounded.
const MAX_LOG_FILES: usize = 14;

/// HKDF `info` label binding the app log's derived key to this store and no
/// other (`openpal_core::crypto::kdf::derive_subkey`'s domain-separation
/// contract).
const APP_LOG_INFO: &[u8] = b"openpal-app-log-v1";

/// The pre-unlock/crash-fallback plaintext log — flagged, single-generation
/// (like `engine.log.prev`), deleted the moment the next unlock succeeds.
const BOOT_LOG_NAME: &str = "boot.log";
const BOOT_LOG_PREV_NAME: &str = "boot.log.prev";
const BOOT_LOG_HEADER: &str =
    "# OpenPal: plaintext — written before the encryption key was available \
     (pre-unlock boot / crash fallback)\n";

// ---------------------------------------------------------------------------
// The live-swappable writer behind the one global subscriber
// ---------------------------------------------------------------------------

/// One encrypted frame per `write()` call (one formatted `tracing` line),
/// sealed under a key re-derived once per calendar day and appended to the
/// same daily-rotated file `RollingFileAppender` already managed before
/// encryption existed — rotation/retention policy is unchanged, only the
/// bytes written are now AEAD frames instead of plain text.
struct EncryptingAppender {
    appender: RollingFileAppender,
    /// `derive_subkey(dek, APP_LOG_INFO)` — this store's purpose-scoped
    /// key, derived once at `upgrade_to_encrypted` time. The actual
    /// per-frame sealing key is a *second* HKDF hop off this one, re-
    /// derived from today's day marker on every write (see
    /// [`EncryptingAppender::write`]) — chaining HKDF this way is standard
    /// (the input is already a uniformly-random 256-bit key, the same
    /// justification `derive_subkey` itself gives for skipping a salt) and
    /// keeps this struct holding exactly one owned key for the session
    /// rather than a live borrow of `AppState.dek`.
    purpose_key: DataKey,
}

impl EncryptingAppender {
    fn new(appender: RollingFileAppender, dek: &DataKey) -> Self {
        Self {
            appender,
            purpose_key: derive_subkey(dek, APP_LOG_INFO),
        }
    }
}

impl std::io::Write for EncryptingAppender {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let day = logframe::today_marker();
        let day_key = derive_subkey(&self.purpose_key, &day.to_be_bytes());
        let frame = logframe::seal_frame(&day_key, day, buf);
        self.appender.write_all(&frame)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.appender.flush()
    }
}

/// What [`LogWriterState::PostUnlock`] actually writes through — encrypted
/// when a DEK was available, or today's exact plaintext behaviour when it
/// wasn't (portable mode's `dek: None`; see `AppState.dek`'s own doc
/// comment). Both wrap the same underlying `RollingFileAppender`
/// rotation/retention.
enum LoggingBackend {
    Encrypted(EncryptingAppender),
    Plaintext(RollingFileAppender),
}

impl std::io::Write for LoggingBackend {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Encrypted(a) => a.write(buf),
            Self::Plaintext(a) => a.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Encrypted(a) => a.flush(),
            Self::Plaintext(a) => a.flush(),
        }
    }
}

enum LogWriterState {
    /// Every line goes straight to `<logs_dir>/boot.log`, plaintext, by
    /// construction (see this module's doc comment for why that's the
    /// deliberate crash-fallback design, not a gap). `Box<dyn Write>`
    /// rather than a concrete `File` so the rare case where `boot.log`
    /// itself can't even be opened degrades to a black-hole
    /// (`std::io::sink()`) instead of a third enum arm — file logging is
    /// still lost pre-unlock in that case, exactly as it always was when
    /// this module's file couldn't be opened, but the post-unlock
    /// encrypted backend below is entirely unaffected by it.
    PreUnlock {
        sink: Box<dyn std::io::Write + Send>,
    },
    PostUnlock {
        backend: LoggingBackend,
    },
}

/// The one `tracing_subscriber` writer installed for the whole process,
/// wrapping a state machine that gets swapped live at `finish_unlock` —
/// see this module's doc comment. `Clone` is cheap (an `Arc` bump); every
/// clone shares the same underlying state.
#[derive(Clone)]
pub struct LogWriter(Arc<Mutex<LogWriterState>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        match &mut *state {
            LogWriterState::PreUnlock { sink } => sink.write_all(buf).map(|()| buf.len()),
            LogWriterState::PostUnlock { backend } => backend.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        match &mut *state {
            LogWriterState::PreUnlock { sink } => sink.flush(),
            LogWriterState::PostUnlock { backend } => backend.flush(),
        }
    }
}

/// Bring up file logging (and stdout in dev). Call once, as early in startup
/// as the logs directory is known — installs the process's one global
/// `tracing` subscriber, pointed at a fresh plaintext `boot.log` (see this
/// module's doc comment). Returns the worker guard the caller must keep
/// alive, and the [`LogWriter`] handle `state::finish_unlock` later hands to
/// [`upgrade_to_encrypted`].
pub fn init(logs_dir: &Path, level: &str) -> (Option<WorkerGuard>, LogWriter) {
    let _ = std::fs::create_dir_all(logs_dir);
    let filter = make_filter(level);

    // A `boot.log` left over from a run that never reached unlock (crash,
    // force-quit, power loss) is evidence of exactly the failure this file
    // exists to capture — preserve it as a single prior generation, the
    // same `.prev` pattern `crate::engine`'s own `engine.log` uses, rather
    // than silently overwriting it.
    let boot_log_path = logs_dir.join(BOOT_LOG_NAME);
    if boot_log_path.exists() {
        let _ = std::fs::rename(&boot_log_path, logs_dir.join(BOOT_LOG_PREV_NAME));
    }
    let boot_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&boot_log_path)
        .and_then(|mut f| f.write_all(BOOT_LOG_HEADER.as_bytes()).map(|()| f));

    let sink: Box<dyn std::io::Write + Send> = match boot_file {
        Ok(file) => Box::new(file),
        Err(err) => {
            tracing::warn!("could not open {boot_log_path:?}: {err}; boot logging to stdout only");
            Box::new(std::io::sink())
        }
    };
    let writer = LogWriter(Arc::new(Mutex::new(LogWriterState::PreUnlock { sink })));

    let (non_blocking, guard) = tracing_appender::non_blocking(writer.clone());
    let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking);
    let subscriber = tracing_subscriber::registry().with(filter).with(file_layer);
    #[cfg(debug_assertions)]
    let _ = subscriber.with(fmt::layer()).try_init();
    #[cfg(not(debug_assertions))]
    let _ = subscriber.try_init();

    install_panic_hook();
    banner();
    (Some(guard), writer)
}

/// Called from `state::finish_unlock`, once `app_db` is open and (in every
/// non-portable case) a DEK exists: swap the live [`LogWriter`] from the
/// pre-unlock plaintext `boot.log` to the real post-unlock backend —
/// encrypted when `dek.is_some()`, plaintext (today's exact behaviour)
/// when it's `None`. Then folds away `boot.log`/`boot.log.prev`: logs a
/// `tracing::warn!` noting the crash-fallback file existed (so the fact
/// itself is recorded in the now-live stream) and deletes both, making
/// "flag it, delete on the next successful start" literally true across
/// relaunches.
pub fn upgrade_to_encrypted(writer: &LogWriter, logs_dir: &Path, dek: Option<&DataKey>) {
    let appender = match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(APP_LOG_PREFIX)
        .filename_suffix("enc")
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir)
    {
        Ok(appender) => appender,
        Err(err) => {
            tracing::warn!("could not open the app log file in {logs_dir:?}: {err}");
            return; // stay on the pre-unlock writer rather than lose logging entirely
        }
    };
    let backend = match dek {
        Some(dek) => LoggingBackend::Encrypted(EncryptingAppender::new(appender, dek)),
        None => LoggingBackend::Plaintext(appender),
    };

    {
        let mut state = writer.0.lock().unwrap();
        *state = LogWriterState::PostUnlock { backend };
    }

    note_and_remove_boot_log(logs_dir, BOOT_LOG_NAME);
    note_and_remove_boot_log(logs_dir, BOOT_LOG_PREV_NAME);
}

fn note_and_remove_boot_log(logs_dir: &Path, name: &str) {
    let path = logs_dir.join(name);
    let Ok(meta) = std::fs::metadata(&path) else {
        return; // didn't exist — nothing to flag or clean up
    };
    tracing::warn!(
        file = name,
        size_bytes = meta.len(),
        modified = ?meta.modified().ok(),
        "found a pre-unlock/crash-fallback plaintext log from a previous run; removing it now \
         that the encrypted stream is live"
    );
    let _ = std::fs::remove_file(&path);
}

/// `RUST_LOG` wins (developers); otherwise the user's saved level; otherwise
/// `info`. Unknown level strings fall back to `info` rather than silently
/// logging nothing.
fn make_filter(level: &str) -> EnvFilter {
    if let Ok(filter) = EnvFilter::try_from_default_env() {
        return filter;
    }
    let level = match level.trim().to_ascii_lowercase().as_str() {
        l @ ("error" | "warn" | "info" | "debug" | "trace") => l.to_string(),
        _ => "info".to_string(),
    };
    EnvFilter::new(level)
}

/// One line at startup so every log opens with the version, OS and CPU — the
/// first thing you want when reading someone else's log.
fn banner() {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "OpenPal starting"
    );
}

/// Route uncaught panics into the log (keeping the default behaviour too, so
/// dev still sees the panic on stderr). Without this, a crash in a background
/// task would leave nothing in the file. Needs no special-casing for which
/// [`LogWriterState`] is live — it flows through whatever `tracing::error!`
/// currently resolves to, exactly like every other log line (see this
/// module's doc comment).
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(target: "panic", "panicked at {location}: {message}\n{backtrace}");
        default_hook(info);
    }));
}

// ---------------------------------------------------------------------------
// Reading logs back (for the in-app viewer and export)
// ---------------------------------------------------------------------------

/// The last `max_lines` lines of the app log at `path`, decrypted. Call
/// [`redact`] on the result before showing it to a user. `dek: None`
/// (portable mode) reads `path` as plain text — today's exact behaviour;
/// `dek: Some` first tries the encrypted frame format, falling back to
/// plain text if the bytes don't actually look like frames (a pre-
/// migration file, or `boot.log` itself, which is always plaintext).
pub fn decrypt_tail(path: &Path, max_lines: usize, dek: Option<&DataKey>) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let text = decrypt_or_plaintext(&bytes, dek);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// Every line of the app log at `path`, decrypted — used by the export
/// path, which (unlike the viewer) wants the whole file, not just a tail.
pub fn decrypt_all(path: &Path, dek: Option<&DataKey>) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    decrypt_or_plaintext(&bytes, dek)
}

fn decrypt_or_plaintext(bytes: &[u8], dek: Option<&DataKey>) -> String {
    let Some(dek) = dek else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    if !logframe::looks_like_frames(bytes) {
        // A legacy plaintext file (pre-migration) or `boot.log` itself.
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let purpose_key = derive_subkey(dek, APP_LOG_INFO);
    let lines = logframe::open_all(bytes, &move |day| {
        derive_subkey(&purpose_key, &day.to_be_bytes())
    });
    lines
        .into_iter()
        .map(|line| String::from_utf8_lossy(&line).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The newest app log file in `logs_dir` (the current day's), if any. The
/// rolling appender names files `openpal.<date>.enc`, and that date sorts
/// lexicographically same as chronologically — so we pick the
/// alphabetically-last filename rather than comparing mtimes. mtime
/// resolution is too coarse on some filesystems (and in CI) to distinguish
/// files written moments apart, which previously let `clear_old_app_logs`
/// delete the actual newest file.
pub fn newest_app_log(logs_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(String, PathBuf)> = None;
    for entry in std::fs::read_dir(logs_dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(APP_LOG_PREFIX) {
            continue;
        }
        if best.as_ref().is_none_or(|(n, _)| name > *n) {
            best = Some((name, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// Every app log file in `logs_dir` (`openpal.<date>.enc`), oldest first —
/// used by the "Export logs" command, which wants every file, not just the
/// newest.
pub fn all_app_logs(logs_dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(logs_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(APP_LOG_PREFIX)
        })
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
}

/// Delete every app log file except the current (newest) one. Returns how many
/// were removed. The active file is left untouched so the running writer isn't
/// disturbed; old daily files are what take up space.
pub fn clear_old_app_logs(logs_dir: &Path) -> usize {
    let keep = newest_app_log(logs_dir);
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(logs_dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(APP_LOG_PREFIX)
        {
            continue;
        }
        if keep.as_ref() == Some(&path) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Hide secrets before logs are shown, copied, or exported. The engine's
/// per-start API key and any bearer tokens can appear in subprocess output;
/// nothing else in a log is sensitive, but these must never leave the
/// machine in a "here are my logs" paste. The single, shared place this
/// runs from is what satisfies the product decision's non-negotiable rule:
/// redaction happens on both display (`commands::get_recent_logs`) and
/// export (`commands::export_logs`) — never duplicated logic, one function,
/// two call sites.
///
/// The 64-hex-char pattern is defense in depth for `DataKey`
/// (`docs/SPACES_AUTHORING.md` §9's stated "redaction prerequisite, before
/// any unlock code is written") — `DataKey`'s own `Debug`/`Display` are the
/// real protection (they never print the bytes at all); this only catches
/// a hex-encoded key that reaches a log line by some other route.
pub fn redact(text: &str) -> String {
    use regex::Regex;
    // Patterns are static and known-valid, so the unwraps can't fire.
    let api_key = Regex::new(r#"(?i)(api[-_]?key["'=:\s]+)(\S+)"#).unwrap();
    let bearer = Regex::new(r#"(?i)(bearer\s+)([A-Za-z0-9._\-]+)"#).unwrap();
    let hex_run = Regex::new(r#"\b[0-9a-fA-F]{64}\b"#).unwrap();
    let masked = api_key.replace_all(text, "${1}«hidden»").into_owned();
    let masked = bearer.replace_all(&masked, "${1}«hidden»").into_owned();
    hex_run.replace_all(&masked, "«hidden»").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_hides_keys_and_tokens() {
        let out = redact("starting --api-key deadbeefcafe --host 127.0.0.1");
        assert!(!out.contains("deadbeefcafe"), "{out}");
        let out = redact("Authorization: Bearer sk-abc123XYZ");
        assert!(!out.contains("sk-abc123XYZ"), "{out}");
    }

    #[test]
    fn redact_hides_a_64_hex_char_run() {
        let hex = "ab".repeat(32); // 64 hex chars — a DataKey's width
        let out = redact(&format!("derived key = {hex}"));
        assert!(!out.contains(&hex), "{out}");
        // Shorter hex runs (e.g. a commit sha) are left alone.
        let out = redact("commit deadbeefcafefeed");
        assert!(out.contains("deadbeefcafefeed"), "{out}");
    }

    #[test]
    fn clear_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("openpal.2026-06-15.enc"), "old").unwrap();
        std::fs::write(dir.path().join("openpal.2026-06-16.enc"), "old").unwrap();
        let newest = dir.path().join("openpal.2026-06-17.enc");
        std::fs::write(&newest, "new").unwrap();
        // Untouched, unrelated file stays.
        std::fs::write(dir.path().join("engine.log.enc"), "engine").unwrap();
        let removed = clear_old_app_logs(dir.path());
        assert!(removed >= 2, "removed {removed}");
        assert!(newest.exists());
        assert!(dir.path().join("engine.log.enc").exists());
    }

    #[test]
    fn all_app_logs_lists_every_matching_file_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("openpal.2026-06-16.enc"), "").unwrap();
        std::fs::write(dir.path().join("openpal.2026-06-15.enc"), "").unwrap();
        std::fs::write(dir.path().join("engine.log.enc"), "").unwrap();
        let files: Vec<String> = all_app_logs(dir.path())
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            files,
            vec!["openpal.2026-06-15.enc", "openpal.2026-06-16.enc"]
        );
    }

    #[test]
    fn decrypt_tail_with_no_dek_reads_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openpal.2026-06-17.enc");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();
        assert_eq!(decrypt_tail(&path, 2, None), "d\ne");
        assert_eq!(decrypt_tail(&path, 99, None), "a\nb\nc\nd\ne");
        assert_eq!(decrypt_tail(&dir.path().join("missing.enc"), 5, None), "");
    }

    #[test]
    fn decrypt_tail_roundtrips_encrypted_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openpal.2026-06-17.enc");
        let dek = DataKey::random();
        let purpose_key = derive_subkey(&dek, APP_LOG_INFO);
        let day = logframe::today_marker();
        let day_key = derive_subkey(&purpose_key, &day.to_be_bytes());
        let mut bytes = Vec::new();
        for line in ["first line", "second line", "third line"] {
            bytes.extend(logframe::seal_frame(&day_key, day, line.as_bytes()));
        }
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(
            decrypt_tail(&path, 99, Some(&dek)),
            "first line\nsecond line\nthird line"
        );
        assert_eq!(decrypt_tail(&path, 1, Some(&dek)), "third line");
    }

    #[test]
    fn decrypt_tail_with_the_wrong_dek_yields_nothing_not_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openpal.2026-06-17.enc");
        let dek = DataKey::random();
        let purpose_key = derive_subkey(&dek, APP_LOG_INFO);
        let day = logframe::today_marker();
        let day_key = derive_subkey(&purpose_key, &day.to_be_bytes());
        std::fs::write(&path, logframe::seal_frame(&day_key, day, b"secret line")).unwrap();

        let wrong = DataKey::random();
        assert_eq!(decrypt_tail(&path, 99, Some(&wrong)), "");
    }

    #[test]
    fn decrypt_tail_falls_back_to_plaintext_for_a_pre_migration_file() {
        // A file that doesn't look like the frame format (e.g. an old
        // plaintext log carried over) must still read back as text, not
        // silently return empty, even when a `dek` is available.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openpal.2026-06-17.enc");
        std::fs::write(&path, "plain old line one\nplain old line two\n").unwrap();
        let dek = DataKey::random();
        assert_eq!(
            decrypt_tail(&path, 99, Some(&dek)),
            "plain old line one\nplain old line two"
        );
    }

    #[test]
    fn upgrade_to_encrypted_writes_decryptable_frames_and_cleans_up_boot_log() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(BOOT_LOG_NAME), "pre-unlock line\n").unwrap();

        let writer = LogWriter(Arc::new(Mutex::new(LogWriterState::PreUnlock {
            sink: Box::new(std::fs::File::create(dir.path().join("scratch")).unwrap()),
        })));
        let dek = DataKey::random();
        upgrade_to_encrypted(&writer, dir.path(), Some(&dek));

        assert!(
            !dir.path().join(BOOT_LOG_NAME).exists(),
            "boot.log must be removed on upgrade"
        );

        {
            let mut w = writer.clone();
            w.write_all(b"hello after unlock\n").unwrap();
            w.flush().unwrap();
        }

        let log_path = newest_app_log(dir.path()).expect("an app log file must now exist");
        assert_eq!(
            decrypt_tail(&log_path, 99, Some(&dek)),
            "hello after unlock"
        );
        // A wrong key must not recover the line.
        assert_eq!(decrypt_tail(&log_path, 99, Some(&DataKey::random())), "");
    }

    #[test]
    fn upgrade_to_encrypted_with_no_dek_writes_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let writer = LogWriter(Arc::new(Mutex::new(LogWriterState::PreUnlock {
            sink: Box::new(std::fs::File::create(dir.path().join("scratch")).unwrap()),
        })));
        upgrade_to_encrypted(&writer, dir.path(), None);

        {
            let mut w = writer.clone();
            w.write_all(b"portable mode line\n").unwrap();
            w.flush().unwrap();
        }

        let log_path = newest_app_log(dir.path()).expect("an app log file must now exist");
        // No dek at all — read straight back as plaintext, byte-identical
        // to this file's pre-encryption behaviour.
        assert_eq!(decrypt_tail(&log_path, 99, None), "portable mode line");
    }
}
