//! `engine-manifest`: package, sign, and publish the currently-staged
//! llama.cpp engine (`crates/app/resources/llama/`, populated by
//! `bundler fetch-engine`) as an OpenPal-controlled artifact on the rolling
//! `engine-latest` GitHub Release — the channel `crates/core/src/engine_update.rs`
//! (the running app's in-app engine updater) downloads from, independent of
//! full OpenPal releases and their own `latest.json`/`manifest` command.
//!
//! Mirrors `bundler/src/manifest.rs` closely (same signing invocation, same
//! SHA-256 computation, same "download-and-merge existing manifest, don't
//! overwrite" pattern) and reuses several of its helpers directly
//! (`sign`, `sha256_hex`, `require_env`, `ensure_gh`, `fetch_existing_manifest`,
//! `REPO`) rather than duplicating security-sensitive signing code.
//!
//! Two deliberate differences from `manifest`:
//! - The archive is repackaged **flat** (server binary + libraries directly
//!   at the zip root, no wrapper directory) rather than reusing whatever
//!   layout the upstream/portable artifact happened to have — the runtime
//!   extractor (`crates/core/src/update_portable.rs`'s `zip` crate, used
//!   read-only) is kept simple by never having to hunt for a nested unit.
//! - `engine-latest` is created **non-draft** immediately (unlike a
//!   version-tag release, which starts as a draft): it's a rolling channel,
//!   not a soft-launch gate for a specific human-reviewed release.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde_json::{Map, Value};

use crate::manifest;
use crate::paths;

/// Fixed rolling release tag engine builds publish to — NOT a version tag
/// (there's no per-engine-release versioning at the GitHub Release level;
/// the manifest's per-target `tag` field, e.g. "b10516", is what the app
/// actually compares). Analogous in spirit to Docker's `:latest` tag.
const ENGINE_RELEASE_TAG: &str = "engine-latest";

/// File name of the published manifest asset, mirroring `latest.json`'s role
/// for the app updater.
const ENGINE_MANIFEST_ASSET: &str = "engine-manifest.json";

pub fn run(tag: Option<&str>, target: &str) -> anyhow::Result<()> {
    manifest::require_env("TAURI_SIGNING_PRIVATE_KEY")?;
    manifest::require_env("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")?;
    manifest::ensure_gh()?;

    let engine_dir = paths::engine_dir();
    let resolved_tag = resolve_tag(tag, &engine_dir.join("engine-version.json"))?;
    let members = archive_members(&engine_dir)?;
    println!(
        "Packaging {} engine file(s) for {target} ({resolved_tag})…",
        members.len()
    );

    let archive_name = format!("openpal-engine-{resolved_tag}-{target}.zip");
    let archive_path = paths::repo_root().join("target").join(&archive_name);
    zip_flat(&engine_dir, &members, &archive_path)?;

    println!("Signing engine archive for {target}…");
    let signature = manifest::sign(&archive_path)?;
    let sha256 = manifest::sha256_hex(&archive_path)?;

    println!("Fetching existing {ENGINE_MANIFEST_ASSET}…");
    let existing = manifest::fetch_existing_manifest(ENGINE_RELEASE_TAG, ENGINE_MANIFEST_ASSET)
        .unwrap_or_default();
    let url = format!(
        "https://github.com/{}/releases/download/{ENGINE_RELEASE_TAG}/{archive_name}",
        manifest::REPO
    );
    let entry = engine_entry(&resolved_tag, &url, &sha256, &signature);
    let merged = merge_engine_entry(existing, target, entry);

    let manifest_path = paths::repo_root()
        .join("target")
        .join(ENGINE_MANIFEST_ASSET);
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&Value::Object(merged))? + "\n",
    )
    .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    println!("Publishing to {ENGINE_RELEASE_TAG}…");
    publish(&[archive_path, manifest_path])?;

    println!(
        "\n✓ Published engine {resolved_tag} ({target}) to {ENGINE_RELEASE_TAG} ({}).",
        manifest::REPO
    );
    Ok(())
}

/// Resolve the tag being published: an explicit `--tag` always wins (it's
/// documented as a sanity-check/override), falling back to whatever
/// `fetch-engine` last recorded in `version_file` (engine-version.json).
/// Errors only when neither is available.
fn resolve_tag(explicit: Option<&str>, version_file: &Path) -> anyhow::Result<String> {
    let staged = fs::read_to_string(version_file)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("tag").and_then(Value::as_str).map(str::to_string));

    match (explicit, staged) {
        (Some(t), Some(staged)) if t != staged => {
            println!(
                "⚠ --tag {t} does not match the staged tag \"{staged}\" in {} — publishing {t} as instructed.",
                version_file.display()
            );
            Ok(t.to_string())
        }
        (Some(t), _) => Ok(t.to_string()),
        (None, Some(staged)) => Ok(staged),
        (None, None) => bail!(
            "no --tag given and {} is missing or unreadable — run `bundler fetch-engine` first, or pass --tag explicitly",
            version_file.display()
        ),
    }
}

