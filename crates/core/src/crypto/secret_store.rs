//! `SecretStore` — the seam between "somewhere to durably persist a small
//! secret string under a name" and the two things that need it: the
//! master-key KEK ([`super::keychain`]) and the account refresh token
//! ([`crate::account`]). [`KeyringSecretStore`] is the `keyring` crate,
//! unmodified from what both of those did before this seam existed — it
//! works on desktop and iOS, where `keyring` has a real OS-keychain backend.
//!
//! Android has none (`keyring` falls back to an in-memory mock there, which
//! doesn't survive a process restart — see `docs/design/MOBILE.md`), so
//! `crates/app` supplies a second implementation backed by a Tauri mobile
//! plugin over Android Keystore. That implementation can't live here:
//! this crate is deliberately GUI/Tauri-free (see its own top-level docs),
//! and reaching Android's Keystore requires a Tauri `AppHandle`/plugin
//! handle this crate never has.

use super::lookup::{classify_keyring_error, KeyLookup};

/// A place to durably store a small secret string under a name.
pub trait SecretStore: Send + Sync {
    fn store(&self, key: &str, value: &str) -> Result<(), String>;
    fn load(&self, key: &str) -> KeyLookup<String>;
    /// Best-effort: an already-absent entry isn't a failure the caller needs
    /// to act on (mirrors `keyring::Error::NoEntry` handling every
    /// implementation must apply).
    fn delete(&self, key: &str) -> Result<(), String>;
}

const SERVICE: &str = "OpenPal";

/// The OS keychain via the `keyring` crate — desktop and iOS.
pub struct KeyringSecretStore;

impl SecretStore for KeyringSecretStore {
    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(|e| e.to_string())?;
        entry.set_password(value).map_err(|e| e.to_string())
    }

    fn load(&self, key: &str) -> KeyLookup<String> {
        let entry = match keyring::Entry::new(SERVICE, key) {
            Ok(entry) => entry,
            Err(e) => return KeyLookup::from_infallible(classify_keyring_error(e)),
        };
        match entry.get_password() {
            Ok(value) => KeyLookup::Found(value),
            Err(e) => KeyLookup::from_infallible(classify_keyring_error(e)),
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(SERVICE, key).map_err(|e| e.to_string())?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}
