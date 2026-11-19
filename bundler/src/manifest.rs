//! `manifest`: sign Portable and Full-SKU release artifacts and publish/merge
//! `latest.json` — the file OpenPal's auto-updater checks (both SKUs; see
//! `crates/core/src/update.rs`). Shells out to the Tauri CLI (`npx tauri
//! signer sign`) for signing and `gh` for the GitHub Release, matching this
//! crate's existing "shell out to standard tooling" convention.
//!
//! Two blocks are maintained here:
//!
//! - `portable` — the no-install artifacts, assembled and published by
//!   `portable.yml` for every tag push, signed with the updater key.
//! - `platforms` — the Full SKU's block (Tauri's own updater-plugin schema),
//!   which `tauri-plugin-updater` reads to download the `.app.tar.gz` updater
//!   archive for the host platform. Full builds/publishes stay a manual,
//!   local step (`bundler release` on the target machine), so this command is
//!   what turns that build's output into manifest entries: given
//!   `--full-dir` (the Tauri bundle output, e.g. `target/release/bundle`), it
//!   archives the signed/notarized `.app` into `<name>.app.tar.gz`, signs it
//!   with the updater key, derives each platform's download URL from the
//!   *actual* artifact file name (guaranteed to match what gets uploaded —
//!   the 404-freedom property), and uploads the archive and `.dmg` with the
//!   manifest. Passing `--full-dir` here replaces the hand-edited Full
//!   `platforms` block that used to drift from the release's real assets.
//!
//! Any `latest.json` already attached to the release is downloaded first and
//! merged into (not overwritten by), so a Portable-only CI run and a later
//! manual Full entry can coexist in one file without clobbering each other.
//!
//! A release created here (because `tag` doesn't have one yet) is a
//! **draft** for `--kind stable` — deliberately: `/releases/latest` never
//! resolves to a draft or pre-release, so publishing stays inert until a
//! human promotes it (`gh release edit <tag> --draft=false`), matching the
//! plan's soft-launch gate for the very first auto-updatable release. A
//! `--kind nightly` run instead creates a **pre-release** whose manifest
//! version is `X.Y.Z+b{N}` semver build metadata — the rolling track's
//! canary builds (`b*` tags), which `/releases/latest` also ignores, so no
//! installed app ever auto-updates to a nightly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};
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

/// Which release track the `manifest` command assembles (docs/RELEASING.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ManifestKind {
    /// Deliberate, human-gated cut: created as a **draft**, manifest version
    /// `X.Y.Z`, promoted only after the per-target smoke gate passes.
    Stable,
    /// Rolling canary build for a `b{N}` tag: created as a **pre-release**
    /// (never seen by `/releases/latest`), manifest version `X.Y.Z+b{N}`.
    Nightly,
}

