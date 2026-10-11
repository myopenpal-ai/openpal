//! Auto-update the llama.cpp engine binary itself, on its own signed channel
//! independent of full OpenPal releases — the engine ships far more often
//! than the app does (see the "Auto-update the llama.cpp engine" plan).
//!
//! Shares its trust chain and download mechanics with
//! [`crate::update_portable`]/[`crate::update`] (same minisign signing key,
//! same resumable/SHA-256-checked [`crate::download::download_file`]), but
//! everything here lands in its own writable sibling directory
//! (`data_dir/engine/<tag>/`) rather than ever touching the bundle's shipped
//! `Resources/llama/` — that path must stay untouched or it breaks macOS
//! code-signing/notarization. A downloaded engine only takes effect on the
//! next engine start (no forced restart), since it's a brand-new sibling
//! directory rather than an in-place overwrite of a path a running process
//! has open.
//!
//! llama.cpp release tags (`b10107`, `b10516`, ...) are plain incrementing
//! build numbers, not semver — [`is_engine_update_available`] compares them
//! as such rather than reusing [`crate::update`]'s `semver`-based logic.

use crate::download;
use crate::types::DownloadEvent;
use crate::update_portable;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// File name of the staged llama.cpp server binary — must stay in sync with
/// `crates/app/src/state.rs::ENGINE_BINARY`. Duplicated rather than shared
/// because this crate (`openpal-core`) can't depend on `crates/app`.
#[cfg(windows)]
const ENGINE_BINARY: &str = "llama-server.exe";
#[cfg(not(windows))]
const ENGINE_BINARY: &str = "llama-server";

const OFFLINE_MSG: &str =
    "Couldn't check for engine updates. Check your internet connection and try again.";
const UNPARSEABLE_MSG: &str =
    "The engine update check got an unexpected response. Please try again later.";
const EXTRACT_MSG: &str = "The downloaded engine update couldn't be unpacked. Please try again.";
const SMOKE_TEST_MSG: &str = "The downloaded engine update failed a startup check and was not \
                              installed. Please try again. If this keeps happening, please \
                              report it.";

/// One platform target's signed engine build, as published to the
/// `engine-latest` GitHub Release tag. `tag` is the llama.cpp release tag
/// this build was cut from (e.g. `b10516`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineEntry {
    pub tag: String,
    pub url: String,
    pub sha256: String,
    pub signature: String,
}

/// The engine-update manifest, keyed by the same bundler `--target` strings
/// as [`crate::update::Manifest::portable`] (`macos-arm64`, `macos-x64`,
/// `win-vulkan-x64`, `ubuntu-vulkan-x64`, `ubuntu-vulkan-arm64`, via
/// [`crate::update::portable_target_key`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineManifest {
    /// `#[serde(default)]` so a manifest published before this target has
    /// any engine builds yet (or with the key omitted entirely during
    /// initial rollout) parses as empty rather than failing outright.
    #[serde(default)]
    pub engine: HashMap<String, EngineEntry>,
}

/// The `current.json` pointer written by [`extract_and_install`] on success,
/// read back by [`resolve_current_engine`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CurrentEngine {
    tag: String,
    dir: String,
}

/// Fetch and parse the engine manifest at `endpoint`. Network/parse failures
/// become messages safe to show a non-technical user, mirroring
/// [`crate::update::fetch_manifest`].
pub async fn fetch_engine_manifest(
    client: &reqwest::Client,
    endpoint: &str,
) -> Result<EngineManifest> {
    let resp = client.get(endpoint).send().await.map_err(|e| {
        tracing::warn!("engine update manifest fetch failed: {e}");
        Error::Update(OFFLINE_MSG.to_string())
    })?;
    if !resp.status().is_success() {
        tracing::warn!("engine update manifest fetch returned {}", resp.status());
        return Err(Error::Update(OFFLINE_MSG.to_string()));
    }
    resp.json::<EngineManifest>().await.map_err(|e| {
        tracing::warn!("engine update manifest parse failed: {e}");
        Error::Update(UNPARSEABLE_MSG.to_string())
    })
}

