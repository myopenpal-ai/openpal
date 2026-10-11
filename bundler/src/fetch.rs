//! `fetch-engine`: download a llama.cpp release and stage the server binaries
//! into `crates/app/resources/llama/` (or `--dest <dir>` when given — a
//! throwaway staging dir for testing a different llama.cpp build without
//! clobbering the shipped engine, e.g. one that adds a model architecture the
//! current build doesn't know).
//!
//! Behavior contract:
//! 1. Resolve the release: `tag == "latest"` → GET
//!    https://api.github.com/repos/ggml-org/llama.cpp/releases/latest, else
//!    GET .../releases/tags/<tag>. Use `curl -fsSL` with a User-Agent header
//!    (GitHub requires one) and parse with serde_json.
//! 2. The resolved release may be a lightweight "pointer" release whose
//!    only relevant asset is `nightly-tag.txt` (llama.cpp's release
//!    automation publishes these instead of attaching binaries directly,
//!    e.g. for the bare "latest" tag). When no asset matches our target but
//!    `nightly-tag.txt` is present, download its content (the real
//!    build-numbered tag, e.g. "b10621") and re-resolve the release via GET
//!    .../releases/tags/<that tag>, replacing the release/assets used
//!    below. One level of indirection only: an empty nightly-tag.txt, or a
//!    re-resolved release that is *also* a pointer, is an error rather than
//!    a longer chain to follow.
//! 3. Find the asset whose name matches the target: for "macos-arm64" the
//!    asset is named like `llama-<tag>-bin-macos-arm64.tar.gz` (verified on
//!    b9606; older releases used .zip). Error with the list of available
//!    asset names if not found.
//! 4. Download with `curl -fL --progress-bar` to a temp dir
//!    (std::env::temp_dir()/openpal-bundler); extract with `tar -xzf` (or
//!    `unzip -q -o` if the asset is a .zip).
//! 5. Verified layout (b9606): a single flat `llama-<tag>/` directory holding
//!    the server binary, many other `llama-*` tools we don't need, and the
//!    shared libraries it needs (libggml*, libllama*, libmtmd*, per-tool
//!    `lib*-impl`). The server name and library extension follow the target's
//!    OS family: `llama-server` + `*.dylib` (macOS), `llama-server` + `*.so`
//!    (Linux), `llama-server.exe` + `*.dll` (Windows). Linux ships each `.so`
//!    as a SONAME-versioned chain (`libfoo.so` -> `libfoo.so.0` ->
//!    `libfoo.so.0.1.2`, the real file) — the loader needs the exact
//!    `libfoo.so.0` name, so matching must treat "so"/"dylib"/"dll" as a
//!    dot-separated component, not just a trailing suffix. Stage the server
//!    and EVERY matching library into paths::engine_dir(), wiping any
//!    previous contents first (preserve `.gitkeep`). Don't hardcode the
//!    directory name: search the extraction dir for whichever directory
//!    contains the server.
//! 6. `chmod +x` the server binary on Unix (Windows has no executable bit);
//!    libraries keep the permission bits `fs::copy` preserves.
//! 7. Write `engine-version.json` into the engine dir:
//!    `{ "tag": "...", "asset": "...", "fetchedAt": "<iso8601 via date -u>" }`.
//! 8. Smoke test: run `<engine_dir>/llama-server --version` (or --help) and
//!    fail loudly if it doesn't execute (catches dylib/rpath problems early).
//!    Skip the smoke test with a warning when the host can't run the target
//!    (e.g. fetching arm64 binaries on an Intel build machine).
//! 9. If `!force` and engine-version.json already matches the requested tag,
//!    print "already staged" and skip everything.
//!
//! Print progress lines for each step ("Resolving latest llama.cpp release…",
//! "Staged llama-server b5600 (14 files, 38 MB)").

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};

use crate::paths;

const RELEASES_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases";
const USER_AGENT: &str = "openpal-bundler";

/// File name of the staged server binary (`.exe` on Windows) — used to check
/// an engine dir is actually staged; mirrors `paths::engine_binary`.
#[cfg(windows)]
const ENGINE_BINARY: &str = "llama-server.exe";
#[cfg(not(windows))]
const ENGINE_BINARY: &str = "llama-server";

