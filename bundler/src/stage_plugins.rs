//! `stage-plugins` (invoked automatically by `build`): copy the repo's
//! built-in plugin sources into `crates/app/resources/plugins/` so packaged
//! (non-portable) builds can discover them — mirrors how `fetch-engine`
//! stages the llama.cpp binaries into `crates/app/resources/llama/`, and
//! fixes a pre-existing gap: without this, `resolve_plugin_dirs`'
//! `resource_dir/plugins` and `resource_dir/resources/plugins` candidates
//! (crates/app/src/state.rs:302-305) are always empty in a release build, so
//! no built-in plugin (web-search, speech, hf-browser, smart-model-router) is ever
//! discovered.
//!
//! Behavior contract:
//! 1. Wipe `crates/app/resources/plugins/` (preserve `.gitkeep`) — this is a
//!    plain local copy, not a versioned download, so there is no "already
//!    staged" fast path to maintain; always re-stage from the source tree.
//! 2. For every direct subdirectory of the repo's `plugins/` dir that
//!    contains a `plugin.json`, copy it recursively into the staging dir,
//!    skipping directories/files never needed at runtime (`__pycache__`,
//!    `.pytest_cache`, `.venv`, `venv`, `tests`, `.git`, `scratch`, `*.pyc`,
//!    `*.log`). The plugin framework treats the plugin dir as read-only (all
//!    mutable state lives under `<app-data>/plugin-data/<id>/`), so copying
//!    the source tree verbatim minus dev/test cruft is safe.
//! 3. Print what was staged.
//!
//! Portable builds intentionally do NOT get built-in plugins (see
//! docs/design/PORTABLE.md — the Python plugins need a host Python + network).
//! `tauri.portable.conf.json` explicitly overrides `bundle.resources` to an
//! engine-only mapping so the Linux AppImage build (the one portable path
//! that runs Tauri's real resource bundler) never picks up whatever happens
//! to be staged here from an earlier `build` run on the same machine.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context};

use crate::paths;

const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    ".pytest_cache",
    ".venv",
    "venv",
    "tests",
    ".git",
    "scratch",
];
const SKIP_FILE_SUFFIXES: &[&str] = &[".pyc", ".log"];

pub fn run() -> anyhow::Result<()> {
    let src_root = paths::repo_root().join("plugins");
    let dest_root = paths::plugins_staging_dir();

    if !src_root.is_dir() {
        bail!("no plugins/ directory found at {}", src_root.display());
    }

    if dest_root.exists() {
        for entry in fs::read_dir(&dest_root)? {
            let entry = entry?;
            if entry.file_name() == ".gitkeep" {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path)?;
            } else {
                fs::remove_file(&path)?;
            }
        }
    } else {
        fs::create_dir_all(&dest_root)
            .with_context(|| format!("failed to create {}", dest_root.display()))?;
    }

    let mut staged_plugins: Vec<String> = Vec::new();
    let mut staged_files = 0usize;
    let mut staged_bytes = 0u64;

    for entry in fs::read_dir(&src_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !path.join("plugin.json").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let dest = dest_root.join(&name);
        let (files, bytes) = copy_plugin_dir(&path, &dest)?;
        staged_plugins.push(name);
        staged_files += files;
        staged_bytes += bytes;
    }

    if staged_plugins.is_empty() {
        bail!(
            "no plugin.json found under any subdirectory of {}",
            src_root.display()
        );
    }

    staged_plugins.sort();
    println!(
        "Staged {} plugin(s): {} ({staged_files} files, {:.1} KB) into {}",
        staged_plugins.len(),
        staged_plugins.join(", "),
        staged_bytes as f64 / 1_000.0,
        dest_root.display(),
    );
    Ok(())
}

fn copy_plugin_dir(src: &Path, dest: &Path) -> anyhow::Result<(usize, u64)> {
    fs::create_dir_all(dest).with_context(|| format!("failed to create {}", dest.display()))?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().to_string();
        if SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        if SKIP_FILE_SUFFIXES.iter().any(|suf| name.ends_with(suf)) {
            continue;
        }
        let path = entry.path();
        let dest_path = dest.join(&file_name);
        if path.is_dir() {
            let (f, b) = copy_plugin_dir(&path, &dest_path)?;
            files += f;
            bytes += b;
        } else {
            bytes += fs::copy(&path, &dest_path)
                .with_context(|| format!("failed to copy {}", path.display()))?;
            files += 1;
        }
    }
    Ok((files, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_plugin_dir_skips_dev_cruft() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dest = tmp.path().join("dest");

        fs::create_dir_all(src.join("__pycache__")).unwrap();
        fs::write(src.join("__pycache__/mod.pyc"), b"cache").unwrap();
        fs::create_dir_all(src.join("tests")).unwrap();
        fs::write(src.join("tests/test_units.py"), b"test").unwrap();
        fs::write(src.join("service.log"), b"log line").unwrap();
        fs::write(src.join("plugin.json"), b"{}").unwrap();
        fs::write(src.join("service.py"), b"print('hi')").unwrap();

        let (files, bytes) = copy_plugin_dir(&src, &dest).unwrap();

        assert_eq!(files, 2, "only plugin.json and service.py should be copied");
        assert_eq!(bytes, b"{}".len() as u64 + b"print('hi')".len() as u64);
        assert!(dest.join("plugin.json").is_file());
        assert!(dest.join("service.py").is_file());
        assert!(!dest.join("__pycache__").exists());
        assert!(!dest.join("tests").exists());
        assert!(!dest.join("service.log").exists());
    }

    #[test]
    fn copy_plugin_dir_recurses_into_kept_subdirectories() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dest = tmp.path().join("dest");

        fs::create_dir_all(src.join("openpal_plugin")).unwrap();
        fs::write(src.join("openpal_plugin/__init__.py"), b"").unwrap();
        fs::write(src.join("openpal_plugin/server.py"), b"code").unwrap();

        let (files, _bytes) = copy_plugin_dir(&src, &dest).unwrap();

        assert_eq!(files, 2);
        assert!(dest.join("openpal_plugin/__init__.py").is_file());
        assert!(dest.join("openpal_plugin/server.py").is_file());
    }
}
