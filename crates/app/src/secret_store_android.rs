//! Android's `SecretStore` implementation — bridges
//! `openpal_core::crypto::secret_store::SecretStore` to the
//! `SecretStorePlugin` Kotlin class
//! (`gen/android/app/src/main/java/com/myopenpal/openpal/SecretStorePlugin.kt`),
//! which persists via Android Keystore-backed `EncryptedSharedPreferences`.
//! `keyring` (what desktop/iOS use, see `secret_store.rs`) has no real
//! Android backend — see `docs/design/MOBILE.md`.
//!
//! `plugin_init()` must be registered in `lib.rs`'s `tauri::Builder` chain
//! (a plugin's own `setup()` runs during `.build()`, before the app-level
//! `.setup(|app| {...})` closure ever fires) so `store()`'s `PluginHandle`
//! is ready by the time `bootstrap::mint_or_unlock` needs it.

use openpal_core::crypto::lookup::KeyLookup;
use openpal_core::crypto::secret_store::SecretStore;
use std::sync::{Arc, OnceLock};
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::Wry;

static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

pub fn plugin_init() -> TauriPlugin<Wry> {
    Builder::new("openpal-secret-store")
        .setup(|_app, api| {
            let handle =
                api.register_android_plugin("com.myopenpal.openpal", "SecretStorePlugin")?;
            let _ = HANDLE.set(handle);
            Ok(())
        })
        .build()
}

pub fn store() -> Arc<dyn SecretStore> {
    Arc::new(PluginSecretStore)
}

struct PluginSecretStore;

#[derive(serde::Serialize)]
struct KeyArgs<'a> {
    key: &'a str,
}
#[derive(serde::Serialize)]
struct StoreArgs<'a> {
    key: &'a str,
    value: &'a str,
}
#[derive(serde::Deserialize)]
struct LoadResp {
    value: String,
}
#[derive(serde::Deserialize)]
struct Empty {}

/// The literal string `SecretStorePlugin.kt`'s `load` command rejects with
/// when the key isn't present — the only signal available to distinguish
/// `KeyLookup::Absent` from `KeyLookup::Unavailable`, since
/// `PluginInvokeError` doesn't otherwise carry a structured "not found" case.
const NOT_FOUND: &str = "NOT_FOUND";

fn handle() -> Result<&'static PluginHandle<Wry>, String> {
    HANDLE
        .get()
        .ok_or_else(|| "the secret-store plugin isn't registered yet".to_string())
}

impl SecretStore for PluginSecretStore {
    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        handle()?
            .run_mobile_plugin::<Empty>("store", StoreArgs { key, value })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn load(&self, key: &str) -> KeyLookup<String> {
        let handle = match handle() {
            Ok(handle) => handle,
            Err(reason) => return KeyLookup::Unavailable(reason),
        };
        match handle.run_mobile_plugin::<LoadResp>("load", KeyArgs { key }) {
            Ok(resp) => KeyLookup::Found(resp.value),
            Err(e) => {
                let reason = e.to_string();
                if reason == NOT_FOUND {
                    KeyLookup::Absent
                } else {
                    KeyLookup::Unavailable(reason)
                }
            }
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        handle()?
            .run_mobile_plugin::<Empty>("delete", KeyArgs { key })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
