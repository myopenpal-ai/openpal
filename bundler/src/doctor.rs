//! `doctor`: verify the build environment and print a friendly checklist.
//!
//! Checks (each prints a ✓/✗ line; returns Err only if a REQUIRED tool is
//! missing):
//! - required: cargo, node (>=18), npm, curl, unzip, xcode command line tools
//!   (`xcode-select -p` succeeds)
//! - required for `build`: the Tauri CLI is resolvable (`npx --no-install
//!   tauri --version` from the repo root after `npm install`); if missing,
//!   tell the user to run `npm install` at the repo root.
//! - optional (signing): `codesign` present, SIGNING_IDENTITY env set
//! - optional (notarization): `xcrun notarytool` present, APPLE_ID /
//!   APPLE_PASSWORD / APPLE_TEAM_ID env set
//! - engine: whether the engine is staged (paths::engine_binary().exists())
//!   and which version (engine-version.json) if so. This is the #1 cause of
//!   "the AI won't load" — a fresh checkout/worktree never has one, since
//!   `resources/llama/` is gitignored. Rather than just print the fix,
//!   `doctor` runs it: fetches the latest llama.cpp release for this host
//!   (see `fetch::default_target`) and stages it, same as
//!   `fetch-engine` would.
//! - orphaned engine processes (unix only): the #2 cause of "the AI won't
//!   load" once the engine IS staged — `EngineManager` spawns `llama-server`
//!   with `kill_on_drop`, but that only fires on a clean app exit. If the app
//!   crashes or is force-killed mid-dev-loop, its `llama-server` child is
//!   reparented to launchd (ppid 1) and keeps running — sometimes with a
//!   full model + KV cache resident — forever. A second one accumulates
//!   every crash, until the survivors' RAM usage OOM-kills every subsequent
//!   engine start (`signal: 9 (SIGKILL)` in the app log, no captured output).
//!   `doctor` finds every `llama-server` process whose parent is 1 (via
//!   `ps -axo pid=,ppid=,comm=` — a live engine's parent is always its
//!   openpal-app, never launchd) and kills it.

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};

use crate::{fetch, paths};

pub fn run() -> anyhow::Result<()> {
    println!("OpenPal bundler doctor");
    let mut missing: Vec<String> = Vec::new();

    println!("\nRequired:");
    check_required(
        &mut missing,
        "cargo",
        capture("cargo", &["--version"]),
        "install Rust via https://rustup.rs",
    );
    check_node(&mut missing);
    check_required(
        &mut missing,
        "npm",
        capture("npm", &["--version"]),
        "comes with Node — install via https://nodejs.org",
    );
    check_required(
        &mut missing,
        "curl",
        capture("curl", &["--version"]),
        "preinstalled on macOS; otherwise `brew install curl`",
    );
    check_required(
        &mut missing,
        "unzip",
        capture("unzip", &["-v"]),
        "preinstalled on macOS; otherwise `brew install unzip`",
    );
    match capture("xcode-select", &["-p"]) {
        Some(p) => println!("  ✓ Xcode command line tools — {p}"),
        None => {
            println!("  ✗ Xcode command line tools — run: xcode-select --install");
            missing.push("Xcode command line tools".to_string());
        }
    }
    match capture_in(
        &paths::repo_root(),
        "npx",
        &["--no-install", "tauri", "--version"],
    ) {
        Some(v) => println!("  ✓ Tauri CLI — {v}"),
        None => {
            println!("  ✗ Tauri CLI — run: npm install (at the repo root)");
            missing.push("Tauri CLI".to_string());
        }
    }

    println!("\nOptional — signing:");
    if tool_present("codesign", &["--version"]) {
        println!("  ✓ codesign — present");
    } else {
        println!("  ✗ codesign — install the Xcode command line tools: xcode-select --install");
    }
    check_env(
        "SIGNING_IDENTITY",
        "needed for `sign`, e.g. \"Developer ID Application: Your Name (TEAMID)\"",
    );

    println!("\nOptional — notarization:");
    if capture("xcrun", &["--find", "notarytool"]).is_some() {
        println!("  ✓ xcrun notarytool — present");
    } else {
        println!(
            "  ✗ xcrun notarytool — install the Xcode command line tools: xcode-select --install"
        );
    }
    check_env(
        "APPLE_ID",
        "needed for `notarize` (your Apple developer account email)",
    );
    check_env(
        "APPLE_PASSWORD",
        "needed for `notarize` (an app-specific password)",
    );
    check_env(
        "APPLE_TEAM_ID",
        "needed for `notarize` (your 10-character team ID)",
    );

    println!("\nEngine:");
    if paths::engine_binary().exists() {
        let tag = staged_engine_tag().unwrap_or_else(|| "unknown version".to_string());
        println!("  ✓ staged: {tag} ({})", paths::engine_dir().display());
    } else {
        println!("  · not staged — fetching now (this is why the AI won't load)…");
        fetch::run("latest", fetch::default_target(), false, None)
            .context("could not fetch the engine — the AI can't start without it")?;
    }

    println!("\nEngine processes:");
    kill_orphaned_engine_processes();

    if missing.is_empty() {
        println!("\nAll required tools are present.");
        Ok(())
    } else {
        println!();
        bail!(
            "missing required tools: {} — see the fixes above",
            missing.join(", ")
        )
    }
}

fn check_required(missing: &mut Vec<String>, name: &str, found: Option<String>, fix: &str) {
    match found {
        Some(v) => println!("  ✓ {name} — {v}"),
        None => {
            println!("  ✗ {name} — not found ({fix})");
            missing.push(name.to_string());
        }
    }
}