pub fn run(
    tag: &str,
    kind: ManifestKind,
    portable_dir: Option<&Path>,
    full_dir: Option<&Path>,
    notes: Option<&str>,
) -> anyhow::Result<()> {
    require_env("TAURI_SIGNING_PRIVATE_KEY")?;
    require_env("TAURI_SIGNING_PRIVATE_KEY_PASSWORD")?;
    ensure_gh()?;

    // The manifest `version` is what the app compares against
    // CARGO_PKG_VERSION: plain `X.Y.Z` for a stable cut, `X.Y.Z+b{N}` semver
    // build metadata for a nightly (the `b{N}` half comes from the tag
    // itself, which rolling.yml names after the build number).
    let manifest_version = match kind {
        ManifestKind::Stable => tag.trim_start_matches('v').to_string(),
        ManifestKind::Nightly => nightly_manifest_version(env!("CARGO_PKG_VERSION"), tag)?,
    };

    let mut manifest = fetch_existing_manifest(tag, "latest.json").unwrap_or_default();
    manifest.insert("version".to_string(), Value::String(manifest_version));
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

    if let Some(dir) = full_dir {
        let platforms = sign_full_platforms(dir, tag, &mut assets)?;
        // Same merge discipline as `portable`: a Full entry from this run must
        // not blank out other platforms'/SKUs' still-good entries already in
        // the release's manifest.
        let mut merged = match manifest.get("platforms") {
            Some(Value::Object(existing)) => existing.clone(),
            _ => Map::new(),
        };
        merged.extend(platforms);
        manifest.insert("platforms".to_string(), Value::Object(merged));
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

    publish(tag, &assets, kind)?;
    println!(
        "\n✓ Published {} file(s) to release {tag} ({REPO}).",
        assets.len()
    );
    Ok(())
}

/// The manifest `version` for a nightly `b{N}` tag: the workspace semver
/// (`CARGO_PKG_VERSION`) plus `+b{N}` build metadata (e.g. `0.2.0+b1234`).
/// The build number is read from the tag itself, which `rolling.yml` names
/// after `git rev-list --count HEAD`. Never consumed by the auto-updater
/// (pre-releases are invisible to `/releases/latest`), but it keeps every
/// nightly's manifest unique and honest.
fn nightly_manifest_version(base: &str, tag: &str) -> anyhow::Result<String> {
    let n = tag
        .strip_prefix('b')
        .ok_or_else(|| anyhow!("nightly tag must look like b<N>, got \"{tag}\""))?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        bail!("nightly tag must look like b<N> with a numeric build number, got \"{tag}\"");
    }
    Ok(format!("{base}+{tag}"))
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

/// Turn a Full-SKU build's output into `latest.json` `platforms` entries —
/// the block `tauri-plugin-updater` consults to download a platform's updater
/// archive. `bundle_dir` is the Tauri bundle output root for this machine
/// (e.g. `target/release/bundle`, containing `macos/` and `dmg/`), `tag` is
/// the release tag the URLs point at, and signed/uploaded files are appended
/// to `assets`.
///
/// The Full SKU's macOS targets in `tauri.conf.json` are `["app", "dmg"]`:
/// the `.app` is what the updater installs (as a gzipped tar archive),
/// and the `.dmg` is the user-facing fresh-install image. This command
/// manufactures the updater archive itself (Tauri's `createUpdaterArtifacts`
/// is deliberately not required — it would force the signing keys onto every
/// `bundler build`), signs it with the updater key, and derives each
/// platform's download URL from the archive's *actual* file name, so the URL
/// always resolves to the asset this command uploads. The `.dmg` is uploaded
/// alongside (no `platforms` entry — the updater never installs from it).
///
/// Platform keys mirror `tauri-plugin-updater`'s `target()`: `{os}-{arch}`
/// with `darwin`/`windows`/`linux` for the OS and `aarch64`/`x86_64`/… for
/// the architecture. Today only macOS is reachable (the bundle targets are
/// macOS-only); anything else fails loudly rather than publishing a
/// half-wired manifest.
fn sign_full_platforms(
    bundle_dir: &Path,
    tag: &str,
    assets: &mut Vec<PathBuf>,
) -> anyhow::Result<Map<String, Value>> {
    let mut out = Map::new();

    if !cfg!(target_os = "macos") {
        bail!(
            "Full-SKU updater artifacts can only be produced on macOS (the bundle targets are \
             app + dmg; this host is {}) — pass --full-dir on the machine that built the Full \
             SKU, or omit it to leave the manifest's `platforms` block untouched",
            std::env::consts::OS
        );
    }

    let key = platform_key(std::env::consts::OS, std::env::consts::ARCH).ok_or_else(|| {
        anyhow!(
            "unsupported host for Full-SKU updates: {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;

    let macos_dir = bundle_dir.join("macos");
    let app = find_app(&macos_dir).ok_or_else(|| {
        anyhow!(
            "no .app bundle found in {} — run `bundler build` / `bundler release` first and \
             point --full-dir at the Tauri bundle output root (what `build` prints, e.g. \
             target/release/bundle)",
            macos_dir.display()
        )
    })?;
    let archive = updater_archive(&app)?;
    let file_name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no valid file name", archive.display()))?
        .to_string();

    println!("Signing {file_name}…");
    let signature = sign(&archive)?;
    let sha256 = sha256_hex(&archive)?;

    out.insert(
        key,
        serde_json::json!({
            "url": format!("https://github.com/{REPO}/releases/download/{tag}/{file_name}"),
            "sha256": sha256,
            "signature": signature,
        }),
    );
    assets.push(archive);

    // The fresh-install image — uploaded so the release actually carries it
    // (Done-when: the manifest's URLs resolve to real assets), never a
    // `platforms` entry (the updater replaces the `.app`, it doesn't run a
    // DMG installer).
    let dmg_dir = bundle_dir.join("dmg");
    if let Some(dmg) = find_dmg(&dmg_dir) {
        println!("Uploading installer {}", dmg.display());
        assets.push(dmg);
    } else {
        println!(
            "(no .dmg found in {} — this release will carry the updater archive only)",
            dmg_dir.display()
        );
    }

    Ok(out)
}

/// The tauri-plugin-updater `platforms` map key for a host OS + arch —
/// mirror of the plugin's `target()` (`darwin-aarch64`, `linux-x86_64`, …).
fn platform_key(os: &str, arch: &str) -> Option<String> {
    let os = match os {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match arch {
        "x86" => "i686",
        "x86_64" => "x86_64",
        "arm" => "armv7",
        "aarch64" => "aarch64",
        "riscv64" => "riscv64",
        _ => return None,
    };
    Some(format!("{os}-{arch}"))
}

/// The updater archive for a macOS `.app` — `<name>.app.tar.gz` next to the
/// bundle, the exact name and layout `tauri-bundler`'s updater package uses
/// (the `.app` at the tarball root). Created on demand from the *final*
/// signed/notarized `.app` (Tauri's build-time archive would predate
/// signing); an existing archive is reused as-is.
fn updater_archive(app: &Path) -> anyhow::Result<PathBuf> {
    let archive = PathBuf::from(format!("{}.tar.gz", app.display()));
    if archive.exists() {
        return Ok(archive);
    }
    let parent = app
        .parent()
        .with_context(|| format!("{} has no parent directory", app.display()))?;
    let name = app
        .file_name()
        .and_then(|n| n.to_str())
        .with_context(|| format!("{} has no valid file name", app.display()))?;

    println!("Archiving {}…", archive.display());
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .args(["-C"])
        .arg(parent)
        .arg(name)
        .status()
        .context("failed to run `tar` (expected on macOS)")?;
    if !status.success() {
        bail!("tar failed to create {}", archive.display());
    }
    Ok(archive)
}

/// The `.app` bundle in `dir` (the Full SKU's `macos/` output), if any.
fn find_app(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        (path.is_dir() && path.extension().and_then(|e| e.to_str()) == Some("app")).then_some(path)
    })
}

/// The `.dmg` installer in `dir` (the Full SKU's `dmg/` output), if any.
fn find_dmg(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        (path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("dmg")).then_some(path)
    })
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
/// `<file>.sig` next to the artifact and returns its contents as a minisign
/// signature string, decoded the way the consumers expect: Tauri writes the
/// `.sig` file as one BASE64 line *wrapping* the 4-line minisign text, so the
/// raw file is not the form `minisign_verify::Signature::decode` (used by
/// `openpal_core::update_portable::verify_signature`, and mirrored in
/// `smoke`) accepts — the decoded text is.
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
    let blob = fs::read_to_string(&sig_path)
        .with_context(|| format!("expected a signature file at {}", sig_path.display()))?;
    decode_sig_blob(&blob).with_context(|| format!("…from {}", sig_path.display()))
}

/// BASE64-decode a `tauri signer sign` `.sig` file back into the 4-line
/// minisign text it wraps. Pure + unit-tested so `sign` stays a thin
/// shell-out on top of it.
fn decode_sig_blob(blob: &str) -> anyhow::Result<String> {
    use base64::Engine as _;
    let trimmed = blob.trim();
    if trimmed.is_empty() {
        bail!("the .sig file is empty (Tauri writes it as one BASE64 line)");
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| {
            anyhow!(
                "{} is not valid BASE64 (Tauri writes .sig as one BASE64 line): {e}",
                trimmed
            )
        })?;
    String::from_utf8(bytes)
        .map_err(|_| anyhow!("the .sig BASE64 does not decode to UTF-8 — not a minisign signature"))
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
    Ok(hex::encode(hasher.finalize()))
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

/// Ensure a release exists for `tag` (creating a draft for a stable cut or a
/// pre-release for a nightly — both invisible to `/releases/latest`), then
/// upload `assets`, overwriting any same-named ones already there.
fn publish(tag: &str, assets: &[PathBuf], kind: ManifestKind) -> anyhow::Result<()> {
    let exists = Command::new("gh")
        .args(["release", "view", tag, "-R", REPO])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !exists {
        let (create_flag, noun, notes) = match kind {
            ManifestKind::Stable => (
                "--draft",
                "draft",
                "Automated build — not yet promoted for auto-update.",
            ),
            ManifestKind::Nightly => (
                "--prerelease",
                "pre-release",
                "Automated nightly canary build — pre-release, invisible to the auto-updater (/releases/latest never resolves to a pre-release).",
            ),
        };
        println!("No release exists yet for {tag} — creating one as a {noun}.");
        let status = Command::new("gh")
            .args([
                "release",
                "create",
                tag,
                create_flag,
                "--title",
                tag,
                "--notes",
                notes,
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
    use base64::Engine as _;

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
    fn decode_sig_blob_unwraps_tauris_base64_sig() {
        // `tauri signer sign` writes the .sig as one BASE64 line wrapping the
        // 4-line minisign text. decode_sig_blob must hand back exactly that
        // text — the form `minisign_verify::Signature::decode` accepts — and
        // reject non-BASE64 input.
        const SIG_TEXT: &str = "untrusted comment: signature from tauri secret key\n\
RUQeXjZl0LQ1Cy/o/W+I+NAhtmBqhoYbxs4jcrghWYDusqdI6pyShw+hXxKNaxxybtWIJT/EBQPGzXgujvrK65CFFcvbMSv4Pws=\n\
trusted comment: timestamp:1794749128\tfile:OpenPal-portable-macos-arm64.zip\n\
pPvxUtM3ZOFeC8+YwSui29fEN9fceotapUFSFxZZ8jr/HFUdcB4T+00LnuWy/JmYOd+ghXtLpP+a1eurCgwFCQ==\n";
        let blob = base64::engine::general_purpose::STANDARD.encode(SIG_TEXT.as_bytes());
        let decoded = decode_sig_blob(&blob).unwrap();
        assert_eq!(decoded, SIG_TEXT);

        // The decoded text is exactly the form the app's verifier (and the
        // smoke gate, which mirrors it) parses — and the raw blob is not.
        assert!(minisign_verify::Signature::decode(&decoded).is_ok());
        assert!(minisign_verify::Signature::decode(&blob).is_err());

        assert!(decode_sig_blob("not base64!!").is_err());
        assert!(decode_sig_blob("").is_err());
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

    #[test]
    fn nightly_manifest_version_appends_build_metadata() {
        // The exact `X.Y.Z+b{N}` string is the contract rolling.yml's tags
        // feed into the nightly manifest: the workspace semver plus the
        // build-number half of the `b{N}` tag as semver build metadata.
        assert_eq!(
            nightly_manifest_version("0.1.0", "b45").unwrap(),
            "0.1.0+b45"
        );
        assert_eq!(
            nightly_manifest_version("0.4.2", "b1234").unwrap(),
            "0.4.2+b1234"
        );
        // Only `b<N>` tags are nightlies; anything else is rejected loudly so
        // a mistyped tag can never ship a mislabelled pre-release.
        assert!(nightly_manifest_version("0.1.0", "45").is_err());
        assert!(nightly_manifest_version("0.1.0", "b").is_err());
        assert!(nightly_manifest_version("0.1.0", "bfoo").is_err());
        assert!(nightly_manifest_version("0.1.0", "v0.2.0").is_err());
    }

    #[test]
    fn platform_key_mirrors_tauri_plugin_updater_target() {
        // The exact `{os}-{arch}` strings tauri-plugin-updater's `target()`
        // resolves to on each host — macOS arm64 is the reporter's platform
        // (the 404 repro), and the values must match what the plugin looks
        // up under `platforms`.
        assert_eq!(platform_key("macos", "aarch64").unwrap(), "darwin-aarch64");
        assert_eq!(platform_key("macos", "x86_64").unwrap(), "darwin-x86_64");
        assert_eq!(platform_key("windows", "x86_64").unwrap(), "windows-x86_64");
        assert_eq!(platform_key("linux", "x86_64").unwrap(), "linux-x86_64");
        assert_eq!(platform_key("linux", "aarch64").unwrap(), "linux-aarch64");
        // 32-bit and exotic hosts map through the plugin's vocabulary too.
        assert_eq!(platform_key("windows", "x86").unwrap(), "windows-i686");
        assert_eq!(platform_key("linux", "riscv64").unwrap(), "linux-riscv64");
        // Anything the plugin can't update is rejected loudly, not mislabelled.
        assert_eq!(platform_key("android", "aarch64"), None);
        assert_eq!(platform_key("macos", "wasm32"), None);
    }

    #[test]
    fn find_app_and_find_dmg_scan_the_bundle_output() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("macos").join("OpenPal.app");
        fs::create_dir_all(app.join("Contents")).unwrap();
        fs::create_dir_all(tmp.path().join("dmg")).unwrap();
        fs::write(
            tmp.path().join("dmg").join("OpenPal_0.2.0_aarch64.dmg"),
            b"",
        )
        .unwrap();

        assert_eq!(
            find_app(&tmp.path().join("macos")).unwrap(),
            tmp.path().join("macos").join("OpenPal.app")
        );
        assert_eq!(
            find_dmg(&tmp.path().join("dmg")).unwrap(),
            tmp.path().join("dmg").join("OpenPal_0.2.0_aarch64.dmg")
        );
        // A docker-compose.yaml or README in the dir is not a bundle.
        assert_eq!(find_app(&tmp.path().join("dmg")), None);
        assert_eq!(find_dmg(&tmp.path().join("macos")), None);
        // Missing directories yield None (a `--full-dir` pointing elsewhere
        // fails with a helpful error from the caller, never a panic).
        assert_eq!(find_app(&tmp.path().join("nope")), None);
    }

    #[test]
    fn updater_archive_tars_the_app_at_the_archive_root() {
        // Shipped `.app` tarballs carry the bundle at the root with all its
        // files inside — the layout `tauri-plugin-updater`'s extractor
        // expects after stripping the first path component, and the name must
        // be `<name>.app.tar.gz` (the platform download URL derives from it).
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("OpenPal.app");
        fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        fs::write(app.join("Contents/Info.plist"), b"<?xml version='1.0'?>").unwrap();
        fs::write(app.join("Contents/MacOS/openpal"), b"#!/bin/sh\n").unwrap();

        let archive = updater_archive(&app).unwrap();
        assert_eq!(
            archive,
            tmp.path().join("OpenPal.app.tar.gz"),
            "the archive name is what the manifest URL is derived from"
        );
        assert!(archive.is_file());

        // Second call reuses the archive rather than rewriting it, and the
        // layout matches the webview-side expectation.
        let again = updater_archive(&app).unwrap();
        assert_eq!(again, archive);
        let listing = Command::new("tar")
            .args(["-tzf"])
            .arg(&archive)
            .output()
            .unwrap();
        assert!(listing.status.success());
        let entries = String::from_utf8_lossy(&listing.stdout).into_owned();
        assert!(
            entries.contains("OpenPal.app/Contents/Info.plist"),
            "tar must contain the .app subtree: {entries}"
        );
        assert!(
            entries.contains("OpenPal.app/Contents/MacOS/openpal"),
            "tar must contain the executables: {entries}"
        );
    }
}
