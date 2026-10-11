//! Kill `llama-server` processes orphaned by a previous OpenPal launch that
//! crashed or was force-killed before its `kill_on_drop` `Child` handle
//! ever ran `Drop` (see `crates/core/src/engine/subprocess.rs`'s module
//! doc). Called once at boot (`BootState::initialize`, `crates/app`),
//! before any new engine is spawned — the exact scenario that piles up:
//! crash → orphan keeps a model + KV cache resident → the next launch's own
//! engine start starves for RAM and gets `SIGKILL`ed with no captured
//! output ("Your AI stopped while loading. Try again, or pick a different
//! model.").
//!
//! Mirrors the dev-only `bundler doctor` orphan check
//! (`bundler/src/doctor.rs`), but cross-platform (`sysinfo`, already a
//! direct dependency of this crate, instead of shelling out to `ps` — which
//! doesn't exist on Windows) and identity- rather than name-based: only a
//! process whose executable is byte-for-byte the *staged* `llama-server`
//! binary this app would itself launch is ever considered, so an unrelated
//! `llama-server` the user happens to run for some other project is never
//! touched.

use std::path::Path;

use sysinfo::{ProcessesToUpdate, System};

/// Kill every running process whose executable is exactly `engine_binary`
/// and whose parent is gone — the signal that separates a true orphan from
/// another OpenPal window's still-active engine (a user can run multiple
/// instances against different data dirs at once, each past its own
/// `InstanceLock`, each with its own live engine child that must never be
/// touched). What "gone" means is platform-dependent, per `sysinfo`'s own
/// docs on [`sysinfo::Process::parent`]: on Windows the recorded parent pid
/// is frozen at spawn time and never updated, so a dead pid there really
/// does mean the spawning process exited; on Unix the kernel reparents an
/// orphan to init/launchd (pid 1) *before* `sysinfo` ever observes it, so
/// the recorded parent pid is always some live process, and "reparented to
/// pid 1" is the actual signal (this mirrors the dev-only `bundler doctor`
/// check, which reads the same fact via `ps -axo ...,ppid=,...`).
///
/// Best-effort and silent about individual failures (an unreadable exe
/// path, a kill that fails) — one uninspectable process must not stop the
/// rest of the sweep. Returns the pids actually killed, purely so the
/// caller can log a friendly count.
pub fn kill_orphaned_engines(engine_binary: &Path) -> Vec<u32> {
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);

    sys.processes()
        .iter()
        .filter(|(_, process)| process.exe() == Some(engine_binary))
        .filter(|(_, process)| is_orphaned(&sys, process))
        .filter(|(_, process)| process.kill())
        .map(|(pid, _)| pid.as_u32())
        .collect()
}

fn is_orphaned(sys: &System, process: &sysinfo::Process) -> bool {
    match process.parent() {
        None => true,
        Some(parent_pid) => {
            (cfg!(unix) && parent_pid.as_u32() == 1) || sys.process(parent_pid).is_none()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_matching_exe_means_nothing_is_killed() {
        let killed = kill_orphaned_engines(Path::new(
            "/definitely/not/a/real/path/to/anything-llama-server-like",
        ));
        assert!(killed.is_empty());
    }

    /// A process matching `engine_binary` whose parent (this test process)
    /// is very much alive must be left alone — it's what a live sibling
    /// OpenPal window's active engine looks like from the outside.
    #[cfg(unix)]
    #[test]
    fn a_live_process_with_a_live_parent_is_not_touched() {
        let exe = std::env::current_exe().unwrap();
        let mut child = spawn_sleeper_helper(&exe);

        let killed = kill_orphaned_engines(&exe);
        assert!(
            !killed.contains(&child.id()),
            "a process whose parent is still alive must not be killed"
        );

        child.kill().ok();
        child.wait().ok();
    }

    /// The orphan case: a shell backgrounds the sleeper helper and exits
    /// immediately, reparenting it away from this test process before we
    /// ever look for it — exactly what `kill_on_drop` failing to fire
    /// leaves behind.
    #[cfg(unix)]
    #[test]
    fn an_orphaned_process_is_found_and_killed() {
        let exe = std::env::current_exe().unwrap();

        let mut intermediate = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "'{}' --exact 'boot::orphan_engine::tests::orphan_test_sleeper_helper' \
                 --ignored --test-threads=1 >/dev/null 2>&1 &",
                exe.display()
            ))
            .spawn()
            .expect("spawn intermediate shell");
        intermediate.wait().expect("reap the intermediate shell");

        // Reparenting is synchronous with the parent's exit on every unix
        // this crate targets, but give sysinfo a couple of retries against
        // any scheduling jitter on a loaded CI box.
        let mut killed = Vec::new();
        for _ in 0..20 {
            killed = kill_orphaned_engines(&exe);
            if !killed.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(
            !killed.is_empty(),
            "expected the orphaned sleeper helper to be found and killed"
        );
    }

    /// Not a real test — a long-lived subprocess body that
    /// [`an_orphaned_process_is_found_and_killed`] invokes by re-execing
    /// this same test binary with `--exact --ignored`, so the resulting
    /// process's `exe()` is a real, freshly-built (non-platform-signed,
    /// copy/re-exec-safe) binary rather than a fabricated stand-in.
    /// `#[ignore]` keeps a normal `cargo test` run from ever entering it.
    #[test]
    #[ignore = "spawned directly as a subprocess by orphan_engine tests, never run standalone"]
    fn orphan_test_sleeper_helper() {
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[cfg(unix)]
    fn spawn_sleeper_helper(exe: &Path) -> std::process::Child {
        std::process::Command::new(exe)
            .args([
                "--exact",
                "boot::orphan_engine::tests::orphan_test_sleeper_helper",
                "--ignored",
                "--test-threads=1",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleeper helper")
    }
}