/// `true` only when `candidate_tag` parses as a llama.cpp `b<N>` tag whose
/// `N` is strictly greater than `current_tag`'s. Fails closed: if *either*
/// tag fails to parse, returns `false` rather than guessing — including the
/// edge case where `current_tag` itself is malformed, since "current
/// version is unreadable" is a reason to stay put, not license to treat any
/// parseable candidate as newer.
pub fn is_engine_update_available(current_tag: &str, candidate_tag: &str) -> bool {
    match (parse_tag(current_tag), parse_tag(candidate_tag)) {
        (Some(current), Some(candidate)) => candidate > current,
        _ => false,
    }
}

/// Strip a leading `b`/`B` and parse the remainder as a plain build number —
/// llama.cpp tags (`b10107`, `b10516`, ...) aren't semver.
fn parse_tag(tag: &str) -> Option<u64> {
    tag.strip_prefix(['b', 'B']).unwrap_or(tag).parse().ok()
}

/// Download `entry`'s archive into `dest_dir` (created if needed), verifying
/// both its SHA-256 (via the existing resumable downloader) and its
/// minisign signature. Returns the path to the downloaded (not yet
/// extracted) archive. Mirrors
/// [`crate::update_portable::download_portable_update`] exactly, reusing
/// its `verify_signature` rather than duplicating the minisign logic.
pub async fn download_and_verify_engine(
    client: &reqwest::Client,
    entry: &EngineEntry,
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
            Error::Update("The engine manifest has no file name in its URL.".to_string())
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

    if let Err(e) = update_portable::verify_signature(&dest, &entry.signature, pubkey_b64) {
        let _ = fs::remove_file(&dest);
        return Err(e);
    }

    Ok(dest)
}

/// Extract `archive` (OpenPal's own canonical format for engine builds: a
/// *flat* zip — no wrapper directory — containing exactly the server binary
/// plus every matching shared library, produced by OpenPal's publishing
/// pipeline rather than a raw upstream release asset) into
/// `engine_updates_dir/<tag>/`, then smoke-test it before ever trusting it:
///
/// 1. Recreate `engine_updates_dir/<tag>/` fresh (removing any stale/partial
///    prior attempt).
/// 2. Extract the archive's contents directly into it.
/// 3. `chmod +x` the server binary (Unix only).
/// 4. Unconditionally strip any macOS quarantine attribute from the
///    directory before ever executing anything in it.
/// 5. Smoke-test: run the new binary with `--version`. A non-zero exit, or a
///    failure to spawn at all (e.g. a missing dylib), aborts here *without*
///    touching `current.json` — the extracted-but-unverified directory is
///    left in place (it'll be overwritten by the next attempt).
/// 6. Only on smoke-test success, atomically write `current.json` (write a
///    temp file, then `rename()` over the final path).
///
/// Returns the path to the new server binary.
pub fn extract_and_install(
    archive: &Path,
    engine_updates_dir: &Path,
    tag: &str,
) -> Result<PathBuf> {
    let target_dir = engine_updates_dir.join(tag);
    let _ = fs::remove_dir_all(&target_dir);
    fs::create_dir_all(&target_dir).map_err(Error::Io)?;

    let file = fs::File::open(archive).map_err(Error::Io)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| {
        tracing::warn!("engine update archive is not a valid zip: {e}");
        Error::Update(EXTRACT_MSG.to_string())
    })?;
    zip.extract(&target_dir).map_err(|e| {
        tracing::warn!("engine update archive extraction failed: {e}");
        Error::Update(EXTRACT_MSG.to_string())
    })?;

    let binary_path = target_dir.join(ENGINE_BINARY);
    set_executable(&binary_path);
    // Strip quarantine unconditionally before ever executing anything from
    // this directory, regardless of platform (a no-op off macOS).
    update_portable::strip_quarantine(&target_dir);

    match Command::new(&binary_path).arg("--version").status() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            tracing::warn!("engine update smoke test exited with {status}");
            return Err(Error::Update(SMOKE_TEST_MSG.to_string()));
        }
        Err(e) => {
            tracing::warn!("engine update smoke test failed to spawn: {e}");
            return Err(Error::Update(SMOKE_TEST_MSG.to_string()));
        }
    }

    write_current_json(engine_updates_dir, tag)?;

    Ok(binary_path)
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

