//! `fetch-model` / `portable --model`: resolve a curated catalog model by id and
//! stage its GGUF weights (and any vision projector) into a Portable build's
//! read-only `models/` folder, so the distribution ships with a model ready to
//! run offline (see docs/design/PORTABLE.md §3).
//!
//! Two guard rails matter here:
//! - **License**: only Apache-2.0 / MIT-style weights embed without friction.
//!   Llama- and Gemma-licensed weights carry pass-through obligations onto the
//!   redistributed artifact, so they require an explicit `--accept-license` and
//!   print what the distributor must then comply with.
//! - **Integrity**: the download is size-checked and, when the catalog records a
//!   SHA-256, hash-verified (best-effort, via the host's hashing tool).

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context};
use serde::Deserialize;

use crate::paths;

/// The subset of a catalog `ModelSpec` the bundler needs. Serde ignores every
/// other field in catalog.json, so this stays in sync without duplicating the
/// whole schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub url: String,
    pub file_name: String,
    pub size_bytes: u64,
    // Shards 2..N of a split GGUF (`extraShards` in catalog.json); empty for
    // every single-file model. `url`/`file_name`/`size_bytes` above always
    // describe shard 1. See `crates/core/src/types.rs`'s `ModelShard`.
    #[serde(default)]
    pub extra_shards: Vec<CatalogShard>,
    #[serde(default)]
    pub sha256: Option<String>,
    pub license: String,
    #[serde(default)]
    pub hidden: bool,
    // Vision models ship a companion multimodal projector that must travel with
    // the weights or the model loads text-only.
    #[serde(default)]
    pub mmproj_url: Option<String>,
    #[serde(default)]
    pub mmproj_file_name: Option<String>,
    #[serde(default)]
    pub mmproj_size_bytes: Option<u64>,
    #[serde(default)]
    pub mmproj_sha256: Option<String>,
}

/// One additional shard of a split GGUF catalog entry — mirrors
/// `crates/core/src/types.rs`'s `ModelShard`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogShard {
    pub url: String,
    pub file_name: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub sha256: Option<String>,
}

impl CatalogModel {
    /// Total download size across every shard — what the "Staging model…"
    /// progress line should show, not just shard 1's own size.
    fn total_size_bytes(&self) -> u64 {
        self.size_bytes + self.extra_shards.iter().map(|s| s.size_bytes).sum::<u64>()
    }
}

/// `fetch-model` subcommand: stage a catalog model into `dest_dir` directly.
/// Used on its own (e.g. to add a model to an already-built bundle's `models/`
/// folder) and internally by `portable --model`.
pub fn run(id: &str, dest_dir: &Path, accept_license: bool) -> anyhow::Result<()> {
    let model = resolve(id)?;
    stage(&model, dest_dir, accept_license)?;
    Ok(())
}

/// Load the catalog and find one model by id. Errors list the embeddable ids so
/// a typo is easy to fix.
pub fn resolve(id: &str) -> anyhow::Result<CatalogModel> {
    let models = load_catalog()?;
    models.iter().find(|m| m.id == id).cloned().ok_or_else(|| {
        let mut ids: Vec<String> = models
            .iter()
            .filter(|m| !m.hidden)
            .map(|m| {
                format!(
                    "{} ({}, {:.2} GB)",
                    m.id,
                    m.license,
                    m.total_size_bytes() as f64 / 1e9
                )
            })
            .collect();
        ids.sort();
        anyhow!(
            "no model with id \"{id}\" in the catalog. Available models:\n  {}",
            ids.join("\n  ")
        )
    })
}

fn load_catalog() -> anyhow::Result<Vec<CatalogModel>> {
    let path = paths::catalog_json();
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read the catalog at {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as the model catalog", path.display()))
}

/// True for the permissive licenses that embed in a redistributable product
/// bundle with at most an attribution notice (which we always generate). Matches
/// on whole tokens, not substrings, so "permitted" doesn't read as MIT and a
/// "Llama … Community License" doesn't slip through; anything outside this
/// allowlist (Llama/Gemma "community"/"terms of use", CC-BY-NC/ND, OpenRAIL use
/// restrictions, …) falls to the `--accept-license` gate, where the operator
/// confirms they will meet that license's terms.
pub fn is_permissive_license(license: &str) -> bool {
    const PERMISSIVE: &[&str] = &[
        "mit",
        "isc",
        "bsd",
        "cc0",
        "unlicense",
        "apache",
        "apache2",
        "mpl",
        "zlib",
    ];
    license
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| PERMISSIVE.contains(&tok))
}