pub fn run(tag: &str, target: &str, force: bool, dest: Option<&Path>) -> anyhow::Result<()> {
    // Where the staged engine lands: `--dest`, or the app's default engine dir.
    let engine_dir = dest
        .map(Path::to_path_buf)
        .unwrap_or_else(paths::engine_dir);

    // Fast path: an explicit tag already staged *for this same target* needs no
    // network. The target check matters because the engine dir is shared across
    // OS/arch/backend variants.
    if !force && tag != "latest" && already_staged(&engine_dir, tag, target) {
        println!(
            "Engine {tag} ({target}) already staged in {} — nothing to do (use --force to re-fetch).",
            engine_dir.display()
        );
        return Ok(());
    }

    // 1. Resolve the release.
    if tag == "latest" {
        println!("Resolving latest llama.cpp release…");
    } else {
        println!("Resolving llama.cpp release {tag}…");
    }
    let url = if tag == "latest" {
        format!("{RELEASES_API}/latest")
    } else {
        format!("{RELEASES_API}/tags/{tag}")
    };
    let body = github_api(&url)?;
    let mut release: serde_json::Value =
        serde_json::from_str(&body).context("failed to parse the GitHub API response as JSON")?;
    let mut resolved_tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| {
            anyhow!("GitHub API response has no tag_name — does release \"{tag}\" exist?")
        })?
        .to_string();
    println!("Resolved release: {resolved_tag}");

    if !force && already_staged(&engine_dir, &resolved_tag, target) {
        println!(
            "Engine {resolved_tag} ({target}) already staged in {} — nothing to do (use --force to re-fetch).",
            engine_dir.display()
        );
        return Ok(());
    }

    // 2. Follow a "pointer" release (its only relevant asset is
    //    nightly-tag.txt) to the real, asset-bearing release named inside
    //    that file.
    {
        let assets = release["assets"]
            .as_array()
            .ok_or_else(|| anyhow!("release {resolved_tag} has no assets array"))?;
        if find_target_asset(assets, target).is_none() {
            if let Some(pointer) = find_nightly_pointer(assets) {
                println!("Release {resolved_tag} points to a nightly build; following…");
                let pointer_url = pointer["browser_download_url"].as_str().ok_or_else(|| {
                    anyhow!(
                        "nightly-tag.txt asset in release {resolved_tag} has no browser_download_url"
                    )
                })?;
                let nightly_tag = github_api(pointer_url)?.trim().to_string();
                if nightly_tag.is_empty() {
                    bail!(
                        "nightly-tag.txt in release {resolved_tag} was empty — can't resolve the real release"
                    );
                }

                let nightly_body = github_api(&format!("{RELEASES_API}/tags/{nightly_tag}"))
                    .with_context(|| {
                        format!(
                            "nightly-tag.txt in release {resolved_tag} pointed to tag \"{nightly_tag}\""
                        )
                    })?;
                let nightly_release: serde_json::Value = serde_json::from_str(&nightly_body)
                    .context("failed to parse the GitHub API response as JSON")?;
                let nightly_resolved_tag = nightly_release["tag_name"]
                    .as_str()
                    .ok_or_else(|| {
                        anyhow!(
                            "GitHub API response has no tag_name — does release \"{nightly_tag}\" exist?"
                        )
                    })?
                    .to_string();
                let nightly_assets = nightly_release["assets"]
                    .as_array()
                    .ok_or_else(|| anyhow!("release {nightly_resolved_tag} has no assets array"))?;
                if find_target_asset(nightly_assets, target).is_none()
                    && find_nightly_pointer(nightly_assets).is_some()
                {
                    bail!(
                        "release {nightly_resolved_tag} (resolved from nightly-tag.txt in {resolved_tag}) is itself a pointer release; refusing to follow more than one level of indirection"
                    );
                }
                println!("Resolved nightly release: {nightly_resolved_tag}");

                release = nightly_release;
                resolved_tag = nightly_resolved_tag;
            }
        }
    }

    // Pointer-following above may have changed resolved_tag from the
    // pointer release's own tag (e.g. "v0.3.0") to the real build tag it
    // pointed to (e.g. "b10621") — re-check "already staged" against that
    // final tag too. Without this, `--tag latest` (whose resolved release
    // is now always a pointer, per the step above) would never hit the fast
    // path above and would re-download+re-extract on every single
    // invocation even when the currently staged engine is already current.
    if !force && already_staged(&engine_dir, &resolved_tag, target) {
        println!(
            "Engine {resolved_tag} ({target}) already staged in {} — nothing to do (use --force to re-fetch).",
            engine_dir.display()
        );
        return Ok(());
    }

    // 3. Find the asset for the requested target.
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| anyhow!("release {resolved_tag} has no assets array"))?;
    let asset = find_target_asset(assets, target).ok_or_else(|| {
        let names: Vec<&str> = assets.iter().filter_map(|a| a["name"].as_str()).collect();
        anyhow!(
            "no asset for target \"{target}\" in release {resolved_tag}; available assets:\n  {}",
            names.join("\n  ")
        )
    })?;
    let asset_name = asset["name"].as_str().expect("checked above").to_string();
    let asset_url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow!("asset {asset_name} has no browser_download_url"))?
        .to_string();

    // 4. Download and extract into a scratch dir.
    let work = std::env::temp_dir().join("openpal-bundler");
    if work.exists() {
        fs::remove_dir_all(&work).with_context(|| format!("failed to clear {}", work.display()))?;
    }
    let extract_dir = work.join("extract");
    fs::create_dir_all(&extract_dir)
        .with_context(|| format!("failed to create {}", extract_dir.display()))?;
    let archive = work.join(&asset_name);

    println!("Downloading {asset_name}…");
    let status = Command::new("curl")
        .args(["-fL", "--progress-bar", "-A", USER_AGENT, "-o"])
        .arg(&archive)
        .arg(&asset_url)
        .status()
        .context("failed to run curl — is it installed and on PATH?")?;
    if !status.success() {
        bail!("download failed: {asset_url} — check your network connection and retry");
    }

    println!("Extracting {asset_name}…");
    extract_archive(&archive, &extract_dir)?;

    // 5. Locate the directory that holds the server binary and stage the files.
    let (server_name, lib_ext) = artifact_names(target);
    let src_dir = find_dir_containing(&extract_dir, server_name).ok_or_else(|| {
        anyhow!("{server_name} not found anywhere inside the extracted {asset_name} — the release layout may have changed")
    })?;

    fs::create_dir_all(&engine_dir)
        .with_context(|| format!("failed to create {}", engine_dir.display()))?;
    println!("Staging into {}…", engine_dir.display());
    for entry in fs::read_dir(&engine_dir)? {
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

    let mut staged_files = 0usize;
    let mut staged_bytes = 0u64;
    for entry in fs::read_dir(&src_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().to_string();
        if name == server_name || is_engine_library(&name, lib_ext) {
            let dest = engine_dir.join(&file_name);
            // fs::copy preserves the source permission bits, so libraries keep theirs.
            staged_bytes += fs::copy(entry.path(), &dest)
                .with_context(|| format!("failed to copy {name} into the engine dir"))?;
            staged_files += 1;
        }
    }
    let engine_binary = engine_dir.join(server_name);
    if !engine_binary.exists() {
        bail!(
            "staging finished but {} is missing — the release layout may have changed",
            engine_binary.display()
        );
    }

    // 6. Ensure the server binary is executable. Unix only: Windows has no
    //    executable bit and `PermissionsExt` isn't available there.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&engine_binary)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&engine_binary, perms)
            .with_context(|| format!("failed to chmod +x {}", engine_binary.display()))?;
    }

    // 7. Record what was staged. `target` lets later steps tell which OS/arch/
    //    backend variant is on disk — the engine dir is shared, so a same-OS
    //    different-target engine must not be silently reused.
    let meta = serde_json::json!({
        "tag": resolved_tag,
        "target": target,
        "asset": asset_name,
        "fetchedAt": iso8601_utc_now(),
    });
    let version_file = engine_dir.join("engine-version.json");
    fs::write(&version_file, serde_json::to_string_pretty(&meta)? + "\n")
        .with_context(|| format!("failed to write {}", version_file.display()))?;

    // 8. Smoke test — catches dylib/rpath problems immediately.
    if host_can_run(target) {
        println!("Smoke testing: llama-server --version…");
        smoke_test(&engine_binary)?;
    } else {
        println!("⚠ Skipping smoke test: this host cannot execute {target} binaries.");
    }

    fs::remove_dir_all(&work).ok();
    println!(
        "Staged llama-server {resolved_tag} ({staged_files} files, {:.1} MB)",
        staged_bytes as f64 / 1_000_000.0
    );
    Ok(())
}

