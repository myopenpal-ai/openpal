//! `build`: produce the .app and .dmg via the Tauri CLI.
//!
//! Behavior contract:
//! 1. Preconditions (fail with the exact fix command):
//!    - engine staged (paths::engine_binary().exists()) → "run: cargo run -p
//!      openpal-bundler -- fetch-engine"
//!    - icons exist (crates/app/icons/icon.icns) → "run: ... -- icon"
//!    - repo-root node_modules exists → "run: npm install"
//!    - ui/node_modules exists → "run: npm --prefix ui install"
//! 2. Re-stage built-in plugins (`stage_plugins::run()`) — cheap local copy,
//!    always fresh, no "already staged" precondition needed.
//! 3. Run `npx --no-install tauri build` (add `--debug` when requested) with
//!    current_dir = paths::app_crate_dir(), inheriting stdio so the user sees
//!    real progress. (The Tauri CLI discovers tauri.conf.json in the cwd; the
//!    beforeBuildCommand builds the frontend.)
//! 3. On success, list what was produced from paths::bundle_dir(debug) and
//!    target/<profile>/bundle/dmg — print absolute paths.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context};

use crate::paths;

pub fn run(debug: bool) -> anyhow::Result<()> {
    let root = paths::repo_root();

    if !paths::engine_binary().exists() {
        bail!("engine is not staged — run: cargo run -p openpal-bundler -- fetch-engine");
    }
    if !root.join("crates/app/icons/icon.icns").exists() {
        bail!("app icons are missing — run: cargo run -p openpal-bundler -- icon");
    }
    if !root.join("node_modules").is_dir() {
        bail!("repo-root node_modules is missing — run: npm install");
    }
    if !root.join("ui/node_modules").is_dir() {
        bail!("ui/node_modules is missing — run: npm --prefix ui install");
    }

    crate::stage_plugins::run()?;

    let profile = if debug { "debug" } else { "release" };
    println!("Building {profile} bundle via the Tauri CLI…");
    let mut cmd = crate::node::node_cli("npx");
    cmd.args(["--no-install", "tauri", "build"]);
    if debug {
        cmd.arg("--debug");
    }
    let status = cmd
        .current_dir(paths::app_crate_dir())
        .status()
        .context("failed to run npx — run: npm install (at the repo root)")?;
    if !status.success() {
        bail!("tauri build failed — see the output above");
    }

    println!("\nBuild artifacts:");
    let mut found = list_artifacts(&paths::bundle_dir(debug));
    found |= list_artifacts(&root.join("target").join(profile).join("bundle/dmg"));
    if !found {
        println!("  (none found — check the tauri output above)");
    }
    Ok(())
}

/// Print every entry of `dir` as an absolute path; returns whether any exist.
fn list_artifacts(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    let mut found = false;
    for entry in entries.flatten() {
        println!("  {}", entry.path().display());
        found = true;
    }
    found
}
