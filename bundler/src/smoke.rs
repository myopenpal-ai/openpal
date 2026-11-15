//! `smoke`: verify an assembled Portable bundle for one target before a stable
//! release is promoted. Runs on the native runner matching the target (each
//! `portable.yml` smoke-matrix leg maps a target to its OS), *after* `publish`
//! has signed the artifacts and assembled the draft release, and checks:
//!
//! 1. `latest.json` (downloaded from the draft) carries the cut's version and
//!    an entry for our target whose SHA-256 matches the artifact on disk.
//! 2. The artifact's minisign signature — stored in `latest.json` as the plain
//!    4-line minisign text (`manifest` base64-decodes Tauri's `.sig` blob before
//!    writing it) — verifies against the configured `TAURI_SIGNING_PUBLIC_KEY`,
//!    the exact scheme the app uses in
//!    `openpal_core::update_portable::verify_signature`, so a signature that
//!    wouldn't validate in the app fails here too.
//! 3. The bundle reports the cut's version from its own metadata — macOS
//!    Info.plist `CFBundleShortVersionString`, Windows exe version resources —
//!    or, on Linux (where the portable AppImage carries the version nowhere
//!    but the app binary), that the main binary embeds the expected version
//!    string. No GUI boot; no display needed.
//! 4. The engine packaged in the bundle executes (`llama-server --version`),
//!    catching a broken nested engine before a human promotes.
//!
//! The workflow only writes promote instructions (and never discards the
//! draft) when every target's smoke leg passes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};
use minisign_verify::{PublicKey, Signature};

use crate::{fetch, manifest};

pub fn run(target: &str, artifact: &Path, manifest_path: &Path, tag: &str) -> anyhow::Result<()> {
    if !fetch::host_can_run(target) {
        bail!(
            "{target} can't be smoke-tested on this host ({}/{}) — each smoke leg must run on a matching native runner",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }
    let pubkey = std::env::var("TAURI_SIGNING_PUBLIC_KEY").unwrap_or_default();
    if pubkey.is_empty() {
        bail!(
            "TAURI_SIGNING_PUBLIC_KEY is not set — the stable smoke gate refuses to run without it \
             (populate the Actions secret from the minisign .pub key that matches \
             TAURI_SIGNING_PRIVATE_KEY; see docs/RELEASING.md)"
        );
    }

    println!("Smoke-testing {target} bundle {}…", artifact.display());

    // 1. The cut's manifest, downloaded from the draft release.
    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", manifest_path.display()))?;
    let manifest_version = manifest["version"]
        .as_str()
        .ok_or_else(|| anyhow!("{} has no \"version\"", manifest_path.display()))?;
    let expected = semver_core(tag);
    if semver_core(manifest_version) != expected {
        bail!(
            "{} says version \"{manifest_version}\" but this run is for {tag}",
            manifest_path.display()
        );
    }
    println!("✓ latest.json version {manifest_version} matches release {tag}");

    let portable = manifest["portable"]
        .as_object()
        .ok_or_else(|| anyhow!("{} has no \"portable\" block", manifest_path.display()))?;
    let entry = portable.get(target).ok_or_else(|| {
        let known: Vec<&str> = portable.keys().map(String::as_str).collect();
        anyhow!(
            "{} has no entry for target \"{target}\" (has: {})",
            manifest_path.display(),
            known.join(", ")
        )
    })?;
    let expected_sha = entry["sha256"]
        .as_str()
        .ok_or_else(|| anyhow!("the {target} entry has no sha256"))?;
    let signature = entry["signature"]
        .as_str()
        .ok_or_else(|| anyhow!("the {target} entry has no signature"))?;

    // 2. SHA-256 of the artifact vs the manifest.
    let actual_sha = manifest::sha256_hex(artifact)?;
    if actual_sha != expected_sha {
        bail!(
            "sha256 mismatch for {target}: manifest says {expected_sha}, the artifact is {actual_sha}"
        );
    }
    println!("✓ sha256 matches {}", manifest_path.display());

    // 3. Updater signature over the artifact.
    verify_signature(artifact, signature, &pubkey)?;
    println!("✓ updater signature verifies");

    // 4. Unpack the bundle and check it reports the cut's version.
    let work = std::env::temp_dir().join(format!("openpal-smoke-{target}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).with_context(|| format!("failed to create {}", work.display()))?;
    unpack(artifact, &work)?;
    let reported = verify_bundle_version(target, &work, expected)?;
    println!("✓ bundle reports version {reported}");

    // 5. The engine packaged inside the bundle executes.
    let (server_name, _) = fetch::artifact_names(target);
    let engine = find_file(&work, &|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == server_name)
    })
    .ok_or_else(|| {
        anyhow!(
            "no `{server_name}` found inside the unpacked {target} bundle — the engine is missing"
        )
    })?;
    fetch::smoke_test(&engine)?;
    println!("✓ engine responds: {}", engine.display());

    fs::remove_dir_all(&work).ok();
    println!("✓ {target} smoke passed");
    Ok(())
}

/// Unpack `artifact` (a `.zip` or `.AppImage`) into `work`. Zips extract via
/// the shared archive extractor; AppImages run `--appimage-extract` (no FUSE
/// needed, drops a `squashfs-root/` AppDir).
fn unpack(artifact: &Path, work: &Path) -> anyhow::Result<()> {
    let name = artifact
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("{} has no file name", artifact.display()))?;
    if name.ends_with(".zip") {
        fetch::extract_archive(artifact, work)?;
    } else if name.ends_with(".AppImage") {
        // The AppImage runtime may be shipped without the exec bit; make sure.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(artifact, fs::Permissions::from_mode(0o755));
        }
        let status = Command::new(artifact)
            .arg("--appimage-extract")
            .current_dir(work)
            .status()
            .with_context(|| format!("failed to run {}", artifact.display()))?;
        if !status.success() {
            bail!(
                "`{} --appimage-extract` failed — the AppImage may be malformed",
                artifact.display()
            );
        }
    } else {
        bail!(
            "don't know how to unpack {} — expected a .zip or .AppImage",
            artifact.display()
        );
    }
    Ok(())
}

