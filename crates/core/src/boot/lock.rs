//! `<data>/.lock` — an exclusive, single-instance guard for one data
//! directory, acquired before anything else touches it. The doc's own
//! considered-and-rejected alternative: "`tauri-plugin-single-instance`
//! avoid — a 20-line `<data>/.lock` does the same for zero crates on all
//! five targets" (`docs/SPACES_AUTHORING.md` §9's dependency table).
//!
//! Not a true OS advisory lock (no `flock`/`LockFileEx`, no new crate for
//! either) — a plain PID file, with staleness checked via `sysinfo`
//! (already a direct dependency of this crate, used elsewhere for
//! memory/GPU probing). The one accepted gap: the stale-recovery path
//! (read the old PID, confirm it's gone, overwrite) isn't atomic against a
//! second launch racing the exact same instant — vanishingly unlikely for
//! a single-user desktop app's own data directory, and the doc's own "20
//! lines, zero crates" framing implies exactly this level of pragmatism
//! over a bulletproof distributed lock.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("OpenPal is already running against this data folder (pid {0})")]
    AlreadyRunning(u32),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Held for the process's lifetime. Removes the lock file on drop — a
/// crash leaves it behind, which is exactly the case the staleness check
/// in [`acquire`] exists to recover from on the next launch.
#[derive(Debug)]
pub struct InstanceLock {
    path: PathBuf,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn acquire(path: &Path) -> Result<InstanceLock, LockError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let pid = std::process::id();
    match write_new(path, pid) {
        Ok(()) => {
            return Ok(InstanceLock {
                path: path.to_path_buf(),
            })
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }

    // A lock file already exists — find out whether its owner is still
    // alive before deciding to take over. `acquire` is only ever called
    // once per process at startup, before this process could have written
    // its own pid here, so there's no "is this actually just me" case to
    // special-case — whatever pid is on disk, check it and trust the
    // answer.
    if let Some(existing_pid) = read_pid(path) {
        if process_is_alive(existing_pid) {
            return Err(LockError::AlreadyRunning(existing_pid));
        }
    }

    // Stale (or unreadable — treat the same way: something else's job
    // will be to diagnose a truly corrupt data dir, not this lock).
    // Overwrite, not `create_new`, since the file already exists.
    std::fs::write(path, pid.to_string())?;
    Ok(InstanceLock {
        path: path.to_path_buf(),
    })
}

fn write_new(path: &Path, pid: u32) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(pid.to_string().as_bytes())
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// A PID match alone isn't proof the recorded instance is still running:
/// PIDs get recycled, and a `.lock` left behind by a killed/crashed process
/// (its `Drop` never ran) can collide with an unrelated process the OS
/// later hands that same number — on a long-lived desktop, "eventually"
/// isn't rare. Confirm the process at `pid` is actually running this same
/// executable before trusting it over the staleness check below. Falls
/// back to the PID-only signal (i.e. assume alive) when either executable
/// path can't be read — e.g. a permissions-restricted process — the same
/// conservative default this module already accepted for the exists/
/// doesn't-exist case.
///
/// Right after another process calls `Command::spawn()`, `sysinfo` can see
/// the new PID before its exe path is resolvable (e.g. `/proc/<pid>/exe`
/// not yet stable immediately post-fork/exec) — a startup race that's more
/// likely to be hit on a loaded/virtualized CI runner than on a quiet local
/// machine. Left unhandled, that reads as "can't tell, assume alive",
/// which can misidentify a freshly-spawned unrelated process as the
/// original lock owner. Retry briefly before falling back to that
/// conservative default, closing the race without weakening it.
fn process_is_alive(pid: u32) -> bool {
    const RETRIES: u32 = 5;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

    let target = sysinfo::Pid::from_u32(pid);
    let current_exe = std::env::current_exe();
    let mut sys = sysinfo::System::new();

    for attempt in 0..RETRIES {
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
        let Some(process) = sys.process(target) else {
            return false;
        };
        match (process.exe(), &current_exe) {
            (Some(exe), Ok(current)) => return exe == current,
            (None, Ok(_)) if attempt + 1 < RETRIES => {
                std::thread::sleep(RETRY_DELAY);
            }
            _ => return true,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PID that's provably not running: spawn a trivial short-lived
    /// child and wait for it to exit (reaping it), then use its former
    /// PID. There's an inherent, vanishingly small chance the OS reuses
    /// that exact PID before the assertion runs — accepted the same way
    /// the module's own doc comment accepts the stale-recovery race.
    fn a_dead_pid() -> u32 {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit", "0"][..]
            } else {
                &[][..]
            })
            .spawn()
            .expect("spawn a trivial child");
        let pid = child.id();
        child.wait().expect("reap the child");
        pid
    }

    /// A live process that is provably *not* the current executable — the
    /// PID-reuse case: some unrelated process (a shell, another app) ends up
    /// running under the exact PID an old `.lock` still names.
    fn spawn_unrelated_process() -> std::process::Child {
        if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
                .spawn()
                .expect("spawn a placeholder process")
        } else {
            std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn a placeholder process")
        }
    }

    #[test]
    fn acquiring_a_free_lock_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let _lock = acquire(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn the_lock_file_is_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        drop(acquire(&path).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn a_lock_held_by_a_live_process_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        // Our own PID is unambiguously alive.
        std::fs::write(&path, std::process::id().to_string()).unwrap();

        let err = acquire(&path).unwrap_err();
        assert!(matches!(err, LockError::AlreadyRunning(_)));
    }

    #[test]
    fn a_lock_naming_a_live_pid_now_owned_by_a_different_executable_is_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        let mut other = spawn_unrelated_process();
        std::fs::write(&path, other.id().to_string()).unwrap();

        let result = acquire(&path);
        other.kill().ok();
        other.wait().ok();

        let _lock = result.expect("a PID reused by an unrelated live process must not block");
        assert_eq!(read_pid(&path), Some(std::process::id()));
    }

    #[test]
    fn a_lock_left_by_a_dead_process_is_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        std::fs::write(&path, a_dead_pid().to_string()).unwrap();

        let _lock = acquire(&path).expect("a stale lock must be recoverable");
        assert_eq!(read_pid(&path), Some(std::process::id()));
    }

    #[test]
    fn an_unparseable_lock_file_is_treated_as_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".lock");
        std::fs::write(&path, "not a pid").unwrap();

        let _lock = acquire(&path).expect("an unreadable lock must not wedge every future launch");
    }
}