/// Fail fast (before a long build) when a non-permissive model is requested
/// without explicit consent. Safe to call more than once.
pub fn check_license(model: &CatalogModel, accept_license: bool) -> anyhow::Result<()> {
    if is_permissive_license(&model.license) || accept_license {
        return Ok(());
    }
    bail!(
        "\"{}\" is under \"{}\", which imposes pass-through redistribution terms \
         (e.g. shipping the license agreement / \"Built with …\" notices) on the \
         Portable artifact.\nOpenPal will not embed it by default. If you have \
         read that license and will comply, re-run with --accept-license. \
         Otherwise pick an Apache-2.0/MIT model, or ship Portable Lite and let \
         users download it themselves.",
        model.name,
        model.license
    )
}

/// Download the weights (and projector, if any) into `dest_dir`, verifying size
/// and — when known — SHA-256. Skips files already present and complete, so a
/// re-run after a partial build is cheap.
pub fn stage(model: &CatalogModel, dest_dir: &Path, accept_license: bool) -> anyhow::Result<()> {
    check_license(model, accept_license)?;
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("failed to create {}", dest_dir.display()))?;

    println!(
        "Staging model \"{}\" by {} ({}, {:.2} GB)…",
        model.name,
        model.publisher,
        model.license,
        model.total_size_bytes() as f64 / 1e9
    );
    if !is_permissive_license(&model.license) {
        println!(
            "⚠ {} is under \"{}\" — you are redistributing under --accept-license; \
             ensure the bundle complies with that license's pass-through terms.",
            model.name, model.license
        );
    }

    fetch_one(
        &model.url,
        &dest_dir.join(&model.file_name),
        model.size_bytes,
        model.sha256.as_deref(),
    )?;
    for (i, shard) in model.extra_shards.iter().enumerate() {
        println!(
            "Staging shard {} of {}…",
            i + 2,
            model.extra_shards.len() + 1
        );
        fetch_one(
            &shard.url,
            &dest_dir.join(&shard.file_name),
            shard.size_bytes,
            shard.sha256.as_deref(),
        )?;
    }

    if let (Some(url), Some(name)) = (&model.mmproj_url, &model.mmproj_file_name) {
        println!("Staging the vision projector {name}…");
        fetch_one(
            url,
            &dest_dir.join(name),
            model.mmproj_size_bytes.unwrap_or(0),
            model.mmproj_sha256.as_deref(),
        )?;
    }

    println!("✓ Model staged into {}", dest_dir.display());
    Ok(())
}

