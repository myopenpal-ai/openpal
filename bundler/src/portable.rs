//! `portable`: assemble the no-install Portable distribution for one target
//! OS/arch (see docs/design/PORTABLE.md).
//!
//! Branches on the `target` string at runtime (no `cfg`), so this whole module
//! compiles on any host; each OS path is meant to run on a matching CI runner.
//! You can build the macOS portable on a Mac today; the Windows and Linux paths
//! need a Windows / Ubuntu-22.04 runner.
//!
//! Layout produced:
//! - macOS: `OpenPal-portable-<target>.zip` = `{ OpenPal.app (ad-hoc signed),
//!   openpal-portable }`. Data lives in a `data/` folder beside the `.app`
//!   at runtime.
//! - Windows: `OpenPal-portable-<target>.zip` = `{ OpenPal.exe, llama/
//!   (engine + MSVC redist DLLs), webview2-runtime/, openpal-portable }`.
//! - Linux: `OpenPal-portable-<target>.AppImage` (self-contained).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context};

use crate::model::CatalogModel;
use crate::{fetch, licensing, model, paths};

/// Marker file that switches the app into portable mode at runtime. Must match
/// `PORTABLE_MARKER` in `crates/app/src/state.rs`.
const MARKER: &str = "openpal-portable";

pub fn run(
    target: &str,
    debug: bool,
    webview2: Option<&Path>,
    model_id: Option<&str>,
    accept_license: bool,
) -> anyhow::Result<()> {
    println!("Building the Portable distribution for {target}…");

    // 0. Resolve and license-check the optional pre-staged model up front, so a
    //    bad id or an un-accepted license fails before the long build.
    let model = match model_id {
        Some(id) => {
            let m = model::resolve(id)?;
            model::check_license(&m, accept_license)?;
            println!("Will pre-stage model: {} ({})", m.name, m.license);
            Some(m)
        }
        None => None,
    };

    // 1. Engine for this target (staged into crates/app/resources/llama). Reuse
    //    only an engine staged for THIS target — the engine dir is shared, so a
    //    leftover same-OS different-arch/backend engine must not be packaged.
    if fetch::is_staged_for(target) {
        println!(
            "Engine for {target} already staged in {} — reusing it.",
            paths::engine_dir().display()
        );
    } else {
        fetch::run("latest", target, false, None)?;
    }

    // 2. Build + assemble for this OS family.
    let out = if target.contains("macos") {
        build_macos(target, debug, model.as_ref(), accept_license)?
    } else if target.contains("win") {
        build_windows(target, debug, webview2, model.as_ref(), accept_license)?
    } else {
        build_linux(target, debug, model.as_ref(), accept_license)?
    };

    println!("\n✓ Portable build ready: {}", out.display());
    println!("  Unpack and run — no installation; data lives beside the app.");
    Ok(())
}

