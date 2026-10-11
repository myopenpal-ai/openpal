//! Code signing & notarization for macOS distribution.
//!
//! `tauri build` packages the .dmg in the same step that produces the .app,
//! before we ever sign anything — so the order here is: sign app (inside-out)
//! -> rebuild + sign DMG -> notarize DMG -> staple. Skipping the rebuild would
//! notarize a disk image that still contains the unsigned app.
//!
//! ## sign(identity)
//! Identity comes from the flag or the SIGNING_IDENTITY env var; error with a
//! pointer to https://developer.apple.com/ if neither is set.
//! Sign inside-out so the nested engine binaries are valid first:
//! 1. Find the built app: paths::bundle_dir(false)/OpenPal.app (error with
//!    "run: ... -- build" if missing).
//! 2. For every `*.dylib` and the `llama-server` binary under
//!    `OpenPal.app/Contents/Resources/llama/`:
//!    `codesign --force --options runtime --timestamp --sign <identity> <file>`
//! 3. Re-sign the whole bundle:
//!    `codesign --force --options runtime --timestamp --sign <identity>
//!     OpenPal.app`
//! 4. Verify: `codesign --verify --deep --strict OpenPal.app` and print the
//!    result.
//! 5. Rebuild the .dmg from the signed app so its contents match what was just
//!    signed: `hdiutil create -volname "OpenPal" -srcfolder <app> -ov -format
//!    UDZO <dmg>`, overwriting the existing target/release/bundle/dmg/*.dmg
//!    (or OpenPal_<version>_aarch64.dmg with the version from
//!    crates/app/tauri.conf.json when none exists yet), then sign the disk
//!    image itself: `codesign --force --timestamp --sign <identity> <dmg>`.
//!
//! ## notarize()
//! Reads APPLE_ID, APPLE_PASSWORD (app-specific password), APPLE_TEAM_ID.
//! 1. Find the .dmg in target/release/bundle/dmg/ — after sign() this is the
//!    rebuilt image containing the signed app (error with "run: ... -- build"
//!    if missing).
//! 2. `xcrun notarytool submit <dmg> --apple-id ... --password ... --team-id
//!    ... --wait` (inherit stdio — this takes minutes).
//! 3. `xcrun stapler staple <dmg>` and print a final "ready to distribute"
//!    line with the absolute path.
//!
//! ## sign_and_notarize_if_configured(identity)
//! Used by `release`: if no identity is configured, print a clear note that
//! the build is unsigned (fine for local use; Gatekeeper will warn on other
//! Macs) and exit Ok. If identity is set, sign; then notarize only when the
//! APPLE_* env vars are present, otherwise note what's missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};

use crate::paths;

pub fn sign(identity: Option<&str>) -> anyhow::Result<()> {
    let identity = resolve_identity(identity).ok_or_else(|| {
        anyhow!(
            "no signing identity — pass --identity \"Developer ID Application: Your Name (TEAMID)\" \
             or set the SIGNING_IDENTITY env var. Get a Developer ID certificate at https://developer.apple.com/"
        )
    })?;

    let app = paths::bundle_dir(false).join("OpenPal.app");
    if !app.exists() {
        bail!(
            "{} not found — run: cargo run -p openpal-bundler -- build",
            app.display()
        );
    }

    // Inside-out: sign the nested engine binaries before the bundle itself.
    let llama_dir = app.join("Contents/Resources/llama");
    let mut nested: Vec<PathBuf> = Vec::new();
    if llama_dir.is_dir() {
        for entry in fs::read_dir(&llama_dir)?.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "llama-server" || name.ends_with(".dylib") {
                nested.push(path);
            }
        }
    }
    nested.sort();

    println!("Signing {} nested engine binaries…", nested.len());
    for file in &nested {
        codesign(&identity, file)?;
    }

    println!("Signing app bundle…");
    // Embed the app's entitlements (microphone access for the Voice add-on) when
    // signing the outer bundle; nested binaries above are signed without them.
    let entitlements = paths::repo_root().join("crates/app/Entitlements.plist");
    let ent = if entitlements.exists() {
        Some(entitlements.as_path())
    } else {
        println!("  (no crates/app/Entitlements.plist — signing without entitlements)");
        None
    };
    codesign_opts(&identity, &app, ent)?;

    println!("Verifying signature…");
    let status = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .status()
        .context("failed to run codesign — are the Xcode command line tools installed?")?;
    if !status.success() {
        bail!("codesign verification failed for {}", app.display());
    }
    println!("✓ Signature verified: {}", app.display());

    rebuild_dmg(&identity, &app)
}

