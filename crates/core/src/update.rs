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
//! see the "Auto-Updater" plan for the accepted rollout gate. This includes
//! the unsafe-version gate in [`is_version_unsafe`]: the same fetch that
//! can't reach a private feed today is what decides whether a version gets
//! gated, so real users see no gate until the feed is public.
//!
//! # The unsafe declaration (fail-open)
//!
//! The manifest may declare versions unsafe via `minimum_version` (semver)
//! and `blocked_versions` (an exact-match list). This is what lets a
//! published critical flaw *block* an old install instead of just softly
//! offering a fix. The declaration is load-bearing, so its posture is
//! explicit: **the gate fails open.** A manifest that cannot be fetched
//! (offline, outage) or that carries no declaration never gates anyone — a
//! networking blip must not silently lock a local-first app, and nothing can
//! stop a determined user of an open-source client anyway, so the gate is a
//! strong nudge, not DRM. Only a successfully fetched manifest that
//! positively names the running version as unsafe gates it.

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
    /// The unsafe declaration: the lowest app version still supported (any
    /// strictly-lower install is gated), semver. Absent in a manifest
    /// published before the declaration existed — and that absence means
    /// "nothing is unsafe" (see the module doc's fail-open posture).
    #[serde(default)]
    pub minimum_version: Option<String>,
    /// The unsafe declaration, blocked list: app versions exact-matched here
    /// (semver) are gated even when they're at or above `minimum_version`.
    #[serde(default)]
    pub blocked_versions: Vec<String>,
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

/// Whether `current` is *declared unsafe* by `manifest`'s unsafe declaration
/// (see the module doc): an exact match on `blocked_versions`, or a version
/// strictly below `minimum_version`. Pure and offline, same as
/// [`is_update_available`] — this is the gate's whole decision, easy to
/// unit-test against fixtures.
///
/// Fails open, on purpose: a manifest with no declaration, an unparseable
/// `minimum_version`, or an unparseable blocked entry never counts as
/// "unsafe". A blocked-list match wins even when the version is at or above
/// `minimum_version` — an explicitly named bad version is bad regardless of
/// the floor. A `current` that's *ahead* of the declaration (e.g. a local
/// dev build) is safe, exactly as `is_update_available` treats an
/// ahead-of-manifest install as "no update".
pub fn is_version_unsafe(current: &Version, manifest: &Manifest) -> bool {
    let blocked = manifest.blocked_versions.iter().any(|v| {
        Version::parse(v.trim_start_matches('v'))
            .map(|blocked| blocked == *current)
            .unwrap_or(false)
    });
    if blocked {
        return true;
    }
    let Some(minimum) = manifest.minimum_version.as_deref() else {
        return false;
    };
    match Version::parse(minimum.trim_start_matches('v')) {
        Ok(minimum) => *current < minimum,
        Err(_) => false,
    }
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
            minimum_version: None,
            blocked_versions: Vec::new(),
        }
    }

    /// `manifest` with the unsafe declaration filled in.
    fn unsafe_manifest(minimum_version: Option<&str>, blocked_versions: &[&str]) -> Manifest {
        Manifest {
            minimum_version: minimum_version.map(str::to_string),
            blocked_versions: blocked_versions.iter().map(|v| (*v).to_string()).collect(),
            ..manifest("9.9.9")
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

    /// The largest version strictly older than `current`, built from a
    /// `saturating_sub` so it never underflows — if the workspace version is
    /// ever `0.0.0`, `prev_patch` degenerates to equality, which the safety
    /// assertions below still hold for (equal-to-minimum is safe, and a
    /// blocked entry can still be tested independently).
    fn prev_patch(current: &Version) -> Version {
        Version::new(
            current.major,
            current.minor,
            current.patch.saturating_sub(1),
        )
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

    // -- is_version_unsafe -------------------------------------------------

    #[test]
    fn version_below_minimum_is_unsafe() {
        let current = current();
        // Minimum is current+2 patches: current is below the floor.
        let minimum = Version::new(current.major, current.minor, current.patch + 2);
        let manifest = unsafe_manifest(Some(&minimum.to_string()), &[]);
        assert!(is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn version_equal_to_minimum_is_safe() {
        let current = current();
        let manifest = unsafe_manifest(Some(&current.to_string()), &[]);
        assert!(!is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn version_above_minimum_is_safe() {
        // The manifest's floor is one patch below current, so current reads
        // as ahead-of-manifest — same "installed is fine" answer as a local
        // dev build newer than anything published.
        let previous = prev_patch(&current()).to_string();
        let manifest = unsafe_manifest(Some(&previous), &[]);
        assert!(!is_version_unsafe(&current(), &manifest));
    }

    #[test]
    fn blocked_version_is_unsafe_even_above_minimum() {
        // The floor is below current (so it would be safe by the floor
        // alone), but current itself is on the blocked list — the exact list
        // wins over the minimum.
        let current = current();
        let previous = prev_patch(&current).to_string();
        let manifest = unsafe_manifest(Some(&previous), &[&current.to_string()]);
        assert!(is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn non_blocked_version_is_safe() {
        let current = current();
        let manifest = unsafe_manifest(None, &[&next_patch(&current).to_string()]);
        assert!(!is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn no_declaration_is_safe() {
        // A manifest published before the declaration existed (all fields
        // serde-default/absent) must never gate anyone — fail open.
        let current = current();
        assert!(!is_version_unsafe(
            &current,
            &manifest(&current.to_string())
        ));
    }

    #[test]
    fn malformed_minimum_is_safe() {
        let current = current();
        let manifest = unsafe_manifest(Some("not-a-version"), &[]);
        assert!(!is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn malformed_blocked_entries_are_ignored() {
        let current = current();
        let manifest = unsafe_manifest(None, &["not-a-version"]);
        assert!(!is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn leading_v_is_tolerated_in_the_declaration() {
        let current = current();
        let minimum = next_patch(&next_patch(&current));
        let manifest = unsafe_manifest(Some(&format!("v{minimum}")), &[&format!("v{minimum}")]);
        assert!(is_version_unsafe(&current, &manifest));
    }

    #[test]
    fn version_missing_from_manifest_is_never_auto_blocked() {
        // A `current` the manifest's `version` field doesn't mention must
        // still be classifiable by the declaration alone — this pins that the
        // gate never leans on the "newer" logic: a *blocked* version keeps
        // reading as unsafe even though `is_update_available` for it would be
        // a no-op.
        let current = current();
        let manifest = unsafe_manifest(
            Some(&next_patch(&current).to_string()),
            &[&current.to_string()],
        );
        assert!(is_version_unsafe(&current, &manifest));
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