/// Find the asset in `assets` that matches `target`: for "macos-arm64" that's
/// a name like `llama-<tag>-bin-macos-arm64.tar.gz` (or `.zip`). Pure and
/// unit-testable — reused both to detect a "pointer" release (no match, see
/// [`find_nightly_pointer`]) and for the real lookup in `run()`, so the two
/// never drift apart.
fn find_target_asset<'a>(
    assets: &'a [serde_json::Value],
    target: &str,
) -> Option<&'a serde_json::Value> {
    let needle = format!("-bin-{target}.");
    assets.iter().find(|a| {
        a["name"]
            .as_str()
            .is_some_and(|n| n.contains(&needle) && (n.ends_with(".tar.gz") || n.ends_with(".zip")))
    })
}

/// Find a `nightly-tag.txt` asset in `assets`, if present. llama.cpp's
/// release automation publishes some releases (e.g. the bare "latest" tag)
/// as a lightweight pointer carrying only this file, whose content is the
/// real build-numbered tag that has the actual binaries.
fn find_nightly_pointer(assets: &[serde_json::Value]) -> Option<&serde_json::Value> {
    assets
        .iter()
        .find(|a| a["name"].as_str() == Some("nightly-tag.txt"))
}

/// Extract a `.zip` or `.tar.gz` engine archive into `dest`. Zips use `unzip`,
/// falling back to `tar -xf` (Windows' built-in bsdtar reads zips) when `unzip`
/// isn't installed — GitHub's Windows runners don't ship it. `.tar.gz` uses
/// `tar` directly.
fn extract_archive(archive: &Path, dest: &Path) -> anyhow::Result<()> {
    let is_zip = archive
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".zip"));

    if is_zip {
        let unzip = Command::new("unzip")
            .args(["-q", "-o"])
            .arg(archive)
            .arg("-d")
            .arg(dest)
            .status();
        match unzip {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => bail!(
                "unzip failed ({s}) for {} — the download may be corrupt; retry with --force",
                archive.display()
            ),
            Err(_) => {
                // `unzip` not found — fall back to bsdtar, which extracts zips.
                println!("  unzip not found; falling back to tar…");
            }
        }
    }

    let status = Command::new("tar")
        .arg(if is_zip { "-xf" } else { "-xzf" })
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .status()
        .context("failed to run the archive extractor (tar/unzip)")?;
    if !status.success() {
        bail!(
            "failed to extract {} — the download may be corrupt; retry with --force",
            archive.display()
        );
    }
    Ok(())
}