fn check_node(missing: &mut Vec<String>) {
    match capture("node", &["--version"]) {
        Some(v) => {
            let major: u32 = v
                .trim_start_matches('v')
                .split('.')
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
            if major >= 18 {
                println!("  ✓ node — {v} (>= 18)");
            } else {
                println!("  ✗ node — found {v}, need >= 18 (upgrade via https://nodejs.org)");
                missing.push("node >= 18".to_string());
            }
        }
        None => {
            println!("  ✗ node — not found (install via https://nodejs.org)");
            missing.push("node".to_string());
        }
    }
}

fn check_env(var: &str, hint: &str) {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => println!("  ✓ {var} is set"),
        _ => println!("  ✗ {var} not set — {hint}"),
    }
}

/// First line of stdout if the command runs successfully.
fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    first_line(&out.stdout)
}

fn capture_in(dir: &Path, cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    first_line(&out.stdout)
}

fn first_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let line = text.lines().find(|l| !l.trim().is_empty())?.trim();
    let mut line = line.to_string();
    if line.len() > 72 {
        line.truncate(72);
        line.push('…');
    }
    Some(line)
}

/// True if the binary exists on PATH (its exit status is irrelevant).
fn tool_present(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).output().is_ok()
}

/// Find every `llama-server` process reparented to launchd (ppid 1) —
/// a crashed/force-killed app's engine child, since a live engine's parent
/// is always its still-running openpal-app — and kill it. Unix only: `ps`'s
/// `pid=,ppid=,comm=` columns and "orphan reparents to pid 1" both assume a
/// Unix process model; Windows has neither, so this is a no-op there.
#[cfg(unix)]
fn kill_orphaned_engine_processes() {
    let out = match Command::new("ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => {
            println!("  ? could not list processes (ps failed) — skipping this check");
            return;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let orphans = find_orphaned_engine_pids(&text);

    if orphans.is_empty() {
        println!("  ✓ no orphaned llama-server processes");
        return;
    }

    for pid in &orphans {
        match Command::new("kill").args(["-9", pid]).status() {
            Ok(s) if s.success() => println!(
                "  ✓ killed orphaned llama-server (pid {pid}, was reparented to launchd — likely from a crashed app)"
            ),
            _ => println!("  ✗ failed to kill orphaned llama-server (pid {pid}) — try manually: kill -9 {pid}"),
        }
    }
}

#[cfg(not(unix))]
fn kill_orphaned_engine_processes() {
    println!("  · orphan-process check is unix-only — skipping");
}

/// Parse `ps -axo pid=,ppid=,comm=` output and return the pids of every
/// `llama-server` line whose ppid is 1 (reparented to launchd).
#[cfg(unix)]
fn find_orphaned_engine_pids(ps_output: &str) -> Vec<String> {
    ps_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?;
            let ppid = fields.next()?;
            let comm = fields.next()?;
            let is_llama_server = comm == "llama-server"
                || comm
                    .rsplit(['/', '\\'])
                    .next()
                    .is_some_and(|base| base == "llama-server");
            (ppid == "1" && is_llama_server).then(|| pid.to_string())
        })
        .collect()
}

fn staged_engine_tag() -> Option<String> {
    let raw = std::fs::read_to_string(paths::engine_dir().join("engine-version.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(v["tag"].as_str()?.to_string())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::find_orphaned_engine_pids;
    use super::first_line;

    #[cfg(unix)]
    #[test]
    fn find_orphaned_engine_pids_matches_ppid_1_llama_server_only() {
        let ps_output = "\
29122     1 /Users/me/Dev/openpal/target/debug/llama/llama-server
18005     1 /Users/me/worktrees/openpal-feature/target/debug/llama/llama-server
82274 81997 /Users/me/worktrees/openpal-feature/target/debug/openpal-app
  501     1 /sbin/launchd
 1234     1 llama-server
";
        assert_eq!(
            find_orphaned_engine_pids(ps_output),
            vec!["29122", "18005", "1234"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn find_orphaned_engine_pids_ignores_live_engine_child_of_a_running_app() {
        // A openpal-app that's still alive is the parent, not launchd — must
        // not be killed out from under an active dev session.
        let ps_output = "82274 81997 /Users/.../openpal-app\n90001 82274 /Users/.../llama-server\n";
        assert!(find_orphaned_engine_pids(ps_output).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn find_orphaned_engine_pids_handles_empty_and_malformed_lines() {
        assert!(find_orphaned_engine_pids("").is_empty());
        assert!(find_orphaned_engine_pids("garbage\n\nnot enough fields").is_empty());
    }

    #[test]
    fn first_line_skips_leading_blank_lines() {
        assert_eq!(
            first_line(b"\n\n  \ncargo 1.80.0 (abc123 2024-01-01)\nextra line"),
            Some("cargo 1.80.0 (abc123 2024-01-01)".to_string())
        );
    }

    #[test]
    fn first_line_is_none_for_all_blank_input() {
        assert_eq!(first_line(b"\n  \n\t\n"), None);
        assert_eq!(first_line(b""), None);
    }

    #[test]
    fn first_line_truncates_past_72_chars_with_an_ellipsis() {
        let long = "x".repeat(100);
        let result = first_line(long.as_bytes()).unwrap();
        assert_eq!(result.chars().count(), 73); // 72 chars + the ellipsis
        assert!(result.ends_with('…'));
        assert!(result.starts_with(&"x".repeat(72)));
    }

    #[test]
    fn first_line_leaves_short_lines_untouched() {
        assert_eq!(first_line(b"npm 10.2.0"), Some("npm 10.2.0".to_string()));
    }
}
