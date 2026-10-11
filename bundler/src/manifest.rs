//! `manifest`: sign Portable release artifacts and publish/merge
//! `latest.json` — the file OpenPal's auto-updater checks (both SKUs; see
//! `crates/core/src/update.rs`). Shells out to the Tauri CLI (`npx tauri
//! signer sign`) for signing and `gh` for the GitHub Release, matching this
//! crate's existing "shell out to standard tooling" convention.
//!
//! Scope: this command only populates the manifest's `portable` block. The
//! Full SKU's `platforms` block (Tauri's own updater-plugin schema) is not
//! yet automated here — Full builds/publishes stay a manual, local step
//! (`bundler release`) per the accepted "Phase B2" plan; wiring its
//! `createUpdaterArtifacts` output into this same `latest.json` is a
//! follow-up, not a regression.
//!
//! Any `latest.json` already attached to the release is downloaded first and
//! merged into (not overwritten by), so a Portable-only CI run and a later
//! manual Full entry can coexist in one file without clobbering each other.
//!
//! A release created here (because `tag` doesn't have one yet) is a
//! **draft** — deliberately: `/releases/latest` never resolves to a draft or
//! pre-release, so publishing stays inert until a human promotes it
//! (`gh release edit <tag> --draft=false`), matching the plan's soft-launch
//! gate for the very first auto-updatable release.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use serde_json::{Map, Value};

use crate::paths;

/// The five Portable target strings this bundler knows how to produce (see
/// `.github/workflows/portable.yml`'s matrix and `portable::run`'s `target`
/// argument) — kept in sync by hand with
/// `openpal_core::update::portable_target_key`.
const PORTABLE_TARGETS: &[&str] = &[
    "macos-arm64",
    "macos-x64",
    "win-vulkan-x64",
    "ubuntu-vulkan-x64",
    "ubuntu-vulkan-arm64",
];

pub(crate) const REPO: &str = "myopenpal-ai/openpal";

pub fn run(tag: &str, portable_dir: Option<&Path>, notes: Option<&str>) -> anyhow::Result<()> {
    require_env("TAURI_SIGNING_PRIVATE_KEY")?;
    require_env("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")?;
    ensure_gh()?;

    let mut manifest = fetch_existing_manifest(tag, "latest.json").unwrap_or_default();
    manifest.insert(
        "version".to_string(),
        Value::String(tag.trim_start_matches('v').to_string()),
    );
    manifest.insert("pubDate".to_string(), Value::String(now_utc_rfc3339()?));
    if let Some(notes) = notes {
        manifest.insert("notes".to_string(), Value::String(notes.to_string()));
    } else {
        manifest
            .entry("notes".to_string())
            .or_insert_with(|| Value::String(String::new()));
    }

    let mut assets: Vec<PathBuf> = Vec::new();
    if let Some(dir) = portable_dir {
        let portable = sign_portable_artifacts(dir, tag, &mut assets)?;
        // Merge onto whatever `portable` object the existing manifest had,
        // rather than replacing it wholesale — a partial matrix run (e.g.
        // only macOS legs completed) must not blank out other platforms'
        // still-good entries from a prior successful run for this same tag.
        let mut merged = match manifest.get("portable") {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        merged.extend(portable);
        manifest.insert("portable".to_string(), Value::Object(merged));
    }

    let manifest_path = paths::repo_root().join("target/latest.json");
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        &manifest_path,
        serde_json::to_string_pretty(&Value::Object(manifest))?,
    )
    .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    assets.push(manifest_path);

    publish(tag, &assets)?;
    println!(
        "\n✓ Published {} file(s) to release {tag} ({REPO}).",
        assets.len()
    );
    Ok(())
}

/// Scan `dir` for `OpenPal-portable-<target>.{zip,AppImage}` files (one per
/// [`PORTABLE_TARGETS`] entry present — a partial matrix run just publishes
/// what it has), sign + checksum each, and return the entries to merge into
/// the manifest's `portable` object. Signed/checksummed files are appended
/// to `assets` for upload.
fn sign_portable_artifacts(
    dir: &Path,
    tag: &str,
    assets: &mut Vec<PathBuf>,
) -> anyhow::Result<Map<String, Value>> {
    let mut out = Map::new();
    for target in PORTABLE_TARGETS {
        let Some(artifact) = find_artifact(dir, target) else {
            continue;
        };
        let file_name = artifact
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("{} has no valid file name", artifact.display()))?
            .to_string();

        println!("Signing {file_name}…");
        let signature = sign(&artifact)?;
        let sha256 = sha256_hex(&artifact)?;

        out.insert(
            target.to_string(),
            serde_json::json!({
                "url": format!("https://github.com/{REPO}/releases/download/{tag}/{file_name}"),
                "sha256": sha256,
                "signature": signature,
            }),
        );
        assets.push(artifact);
    }
    if out.is_empty() {
        bail!(
            "no Portable artifacts found in {} matching a known target ({})",
            dir.display(),
            PORTABLE_TARGETS.join(", "),
        );
    }
    Ok(out)
}