/// GET a GitHub API URL via curl, with the User-Agent header GitHub requires.
/// Authenticates with `GITHUB_TOKEN`/`GH_TOKEN` when set (e.g. in CI) to avoid
/// the much stricter unauthenticated rate limit that otherwise causes
/// intermittent 403s.
fn github_api(url: &str) -> anyhow::Result<String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-fsSL", "-A", USER_AGENT]);
    if let Ok(token) = std::env::var("GITHUB_TOKEN").or_else(|_| std::env::var("GH_TOKEN")) {
        cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    let out = cmd
        .arg(url)
        .output()
        .context("failed to run curl — is it installed and on PATH?")?;
    if !out.status.success() {
        bail!(
            "GitHub API request failed: {url}\n{}\nCheck the release tag and your network connection.",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// Whether an engine already staged in `engine_dir` matches `tag`/`target`
/// exactly — the "nothing to do" fast path, checked both before and after the
/// pointer-following step above (`resolved_tag` can change there, from a
/// pointer release's own tag to the real build tag it named).
fn already_staged(engine_dir: &Path, tag: &str, target: &str) -> bool {
    staged_tag(engine_dir).as_deref() == Some(tag)
        && staged_target(engine_dir).as_deref() == Some(target)
}

/// Tag recorded in engine-version.json, if an engine binary is staged.
fn staged_tag(engine_dir: &Path) -> Option<String> {
    staged_field(engine_dir, "tag")
}

/// Target recorded in engine-version.json. `None` for engines staged by an older
/// bundler that didn't record it — treated as "unknown", so callers re-fetch.
fn staged_target(engine_dir: &Path) -> Option<String> {
    staged_field(engine_dir, "target")
}

fn staged_field(engine_dir: &Path, key: &str) -> Option<String> {
    if !engine_dir.join(ENGINE_BINARY).exists() {
        return None;
    }
    let raw = fs::read_to_string(engine_dir.join("engine-version.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    Some(v[key].as_str()?.to_string())
}

/// Whether a staged engine binary exists *and* was fetched for `target`. The
/// Portable build uses this so it never packages a wrong-arch engine that
/// happens to be staged from an earlier build of a different target.
pub fn is_staged_for(target: &str) -> bool {
    paths::engine_binary().exists()
        && staged_target(&paths::engine_dir()).as_deref() == Some(target)
}

/// Depth-first search for the directory that directly contains `file_name`.
fn find_dir_containing(dir: &Path, file_name: &str) -> Option<PathBuf> {
    if dir.join(file_name).is_file() {
        return Some(dir.to_path_buf());
    }
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_dir_containing(&path, file_name) {
                return Some(found);
            }
        }
    }
    None
}

/// Server binary name and shared-library extension for an engine `target`
/// (e.g. "macos-arm64", "win-vulkan-x64", "ubuntu-vulkan-x64"). Both follow the
/// target's OS family, which is what the release archive ships.
fn artifact_names(target: &str) -> (&'static str, &'static str) {
    if target.contains("win") {
        ("llama-server.exe", "dll")
    } else if target.contains("macos") {
        ("llama-server", "dylib")
    } else {
        // ubuntu / linux (and any other unix-like target)
        ("llama-server", "so")
    }
}

/// Whether `name` is a shared library for `lib_ext` ("so"/"dylib"/"dll").
///
/// Can't just check `name.ends_with(".{lib_ext}")`: Linux ships each library
/// as a SONAME-versioned chain — `libfoo.so` -> `libfoo.so.0` ->
/// `libfoo.so.0.1.2` (the real file) — where the version trails the
/// extension, so only the dev-only unversioned name would ever match and the
/// actual `libfoo.so.0` the binary's DT_NEEDED entry asks for never gets
/// staged. Treating "so" as a dot-separated component (not just a suffix)
/// catches every link in the chain; it also still matches macOS's `.dylib`,
/// which versions the other way (`libfoo.0.1.2.dylib`) and would already
/// satisfy a plain `ends_with`.
fn is_engine_library(name: &str, lib_ext: &str) -> bool {
    name.split('.').any(|part| part == lib_ext)
}

/// The engine target string matching this host's OS/arch (e.g. "macos-arm64"
/// on Apple Silicon) — what `doctor` fetches for when no engine is staged.
/// Windows/Linux default to the Vulkan backend variant; a host that needs a
/// different backend (e.g. CPU-only) should pass an explicit `--target` to
/// `fetch-engine` instead.
pub fn default_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "macos-arm64",
        ("macos", _) => "macos-x64",
        ("windows", _) => "win-vulkan-x64",
        ("linux", "aarch64") => "ubuntu-vulkan-arm64",
        (_, _) => "ubuntu-vulkan-x64",
    }
}

/// Can this host execute binaries built for `target`?
fn host_can_run(target: &str) -> bool {
    let (host_os, host_arch) = (std::env::consts::OS, std::env::consts::ARCH);
    let os_ok = (target.contains("macos") && host_os == "macos")
        || (target.contains("win") && host_os == "windows")
        || ((target.contains("ubuntu") || target.contains("linux")) && host_os == "linux");
    let arch_ok = (target.contains("arm64") && host_arch == "aarch64")
        || ((target.contains("x64") || target.contains("x86_64")) && host_arch == "x86_64");
    os_ok && arch_ok
}

fn smoke_test(binary: &Path) -> anyhow::Result<()> {
    let mut last_error = String::new();
    for flag in ["--version", "--help"] {
        match Command::new(binary).arg(flag).output() {
            Ok(out) if out.status.success() => {
                let text = if out.stdout.is_empty() {
                    out.stderr
                } else {
                    out.stdout
                };
                if let Some(line) = String::from_utf8_lossy(&text)
                    .lines()
                    .find(|l| !l.trim().is_empty())
                {
                    println!("  {}", line.trim());
                }
                return Ok(());
            }
            Ok(out) => {
                last_error = format!(
                    "`llama-server {flag}` exited with {}:\n{}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Err(e) => last_error = format!("failed to execute {}: {e}", binary.display()),
        }
    }
    bail!(
        "engine smoke test failed — the staged llama-server does not run (likely missing dylibs):\n{last_error}\nRe-fetch with: cargo run -p openpal-bundler -- fetch-engine --force"
    )
}

fn iso8601_utc_now() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_target_matches_a_target_this_host_can_run() {
        // Whatever OS/arch this test runs on, `doctor` must pick a target
        // string that same host can actually execute.
        assert!(host_can_run(default_target()));
    }

    #[test]
    fn artifact_names_matches_target_os_family() {
        assert_eq!(
            artifact_names("win-vulkan-x64"),
            ("llama-server.exe", "dll")
        );
        assert_eq!(artifact_names("macos-arm64"), ("llama-server", "dylib"));
        assert_eq!(artifact_names("macos-x64"), ("llama-server", "dylib"));
        assert_eq!(artifact_names("ubuntu-vulkan-x64"), ("llama-server", "so"));
        assert_eq!(
            artifact_names("ubuntu-vulkan-arm64"),
            ("llama-server", "so")
        );
        // Any other unix-like target string falls back to the Linux naming.
        assert_eq!(
            artifact_names("some-future-linux-target"),
            ("llama-server", "so")
        );
    }

    #[test]
    fn is_engine_library_matches_soname_versioned_linux_libs() {
        // Real names from llama.cpp b10088's ubuntu-vulkan-x64 asset: the
        // unversioned dev symlink, the SONAME symlink, and the real
        // versioned file all need to be staged so the binary's DT_NEEDED
        // entry (the SONAME form) resolves at runtime.
        assert!(is_engine_library("libllama-common.so", "so"));
        assert!(is_engine_library("libllama-common.so.0", "so"));
        assert!(is_engine_library("libllama-common.so.0.0.10088", "so"));
        assert!(is_engine_library("libllama-server-impl.so", "so"));
    }

    #[test]
    fn is_engine_library_matches_prefix_versioned_macos_dylibs() {
        // macOS versions the other way (number before the extension) — still
        // matches since "dylib" is its own trailing dot-component.
        assert!(is_engine_library("libggml-base.0.17.0.dylib", "dylib"));
        assert!(is_engine_library("libllama-common.dylib", "dylib"));
    }

    #[test]
    fn is_engine_library_rejects_unrelated_names() {
        assert!(!is_engine_library("llama-server", "so"));
        assert!(!is_engine_library("ggml-rpc-server", "so"));
        assert!(!is_engine_library("engine-version.json", "so"));
        assert!(!is_engine_library("LICENSE", "so"));
    }

    fn asset(name: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "browser_download_url": format!("https://example.com/{name}"),
        })
    }

    #[test]
    fn find_target_asset_matches_direct_target_asset() {
        let assets = vec![
            asset("llama-b10621-bin-ubuntu-vulkan-x64.tar.gz"),
            asset("llama-b10621-bin-macos-arm64.tar.gz"),
        ];
        let found = find_target_asset(&assets, "ubuntu-vulkan-x64").unwrap();
        assert_eq!(
            found["name"].as_str(),
            Some("llama-b10621-bin-ubuntu-vulkan-x64.tar.gz")
        );
        // A real release ships its target asset directly — not a pointer.
        assert!(find_nightly_pointer(&assets).is_none());
    }

    #[test]
    fn find_nightly_pointer_detects_pointer_release() {
        // llama.cpp's "latest" tag today: its only asset is nightly-tag.txt,
        // with no asset for any target.
        let assets = vec![asset("nightly-tag.txt")];
        assert!(find_target_asset(&assets, "ubuntu-vulkan-x64").is_none());
        let pointer = find_nightly_pointer(&assets).unwrap();
        assert_eq!(pointer["name"].as_str(), Some("nightly-tag.txt"));
    }

    #[test]
    fn find_target_asset_and_nightly_pointer_both_absent_is_the_ordinary_no_asset_case() {
        // Neither the target asset nor nightly-tag.txt is present: an
        // ordinary release that simply doesn't ship this target (e.g. a
        // Windows-only release queried for "ubuntu-vulkan-x64"), not a
        // pointer. run() must still surface its existing "no asset for
        // target" error instead of trying to follow a pointer that isn't
        // there.
        let assets = vec![asset("llama-b10621-bin-win-vulkan-x64.tar.gz")];
        assert!(find_target_asset(&assets, "ubuntu-vulkan-x64").is_none());
        assert!(find_nightly_pointer(&assets).is_none());
    }

    #[test]
    fn find_dir_containing_searches_recursively() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("llama-server"), b"").unwrap();

        assert_eq!(
            find_dir_containing(tmp.path(), "llama-server"),
            Some(nested)
        );
        assert_eq!(find_dir_containing(tmp.path(), "does-not-exist"), None);
    }

    #[test]
    fn find_dir_containing_prefers_the_top_level_match() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("llama-server"), b"").unwrap();
        let nested = tmp.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("llama-server"), b"").unwrap();

        assert_eq!(
            find_dir_containing(tmp.path(), "llama-server"),
            Some(tmp.path().to_path_buf())
        );
    }
}
