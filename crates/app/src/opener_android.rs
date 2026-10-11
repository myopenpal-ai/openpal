//! Android bridge for `OpenerPlugin.kt` — `Intent.ACTION_VIEW` is Android's
//! real equivalent of desktop's OS-open subprocess spawn (see `opener.rs`);
//! spawning a subprocess like `xdg-open` doesn't work here at all (no such
//! binary exists on Android).
//!
//! `plugin_init()` must be registered in `lib.rs`'s `tauri::Builder` chain
//! (a plugin's own `setup()` runs during `.build()`) — mirrors
//! `secret_store_android.rs`/`wifi_lock_android.rs`'s exact shape.

use std::path::Path;
use std::sync::OnceLock;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::Wry;

static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

pub fn plugin_init() -> TauriPlugin<Wry> {
    Builder::new("openpal-opener")
        .setup(|_app, api| {
            let handle = api.register_android_plugin("com.myopenpal.openpal", "OpenerPlugin")?;
            let _ = HANDLE.set(handle);
            Ok(())
        })
        .build()
}

fn handle() -> Result<&'static PluginHandle<Wry>, String> {
    HANDLE
        .get()
        .ok_or_else(|| "the opener plugin isn't registered yet".to_string())
}

#[derive(serde::Serialize)]
struct UrlArgs<'a> {
    url: &'a str,
}

/// Open an http(s) URL via Android's `Intent.ACTION_VIEW`.
pub fn open(url: &str) -> Result<(), String> {
    handle()?
        .run_mobile_plugin::<serde_json::Value>("open", UrlArgs { url })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// No Android equivalent — there's no local model-management folder to
/// reveal on a phone chatting through `RemoteBackend` at all; reworking
/// Library's own Android UX (which still points at this dead end) is
/// already named as its own separate follow-up in `docs/design/MOBILE.md`.
pub fn reveal_folder(_path: &Path) -> Result<(), String> {
    Err("Not available on this device.".to_string())
}