/// Stage `LICENSE`, a generated `NOTICE`, and (when requested) the pre-staged
/// model into a bundle's staging dir. Shared by every OS path; the model lands
/// in a read-only `models/` folder the app discovers via `bundled_models_dir`.
fn stage_sidecars(
    stage: &Path,
    target: &str,
    model: Option<&CatalogModel>,
    accept_license: bool,
) -> anyhow::Result<()> {
    licensing::write_license(stage)?;
    licensing::write_notice(stage, target, model)?;
    if let Some(m) = model {
        model::stage(m, &stage.join("models"), accept_license)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS: build the .app, ad-hoc sign it, drop the marker beside it, zip.
// ---------------------------------------------------------------------------

fn build_macos(
    target: &str,
    debug: bool,
    model: Option<&CatalogModel>,
    accept_license: bool,
) -> anyhow::Result<PathBuf> {
    tauri_build(&["--config", "tauri.portable.conf.json"], debug)?;
    let built = paths::bundle_dir(debug).join("OpenPal.app");
    if !built.exists() {
        bail!(
            "{} not found after the build — check the tauri output above",
            built.display()
        );
    }

    let stage = stage_dir(target)?;
    let app = stage.join("OpenPal.app");
    copy_app_bundle(&built, &app)?;
    // The marker, licenses and model all sit next to the .app (siblings), so they
    // do not affect the bundle's signature; sign after they are in place.
    fs::write(stage.join(MARKER), b"").context("failed to write the portable marker")?;
    stage_sidecars(&stage, target, model, accept_license)?;
    adhoc_sign(&app)?;
    zip_stage(&stage, target)
}

/// Ad-hoc sign the bundle inside-out (`codesign --sign -`): nested engine
/// binaries first, then the app. Apple Silicon SIGKILLs unsigned code, so this
/// is required even though it isn't notarized.
fn adhoc_sign(app: &Path) -> anyhow::Result<()> {
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

    println!("Ad-hoc signing {} nested engine binaries…", nested.len());
    for file in &nested {
        codesign_adhoc(file)?;
    }
    println!("Ad-hoc signing the app bundle…");
    codesign_adhoc(app)?;

    let status = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .status()
        .context("failed to run codesign — are the Xcode command line tools installed?")?;
    if !status.success() {
        bail!("codesign verification failed for {}", app.display());
    }
    println!("✓ Ad-hoc signature verified: {}", app.display());
    Ok(())
}

fn codesign_adhoc(path: &Path) -> anyhow::Result<()> {
    let status = Command::new("codesign")
        .args(["--force", "--sign", "-"])
        .arg(path)
        .status()
        .context("failed to run codesign — are the Xcode command line tools installed?")?;
    if !status.success() {
        bail!("codesign failed for {}", path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows: build the bare exe, assemble exe + engine + WebView2 + marker, zip.
// ---------------------------------------------------------------------------

fn build_windows(
    target: &str,
    debug: bool,
    webview2: Option<&Path>,
    model: Option<&CatalogModel>,
    accept_license: bool,
) -> anyhow::Result<PathBuf> {
    tauri_build(
        &["--no-bundle", "--config", "tauri.portable.conf.json"],
        debug,
    )?;
    let exe = find_app_exe(debug)?;

    let stage = stage_dir(target)?;
    fs::copy(&exe, stage.join("OpenPal.exe"))
        .with_context(|| format!("failed to copy {}", exe.display()))?;

    // Engine (llama-server.exe + ggml-*.dll) staged in resources/llama, plus the
    // MSVC runtime DLLs the engine links but Windows doesn't guarantee.
    let llama = stage.join("llama");
    copy_dir_recursive(&paths::engine_dir(), &llama)?;
    stage_msvc_redists(&llama);

    // Fixed-version WebView2 runtime so the app launches with no system install.
    match webview2 {
        Some(rt) => copy_dir_recursive(rt, &stage.join("webview2-runtime"))?,
        None => println!(
            "⚠ No --webview2 runtime provided. The portable build will not launch until an \
             extracted fixed-version WebView2 runtime is placed in webview2-runtime/."
        ),
    }

    fs::write(stage.join(MARKER), b"").context("failed to write the portable marker")?;
    stage_sidecars(&stage, target, model, accept_license)?;
    zip_stage(&stage, target)
}

/// Locate the bare app `.exe` left by `tauri build --no-bundle`.
fn find_app_exe(debug: bool) -> anyhow::Result<PathBuf> {
    let dir = target_profile_dir(debug);
    for name in ["OpenPal.exe", "openpal-app.exe", "openpal_app.exe"] {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    // Fallback: the first top-level `.exe` (build scripts live under build/).
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|x| x == "exe") {
                return Ok(path);
            }
        }
    }
    bail!(
        "couldn't find the built app .exe in {} — did `tauri build --no-bundle` run?",
        dir.display()
    )
}

/// Copy the MSVC 2015-2022 runtime DLLs the llama.cpp binaries link (and Windows
/// doesn't guarantee) next to `llama-server.exe`. Best-effort.
fn stage_msvc_redists(llama_dir: &Path) {
    let system32 =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
            .join("System32");
    for dll in ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"] {
        let from = system32.join(dll);
        if !from.exists() {
            println!("⚠ {dll} not found in System32 — bundle it manually or llama-server may not launch.");
            continue;
        }
        if let Err(e) = fs::copy(&from, llama_dir.join(dll)) {
            println!("⚠ couldn't stage {dll}: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Linux: build the AppImage (self-contained) and copy it out.
// ---------------------------------------------------------------------------

fn build_linux(
    target: &str,
    debug: bool,
    model: Option<&CatalogModel>,
    accept_license: bool,
) -> anyhow::Result<PathBuf> {
    tauri_build(
        &[
            "--bundles",
            "appimage",
            "--config",
            "tauri.portable.conf.json",
        ],
        debug,
    )?;
    let appimage = find_appimage(debug)?;
    let out = paths::portable_out_dir();
    fs::create_dir_all(&out)?;

    // No pre-staged model: ship the self-contained AppImage as a single file.
    // (Run from anywhere; data goes to the standard XDG location unless a
    // `openpal-portable` marker is placed beside it.)
    if model.is_none() {
        let dest = out.join(format!("OpenPal-portable-{target}.AppImage"));
        fs::copy(&appimage, &dest)
            .with_context(|| format!("failed to copy {}", appimage.display()))?;
        make_executable(&dest);
        return Ok(dest);
    }

    // With a pre-staged model the model file must sit *beside* the AppImage (the
    // app finds it via `$APPIMAGE`'s folder), so we ship a marked folder: the
    // marker turns on portable mode and `models/` is the read-only model source.
    let stage = stage_dir(target)?;
    let app_dest = stage.join("OpenPal.AppImage");
    fs::copy(&appimage, &app_dest)
        .with_context(|| format!("failed to copy {}", appimage.display()))?;
    make_executable(&app_dest);
    fs::write(stage.join(MARKER), b"").context("failed to write the portable marker")?;
    stage_sidecars(&stage, target, model, accept_license)?;
    tar_gz_stage(&stage, target)
}

fn find_appimage(debug: bool) -> anyhow::Result<PathBuf> {
    let dir = target_profile_dir(debug).join("bundle/appimage");
    fs::read_dir(&dir)
        .ok()
        .and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .find(|p| p.extension().is_some_and(|x| x == "AppImage"))
        })
        .ok_or_else(|| {
            anyhow!(
                "no .AppImage found in {} — did the appimage bundle build?",
                dir.display()
            )
        })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Run `npx tauri build` from the app crate (where tauri.conf.json lives) with
/// extra args appended. Mirrors `build.rs` but lets the caller pass `--config`,
/// `--no-bundle`, `--bundles …`.
fn tauri_build(extra: &[&str], debug: bool) -> anyhow::Result<()> {
    println!("Running tauri build {}…", extra.join(" "));
    let mut cmd = crate::node::node_cli("npx");
    cmd.args(["--no-install", "tauri", "build"]);
    if debug {
        cmd.arg("--debug");
    }
    cmd.args(extra);
    let status = cmd
        .current_dir(paths::app_crate_dir())
        .status()
        .context("failed to run npx — run: npm install (at the repo root)")?;
    if !status.success() {
        bail!("tauri build failed — see the output above");
    }
    Ok(())
}

fn target_profile_dir(debug: bool) -> PathBuf {
    paths::repo_root()
        .join("target")
        .join(if debug { "debug" } else { "release" })
}

/// A fresh `<target>/portable/OpenPal-portable-<target>/` staging directory.
fn stage_dir(target: &str) -> anyhow::Result<PathBuf> {
    let dir = paths::portable_out_dir().join(format!("OpenPal-portable-{target}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir)
}

/// Zip a staged folder into `OpenPal-portable-<target>.zip`, keeping the staged
/// folder as the zip root. macOS uses `ditto` (preserves the .app's symlinks,
/// permissions and resource forks). Windows uses the in-box `tar.exe` (bsdtar /
/// libarchive), which infers the zip format from the `.zip` suffix and writes
/// ZIP64 — unlike Windows PowerShell 5.1's `Compress-Archive`, which fails on
/// any member over 2 GB ("Stream was too long"), exactly the pre-staged-model
/// case.
fn zip_stage(stage: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let zip = paths::portable_out_dir().join(format!("OpenPal-portable-{target}.zip"));
    let _ = fs::remove_file(&zip);
    println!("Zipping {}…", zip.display());

    let status = if target.contains("macos") {
        Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(stage)
            .arg(&zip)
            .status()
            .context("failed to run ditto")?
    } else {
        // `tar -a -c -f out.zip -C <parent> <stage-name>`: -a picks the format
        // from the `.zip` suffix; -C keeps the staged folder as the archive root.
        let parent = stage
            .parent()
            .ok_or_else(|| anyhow!("stage dir has no parent"))?;
        let name = stage
            .file_name()
            .ok_or_else(|| anyhow!("stage dir has no name"))?;
        Command::new("tar")
            .arg("-a")
            .arg("-c")
            .arg("-f")
            .arg(&zip)
            .arg("-C")
            .arg(parent)
            .arg(name)
            .status()
            .context("failed to run tar — Windows 10 1803+ ships it (bsdtar) in System32")?
    };
    if !status.success() {
        bail!("zip step failed for {}", zip.display());
    }
    Ok(zip)
}

/// Archive a staged folder into `OpenPal-portable-<target>.tar.gz` (Linux),
/// preserving the executable bit on the AppImage. The staged folder is kept as
/// the archive's top-level directory (`-C` its parent, archive its name).
fn tar_gz_stage(stage: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let archive = paths::portable_out_dir().join(format!("OpenPal-portable-{target}.tar.gz"));
    let _ = fs::remove_file(&archive);
    let parent = stage
        .parent()
        .ok_or_else(|| anyhow!("stage dir has no parent"))?;
    let name = stage
        .file_name()
        .ok_or_else(|| anyhow!("stage dir has no name"))?;
    println!("Archiving {}…", archive.display());
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(parent)
        .arg(name)
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        bail!("tar step failed for {}", archive.display());
    }
    Ok(archive)
}

/// Copy a macOS `.app` with `ditto` (preserves symlinks/permissions a plain
/// recursive file copy would flatten).
fn copy_app_bundle(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let _ = fs::remove_dir_all(dst);
    let status = Command::new("ditto")
        .arg(src)
        .arg(dst)
        .status()
        .context("failed to run ditto")?;
    if !status.success() {
        bail!("failed to copy {} -> {}", src.display(), dst.display());
    }
    Ok(())
}

/// Plain recursive copy for directories of regular files (engine, WebView2).
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to).with_context(|| format!("failed to copy {}", from.display()))?;
        }
    }
    Ok(())
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}
