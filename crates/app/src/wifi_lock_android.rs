//! Android bridge for `WifiLockPlugin.kt` — holds a
//! `WifiManager.MulticastLock` while `openpal_core::remote::client::
//! discovery::Browser` is browsing for a paired desktop's mDNS
//! advertisement. Many OEM Wi-Fi stacks otherwise drop multicast receive
//! entirely (`WifiManager.createMulticastLock`'s own docs). Foreground-
//! only: `commands::remote_discover_hosts`/`remote_stop_discovery` acquire
//! and release this around the `Browser`'s own lifetime, never held
//! indefinitely.
//!
//! `plugin_init()` must be registered in `lib.rs`'s `tauri::Builder` chain
//! (a plugin's own `setup()` runs during `.build()`) — mirrors
//! `secret_store_android.rs`'s exact shape for the same kind of
//! Android-only OS capability.

use std::sync::OnceLock;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::Wry;

static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

pub fn plugin_init() -> TauriPlugin<Wry> {
    Builder::new("openpal-wifi-lock")
        .setup(|_app, api| {
            let handle = api.register_android_plugin("com.myopenpal.openpal", "WifiLockPlugin")?;
            let _ = HANDLE.set(handle);
            Ok(())
        })
        .build()
}

fn handle() -> Result<&'static PluginHandle<Wry>, String> {
    HANDLE
        .get()
        .ok_or_else(|| "the wifi-lock plugin isn't registered yet".to_string())
}

pub fn acquire() -> Result<(), String> {
    handle()?
        .run_mobile_plugin::<serde_json::Value>("acquire", serde_json::json!({}))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn release() -> Result<(), String> {
    handle()?
        .run_mobile_plugin::<serde_json::Value>("release", serde_json::json!({}))
        .map(|_| ())
        .map_err(|e| e.to_string())
}
