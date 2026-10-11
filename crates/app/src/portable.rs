//! Portable-build setup that must run *before* the webview is created.
//!
//! On the Windows Portable build the app ships its own fixed-version WebView2
//! runtime next to the executable (`webview2-runtime/`), so it launches with no
//! system WebView2 install. WebView2/wry only use a bundled runtime when the
//! `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` env var points at it (honored by
//! Tauri >= 2.8.3), and it must be set before the webview is initialized —
//! hence this runs at the very top of `run()`. macOS (WKWebView) and Linux
//! (WebKitGTK) use the OS webview and need none of this.
//!
//! The whole body is guarded by `cfg!(windows)` (a runtime check, not `#[cfg]`)
//! so it still type-checks on every platform; it only *does* anything on
//! Windows. It uses cross-platform `std` only.

use std::path::{Path, PathBuf};

/// Finish an in-place Portable update if this launch is the update helper
/// spawned by `commands::install_update` (see
/// `openpal_core::update_portable`), rather than an ordinary start. Must run
/// before the webview is created, like `prepare_webview2` below — a no-op
/// on an ordinary launch, but on the helper path this process is a
/// throwaway copy still sitting in the update-staging directory: it waits
/// for the old process to exit, swaps the update into place, spawns the
/// swapped-in binary as a fresh normal launch, then exits immediately
/// (success or failure) so this staging copy never itself proceeds to open
/// a window or start the engine.
pub fn apply_pending_update() {
    let Some(pending) = openpal_core::update_portable::take_pending_apply() else {
        return;
    };
    openpal_core::update_portable::apply_and_relaunch(pending);
    std::process::exit(0);
}

/// Point WebView2 at the bundled fixed-version runtime and grant the Windows 10
/// AppContainer ACLs it needs. No-op unless this is a Windows portable layout.
pub fn prepare_webview2() {
    if !cfg!(windows) {
        return;
    }
    let Some(runtime) = bundled_webview2_runtime() else {
        return;
    };
    // Honored by Tauri/wry when creating the webview (Tauri >= 2.8.3). Safe here:
    // single-threaded, before any webview or extra threads are spawned.
    std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", &runtime);
    tracing::info!(runtime = %runtime.display(), "using bundled WebView2 runtime");
    grant_appcontainer_acls(&runtime);
}

/// `<exe dir>/webview2-runtime/` when it actually holds the fixed-version
/// runtime (`msedgewebview2.exe`); `None` for a normal (installed) build.
fn bundled_webview2_runtime() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("webview2-runtime");
    dir.join("msedgewebview2.exe").exists().then_some(dir)
}

/// Windows 10 runs the WebView2 (Fixed Version >= 120) renderer in an
/// AppContainer, so an unpackaged app must grant `ALL APPLICATION PACKAGES` and
/// `ALL RESTRICTED APPLICATION PACKAGES` read/execute on the runtime folder or
/// the webview fails to start. Windows 11 is unaffected (the grant is harmless
/// there). Best-effort and once-only via a sentinel file so normal launches pay
/// nothing.
fn grant_appcontainer_acls(runtime: &Path) {
    let sentinel = runtime.join(".acl-granted");
    if sentinel.exists() {
        return;
    }
    let mut granted = true;
    // *S-1-15-2-1 = ALL APPLICATION PACKAGES, *S-1-15-2-2 = ALL RESTRICTED
    // APPLICATION PACKAGES. (OI)(CI) makes the grant inherit to the contents.
    for sid in ["*S-1-15-2-1", "*S-1-15-2-2"] {
        let status = std::process::Command::new("icacls")
            .arg(runtime)
            .arg("/grant")
            .arg(format!("{sid}:(OI)(CI)(RX)"))
            .status();
        granted &= matches!(status, Ok(s) if s.success());
    }
    if granted {
        let _ = std::fs::write(&sentinel, b"");
    } else {
        tracing::warn!(
            "could not grant WebView2 AppContainer ACLs; the webview may fail on Windows 10"
        );
    }
}