/// Download one file to `dest`, verifying it afterwards. A file already on disk
/// at the expected size (and hash, when known) is accepted as-is.
fn fetch_one(
    url: &str,
    dest: &Path,
    expected_size: u64,
    sha256: Option<&str>,
) -> anyhow::Result<()> {
    if dest.exists() {
        let len = fs::metadata(dest)?.len();
        let size_ok = expected_size == 0 || len == expected_size;
        // Only trust an existing file if we have *something* to check it against
        // (a size or a hash) and that check passes — otherwise re-download rather
        // than ship something unverified. The hash is computed at most once here.
        let have_check = expected_size != 0 || sha256.is_some();
        if have_check && size_ok {
            match sha256 {
                None => {
                    println!(
                        "  {} already staged (size {len}) — reusing it.",
                        file_name(dest)
                    );
                    return Ok(());
                }
                Some(expected) => match sha256_of(dest) {
                    Some(actual) if actual.eq_ignore_ascii_case(expected) => {
                        println!(
                            "  {} already staged (SHA-256 verified) — reusing it.",
                            file_name(dest)
                        );
                        return Ok(());
                    }
                    Some(_) => {} // hash mismatch — fall through and re-download
                    None => {
                        println!(
                            "  ⚠ no SHA-256 tool found — reusing {} on size alone.",
                            file_name(dest)
                        );
                        return Ok(());
                    }
                },
            }
        }
        println!(
            "  {} present but unverifiable/incomplete — re-downloading.",
            file_name(dest)
        );
        fs::remove_file(dest).ok();
    }

    println!("Downloading {}…", file_name(dest));
    let status = Command::new("curl")
        .args(["-fL", "--progress-bar", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .context("failed to run curl — is it installed and on PATH?")?;
    if !status.success() {
        bail!("download failed: {url} — check your network connection and retry");
    }

    if expected_size != 0 {
        let len = fs::metadata(dest)?.len();
        if len != expected_size {
            bail!(
                "size mismatch for {}: got {len} bytes, expected {expected_size}. The download \
                 may be truncated or the URL may have changed — retry.",
                file_name(dest)
            );
        }
    }
    if let Some(expected) = sha256 {
        match sha256_of(dest) {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => {
                println!("  SHA-256 verified.");
            }
            Some(actual) => bail!(
                "SHA-256 mismatch for {}:\n  expected {expected}\n  got      {actual}\nThe download is corrupt or the catalog hash is stale.",
                file_name(dest)
            ),
            None => println!("  ⚠ no SHA-256 tool found on this host — skipping hash check."),
        }
    }
    Ok(())
}

/// SHA-256 of a file as lowercase hex, using whichever hashing tool the host
/// provides (`sha256sum`/`shasum` on Unix, `certutil` on Windows). `None` if no
/// tool is available — the caller then skips verification with a warning.
fn sha256_of(path: &Path) -> Option<String> {
    // Linux: coreutils `sha256sum <file>` -> "<hex>  <file>".
    if let Some(hex) = run_hash("sha256sum", &[path.as_os_str()]) {
        return Some(hex);
    }
    // macOS (and Perl-equipped Unix): `shasum -a 256 <file>` -> "<hex>  <file>".
    if let Some(hex) = run_hash(
        "shasum",
        &[
            std::ffi::OsStr::new("-a"),
            std::ffi::OsStr::new("256"),
            path.as_os_str(),
        ],
    ) {
        return Some(hex);
    }
    // Windows: `certutil -hashfile <file> SHA256` -> hex on the 2nd line.
    if let Some(hex) = run_hash(
        "certutil",
        &[
            std::ffi::OsStr::new("-hashfile"),
            path.as_os_str(),
            std::ffi::OsStr::new("SHA256"),
        ],
    ) {
        return Some(hex);
    }
    None
}

/// Run a hashing command and pull the 64-char hex digest out of its output.
/// `sha256sum`/`shasum` print the digest as the first token; `certutil` prints
/// it alone on the second line (we strip any spacing).
fn run_hash(cmd: &str, args: &[&std::ffi::OsStr]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if cmd == "certutil" {
        // Line 0 is a header, line 1 is the digest (possibly space-separated).
        let line = text.lines().nth(1)?;
        let hex: String = line.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        return (hex.len() == 64).then_some(hex.to_ascii_lowercase());
    }
    let first = text.split_whitespace().next()?;
    (first.len() == 64 && first.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| first.to_ascii_lowercase())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{check_license, is_permissive_license, CatalogModel};

    fn fixture(license: &str) -> CatalogModel {
        CatalogModel {
            id: "test-model".to_string(),
            name: "Test Model".to_string(),
            publisher: "Test Publisher".to_string(),
            url: "https://example.com/test.gguf".to_string(),
            file_name: "test.gguf".to_string(),
            size_bytes: 1_000,
            extra_shards: Vec::new(),
            sha256: None,
            license: license.to_string(),
            hidden: false,
            mmproj_url: None,
            mmproj_file_name: None,
            mmproj_size_bytes: None,
            mmproj_sha256: None,
        }
    }

    #[test]
    fn permissive_license_never_needs_accept_license() {
        let model = fixture("MIT");
        assert!(check_license(&model, false).is_ok());
        assert!(check_license(&model, true).is_ok());
    }

    #[test]
    fn non_permissive_license_is_gated_without_accept_license() {
        let model = fixture("Llama 3.2 Community License");
        assert!(check_license(&model, false).is_err());
        assert!(check_license(&model, true).is_ok());
    }

    #[test]
    fn permissive_licenses_pass() {
        for ok in [
            "Apache 2.0",
            "Apache-2.0",
            "MIT",
            "MIT License",
            "BSD-3-Clause",
            "ISC",
            "CC0-1.0",
            "The Unlicense",
            "MPL-2.0",
            "zlib",
        ] {
            assert!(is_permissive_license(ok), "{ok:?} should be permissive");
        }
    }

    #[test]
    fn non_permissive_licenses_are_gated() {
        for blocked in [
            "Llama 3.2 Community License",
            "Llama 3.3 Community License",
            "Gemma Terms of Use",
            // Restricted Creative Commons variants must NOT read as permissive.
            "CC-BY-NC-4.0",
            "CC-BY-ND-4.0",
            "CC-BY-NC-SA-4.0",
            "CC-BY-4.0",
            "OpenRAIL-M",
            "Proprietary",
        ] {
            assert!(!is_permissive_license(blocked), "{blocked:?} must be gated");
        }
    }

    #[test]
    fn short_tokens_do_not_substring_match() {
        // Whole-word matching: these contain "mit"/"isc"/"bsd" as substrings but
        // are not those licenses.
        for blocked in [
            "Permitted use only",
            "Submitted works license",
            "Misc terms",
        ] {
            assert!(
                !is_permissive_license(blocked),
                "{blocked:?} must not match"
            );
        }
    }
}
