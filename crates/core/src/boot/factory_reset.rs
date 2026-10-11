//! Deletes everything in the data directory except `models_dir` (a
//! multi-gigabyte download nobody wants to repeat) and the live instance
//! lock file (`.lock`) — that file belongs to the currently-running process
//! and gets recreated automatically, so removing it out from under a live
//! process risks trouble for no benefit; it isn't user data worth erasing
//! either way. Pure filesystem, no crypto: `crate::crypto::keychain::delete`
//! handles the OS keychain entry separately, and
//! `crates/app/src/factory_reset.rs` orchestrates the relaunch this runs
//! inside of.

use std::path::Path;
use std::time::Duration;

/// How long to wait for the old process to exit before wiping anyway (same
/// value and reasoning as `update_portable.rs`'s own
/// `OLD_PROCESS_EXIT_TIMEOUT`): long enough for an ordinary graceful
/// shutdown, short enough that a hung old process doesn't leave the user
/// stuck on the reset screen forever.
pub const OLD_PROCESS_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Poll until `pid` is no longer running, or `timeout` elapses. The caller
/// (`crates/app/src/factory_reset.rs::apply_pending_reset`) must call this
/// before `wipe` — the old process still holds `app.sqlite` and the log
/// files open at the moment it spawns this relaunch (`spawn()` returns as
/// soon as the child process starts, well before the parent's own Tauri
/// shutdown finishes closing them). Deleting files another process still
/// has open silently "works" on Unix (the directory entry goes away; the
/// old process's already-open handle keeps working until it closes) but
/// fails outright on Windows (a sharing violation), so skipping this wait
/// isn't just a cosmetic race — `wipe` would return `Err` partway through
/// and leave a partially-wiped data directory. Mirrors
/// `update_portable.rs`'s own `wait_for_pid_exit` rather than sharing it —
/// same reasoning as this module's other duplication-over-abstraction
/// calls: distinct log messages, distinct callers, no shared state.
pub fn wait_for_pid_exit(pid: u32, timeout: Duration) {
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
                "factory reset: old process {pid} still running after {timeout:?}; wiping anyway"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub fn wipe(data_dir: &Path, models_dir: &Path) -> std::io::Result<()> {
    if !data_dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(data_dir)? {
        let path = entry?.path();
        if path == models_dir {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some(".lock") {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_tree(root: &Path) {
        std::fs::write(root.join("app.sqlite"), b"db").unwrap();
        std::fs::create_dir_all(root.join("keys")).unwrap();
        std::fs::write(root.join("keys/keychain.wrap"), b"wrap").unwrap();
        std::fs::write(root.join("applock.json"), b"{}").unwrap();
        std::fs::create_dir_all(root.join("models")).unwrap();
        std::fs::write(root.join("models/foo.gguf"), b"model").unwrap();
        std::fs::write(root.join(".lock"), b"lock").unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::write(root.join("logs/x.log"), b"log").unwrap();
    }

    #[test]
    fn wipe_deletes_everything_except_models_dir_and_the_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        build_tree(dir.path());
        let models_dir = dir.path().join("models");

        wipe(dir.path(), &models_dir).unwrap();

        let remaining: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            remaining.len(),
            2,
            "only models/ and .lock should survive: {remaining:?}"
        );
        assert!(dir.path().join("models/foo.gguf").exists());
        assert!(dir.path().join(".lock").exists());
        assert!(!dir.path().join("app.sqlite").exists());
        assert!(!dir.path().join("keys").exists());
        assert!(!dir.path().join("applock.json").exists());
        assert!(!dir.path().join("logs").exists());
    }

    #[test]
    fn wipe_is_a_no_op_on_a_nonexistent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        wipe(&missing, &missing.join("models")).unwrap();
    }

    #[test]
    fn wipe_is_a_no_op_on_an_already_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        wipe(dir.path(), &dir.path().join("models")).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