/// Same atomic-write-then-rename discipline `download::download_file`
/// already uses for its own final file.
fn write_current_json(engine_updates_dir: &Path, tag: &str) -> Result<()> {
    let current = CurrentEngine {
        tag: tag.to_string(),
        dir: tag.to_string(),
    };
    let json = serde_json::to_vec_pretty(&current)?;
    let tmp_path = engine_updates_dir.join("current.json.tmp");
    let final_path = engine_updates_dir.join("current.json");
    fs::write(&tmp_path, &json).map_err(Error::Io)?;
    fs::rename(&tmp_path, &final_path).map_err(Error::Io)?;
    Ok(())
}

/// The engine to actually run: reads `current.json` under
/// `engine_updates_dir` and, if it exists, parses, *and* the binary it
/// points at still exists on disk, returns that path. Otherwise (missing or
/// corrupt `current.json`, or a `current.json` pointing at a since-deleted
/// binary) falls back to `bundled_fallback` verbatim. Never panics.
pub fn resolve_current_engine(
    engine_updates_dir: &Path,
    bundled_fallback: Option<&Path>,
) -> Option<PathBuf> {
    let installed = read_current_json(engine_updates_dir)
        .map(|current| engine_updates_dir.join(&current.dir).join(ENGINE_BINARY))
        .filter(|path| path.exists());

    installed.or_else(|| bundled_fallback.map(Path::to_path_buf))
}

