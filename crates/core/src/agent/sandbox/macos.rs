//! macOS Seatbelt (`sandbox-exec`) implementation of [`super::Sandbox`].
//!
//! Beyond the static profile itself: `pre_exec` puts the child in its own
//! session (so it can be killed as a unit, alongside its process group) and
//! sets resource limits (`RLIMIT_NPROC`/`RLIMIT_FSIZE`/`RLIMIT_AS`) as a
//! backstop against fork bombs and disk/memory exhaustion inside the wall-
//! clock timeout window; a disk-quota watchdog polls `workspace`+`scratch`
//! size independently of `RLIMIT_FSIZE` (which only bounds one file at a
//! time, not many small ones). A fully self-detaching descendant (calling
//! `setsid`/`daemon()` again itself) can still escape process-tree-based
//! kill — an inherent POSIX limitation, not specific to this design — but it
//! remains bounded by the inherited rlimits and the unconditional
//! `deny network*` in both profiles.

use super::{Sandbox, SandboxProfile, SandboxSpec, SandboxedOutput};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const SHELL_PROFILE: &str = include_str!("shell.sb");
const CODE_PROFILE: &str = include_str!("code.sb");

/// Headroom added on top of the account's *ambient* process count before the
/// fork-bomb backstop kicks in. `RLIMIT_NPROC` is enforced on Darwin/BSD
/// against the real UID's total process count **system-wide**, not scoped to
/// this process's subtree (see `setrlimit(2)`), so a fixed absolute cap
/// breaks on any real machine that already has more than that many processes
/// running — confirmed empirically on a normal dev Mac with 600+ ambient
/// processes for the logged-in user, where an earlier absolute
/// `RLIMIT_NPROC = 64` made every sandboxed `fork()` fail immediately with
/// `EAGAIN`, even for a plain `echo`.
///
/// Chosen generously for realistic concurrent scripting (several dozen
/// parallel subprocesses) while staying far below the account's hard
/// ceiling. Note this is *not* tuned against how fast a fork bomb gets
/// contained: a classic pipe-and-background bomb (`:(){ :|:& };:`) doesn't
/// grow monotonically — each generation's processes exit almost as soon as
/// they've backgrounded their own children, so its live-process count
/// oscillates in a noisy, CPU-speed-dependent equilibrium rather than
/// climbing forever, and bash retries `fork()` on `EAGAIN` instead of giving
/// up. That makes "does the bomb finish within N seconds" a bad, flaky proxy
/// for whether this value is correct, at sizes both much larger and much
/// smaller than this one (verified empirically, including a real CI
/// failure) — see the `rlimit_nproc_inside_the_sandbox_...` test in this
/// file's test module for the direct, deterministic check instead.
const RLIMIT_NPROC_HEADROOM: u64 = 64;
const RLIMIT_FSIZE_BYTES: u64 = 1024 * 1024 * 1024;
const DISK_WATCHDOG_INTERVAL: Duration = Duration::from_millis(500);
/// Safety cap on captured output while still fully draining the pipe (so a
/// chatty process can't block on a full pipe buffer forever) — separate from
/// and looser than [`crate::agent::tool::MAX_TOOL_OUTPUT_BYTES`], which is
/// applied uniformly afterward.
const MAX_CAPTURED_BYTES: usize = 4 * 1024 * 1024;

pub struct SeatbeltSandbox;

#[async_trait::async_trait]
impl Sandbox for SeatbeltSandbox {
    async fn run(
        &self,
        profile: &SandboxProfile,
        program: &Path,
        args: &[String],
        spec: &SandboxSpec,
    ) -> Result<SandboxedOutput, String> {
        std::fs::create_dir_all(&spec.scratch).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&spec.workspace).map_err(|e| e.to_string())?;

        // Canonicalize before binding as `-D` params: Seatbelt's `subpath`
        // match is against the kernel's resolved view of an access, so a
        // param bound to an unresolved path (e.g. macOS's `/var` -> `/private
        // /var` symlink, which is exactly what `$TMPDIR` is expressed
        // through) silently never matches real accesses, denying reads/
        // writes that should be allowed — verified empirically. Don't trust
        // callers to have already done this.
        let workspace = spec
            .workspace
            .canonicalize()
            .map_err(|e| format!("workspace: {e}"))?;
        let scratch = spec
            .scratch
            .canonicalize()
            .map_err(|e| format!("scratch: {e}"))?;
        let cwd = spec.cwd.canonicalize().map_err(|e| format!("cwd: {e}"))?;

