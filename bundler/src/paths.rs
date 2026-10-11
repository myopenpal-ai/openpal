//! Repo-relative paths. The bundler is compiled inside the workspace, so the
//! repo root is always one level above this crate's manifest dir — this works
//! no matter which directory the user runs it from.

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("bundler crate lives directly under the repo root")
        .to_path_buf()
}

/// Where the engine is staged for both dev runs and bundling.
pub fn engine_dir() -> PathBuf {
    repo_root().join("crates/app/resources/llama")
}

/// File name of the staged server binary (`.exe` on Windows). Builds happen on
/// the matching OS, so the host's extension is the right one to look for.
#[cfg(windows)]
const ENGINE_BINARY: &str = "llama-server.exe";
#[cfg(not(windows))]
const ENGINE_BINARY: &str = "llama-server";

pub fn engine_binary() -> PathBuf {
    engine_dir().join(ENGINE_BINARY)
}

pub fn app_crate_dir() -> PathBuf {
    repo_root().join("crates/app")
}

/// Tauri CLI output directory for macOS bundles.
pub fn bundle_dir(debug: bool) -> PathBuf {
    repo_root()
        .join("target")
        .join(if debug { "debug" } else { "release" })
        .join("bundle/macos")
}

/// Where Portable distributions are assembled (under `target/`, already
/// git-ignored).
pub fn portable_out_dir() -> PathBuf {
    repo_root().join("target/portable")
}

/// Where built-in plugin sources are staged for packaged (non-portable)
/// builds, mirroring `engine_dir()`. See `stage_plugins`.
pub fn plugins_staging_dir() -> PathBuf {
    repo_root().join("crates/app/resources/plugins")
}

/// The curated model catalog the app embeds at compile time. The bundler reads
/// the same file to resolve `--model <id>` for pre-staged Portable builds.
pub fn catalog_json() -> PathBuf {
    repo_root().join("crates/core/data/catalog.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_is_the_actual_checkout() {
        // A handful of files/dirs that must exist directly under the real repo
        // root — catches a refactor that moves this crate (breaking the
        // "one level above CARGO_MANIFEST_DIR" assumption) before it ships.
        let root = repo_root();
        assert!(root.join("Cargo.toml").is_file());
        assert!(root.join("crates/core").is_dir());
        assert!(root.join("plugins").is_dir());
    }

    #[test]
    fn joiners_are_relative_to_repo_root() {
        let root = repo_root();
        assert_eq!(engine_dir(), root.join("crates/app/resources/llama"));
        assert_eq!(app_crate_dir(), root.join("crates/app"));
        assert_eq!(portable_out_dir(), root.join("target/portable"));
        assert_eq!(
            plugins_staging_dir(),
            root.join("crates/app/resources/plugins")
        );
        assert_eq!(catalog_json(), root.join("crates/core/data/catalog.json"));
    }

    #[test]
    fn bundle_dir_switches_on_debug() {
        let root = repo_root();
        assert_eq!(bundle_dir(true), root.join("target/debug/bundle/macos"));
        assert_eq!(bundle_dir(false), root.join("target/release/bundle/macos"));
    }

    #[test]
    fn engine_binary_uses_the_host_extension() {
        let expected = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        assert_eq!(engine_binary(), engine_dir().join(expected));
    }
}