/// Assert the unpacked bundle reports the cut's version, returning a
/// human-readable description of the evidence.
fn verify_bundle_version(target: &str, work: &Path, expected: &str) -> anyhow::Result<String> {
    if target.contains("macos") {
        let version = macos_bundle_version(work)?;
        if semver_core(&version) != expected {
            bail!("bundle reports version {version}, but this cut is {expected}");
        }
        Ok(version)
    } else if target.contains("win") {
        let version = windows_exe_version(work)?;
        if semver_core(&version) != expected {
            bail!("bundle reports version {version}, but this cut is {expected}");
        }
        Ok(version)
    } else {
        // Linux: the portable AppImage carries the version nowhere but the app
        // binary (`CARGO_PKG_VERSION` is compiled into it), so assert the main
        // binary embeds the expected string rather than reporting a value.
        let binary = find_file(work, &|p| {
            p.file_name().and_then(|n| n.to_str()) == Some("openpal")
        })
        .ok_or_else(|| anyhow!("no `openpal` binary found in the extracted AppDir"))?;
        let bytes =
            fs::read(&binary).with_context(|| format!("failed to read {}", binary.display()))?;
        if !bytes
            .windows(expected.len())
            .any(|w| w == expected.as_bytes())
        {
            bail!(
                "the {} binary does not embed the version string \"{expected}\" — this bundle is not this cut",
                binary.display()
            );
        }
        Ok(expected.to_string())
    }
}