        let (profile_text, interpreter) = match profile {
            SandboxProfile::Shell => (SHELL_PROFILE, None),
            SandboxProfile::Code { interpreter } => (CODE_PROFILE, Some(interpreter.clone())),
        };
        let profile_path = scratch.join(".profile.sb");
        std::fs::write(&profile_path, profile_text).map_err(|e| e.to_string())?;

        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-D")
            .arg(format!("WORKSPACE={}", workspace.display()));
        cmd.arg("-D").arg(format!("SCRATCH={}", scratch.display()));
        if let Some(interp) = &interpreter {
            cmd.arg("-D")
                .arg(format!("INTERPRETER={}", interp.display()));
        }
        cmd.arg("-f")
            .arg(&profile_path)
            .arg("--")
            .arg(program)
            .args(args);

        cmd.current_dir(&cwd)
            .env_clear()
            .env(
                "PATH",
                "/usr/bin:/bin:/usr/local/bin:/usr/local/sbin:/opt/homebrew/bin:/opt/homebrew/sbin",
            )
            .env("HOME", &scratch)
            .env("TMPDIR", &scratch)
            .env("LANG", "en_US.UTF-8")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // Computed here, in the parent, before fork: deriving the cap
        // requires spawning `ps` (to count the account's ambient processes)
        // and reading the account's own hard ceiling, neither of which is
        // async-signal-safe, so none of it can happen inside `pre_exec`
        // itself. `nproc_cap` is `None` when we can't determine a safe value
        // (e.g. `ps` failed) — in that case we leave `RLIMIT_NPROC` at
        // whatever the OS default already is rather than risk setting a cap
        // that's wrong in either direction.
        let nproc_cap = nproc_cap_with_headroom().await;

        // SAFETY: only async-signal-safe calls (setsid, setrlimit) run
        // between fork and exec, per signal-safety(7).
        //
        // No `RLIMIT_AS` here: Darwin defines the constant but the kernel
        // doesn't enforce it (`setrlimit(RLIMIT_AS, ...)` — and even plain
        // `ulimit -v` — fails with EINVAL on macOS, verified empirically).
        // Memory exhaustion inside the timeout window is bounded instead by
        // `RLIMIT_FSIZE` + the disk-quota watchdog (can't page a working set
        // larger than what fits on disk in `WORKSPACE`/`SCRATCH`) and the
        // wall-clock timeout itself, not a hard address-space ceiling.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(cap) = nproc_cap {
                    set_rlimit(libc::RLIMIT_NPROC, cap)?;
                }
                set_rlimit(libc::RLIMIT_FSIZE, RLIMIT_FSIZE_BYTES)?;
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Could not launch the sandboxed process: {e}"))?;
        let pid = child.id().ok_or("sandboxed process has no pid")? as i32;

        let quota_hit = Arc::new(AtomicBool::new(false));
        let watchdog = spawn_disk_watchdog(
            workspace,
            scratch,
            spec.disk_quota_bytes,
            pid,
            quota_hit.clone(),
        );

        let stdout_pipe = child.stdout.take().expect("piped stdout");
        let stderr_pipe = child.stderr.take().expect("piped stderr");

        let run = async {
            let (stdout, stderr) = tokio::join!(read_capped(stdout_pipe), read_capped(stderr_pipe));
            let status = child.wait().await;
            (status, stdout, stderr)
        };

        let outcome = tokio::time::timeout(spec.timeout, run).await;
        watchdog.abort();

        match outcome {
            Ok((status, stdout, stderr)) => {
                let status = status.map_err(|e| e.to_string())?;
                Ok(SandboxedOutput {
                    exit_code: status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                    disk_quota_exceeded: quota_hit.load(Ordering::SeqCst),
                })
            }
            Err(_elapsed) => {
                kill_process_group(pid);
                let _ = child.wait().await;
                Ok(SandboxedOutput {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: true,
                    disk_quota_exceeded: quota_hit.load(Ordering::SeqCst),
                })
            }
        }
    }
}