fn read_current_json(engine_updates_dir: &Path) -> Option<CurrentEngine> {
    let bytes = fs::read(engine_updates_dir.join("current.json")).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Best-effort delete every immediate subdirectory of `engine_updates_dir`
/// other than `keep_tag`, freeing space from superseded engine builds.
/// Never errors or panics — a locked directory on Windows (e.g. an old
/// engine still mid-shutdown) is logged and skipped, not propagated.
pub fn prune_old_engines(engine_updates_dir: &Path, keep_tag: &str) {
    let entries = match fs::read_dir(engine_updates_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(
                "engine update: could not list {} to prune old engines: {e}",
                engine_updates_dir.display()
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == keep_tag || name == "current.json" {
            continue;
        }
        if let Err(e) = fs::remove_dir_all(&path) {
            tracing::warn!(
                "engine update: could not prune old engine at {}: {e}",
                path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- is_engine_update_available ----------------------------------------

    #[test]
    fn higher_tag_is_available() {
        assert!(is_engine_update_available("b10107", "b10516"));
    }

    #[test]
    fn equal_tag_is_not_available() {
        assert!(!is_engine_update_available("b10107", "b10107"));
    }

    #[test]
    fn lower_tag_is_not_available() {
        assert!(!is_engine_update_available("b10516", "b10107"));
    }

    #[test]
    fn malformed_candidate_tag_is_not_available() {
        assert!(!is_engine_update_available("b10107", "not-a-tag"));
    }

    #[test]
    fn malformed_current_tag_is_not_available() {
        // Documented edge case: an unreadable current tag does not make any
        // parseable candidate count as "available" — fail closed either way.
        assert!(!is_engine_update_available("not-a-tag", "b10107"));
    }

    #[test]
    fn uppercase_b_prefix_is_tolerated() {
        assert!(is_engine_update_available("B10107", "B10516"));
    }

    // -- EngineManifest -------------------------------------------------

    #[test]
    fn engine_manifest_missing_engine_key_parses_as_empty() {
        let manifest: EngineManifest = serde_json::from_str("{}").unwrap();
        assert!(manifest.engine.is_empty());
    }

    // -- resolve_current_engine ----------------------------------------

    #[test]
    fn resolve_current_engine_prefers_a_present_and_valid_current_json() {
        let dir = tempfile::tempdir().unwrap();
        let tag_dir = dir.path().join("b10516");
        fs::create_dir_all(&tag_dir).unwrap();
        fs::write(tag_dir.join(ENGINE_BINARY), b"stub").unwrap();
        fs::write(
            dir.path().join("current.json"),
            r#"{"tag":"b10516","dir":"b10516"}"#,
        )
        .unwrap();

        let fallback = dir.path().join("bundled-fallback");
        fs::write(&fallback, b"bundled").unwrap();

        let resolved = resolve_current_engine(dir.path(), Some(&fallback));

        assert_eq!(resolved, Some(tag_dir.join(ENGINE_BINARY)));
    }

    #[test]
    fn resolve_current_engine_falls_back_when_current_json_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join("bundled-fallback");
        fs::write(&fallback, b"bundled").unwrap();

        let resolved = resolve_current_engine(dir.path(), Some(&fallback));

        assert_eq!(resolved, Some(fallback));
    }

    #[test]
    fn resolve_current_engine_falls_back_when_current_json_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("current.json"), b"not json").unwrap();
        let fallback = dir.path().join("bundled-fallback");
        fs::write(&fallback, b"bundled").unwrap();

        let resolved = resolve_current_engine(dir.path(), Some(&fallback));

        assert_eq!(resolved, Some(fallback));
    }

    #[test]
    fn resolve_current_engine_falls_back_when_the_referenced_binary_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        // current.json points at a tag directory that was never created.
        fs::write(
            dir.path().join("current.json"),
            r#"{"tag":"b10516","dir":"b10516"}"#,
        )
        .unwrap();
        let fallback = dir.path().join("bundled-fallback");
        fs::write(&fallback, b"bundled").unwrap();

        let resolved = resolve_current_engine(dir.path(), Some(&fallback));

        assert_eq!(resolved, Some(fallback));
    }

    #[test]
    fn resolve_current_engine_returns_none_with_no_fallback_provided() {
        let dir = tempfile::tempdir().unwrap();

        let resolved = resolve_current_engine(dir.path(), None);

        assert_eq!(resolved, None);
    }

    // -- prune_old_engines ------------------------------------------------

    #[test]
    fn prune_old_engines_deletes_everything_but_the_kept_tag() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("b10107")).unwrap();
        fs::create_dir_all(dir.path().join("b10516")).unwrap();
        fs::write(dir.path().join("current.json"), b"{}").unwrap();

        prune_old_engines(dir.path(), "b10516");

        assert!(!dir.path().join("b10107").exists());
        assert!(dir.path().join("b10516").exists());
        assert!(dir.path().join("current.json").exists());
    }

    #[test]
    fn prune_old_engines_on_a_missing_directory_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        prune_old_engines(&dir.path().join("does-not-exist"), "b10516");
    }

    // -- extract_and_install ------------------------------------------------

    /// Build a flat zip (no wrapper directory) at `archive_path` containing
    /// one file named [`ENGINE_BINARY`] with `script` as its contents.
    #[cfg(unix)]
    fn build_flat_archive_with_binary(archive_path: &Path, script: &[u8]) {
        use std::io::Write;
        let file = fs::File::create(archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(ENGINE_BINARY, options).unwrap();
        zip.write_all(script).unwrap();
        zip.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn extract_and_install_does_not_write_current_json_when_the_smoke_test_fails() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("engine.zip");
        // A "binary" that exits non-zero for --version, same as a real
        // engine build that's missing a dylib or otherwise broken.
        build_flat_archive_with_binary(&archive, b"#!/bin/sh\nexit 1\n");
        let engine_updates_dir = dir.path().join("engine");

        let result = extract_and_install(&archive, &engine_updates_dir, "b10516");

        assert!(result.is_err());
        assert!(
            !engine_updates_dir.join("current.json").exists(),
            "current.json must never be written when the smoke test fails"
        );
    }

    #[cfg(unix)]
    #[test]
    fn extract_and_install_writes_current_json_when_the_smoke_test_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("engine.zip");
        build_flat_archive_with_binary(&archive, b"#!/bin/sh\nexit 0\n");
        let engine_updates_dir = dir.path().join("engine");

        let binary_path = extract_and_install(&archive, &engine_updates_dir, "b10516").unwrap();

        assert_eq!(
            binary_path,
            engine_updates_dir.join("b10516").join(ENGINE_BINARY)
        );
        let current: CurrentEngine =
            serde_json::from_slice(&fs::read(engine_updates_dir.join("current.json")).unwrap())
                .unwrap();
        assert_eq!(current.tag, "b10516");
        assert_eq!(current.dir, "b10516");
    }

    #[cfg(unix)]
    #[test]
    fn extract_and_install_removes_a_stale_prior_attempt_first() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("engine.zip");
        build_flat_archive_with_binary(&archive, b"#!/bin/sh\nexit 0\n");
        let engine_updates_dir = dir.path().join("engine");
        let stale = engine_updates_dir.join("b10516").join("leftover.txt");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, b"stale").unwrap();

        extract_and_install(&archive, &engine_updates_dir, "b10516").unwrap();

        assert!(!stale.exists(), "the stale prior attempt must be cleared");
    }
}
