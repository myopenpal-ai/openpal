//! Check for OpenPal updates against a `latest.json` manifest published to
//! GitHub Releases.
//!
//! One manifest, two readers: the Full SKU's `platforms` block is read
//! directly by Tauri's own updater plugin (this module never touches it —
//! see `crates/app/src/commands.rs`), while the Portable SKU has no
//! installer for that plugin to drive, so this crate reads the manifest's
//! `portable` block itself and [`crate::update_portable`] does the
//! download/verify/apply.
//!
//! Known gap: the manifest is hosted on GitHub Releases for a currently
//! *private* repo, which real users' installs can't fetch anonymously.
//! Nothing here works end-to-end for end users until the repo goes public —
//! see the "Auto-Updater" plan for the accepted rollout gate.

use crate::{Error, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One release's manifest, as published to GitHub Releases alongside the
/// build artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    /// Tauri's own updater-plugin schema, keyed by target triple (e.g.
    /// `darwin-aarch64`). Only consumed by the plugin itself on the Full SKU.
    #[serde(default)]
    pub platforms: HashMap<String, PlatformEntry>,
    /// This crate's addition for the Portable SKU, keyed by the bundler's
    /// own `--target` strings (`macos-arm64`, `macos-x64`, `win-vulkan-x64`,
    /// `ubuntu-vulkan-x64`, `ubuntu-vulkan-arm64`).
    #[serde(default)]
    pub portable: HashMap<String, PortableEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformEntry {
    pub signature: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableEntry {
    pub url: String,
    pub sha256: String,
    pub signature: String,
}

/// A newer version, ready to show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: String,
    pub pub_date: String,
}

const OFFLINE_MSG: &str =
    "Couldn't check for updates. Check your internet connection and try again.";
const UNPARSEABLE_MSG: &str =
    "The update check got an unexpected response. Please try again later.";

/// This build's version, from the crate's own `Cargo.toml`
/// (`version.workspace = true` — the same version Tauri bundles, now that
/// `tauri.conf.json` no longer hardcodes one).
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is always valid semver")
}

/// Fetch and parse the manifest at `endpoint`. Network/parse failures become
/// messages safe to show a non-technical user.
pub async fn fetch_manifest(client: &reqwest::Client, endpoint: &str) -> Result<Manifest> {
    let resp = client.get(endpoint).send().await.map_err(|e| {
        tracing::warn!("update manifest fetch failed: {e}");
        Error::Update(OFFLINE_MSG.to_string())
    })?;
    if !resp.status().is_success() {
        tracing::warn!("update manifest fetch returned {}", resp.status());
        return Err(Error::Update(OFFLINE_MSG.to_string()));
    }
    resp.json::<Manifest>().await.map_err(|e| {
        tracing::warn!("update manifest parse failed: {e}");
        Error::Update(UNPARSEABLE_MSG.to_string())
    })
}

/// `Some(update)` when `manifest.version` is newer than `current`; `None`
/// when already current (or somehow ahead, e.g. a local dev build) or the
/// manifest's version string doesn't parse. Pure and offline — no network,
/// easy to unit-test against fixtures.
pub fn is_update_available(current: &Version, manifest: &Manifest) -> Option<AvailableUpdate> {
    let manifest_version = Version::parse(manifest.version.trim_start_matches('v')).ok()?;
    if manifest_version <= *current {
        return None;
    }
    Some(AvailableUpdate {
        version: manifest.version.clone(),
        notes: manifest.notes.clone(),
        pub_date: manifest.pub_date.clone(),
    })
}

/// This machine's key into `Manifest::portable`, matching the bundler's own
/// `--target` strings (`bundler/src/portable.rs`) — the two vocabularies
/// must stay in sync by hand, since one is Rust `env::consts` and the other
/// a CLI flag. `None` on an unmapped OS/ARCH pair (e.g. a future
/// Windows-ARM build) rather than guessing, so an unknown combination just
/// reports "no update available" instead of downloading the wrong binary.
pub fn portable_target_key() -> Option<&'static str> {
    map_platform(std::env::consts::OS, std::env::consts::ARCH)
}

fn map_platform(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("macos-arm64"),
        ("macos", "x86_64") => Some("macos-x64"),
        ("windows", "x86_64") => Some("win-vulkan-x64"),
        ("linux", "x86_64") => Some("ubuntu-vulkan-x64"),
        ("linux", "aarch64") => Some("ubuntu-vulkan-arm64"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str) -> Manifest {
        Manifest {
            version: version.to_string(),
            notes: "notes".to_string(),
            pub_date: "2026-01-01T00:00:00Z".to_string(),
            platforms: HashMap::new(),
            portable: HashMap::new(),
        }
    }

    /// The version this build actually reports (`env!("CARGO_PKG_VERSION")`).
    /// The comparison fixtures below derive from it instead of hardcoding
    /// literals, so a workspace version bump can never leave a stale "0.1.0"
    /// in the update tests — they always describe whatever build they run in.
    fn current() -> Version {
        current_version()
    }

    /// The smallest version that is strictly newer than `current` (patch+1).
    fn next_patch(current: &Version) -> Version {
        Version::new(current.major, current.minor, current.patch + 1)
    }

    #[test]
    fn newer_version_is_available() {
        let current = current();
        let newer = next_patch(&current);
        let update = is_update_available(&current, &manifest(&newer.to_string())).unwrap();
        assert_eq!(update.version, newer.to_string());
    }

    #[test]
    fn same_version_is_not_available() {
        let current = current();
        assert!(is_update_available(&current, &manifest(&current.to_string())).is_none());
    }

    #[test]
    fn older_version_is_not_available() {
        let current = current();
        let older = next_patch(&current);
        assert!(is_update_available(&older, &manifest(&current.to_string())).is_none());
    }

    #[test]
    fn a_leading_v_is_tolerated() {
        let current = current();
        let newer = next_patch(&current);
        let update = is_update_available(&current, &manifest(&format!("v{newer}"))).unwrap();
        assert_eq!(update.version, format!("v{newer}"));
    }

    #[test]
    fn malformed_manifest_version_is_not_available() {
        let current = current();
        assert!(is_update_available(&current, &manifest("not-a-version")).is_none());
    }

    #[test]
    fn map_platform_covers_every_key_the_bundler_produces() {
        assert_eq!(map_platform("macos", "aarch64"), Some("macos-arm64"));
        assert_eq!(map_platform("macos", "x86_64"), Some("macos-x64"));
        assert_eq!(map_platform("windows", "x86_64"), Some("win-vulkan-x64"));
        assert_eq!(map_platform("linux", "x86_64"), Some("ubuntu-vulkan-x64"));
        assert_eq!(
            map_platform("linux", "aarch64"),
            Some("ubuntu-vulkan-arm64")
        );
    }

    #[test]
    fn map_platform_is_none_for_an_unshipped_combination() {
        assert_eq!(map_platform("windows", "aarch64"), None);
        assert_eq!(map_platform("freebsd", "x86_64"), None);
    }
}
