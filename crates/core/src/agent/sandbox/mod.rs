//! `Sandbox` — OS-native process confinement for `run_shell`/`run_code`.
//! macOS uses Seatbelt (`sandbox-exec`); other platforms get a stub that
//! reports the feature as unavailable rather than running unconfined.

use std::path::PathBuf;
use std::time::Duration;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::SeatbeltSandbox;

/// Which static Seatbelt profile to apply, and any per-profile binding.
#[derive(Debug, Clone)]
pub enum SandboxProfile {
    /// `shell.sb` — a broad exec allowlist across common binary dirs.
    Shell,
    /// `code.sb` — exec locked to exactly one resolved interpreter.
    Code { interpreter: PathBuf },
}

#[derive(Debug, Clone)]
pub struct SandboxSpec {
    /// The full jail root, bound into the profile as `WORKSPACE` — always the
    /// agent workspace root, regardless of which subdirectory the process
    /// actually runs in.
    pub workspace: PathBuf,
    /// The process's actual working directory — the workspace root or a
    /// validated descendant of it.
    pub cwd: PathBuf,
    /// Per-call scratch dir: holds the written command/code file and the
    /// generated Seatbelt profile; also the sandboxed process's `$HOME`/
    /// `$TMPDIR`, since it's the one writable location besides `workspace`.
    pub scratch: PathBuf,
    pub timeout: Duration,
    /// Combined size ceiling for `workspace` + `scratch`, polled while the
    /// process runs — catches "many small files" and disk-fill attempts a
    /// single-file `RLIMIT_FSIZE` wouldn't.
    pub disk_quota_bytes: u64,
}

impl SandboxSpec {
    pub fn new(workspace: PathBuf, scratch: PathBuf) -> Self {
        Self {
            cwd: workspace.clone(),
            workspace,
            scratch,
            timeout: Duration::from_secs(30),
            disk_quota_bytes: 500 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SandboxedOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub disk_quota_exceeded: bool,
}

/// Run `program` with `args` confined per `profile`/`spec`. `program`/`args`
/// must never carry secret-bearing text on argv (callers write command/code
/// text to a scratch file and pass its path instead) — the sandbox itself
/// doesn't enforce that, it's a caller contract.
// See `EngineBackend` for why `double_must_use` is allowed on `async_trait` traits.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Sandbox: Send + Sync {
    async fn run(
        &self,
        profile: &SandboxProfile,
        program: &std::path::Path,
        args: &[String],
        spec: &SandboxSpec,
    ) -> Result<SandboxedOutput, String>;
}

/// Stub used on platforms without a `Sandbox` implementation yet (everything
/// except macOS, for now). Returns `Unsupported` rather than ever running a
/// command unconfined — the Settings toggle for agent mode's exec tools must
/// be hidden, not merely disabled, wherever this is the platform sandbox.
pub struct NoSandbox;

#[async_trait::async_trait]
impl Sandbox for NoSandbox {
    async fn run(
        &self,
        _profile: &SandboxProfile,
        _program: &std::path::Path,
        _args: &[String],
        _spec: &SandboxSpec,
    ) -> Result<SandboxedOutput, String> {
        Err("Sandboxed command/code execution isn't available on this platform yet.".to_string())
    }
}

/// `true` on the platforms shipping a real sandbox today (macOS only).
pub fn platform_supports_sandbox() -> bool {
    cfg!(target_os = "macos")
}

pub fn platform_sandbox() -> Box<dyn Sandbox> {
    #[cfg(target_os = "macos")]
    {
        Box::new(SeatbeltSandbox)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(NoSandbox)
    }
}
