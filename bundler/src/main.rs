//! OpenPal bundling & deployment CLI.
//!
//! This crate owns everything about *shipping* OpenPal and nothing about how
//! the app works: fetching the llama.cpp engine, staging it into the app's
//! resources, icons, release builds, code signing and notarization. It shells
//! out to standard macOS tooling (curl, unzip, codesign, xcrun) and the Tauri
//! CLI (via npx) instead of reimplementing them.
//!
//! Usage (from anywhere in the repo):
//!   cargo run -p openpal-bundler -- doctor
//!   cargo run -p openpal-bundler -- fetch-engine
//!   cargo run -p openpal-bundler -- icon
//!   cargo run -p openpal-bundler -- build [--debug]
//!   cargo run -p openpal-bundler -- sign --identity "Developer ID Application: ..."
//!   cargo run -p openpal-bundler -- notarize
//!   cargo run -p openpal-bundler -- release
//!   cargo run -p openpal-bundler -- manifest --tag v0.2.0 --portable-dir target/portable-artifacts
//!   cargo run -p openpal-bundler -- engine-manifest --target macos-arm64

mod build;
mod doctor;
mod engine_manifest;
mod fetch;
mod icon;
mod licensing;
mod manifest;
mod model;
mod node;
mod paths;
mod portable;
mod signing;
mod stage_plugins;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "openpal-bundler", about, version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check that every tool needed to build and ship OpenPal is available,
    /// and fetch/stage the engine automatically if it's missing (the usual
    /// reason a fresh checkout's AI won't load).
    Doctor,
    /// Download the llama.cpp engine (llama-server + dylibs) and stage it
    /// into crates/app/resources/llama/ for dev builds and bundling.
    FetchEngine {
        /// llama.cpp release tag, e.g. "b5600". Defaults to the latest release.
        #[arg(long, default_value = "latest")]
        tag: String,
        /// Target platform for the engine binaries.
        #[arg(long, default_value = "macos-arm64")]
        target: String,
        /// Re-download even if an engine is already staged.
        #[arg(long)]
        force: bool,
        /// Stage into this directory instead of the default
        /// `crates/app/resources/llama/` — handy for trying a different
        /// llama.cpp build (e.g. one with newer model-architecture support)
        /// without clobbering the shipped engine.
        #[arg(long)]
        dest: Option<PathBuf>,
    },
    /// Stage the repo's built-in plugin sources into
    /// crates/app/resources/plugins/ so packaged builds can discover them
    /// (run automatically by `build`; exposed for manual/debug use).
    StagePlugins,
    /// Generate the app icon set (procedural source PNG + `tauri icon`).
    Icon {
        /// Only regenerate the 1024px source PNG, skip `tauri icon`.
        #[arg(long)]
        source_only: bool,
    },
    /// Build the app bundle (.app + .dmg) via the Tauri CLI.
    Build {
        /// Build a debug bundle (much faster, for testing the pipeline).
        #[arg(long)]
        debug: bool,
    },
    /// Build the no-install Portable distribution for a target OS/arch
    /// (see docs/design/PORTABLE.md). macOS builds on a Mac; Windows/Linux need a
    /// matching runner.
    Portable {
        /// Engine target / OS-arch, e.g. "macos-arm64", "macos-x64",
        /// "win-vulkan-x64", "win-cpu-arm64", "ubuntu-vulkan-x64".
        #[arg(long, default_value = "macos-arm64")]
        target: String,
        /// Build a debug bundle (much faster, for testing the pipeline).
        #[arg(long)]
        debug: bool,
        /// Path to an extracted fixed-version WebView2 runtime (Windows only).
        #[arg(long)]
        webview2: Option<PathBuf>,
        /// Pre-stage a catalog model by id (e.g. "qwen3-4b-instruct-2507") into
        /// the bundle's read-only `models/` folder for offline use. Omit for the
        /// lighter "Portable Lite" build that downloads on first run.
        #[arg(long)]
        model: Option<String>,
        /// Required to embed a non-permissive (e.g. Llama/Gemma) model; confirms
        /// you will comply with that license's pass-through redistribution terms.
        #[arg(long)]
        accept_license: bool,
    },
    /// Download a catalog model's GGUF (and any vision projector) into a folder,
    /// verifying size and SHA-256. Use to pre-stage a model into a Portable
    /// build's `models/` folder; `portable --model` calls this internally.
    FetchModel {
        /// Catalog model id, e.g. "qwen3-4b-instruct-2507".
        #[arg(long)]
        id: String,
        /// Destination folder the GGUF is written into.
        #[arg(long)]
        dest: PathBuf,
        /// Required for non-permissive (Llama/Gemma) licenses.
        #[arg(long)]
        accept_license: bool,
    },
    /// Code-sign the built .app, including the nested engine binaries.
    Sign {
        /// e.g. "Developer ID Application: Your Name (TEAMID)". Defaults to
        /// the SIGNING_IDENTITY env var.
        #[arg(long)]
        identity: Option<String>,
    },
    /// Notarize and staple the .dmg. Reads APPLE_ID, APPLE_PASSWORD and
    /// APPLE_TEAM_ID from the environment.
    Notarize,
    /// Full pipeline: fetch-engine (if missing) -> icon (if missing) -> build
    /// -> sign + notarize (if credentials are configured).
    Release {
        #[arg(long)]
        identity: Option<String>,
    },
    /// Sign Portable artifacts and publish/merge `latest.json` — the
    /// manifest OpenPal's auto-updater checks — to a GitHub Release. Reads
    /// TAURI_SIGNING_PRIVATE_KEY / TAURI_SIGNING_PRIVATE_KEY_PASSWORD from
    /// the environment; requires `gh` (authenticated).
    Manifest {
        /// The release tag, e.g. "v0.2.0" (matches `github.ref_name` on a
        /// tag-push trigger).
        #[arg(long)]
        tag: String,
        /// Folder to scan for `OpenPal-portable-<target>.{zip,AppImage}`
        /// artifacts. Omit when publishing a Full-only update (nothing
        /// Portable to add this run) — any existing `portable` entries in
        /// the release's manifest are preserved either way.
        #[arg(long)]
        portable_dir: Option<PathBuf>,
        /// Release notes shown in the update dialog. Omitted: keeps
        /// whatever the manifest already has.
        #[arg(long)]
        notes: Option<String>,
    },
    /// Package, sign, and publish the currently-staged llama.cpp engine
    /// (crates/app/resources/llama/) as an OpenPal-controlled, minisign-signed
    /// artifact on the rolling `engine-latest` GitHub Release — the channel the
    /// running app's in-app engine updater downloads from, independent of full
    /// OpenPal releases.
    EngineManifest {
        /// The engine tag being published, e.g. "b10516" (must match what's
        /// actually staged in engine_dir — read from engine-version.json there
        /// rather than trusting this blindly; use this flag as a sanity check /
        /// override).
        #[arg(long)]
        tag: Option<String>,
        /// Which per-platform target this staged engine is for (same vocabulary
        /// as `fetch-engine --target` / `openpal_core::update::portable_target_key`):
        /// macos-arm64, macos-x64, win-vulkan-x64, ubuntu-vulkan-x64, ubuntu-vulkan-arm64.
        #[arg(long, default_value = "macos-arm64")]
        target: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor => doctor::run(),
        Command::FetchEngine {
            tag,
            target,
            force,
            dest,
        } => fetch::run(&tag, &target, force, dest.as_deref()),
        Command::StagePlugins => stage_plugins::run(),
        Command::Icon { source_only } => icon::run(source_only),
        Command::Build { debug } => build::run(debug),
        Command::Portable {
            target,
            debug,
            webview2,
            model,
            accept_license,
        } => portable::run(
            &target,
            debug,
            webview2.as_deref(),
            model.as_deref(),
            accept_license,
        ),
        Command::FetchModel {
            id,
            dest,
            accept_license,
        } => model::run(&id, &dest, accept_license),
        Command::Sign { identity } => signing::sign(identity.as_deref()),
        Command::Notarize => signing::notarize(),
        Command::Release { identity } => {
            // `doctor` now fetches/stages the engine itself when missing, so
            // by the time it returns Ok, `paths::engine_binary()` exists.
            doctor::run()?;
            if !paths::repo_root()
                .join("crates/app/icons/icon.icns")
                .exists()
            {
                icon::run(false)?;
            }
            build::run(false)?;
            signing::sign_and_notarize_if_configured(identity.as_deref())
        }
        Command::Manifest {
            tag,
            portable_dir,
            notes,
        } => manifest::run(&tag, portable_dir.as_deref(), notes.as_deref()),
        Command::EngineManifest { tag, target } => engine_manifest::run(tag.as_deref(), &target),
    }
}