/// Every file staged in `dir` except the bookkeeping files `fetch-engine`
/// writes/preserves (`engine-version.json`, `.gitkeep`) — by the time this
/// runs, the engine dir should hold exactly the server binary and its
/// matching shared libraries, so no further per-OS filtering is needed
/// (unlike `fetch::artifact_names`/`is_engine_library`, which have to pick
/// those out of a much larger upstream release tree).
fn archive_members(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut members = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "engine-version.json" || name == ".gitkeep" {
            continue;
        }
        if entry.path().is_file() {
            members.push(entry.path());
        }
    }
    if members.is_empty() {
        bail!(
            "no engine files staged in {} — run `bundler fetch-engine` first",
            dir.display()
        );
    }
    members.sort();
    Ok(members)
}

/// Build a flat zip at `out` containing exactly `members`, each stored under
/// its bare file name at the archive root — deliberately NOT wrapped in a
/// directory (unlike `portable::zip_stage`, which keeps one on purpose for
/// the app bundle). `members` must all live directly inside `dir`.
///
/// `bundler` is a dev/CI-only tool, so shelling out is fine; a zip-writing
/// Rust crate isn't already a dependency here (`crates/core` only reads zips,
/// via the `zip` crate, for its own update extraction). Windows' in-box
/// `tar.exe` (bsdtar) infers the zip format from the `.zip` suffix — the
/// same trick `portable::zip_stage` already relies on for Windows — while
/// macOS and Linux both ship Info-ZIP `zip`, whose `-j` ("junk paths") flag
/// stores each member at the archive root regardless of the path passed in.
fn zip_flat(dir: &Path, members: &[PathBuf], out: &Path) -> anyhow::Result<()> {
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let _ = fs::remove_file(out);

    let names = members
        .iter()
        .map(|m| {
            m.file_name()
                .with_context(|| format!("{} has no file name", m.display()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let status = if cfg!(target_os = "windows") {
        let mut cmd = Command::new("tar");
        cmd.arg("-a")
            .arg("-c")
            .arg("-f")
            .arg(out)
            .arg("-C")
            .arg(dir);
        cmd.args(&names);
        cmd.status().context("failed to run tar")?
    } else {
        let mut cmd = Command::new("zip");
        cmd.args(["-q", "-j"]).arg(out);
        for name in &names {
            cmd.arg(dir.join(name));
        }
        cmd.status()
            .context("failed to run zip — is it installed and on PATH?")?
    };
    if !status.success() {
        bail!(
            "failed to build the flat engine archive at {}",
            out.display()
        );
    }
    Ok(())
}

/// The JSON shape `crates/core/src/engine_update.rs`'s `EngineEntry` serde
/// type deserializes: `{"tag", "url", "sha256", "signature"}`, matching
/// `EngineEntry`/`PortableEntry`'s field names one-for-one.
fn engine_entry(tag: &str, url: &str, sha256: &str, signature: &str) -> Value {
    serde_json::json!({
        "tag": tag,
        "url": url,
        "sha256": sha256,
        "signature": signature,
    })
}

/// Update just `target`'s entry in an engine manifest's `engine` map,
/// leaving every other target's entry untouched — the whole point of
/// fetch-and-merge (mirrors `manifest::run`'s `portable` merge), so a
/// single-target publish never clobbers other platforms' still-good
/// entries.
fn merge_engine_entry(
    existing: Map<String, Value>,
    target: &str,
    entry: Value,
) -> Map<String, Value> {
    let mut manifest = existing;
    let mut engine = match manifest.get("engine") {
        Some(Value::Object(existing)) => existing.clone(),
        _ => Map::new(),
    };
    engine.insert(target.to_string(), entry);
    manifest.insert("engine".to_string(), Value::Object(engine));
    manifest
}

/// Ensure a **non-draft** release exists for `ENGINE_RELEASE_TAG` (creating
/// one if not — deliberately without `--draft`, unlike `manifest::publish`'s
/// version-tag releases: this is a rolling channel, not a soft-launch gate),
/// then upload `assets`, overwriting any same-named ones already there.
fn publish(assets: &[PathBuf]) -> anyhow::Result<()> {
    let exists = Command::new("gh")
        .args(["release", "view", ENGINE_RELEASE_TAG, "-R", manifest::REPO])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !exists {
        println!("No release exists yet for {ENGINE_RELEASE_TAG} — creating one.");
        let status = Command::new("gh")
            .args([
                "release",
                "create",
                ENGINE_RELEASE_TAG,
                "--title",
                ENGINE_RELEASE_TAG,
                "--notes",
                "Rolling llama.cpp engine channel for OpenPal's in-app engine updater",
                "-R",
                manifest::REPO,
            ])
            .status()
            .context("failed to run gh — is it installed and authenticated? (`gh auth login`)")?;
        if !status.success() {
            bail!("gh release create failed for {ENGINE_RELEASE_TAG}");
        }
    }

    let mut cmd = Command::new("gh");
    cmd.args([
        "release",
        "upload",
        ENGINE_RELEASE_TAG,
        "--clobber",
        "-R",
        manifest::REPO,
    ]);
    for asset in assets {
        cmd.arg(asset);
    }
    let status = cmd.status().context("failed to run gh release upload")?;
    if !status.success() {
        bail!("gh release upload failed for {ENGINE_RELEASE_TAG}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_tag_prefers_the_explicit_flag_over_the_staged_file() {
        let tmp = tempfile::tempdir().unwrap();
        let version_file = tmp.path().join("engine-version.json");
        fs::write(&version_file, r#"{"tag": "b10000"}"#).unwrap();

        assert_eq!(
            resolve_tag(Some("b10516"), &version_file).unwrap(),
            "b10516"
        );
    }

    #[test]
    fn resolve_tag_falls_back_to_the_staged_file_when_no_flag_given() {
        let tmp = tempfile::tempdir().unwrap();
        let version_file = tmp.path().join("engine-version.json");
        fs::write(&version_file, r#"{"tag": "b10000"}"#).unwrap();

        assert_eq!(resolve_tag(None, &version_file).unwrap(), "b10000");
    }

    #[test]
    fn resolve_tag_errors_when_neither_is_available() {
        let tmp = tempfile::tempdir().unwrap();
        let version_file = tmp.path().join("does-not-exist.json");

        assert!(resolve_tag(None, &version_file).is_err());
    }

    #[test]
    fn archive_members_excludes_bookkeeping_files() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("llama-server"), b"bin").unwrap();
        fs::write(tmp.path().join("libggml.dylib"), b"lib").unwrap();
        fs::write(tmp.path().join("engine-version.json"), b"{}").unwrap();
        fs::write(tmp.path().join(".gitkeep"), b"").unwrap();

        let members = archive_members(tmp.path()).unwrap();

        assert_eq!(
            members,
            vec![
                tmp.path().join("libggml.dylib"),
                tmp.path().join("llama-server"),
            ]
        );
    }

    #[test]
    fn archive_members_errors_when_nothing_is_staged() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(".gitkeep"), b"").unwrap();

        assert!(archive_members(tmp.path()).is_err());
    }

    #[test]
    fn engine_entry_matches_the_engine_update_json_shape() {
        let entry = engine_entry("b10516", "https://example.com/a.zip", "deadbeef", "sig==");
        assert_eq!(
            entry,
            serde_json::json!({
                "tag": "b10516",
                "url": "https://example.com/a.zip",
                "sha256": "deadbeef",
                "signature": "sig==",
            })
        );
    }

    #[test]
    fn merge_engine_entry_inserts_into_an_empty_manifest() {
        let merged = merge_engine_entry(
            Map::new(),
            "macos-arm64",
            engine_entry("b10516", "url", "sha", "sig"),
        );

        assert_eq!(
            merged.get("engine"),
            Some(&serde_json::json!({
                "macos-arm64": {
                    "tag": "b10516",
                    "url": "url",
                    "sha256": "sha",
                    "signature": "sig",
                }
            }))
        );
    }

    #[test]
    fn merge_engine_entry_leaves_other_targets_untouched() {
        let mut existing = Map::new();
        existing.insert(
            "engine".to_string(),
            serde_json::json!({
                "macos-arm64": {"tag": "b10000", "url": "old", "sha256": "old", "signature": "old"},
                "win-vulkan-x64": {"tag": "b10000", "url": "win", "sha256": "win", "signature": "win"},
            }),
        );

        let merged = merge_engine_entry(
            existing,
            "macos-arm64",
            engine_entry("b10516", "new", "new", "new"),
        );

        let engine = merged.get("engine").unwrap();
        assert_eq!(engine["macos-arm64"]["tag"], "b10516");
        assert_eq!(engine["macos-arm64"]["url"], "new");
        // The untouched target keeps its prior entry byte-for-byte.
        assert_eq!(engine["win-vulkan-x64"]["tag"], "b10000");
        assert_eq!(engine["win-vulkan-x64"]["url"], "win");
    }

    #[test]
    #[cfg(unix)]
    fn zip_flat_produces_a_flat_archive_with_no_wrapper_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("llama-server"), b"binary").unwrap();
        fs::write(src.join("libggml.dylib"), b"lib").unwrap();
        let out = tmp.path().join("out/engine.zip");

        zip_flat(
            &src,
            &[src.join("llama-server"), src.join("libggml.dylib")],
            &out,
        )
        .unwrap();

        assert!(out.exists());
        let listing = Command::new("unzip")
            .arg("-Z1")
            .arg(&out)
            .output()
            .expect("unzip must be installed to verify the archive layout");
        assert!(listing.status.success());
        let mut names: Vec<String> = String::from_utf8_lossy(&listing.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["libggml.dylib".to_string(), "llama-server".to_string()],
            "archive members must sit at the zip root, not inside a wrapper directory"
        );
    }
}