/// `tauri build` packaged the .dmg before sign() ran, so that image still
/// holds the unsigned app — recreate it from the signed bundle and sign the
/// disk image itself, so notarize() submits exactly what was signed.
fn rebuild_dmg(identity: &str, app: &Path) -> anyhow::Result<()> {
    let dmg = find_dmg().or_else(|_| default_dmg_path())?;
    if let Some(dir) = dmg.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }

    println!("Rebuilding the disk image from the signed app…");
    let status = Command::new("hdiutil")
        .args(["create", "-volname", "OpenPal", "-srcfolder"])
        .arg(app)
        .args(["-ov", "-format", "UDZO"])
        .arg(&dmg)
        .status()
        .context("failed to run hdiutil")?;
    if !status.success() {
        bail!("hdiutil failed to rebuild {}", dmg.display());
    }

    println!("Signing the disk image…");
    let status = Command::new("codesign")
        .args(["--force", "--timestamp", "--sign", identity])
        .arg(&dmg)
        .status()
        .context("failed to run codesign — are the Xcode command line tools installed?")?;
    if !status.success() {
        bail!("codesign failed for {}", dmg.display());
    }

    println!("✓ Disk image rebuilt and signed: {}", dmg.display());
    Ok(())
}

/// Where the .dmg goes when `tauri build` hasn't left one to overwrite,
/// named the way Tauri names it. `crates/app/tauri.conf.json` no longer
/// hardcodes a version (Tauri falls back to the app crate's Cargo.toml
/// version, which is `version.workspace = true` — same as this crate's own
/// `CARGO_PKG_VERSION`), so read the version from there unless the config
/// still sets one explicitly.
fn default_dmg_path() -> anyhow::Result<PathBuf> {
    let conf = paths::repo_root().join("crates/app/tauri.conf.json");
    let raw =
        fs::read_to_string(&conf).with_context(|| format!("failed to read {}", conf.display()))?;
    let json: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", conf.display()))?;
    let version = json
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or(env!("CARGO_PKG_VERSION"));
    Ok(paths::repo_root()
        .join("target/release/bundle/dmg")
        .join(format!("OpenPal_{version}_aarch64.dmg")))
}

pub fn notarize() -> anyhow::Result<()> {
    let apple_id = required_env("APPLE_ID", "your Apple developer account email")?;
    let password = required_env("APPLE_PASSWORD", "an app-specific password")?;
    let team_id = required_env("APPLE_TEAM_ID", "your 10-character team ID")?;

    let dmg = find_dmg()?;
    println!(
        "Submitting {} for notarization (this takes minutes)…",
        dmg.display()
    );
    let status = Command::new("xcrun")
        .args(["notarytool", "submit"])
        .arg(&dmg)
        .args([
            "--apple-id",
            &apple_id,
            "--password",
            &password,
            "--team-id",
            &team_id,
            "--wait",
        ])
        .status()
        .context("failed to run xcrun notarytool — are the Xcode command line tools installed?")?;
    if !status.success() {
        bail!("notarization failed — see the notarytool output above");
    }

    println!("Stapling notarization ticket…");
    let status = Command::new("xcrun")
        .args(["stapler", "staple"])
        .arg(&dmg)
        .status()
        .context("failed to run xcrun stapler")?;
    if !status.success() {
        bail!("stapling failed for {}", dmg.display());
    }

    println!("✓ Ready to distribute: {}", dmg.display());
    Ok(())
}

