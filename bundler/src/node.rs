//! Spawning Node-provided CLIs (`npx`, `npm`) portably.
//!
//! On Windows these ship as `.cmd` shims (`npx.cmd`, `npm.cmd`) — there is no
//! `npx.exe`. Rust's `std::process::Command` resolves a bare, extensionless
//! program name by appending only `.exe` (it does not consult `PATHEXT`), so
//! `Command::new("npx")` fails to spawn on Windows. Route through `cmd /C` there
//! so the shim is found; on Unix the name resolves directly.

use std::process::Command;

/// A `Command` that runs a Node CLI (`npx`, `npm`) on any host. Append the
/// tool's own args with `.args(...)` as usual. Uses a runtime `cfg!(windows)`
/// check (compiled on every platform) so this module builds on macOS/Linux too.
pub fn node_cli(bin: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(bin);
        c
    } else {
        Command::new(bin)
    }
}
