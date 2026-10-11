//! Portable-SKU update mechanics: download, verify, extract, and apply a new
//! build in place, then relaunch. Tauri's own updater plugin only drives the
//! Full SKU's native installer; Portable has none, so this module is the
//! entire mechanism, and it's the same code path on all three OSes.
//!
//! Windows can't overwrite its own running `.exe`, so rather than
//! special-case that, every OS follows the same shape: the *new* build —
//! already downloaded, verified, and extracted, but not yet live — is
//! spawned as a detached helper carrying a few env vars
//! ([`spawn_apply_helper`]). It waits for the *old* process to exit, swaps
//! itself into the install path ([`apply_and_relaunch`]), then re-execs as
//! an ordinary launch. The helper side is wired in at
//! `crates/app/src/portable.rs::apply_pending_update`, called at the very
//! top of `run()`, before the webview is created (same place
//! `prepare_webview2` runs) — by the time Tauri itself starts up, either
//! this was a normal launch, or the update is already applied and this
//! process is about to re-exec and exit.

use crate::download;
use crate::types::DownloadEvent;
use crate::update::PortableEntry;
use crate::{Error, Result};
use minisign_verify::{PublicKey, Signature};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

const ENV_STAGED_UNIT: &str = "OPENPAL_APPLY_STAGED_UNIT";
const ENV_INSTALL_PATH: &str = "OPENPAL_APPLY_INSTALL_PATH";
const ENV_OLD_PID: &str = "OPENPAL_APPLY_OLD_PID";

/// How long to wait for the old process to exit before applying the swap
/// anyway. A wedged old process holding files open then fails the rename
/// with a clear log line rather than hanging the relaunch forever.
const OLD_PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

const VERIFY_MSG: &str = "The downloaded update failed a security check and was discarded. \
                          Please try again. If this keeps happening, please report it.";
const EXTRACT_MSG: &str = "The downloaded update couldn't be unpacked. Please try again.";
const APPLY_MSG: &str = "The update downloaded correctly but couldn't be installed. \
                         Please try again, or reinstall OpenPal from the website.";

/// Download `entry`'s archive into `dest_dir` (created if needed), verifying
/// both its SHA-256 (via the existing resumable downloader) and its
/// minisign signature. Returns the path to the downloaded (not yet
/// extracted) archive.
pub async fn download_portable_update(
    client: &reqwest::Client,
    entry: &PortableEntry,
    pubkey_b64: &str,
    dest_dir: &Path,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(DownloadEvent) + Send,
) -> Result<PathBuf> {
    fs::create_dir_all(dest_dir).map_err(Error::Io)?;
    let file_name = entry
        .url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            Error::Update("The update manifest has no file name in its URL.".to_string())
        })?;
    let dest = dest_dir.join(file_name);

    download::download_file(
        client,
        &entry.url,
        &dest,
        None,
        Some(&entry.sha256),
        cancel,
        &mut on_event,
    )
    .await?;

    if let Err(e) = verify_signature(&dest, &entry.signature, pubkey_b64) {
        let _ = fs::remove_file(&dest);
        return Err(e);
    }

    Ok(dest)
}

pub(crate) fn verify_signature(path: &Path, signature_b64: &str, pubkey_b64: &str) -> Result<()> {
    let pubkey = PublicKey::from_base64(pubkey_b64).map_err(|e| {
        tracing::error!("update: the configured updater pubkey is invalid: {e}");
        Error::Update(VERIFY_MSG.to_string())
    })?;
    let signature = Signature::decode(signature_b64).map_err(|e| {
        tracing::warn!("update: the manifest's signature is malformed: {e}");
        Error::Update(VERIFY_MSG.to_string())
    })?;
    let bytes = fs::read(path).map_err(Error::Io)?;
    // `allow_legacy = false`: Tauri's updater signer never produces the
    // older pre-hashed format this would exist to accept.
    pubkey.verify(&bytes, &signature, false).map_err(|e| {
        tracing::warn!("update: signature verification failed: {e}");
        Error::Update(VERIFY_MSG.to_string())
    })
}

/// Unpack `archive` into `dest_dir` (recreated fresh) and return the path to
/// the "unit" that should replace the live install: the `.app` bundle
/// (macOS), the folder holding `OpenPal.exe` (Windows), or the `.AppImage`
/// file itself (Linux, which has nothing to unpack).
pub fn extract(archive: &Path, dest_dir: &Path) -> Result<PathBuf> {
    let _ = fs::remove_dir_all(dest_dir);
    fs::create_dir_all(dest_dir).map_err(Error::Io)?;

    if archive
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("appimage"))
    {
        let target = dest_dir.join(archive.file_name().unwrap_or_default());
        fs::copy(archive, &target).map_err(Error::Io)?;
        set_executable(&target);
        return Ok(target);
    }

    let file = fs::File::open(archive).map_err(Error::Io)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| {
        tracing::warn!("update archive is not a valid zip: {e}");
        Error::Update(EXTRACT_MSG.to_string())
    })?;
    zip.extract(dest_dir).map_err(|e| {
        tracing::warn!("update archive extraction failed: {e}");
        Error::Update(EXTRACT_MSG.to_string())
    })?;

    resolve_staged_unit(dest_dir)
}