/// The `RLIMIT_NPROC` value to hand `pre_exec`, or `None` to leave the OS
/// default in place. Computed as the account's current ambient process
/// count (system-wide, matching what the kernel actually checks against)
/// plus [`RLIMIT_NPROC_HEADROOM`], clamped to the account's own hard
/// ceiling — `setrlimit` can't raise `rlim_max` without root, and asking for
/// more than that would just fail the whole spawn.
async fn nproc_cap_with_headroom() -> Option<u64> {
    let ambient = ambient_process_count().await?;
    let cap = ambient.saturating_add(RLIMIT_NPROC_HEADROOM);
    Some(match nproc_hard_limit() {
        Some(hard) => cap.min(hard),
        None => cap,
    })
}

/// Count of processes currently running system-wide under this account's
/// real UID — the same population `setrlimit(RLIMIT_NPROC, ...)` checks
/// future `fork()` calls against on Darwin/BSD. `None` if `ps` couldn't be
/// run or produced no parseable output; callers must treat that as "unknown"
/// rather than "zero".
async fn ambient_process_count() -> Option<u64> {
    let uid = unsafe { libc::getuid() }.to_string();
    let output = tokio::process::Command::new("/bin/ps")
        .args(["-axo", "ruid="])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let count = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.trim() == uid)
        .count();
    if count == 0 {
        return None; // couldn't parse `ps`'s output; don't trust a bogus zero
    }
    Some(count as u64)
}

/// This account's current hard ceiling for `RLIMIT_NPROC` (`rlim_max`), or
/// `None` if it couldn't be read.
fn nproc_hard_limit() -> Option<u64> {
    let mut rl = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NPROC, &mut rl) } != 0 {
        return None;
    }
    if rl.rlim_max == libc::RLIM_INFINITY {
        return None;
    }
    Some(rl.rlim_max)
}