/// The version in a macOS bundle's `Info.plist` (`CFBundleShortVersionString`,
/// set by Tauri from the Cargo version).
fn macos_bundle_version(work: &Path) -> anyhow::Result<String> {
    let app = find_file(work, &|p| {
        p.is_dir() && p.extension().and_then(|e| e.to_str()) == Some("app")
    })
    .ok_or_else(|| anyhow!("no .app bundle found in the unpacked zip"))?;
    let plist = app.join("Contents/Info.plist");
    let out = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleShortVersionString"])
        .arg(&plist)
        .output()
        .with_context(|| format!("failed to run PlistBuddy on {}", plist.display()))?;
    if !out.status.success() {
        bail!(
            "couldn't read CFBundleShortVersionString from {}",
            plist.display()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The version in a Windows exe's version resources (set by Tauri via
/// winres from the Cargo version).
fn windows_exe_version(work: &Path) -> anyhow::Result<String> {
    let exe = find_file(work, &|p| {
        p.file_name().and_then(|n| n.to_str()) == Some("OpenPal.exe")
    })
    .ok_or_else(|| anyhow!("no OpenPal.exe found in the unpacked zip"))?;
    let script = format!(
        "$v=(Get-Item -LiteralPath '{}').VersionInfo; if ($v.FileVersion) {{ $v.FileVersion }} else {{ $v.ProductVersion }}",
        exe.display()
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .with_context(|| format!("failed to run powershell on {}", exe.display()))?;
    if !out.status.success() {
        bail!("couldn't read the version resources of {}", exe.display());
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        bail!("{} has no FileVersion/ProductVersion", exe.display());
    }
    Ok(version)
}

/// The semver core of a version string ("v0.2.0+abc" → "0.2.0"): the cut tag,
/// the manifest's version, and each bundle's reported version must all agree
/// modulo the leading `v` and any nightly `+b{N}` build metadata.
fn semver_core(v: &str) -> &str {
    v.trim_start_matches('v')
        .split(['+', '-'])
        .next()
        .unwrap_or("")
}

/// Verify a minisign signature over `path` — mirrors
/// `openpal_core::update_portable::verify_signature` (same crate, same strict
/// `allow_legacy = false`, same `Signature::decode` of the stored text), so the
/// gate only passes when the app would accept the artifact.
fn verify_signature(path: &Path, signature: &str, pubkey_b64: &str) -> anyhow::Result<()> {
    let pubkey = PublicKey::from_base64(pubkey_b64)
        .map_err(|e| anyhow!("TAURI_SIGNING_PUBLIC_KEY is not a valid minisign public key: {e}"))?;
    let signature = Signature::decode(signature)
        .map_err(|e| anyhow!("the manifest's signature for this artifact is malformed: {e}"))?;
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    pubkey
        .verify(&bytes, &signature, false)
        .map_err(|e| anyhow!("updater signature verification failed: {e}"))?;
    Ok(())
}

/// Depth-first find of the first path under `dir` matching `predicate`.
fn find_file(dir: &Path, predicate: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if predicate(&path) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(found) = find_file(&path, predicate) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_core_strips_v_and_build_metadata() {
        assert_eq!(semver_core("v0.2.0"), "0.2.0");
        assert_eq!(semver_core("0.2.0"), "0.2.0");
        assert_eq!(semver_core("v0.2.0+b123"), "0.2.0");
        assert_eq!(semver_core("0.2.0-rc.1"), "0.2.0");
        assert_eq!(semver_core(""), "");
    }

    #[test]
    fn find_file_searches_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("a/b/c")).unwrap();
        fs::write(tmp.path().join("a/b/c/OpenPal.exe"), b"").unwrap();

        assert_eq!(
            find_file(tmp.path(), &|p| p.file_name().and_then(|n| n.to_str())
                == Some("OpenPal.exe")),
            Some(tmp.path().join("a/b/c/OpenPal.exe"))
        );
        assert_eq!(
            find_file(tmp.path(), &|p| p.file_name().and_then(|n| n.to_str())
                == Some("nope.exe")),
            None
        );
    }

    #[test]
    fn binary_version_string_is_embedded() {
        // The Linux check is `bytes.windows(...).any(...)`; assert the
        // scanning logic against an in-memory buffer.
        let buf = b"prefix 0.2.0 suffix\0other";
        let version = "0.2.0";
        assert!(buf.windows(version.len()).any(|w| w == version.as_bytes()));
        let missing = "9.9.9";
        assert!(!buf.windows(missing.len()).any(|w| w == missing.as_bytes()));
    }
}