/// The zip keeps a wrapping stage folder (`bundler/src/portable.rs::zip_stage`)
/// whose exact name isn't worth depending on, so find the real unit by name
/// instead of assuming a fixed depth.
#[cfg(target_os = "macos")]
fn resolve_staged_unit(root: &Path) -> Result<PathBuf> {
    find_by_name(root, "OpenPal.app", 3).ok_or_else(|| Error::Update(EXTRACT_MSG.to_string()))
}
#[cfg(target_os = "windows")]
fn resolve_staged_unit(root: &Path) -> Result<PathBuf> {
    let exe = find_by_name(root, "OpenPal.exe", 3)
        .ok_or_else(|| Error::Update(EXTRACT_MSG.to_string()))?;
    Ok(exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.to_path_buf()))
}
#[cfg(all(unix, not(target_os = "macos")))]
fn resolve_staged_unit(_root: &Path) -> Result<PathBuf> {
    // Linux ships as a bare .AppImage — handled in `extract` before this is
    // ever reached; a zip on Linux would mean a bad manifest entry.
    Err(Error::Update(EXTRACT_MSG.to_string()))
}

/// Used by `resolve_staged_unit` on macOS and Windows; unreachable (and so
/// unused) on the other Unixes, where Portable ships as a bare `.AppImage`.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn find_by_name(root: &Path, name: &str, max_depth: u8) -> Option<PathBuf> {
    if max_depth == 0 {
        return None;
    }
    let mut subdirs = Vec::new();
    for entry in fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir() {
            subdirs.push(path);
        }
    }
    subdirs
        .into_iter()
        .find_map(|dir| find_by_name(&dir, name, max_depth - 1))
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        let _ = fs::set_permissions(path, perms);
    }
}
#[cfg(not(unix))]
fn set_executable(_path: &Path) {}

/// Strip macOS's quarantine flag from the newly swapped-in `.app` — Portable
/// macOS is ad-hoc-signed, not notarized, so Gatekeeper would otherwise
/// refuse to launch it (see `docs/design/PORTABLE.md`).
#[cfg(target_os = "macos")]
pub(crate) fn strip_quarantine(path: &Path) {
    let _ = Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(path)
        .status();
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn strip_quarantine(_path: &Path) {}

/// The executable to run inside a resolved unit: `Contents/MacOS/<binary>`
/// for a `.app`, `OpenPal.exe` for a Windows folder, or the unit itself for
/// a single-file Linux `.AppImage`.
fn executable_within(unit: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let macos_dir = unit.join("Contents/MacOS");
        fs::read_dir(&macos_dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_file())
    }
    #[cfg(target_os = "windows")]
    {
        let exe = unit.join("OpenPal.exe");
        exe.exists().then_some(exe)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        unit.is_file().then(|| unit.to_path_buf())
    }
}