fn set_rlimit(resource: libc::c_int, limit: u64) -> std::io::Result<()> {
    let rl = libc::rlimit {
        rlim_cur: limit as libc::rlim_t,
        rlim_max: limit as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(resource, &rl) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Send `SIGKILL` to the process group (and, since the child called
/// `setsid()`, its session) led by `pid`. Reaps a `sleep 999 &`-backgrounded
/// grandchild that stays in the same group/session as the sandboxed parent.
fn kill_process_group(pid: i32) {
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

fn spawn_disk_watchdog(
    workspace: std::path::PathBuf,
    scratch: std::path::PathBuf,
    quota_bytes: u64,
    pid: i32,
    quota_hit: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(DISK_WATCHDOG_INTERVAL).await;
            let size = dir_size(&workspace).saturating_add(dir_size(&scratch));
            if size > quota_bytes {
                quota_hit.store(true, Ordering::SeqCst);
                kill_process_group(pid);
                break;
            }
        }
    })
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

async fn read_capped(mut pipe: impl tokio::io::AsyncRead + Unpin) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match pipe.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < MAX_CAPTURED_BYTES {
                    buf.extend_from_slice(&chunk[..n]);
                }
                // Keep draining even past the cap so a chatty process can't
                // block forever on a full pipe buffer while we've stopped
                // storing its output.
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sandbox::SandboxSpec;

    fn spec(tmp: &std::path::Path) -> SandboxSpec {
        let workspace = tmp.join("workspace");
        let scratch = tmp.join("scratch");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let mut spec = SandboxSpec::new(workspace, scratch);
        spec.timeout = Duration::from_secs(10);
        spec
    }

    #[tokio::test]
    async fn runs_a_simple_command_and_captures_stdout() {
        let tmp = tempfile::tempdir().unwrap();
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/echo"),
                &["hello".to_string()],
                &spec(tmp.path()),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout.trim(), "hello");
        assert!(!out.timed_out);
    }

    #[tokio::test]
    async fn denies_network_access() {
        let tmp = tempfile::tempdir().unwrap();
        // curl is in the exec allowlist (it lives under /usr/bin), so this
        // exercises the profile's `deny network*`, not a missing-binary
        // false negative.
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/usr/bin/curl"),
                &[
                    "-sS".to_string(),
                    "--max-time".to_string(),
                    "3".to_string(),
                    "http://example.com".to_string(),
                ],
                &spec(tmp.path()),
            )
            .await
            .unwrap();
        assert_ne!(out.exit_code, Some(0), "network access must be denied");
    }

    #[tokio::test]
    async fn confines_writes_to_workspace_and_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(tmp.path());
        let outside = tmp.path().join("outside.txt");
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/bash"),
                &[
                    "-c".to_string(),
                    format!("echo nope > {}", outside.display()),
                ],
                &s,
            )
            .await
            .unwrap();
        assert_ne!(
            out.exit_code,
            Some(0),
            "a write outside workspace/scratch must fail"
        );
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn allows_writes_inside_the_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(tmp.path());
        let inside = s.workspace.join("ok.txt");
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/bash"),
                &["-c".to_string(), format!("echo yes > {}", inside.display())],
                &s,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(std::fs::read_to_string(&inside).unwrap().trim(), "yes");
    }

    #[tokio::test]
    async fn a_script_written_into_the_workspace_can_be_executed_directly() {
        // Regression test for a real bug: the exec allowlist originally
        // only covered system dirs (/bin, /usr/bin, Homebrew paths), not
        // WORKSPACE/SCRATCH, so a script the agent itself wrote into its own
        // workspace (e.g. `write_file` then `run_shell: ./foo.sh`) could
        // never be executed -- it failed with "bad interpreter: Operation
        // not permitted" even though /bin/bash itself was exec-allowed,
        // because the denial hits the *script's own* path, not the
        // interpreter it's forwarded to.
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(tmp.path());
        let script = s.workspace.join("hello.sh");
        std::fs::write(&script, "#!/bin/bash\necho hello-from-a-workspace-script\n").unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(&script, perms).unwrap();

        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/bash"),
                &["-c".to_string(), "./hello.sh".to_string()],
                &s,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
        assert_eq!(out.stdout.trim(), "hello-from-a-workspace-script");
    }

    /// Read is broad (see shell.sb's doc comment for why — dyld needs it to
    /// boot at all on current macOS), so the confinement that actually
    /// matters is the explicit deny on every user's home directory,
    /// overridden back to allowed only for the configured
    /// workspace/scratch. Exercised against the real `$HOME`, not a temp
    /// dir, since temp dirs don't live under `/Users` and wouldn't
    /// otherwise touch this rule at all.
    #[tokio::test]
    async fn denies_reading_a_sibling_file_in_the_real_home_directory() {
        let Ok(home) = std::env::var("HOME") else {
            return; // no HOME in this environment; nothing to verify
        };
        let secret = std::path::PathBuf::from(&home).join(".openpal-agent-sandbox-test-secret");
        std::fs::write(&secret, "top secret").unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/cat"),
                &[secret.to_string_lossy().into_owned()],
                &spec(tmp.path()),
            )
            .await
            .unwrap();
        let _ = std::fs::remove_file(&secret);
        assert_ne!(
            out.exit_code,
            Some(0),
            "a file in the real home directory, outside the configured workspace, must not be readable"
        );
    }

    #[tokio::test]
    async fn a_real_homebrew_or_system_binary_executes() {
        let tmp = tempfile::tempdir().unwrap();
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/usr/bin/env"),
                &["true".to_string()],
                &spec(tmp.path()),
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
    }

    #[tokio::test]
    async fn command_text_never_appears_in_argv() {
        // The caller contract (builtin/shell.rs) writes command text to a
        // scratch file and execs the interpreter against that file's path,
        // never `-c "<text>"`. Verify the sandboxed process's own argv (as
        // seen by `ps`) doesn't contain a marker we'd only see if it were
        // passed on the command line.
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(tmp.path());
        let marker = "SECRET_MARKER_SHOULD_NOT_APPEAR_IN_ARGV";
        let script = s.scratch.join(".cmd");
        std::fs::write(&script, format!("echo {marker}\nsleep 1\n")).unwrap();
        let mut child = tokio::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-D")
            .arg(format!("WORKSPACE={}", s.workspace.display()))
            .arg("-D")
            .arg(format!("SCRATCH={}", s.scratch.display()))
            .arg("-f")
            .arg({
                let p = s.scratch.join(".profile.sb");
                std::fs::write(&p, SHELL_PROFILE).unwrap();
                p
            })
            .arg("--")
            .arg("/bin/bash")
            .arg("--noprofile")
            .arg("--norc")
            .arg(&script)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Give it a moment to actually be running, then inspect argv.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let pid = child.id().unwrap();
        let ps = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .unwrap();
        let argv = String::from_utf8_lossy(&ps.stdout);
        assert!(
            !argv.contains(marker),
            "command text must not be visible in argv via ps: {argv}"
        );
        let _ = child.kill().await;
    }

    #[tokio::test]
    async fn a_script_that_forks_to_run_an_external_command_succeeds() {
        // Regression test: an earlier absolute `RLIMIT_NPROC = 64` made any
        // in-sandbox fork fail with "fork: Resource temporarily unavailable"
        // on a real machine, since `RLIMIT_NPROC` counts the account's
        // ambient processes system-wide -- true even for a trivial one-line
        // script, because `run_shell` always execs the user's command via
        // `bash script_path`, and bash forks to run any *external* command.
        // Invoking `/bin/echo` by absolute path forces that fork (bash's
        // `echo` builtin would not), so this exercises the exact path the
        // bug hit -- a builtin-only script would not have caught it.
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(tmp.path());
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/bash"),
                &[
                    "-c".to_string(),
                    "/bin/echo hello-from-a-forked-child".to_string(),
                ],
                &s,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout.trim(), "hello-from-a-forked-child");
    }

    #[tokio::test]
    async fn nproc_cap_always_clears_the_accounts_current_ambient_process_count() {
        // The exact invariant the real bug violated: a fixed absolute cap
        // was far below a real machine's ambient process count, so every
        // fork() inside the sandbox failed immediately. The computed cap
        // must always clear *current* usage by a safe margin, on whatever
        // machine the test runs on.
        let ambient = ambient_process_count()
            .await
            .expect("ps should be available in the test environment");
        let cap = nproc_cap_with_headroom()
            .await
            .expect("cap should be computable in the test environment");
        assert!(
            cap > ambient,
            "cap ({cap}) must exceed ambient usage ({ambient})"
        );
    }

    #[tokio::test]
    async fn rlimit_nproc_inside_the_sandbox_clears_ambient_usage_but_stays_bounded() {
        // Verifies the actual applied rlimit directly (via the sandboxed
        // process's own `ulimit -u`), rather than inferring it from a fork
        // bomb's behavior. A classic pipe-and-background bomb turned out to
        // be a bad proxy for this: its live-process count oscillates in a
        // noisy, CPU-speed-dependent equilibrium (bash retries `fork()` on
        // EAGAIN rather than giving up), so "does it finish well before an
        // N-second timeout" is nondeterministic in a way that has nothing to
        // do with whether the rlimit itself is correct -- it flaked on CI at
        // headroom values both larger and smaller than what eventually
        // shipped. This test instead checks the one thing that's actually
        // deterministic: the cap set inside the sandbox must exceed ambient
        // usage (or ordinary scripts fail to fork, the original bug) while
        // staying far below the account's hard ceiling (or the fork-bomb
        // backstop is meaningless).
        let tmp = tempfile::tempdir().unwrap();
        let s = spec(tmp.path());
        let ambient_before = ambient_process_count()
            .await
            .expect("ps should be available in the test environment");
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/bash"),
                &["-c".to_string(), "ulimit -u".to_string()],
                &s,
            )
            .await
            .unwrap();
        assert_eq!(out.exit_code, Some(0), "stderr: {}", out.stderr);
        let reported: u64 = out
            .stdout
            .trim()
            .parse()
            .expect("ulimit -u should print a plain number");
        assert!(
            reported > ambient_before,
            "sandboxed RLIMIT_NPROC ({reported}) must clear ambient usage \
             ({ambient_before}), or an ordinary script's first fork fails"
        );
        assert!(
            reported < ambient_before + 10_000,
            "sandboxed RLIMIT_NPROC ({reported}) is unreasonably high above \
             ambient usage ({ambient_before}), defeating the fork-bomb backstop"
        );
    }

    #[tokio::test]
    async fn disk_quota_kills_before_timeout() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = spec(tmp.path());
        s.disk_quota_bytes = 2 * 1024 * 1024; // 2 MB
        s.timeout = Duration::from_secs(20);
        let out = SeatbeltSandbox
            .run(
                &SandboxProfile::Shell,
                Path::new("/bin/bash"),
                &[
                    "-c".to_string(),
                    format!(
                        "dd if=/dev/zero of={}/fill bs=1m count=200 2>/dev/null",
                        s.workspace.display()
                    ),
                ],
                &s,
            )
            .await
            .unwrap();
        assert!(
            out.disk_quota_exceeded || !out.timed_out,
            "either the quota watchdog caught it, or dd finished on its own"
        );
        let written = dir_size(&s.workspace);
        assert!(
            written < 50 * 1024 * 1024,
            "the quota watchdog should have killed the writer well before 50MB, got {written} bytes"
        );
    }
}