/// The zip (mac/win) or bare `.AppImage` (Linux) for `target` in `dir` — the
/// exact names `bundler portable` produces (`portable::zip_stage` /
/// `build_linux`). Absent when that matrix leg didn't run or didn't upload.
fn find_artifact(dir: &Path, target: &str) -> Option<PathBuf> {
    for ext in ["zip", "AppImage"] {
        let candidate = dir.join(format!("OpenPal-portable-{target}.{ext}"));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// `npx tauri signer sign <file>` (key/password picked up from the
/// `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` env vars the CLI itself reads —
/// never passed as arguments, which would leak them to `ps`) — writes
/// `<file>.sig` next to the artifact and returns its contents: the same
/// minisign format `openpal_core::update_portable::verify_signature` (and
/// Tauri's own updater plugin, for the Full SKU) expects.
pub(crate) fn sign(artifact: &Path) -> anyhow::Result<String> {
    let mut cmd = crate::node::node_cli("npx");
    cmd.args(["--no-install", "tauri", "signer", "sign"])
        .arg(artifact);
    cmd.current_dir(paths::repo_root());
    let status = cmd
        .status()
        .context("failed to run npx — run: npm install (at the repo root)")?;
    if !status.success() {
        bail!("tauri signer sign failed for {}", artifact.display());
    }
    let sig_path = PathBuf::from(format!("{}.sig", artifact.display()));
    fs::read_to_string(&sig_path)
        .with_context(|| format!("expected a signature file at {}", sig_path.display()))
}

pub(crate) fn sha256_hex(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Download the `tag` release's existing `asset` manifest file, if any —
/// `None` for a brand-new tag/release, or one that hasn't published that
/// asset yet. Shared by `manifest` (tag = a version tag, asset =
/// "latest.json") and `engine_manifest` (tag = the fixed "engine-latest",
/// asset = "engine-manifest.json").
pub(crate) fn fetch_existing_manifest(tag: &str, asset: &str) -> Option<Map<String, Value>> {
    let tmp = std::env::temp_dir().join(format!(
        "openpal-manifest-{}-{}",
        tag.replace(['/', '\\'], "_"),
        asset.replace(['/', '\\'], "_"),
    ));
    let _ = fs::remove_file(&tmp);
    let status = Command::new("gh")
        .args(["release", "download", tag, "-p", asset, "-O"])
        .arg(&tmp)
        .args(["-R", REPO])
        .status()
        .ok()?;
    if !status.success() || !tmp.exists() {
        return None;
    }
    let raw = fs::read_to_string(&tmp).ok();
    let _ = fs::remove_file(&tmp);
    match serde_json::from_str::<Value>(&raw?).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

/// Ensure a release exists for `tag` (creating a draft if not), then upload
/// `assets`, overwriting any same-named ones already there.
fn publish(tag: &str, assets: &[PathBuf]) -> anyhow::Result<()> {
    let exists = Command::new("gh")
        .args(["release", "view", tag, "-R", REPO])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !exists {
        println!("No release exists yet for {tag} — creating one as a draft.");
        let status = Command::new("gh")
            .args([
                "release",
                "create",
                tag,
                "--draft",
                "--title",
                tag,
                "--notes",
                "Automated build — not yet promoted for auto-update.",
                "-R",
                REPO,
            ])
            .status()
            .context("failed to run gh — is it installed and authenticated? (`gh auth login`)")?;
        if !status.success() {
            bail!("gh release create failed for {tag}");
        }
    }

    let mut cmd = Command::new("gh");
    cmd.args(["release", "upload", tag, "--clobber", "-R", REPO]);
    for asset in assets {
        cmd.arg(asset);
    }
    let status = cmd.status().context("failed to run gh release upload")?;
    if !status.success() {
        bail!("gh release upload failed for {tag}");
    }
    Ok(())
}

pub(crate) fn ensure_gh() -> anyhow::Result<()> {
    let ok = Command::new("gh")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if ok {
        Ok(())
    } else {
        bail!("gh (GitHub CLI) not found — install from https://cli.github.com and run `gh auth login`")
    }
}

pub(crate) fn require_env(var: &str) -> anyhow::Result<()> {
    match std::env::var(var) {
        Ok(v) if !v.is_empty() => Ok(()),
        _ => bail!("{var} is not set — export it (the updater signing key/password)"),
    }
}

fn now_utc_rfc3339() -> anyhow::Result<String> {
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .context("failed to run `date`")?;
    if !out.status.success() {
        bail!("`date` failed");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_the_well_known_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("input");
        fs::write(&file, b"abc").unwrap();

        assert_eq!(
            sha256_hex(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_hex_errors_on_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(sha256_hex(&tmp.path().join("nope")).is_err());
    }

    #[test]
    fn find_artifact_locates_zip_or_appimage_by_target() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("OpenPal-portable-macos-arm64.zip"), b"").unwrap();
        fs::write(
            tmp.path()
                .join("OpenPal-portable-ubuntu-vulkan-x64.AppImage"),
            b"",
        )
        .unwrap();

        assert_eq!(
            find_artifact(tmp.path(), "macos-arm64"),
            Some(tmp.path().join("OpenPal-portable-macos-arm64.zip"))
        );
        assert_eq!(
            find_artifact(tmp.path(), "ubuntu-vulkan-x64"),
            Some(
                tmp.path()
                    .join("OpenPal-portable-ubuntu-vulkan-x64.AppImage")
            )
        );
        assert_eq!(find_artifact(tmp.path(), "win-vulkan-x64"), None);
    }
}