/// Rename `install_path` to a sibling `.old`, move `staged_path` into its
/// place, then best-effort delete `.old`. Works whether the unit is a
/// directory (macOS `.app`, a Windows folder) or a single file (Linux
/// `.AppImage`) — `rename` doesn't care which, as long as both sides agree.
/// If the second rename fails, best-effort restores the original so the
/// install isn't left missing entirely.
pub fn swap_install(install_path: &Path, staged_path: &Path) -> Result<()> {
    let old = sibling_path(install_path, "old");
    let _ = remove_path(&old);
    fs::rename(install_path, &old).map_err(|e| {
        tracing::error!(
            "update: could not move aside {}: {e}",
            install_path.display()
        );
        Error::Update(APPLY_MSG.to_string())
    })?;
    if let Err(e) = fs::rename(staged_path, install_path) {
        tracing::error!("update: could not move the staged build into place: {e}");
        let _ = fs::rename(&old, install_path);
        return Err(Error::Update(APPLY_MSG.to_string()));
    }
    let _ = remove_path(&old);
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Spawn the executable inside `staged_unit` as a detached helper carrying
/// the env vars that mark it as an update helper rather than a normal
/// launch. The caller must already have torn down the engine/plugin
/// subprocesses (same teardown as a normal quit) and should exit right
/// after this returns — the helper takes over from there.
pub fn spawn_apply_helper(staged_unit: &Path, install_path: &Path) -> Result<()> {
    let exe = executable_within(staged_unit).ok_or_else(|| {
        tracing::error!(
            "update: no executable found inside {}",
            staged_unit.display()
        );
        Error::Update(APPLY_MSG.to_string())
    })?;
    Command::new(&exe)
        .env(ENV_STAGED_UNIT, staged_unit)
        .env(ENV_INSTALL_PATH, install_path)
        .env(ENV_OLD_PID, std::process::id().to_string())
        .spawn()
        .map_err(|e| {
            tracing::error!("update: could not launch the staged build: {e}");
            Error::Update(APPLY_MSG.to_string())
        })?;
    Ok(())
}

/// A launch that turned out to be the update helper spawned by
/// [`spawn_apply_helper`], rather than an ordinary start.
pub struct PendingApply {
    pub staged_unit: PathBuf,
    pub install_path: PathBuf,
    pub old_pid: u32,
}

/// Checked once, at the very top of `run()`, before the webview is created.
/// `None` on an ordinary launch (the common case — cheap: three env var
/// reads). Consumes the env vars either way so they never leak into a
/// re-exec or a child process.
pub fn take_pending_apply() -> Option<PendingApply> {
    let staged_unit = std::env::var_os(ENV_STAGED_UNIT);
    let install_path = std::env::var_os(ENV_INSTALL_PATH);
    let old_pid = std::env::var(ENV_OLD_PID).ok();
    std::env::remove_var(ENV_STAGED_UNIT);
    std::env::remove_var(ENV_INSTALL_PATH);
    std::env::remove_var(ENV_OLD_PID);
    Some(PendingApply {
        staged_unit: PathBuf::from(staged_unit?),
        install_path: PathBuf::from(install_path?),
        old_pid: old_pid?.parse().ok()?,
    })
}

/// Apply a pending update: wait for the old process to exit, swap
/// `pending.staged_unit` into `pending.install_path`, strip macOS
/// quarantine, then spawn the swapped-in binary as a normal launch. The
/// caller should exit immediately after calling this regardless of outcome
/// — on failure the old install is left in place (best-effort restored by
/// `swap_install`) and the caller has nothing further to run as *this*
/// process.
pub fn apply_and_relaunch(pending: PendingApply) {
    wait_for_pid_exit(pending.old_pid, OLD_PROCESS_EXIT_TIMEOUT);

    if let Err(e) = swap_install(&pending.install_path, &pending.staged_unit) {
        tracing::error!("update: applying the update failed: {e}");
        return;
    }
    strip_quarantine(&pending.install_path);

    match executable_within(&pending.install_path) {
        Some(exe) => {
            if let Err(e) = Command::new(&exe).spawn() {
                tracing::error!("update: applied, but relaunch failed: {e}");
            }
        }
        None => tracing::error!(
            "update: applied, but couldn't find the executable to relaunch at {}",
            pending.install_path.display()
        ),
    }
}

/// Poll until `pid` is no longer running, or `timeout` elapses.
fn wait_for_pid_exit(pid: u32, timeout: Duration) {
    let target = sysinfo::Pid::from_u32(pid);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut sys = sysinfo::System::new_all();
        sys.refresh_all();
        if sys.process(target).is_none() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                "update: old process {pid} still running after {timeout:?}; applying anyway"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_install_moves_staged_into_place_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("OpenPal.app");
        let staged = dir.path().join("staged/OpenPal.app");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("old.txt"), b"old").unwrap();
        fs::create_dir_all(&staged).unwrap();
        fs::write(staged.join("new.txt"), b"new").unwrap();

        swap_install(&install, &staged).unwrap();

        assert!(install.join("new.txt").exists());
        assert!(!install.join("old.txt").exists());
        assert!(!staged.exists());
        assert!(!sibling_path(&install, "old").exists());
    }

    #[test]
    fn swap_install_restores_original_if_the_second_rename_fails() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("OpenPal.app");
        fs::create_dir_all(&install).unwrap();
        fs::write(install.join("old.txt"), b"old").unwrap();
        // A staged path that doesn't exist makes the second rename fail.
        let staged = dir.path().join("does-not-exist");

        let result = swap_install(&install, &staged);

        assert!(result.is_err());
        assert!(
            install.join("old.txt").exists(),
            "original install must be restored"
        );
    }

    #[test]
    fn swap_install_works_for_a_single_file_unit() {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("OpenPal.AppImage");
        let staged = dir.path().join("OpenPal-new.AppImage");
        fs::write(&install, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();

        swap_install(&install, &staged).unwrap();

        assert_eq!(fs::read(&install).unwrap(), b"new");
        assert!(!staged.exists());
    }

    #[test]
    fn find_by_name_locates_a_nested_entry() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("stage-folder/OpenPal.app");
        fs::create_dir_all(&nested).unwrap();

        let found = find_by_name(dir.path(), "OpenPal.app", 3);

        assert_eq!(found, Some(nested));
    }

    #[test]
    fn find_by_name_gives_up_past_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c/OpenPal.app");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_by_name(dir.path(), "OpenPal.app", 2), None);
    }
}