pub fn sign_and_notarize_if_configured(identity: Option<&str>) -> anyhow::Result<()> {
    let Some(identity) = resolve_identity(identity) else {
        println!("No signing identity configured (--identity / SIGNING_IDENTITY) — leaving the build unsigned.");
        println!(
            "That's fine for local use; Gatekeeper will warn when the app is opened on other Macs."
        );
        return Ok(());
    };
    sign(Some(&identity))?;

    let missing: Vec<&str> = ["APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID"]
        .into_iter()
        .filter(|var| std::env::var(var).map_or(true, |v| v.is_empty()))
        .collect();
    if missing.is_empty() {
        notarize()
    } else {
        println!(
            "Skipping notarization — missing env: {}.",
            missing.join(", ")
        );
        Ok(())
    }
}

fn resolve_identity(identity: Option<&str>) -> Option<String> {
    identity
        .map(str::to_string)
        .or_else(|| std::env::var("SIGNING_IDENTITY").ok())
        .filter(|s| !s.is_empty())
}

fn codesign(identity: &str, path: &Path) -> anyhow::Result<()> {
    codesign_opts(identity, path, None)
}

/// codesign under the hardened runtime, optionally embedding an entitlements
/// plist. The app bundle is signed with `crates/app/Entitlements.plist` so the
/// microphone (Voice add-on) works in a notarized build; nested binaries get no
/// entitlements.
fn codesign_opts(identity: &str, path: &Path, entitlements: Option<&Path>) -> anyhow::Result<()> {
    println!("  signing {}", path.display());
    let mut cmd = Command::new("codesign");
    cmd.args([
        "--force",
        "--options",
        "runtime",
        "--timestamp",
        "--sign",
        identity,
    ]);
    if let Some(ent) = entitlements {
        cmd.arg("--entitlements").arg(ent);
    }
    cmd.arg(path);
    let status = cmd
        .status()
        .context("failed to run codesign — are the Xcode command line tools installed?")?;
    if !status.success() {
        bail!("codesign failed for {}", path.display());
    }
    Ok(())
}

fn required_env(var: &str, what: &str) -> anyhow::Result<String> {
    std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("{var} is not set — export {var}=… ({what})"))
}

fn find_dmg() -> anyhow::Result<PathBuf> {
    let dmg_dir = paths::repo_root().join("target/release/bundle/dmg");
    fs::read_dir(&dmg_dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|ext| ext == "dmg"))
        })
        .ok_or_else(|| {
            anyhow!(
                "no .dmg found in {} — run: cargo run -p openpal-bundler -- build",
                dmg_dir.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::resolve_identity;

    /// A single test, not three, deliberately: `resolve_identity` reads the
    /// process-global `SIGNING_IDENTITY` env var, which races under `cargo
    /// test`'s default parallel threads if split across independent `#[test]`
    /// fns. Exercising every precedence case in sequence inside one test
    /// avoids that without needing a serialization crate.
    #[test]
    fn resolve_identity_precedence() {
        let saved = std::env::var("SIGNING_IDENTITY").ok();

        std::env::remove_var("SIGNING_IDENTITY");
        assert_eq!(resolve_identity(None), None);

        std::env::set_var("SIGNING_IDENTITY", "Developer ID Application: Env (TEAMID)");
        assert_eq!(
            resolve_identity(None),
            Some("Developer ID Application: Env (TEAMID)".to_string())
        );

        // An explicit flag wins over the env var.
        assert_eq!(
            resolve_identity(Some("Developer ID Application: Flag (TEAMID)")),
            Some("Developer ID Application: Flag (TEAMID)".to_string())
        );

        // An empty env var is treated as unset, not as a valid (empty) identity.
        std::env::set_var("SIGNING_IDENTITY", "");
        assert_eq!(resolve_identity(None), None);

        match saved {
            Some(v) => std::env::set_var("SIGNING_IDENTITY", v),
            None => std::env::remove_var("SIGNING_IDENTITY"),
        }
    }
}
