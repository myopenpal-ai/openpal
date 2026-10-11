//! Desktop/iOS: hand a URL or folder path to the OS's default handler via a
//! spawned subprocess. Done in Rust (not the webview) because the webview's
//! openers are unreliable across WebView2 and WebKitGTK.
//!
//! See `opener_android.rs` for Android's real equivalent
//! (`Intent.ACTION_VIEW`) — before this module split existed, the desktop
//! `xdg-open` arm below (`all(unix, not(target_os = "macos"))`) wrongly
//! matched Android too (`cfg(unix)` is true there), silently failing every
//! `open_link` call since Android has no `xdg-open` binary at all.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

/// Best-effort: the child is only spawned, never waited on — Windows'
/// `explorer` in particular returns a non-zero exit code even on success.
fn spawn(target: impl AsRef<OsStr>) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new("explorer");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");

    cmd.arg(target);
    cmd.spawn().map(|_| ())
}

/// Open an http(s) URL in the system browser.
pub fn open(url: &str) -> Result<(), String> {
    spawn(url).map_err(|e| e.to_string())
}

/// Reveal a folder in the OS's file manager.
pub fn reveal_folder(path: &Path) -> Result<(), String> {
    spawn(path).map_err(|e| e.to_string())
}
