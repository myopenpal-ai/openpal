//! Client-side pairing persistence: a small non-secret JSON record (which
//! desktop this phone is paired with, and its pinned TLS fingerprint) plus
//! the bearer token in the OS-backed [`SecretStore`] — the exact shape
//! `crate::account::AccountManager` already uses for its own refresh
//! token, reused here rather than inventing a second pattern.

use crate::crypto::lookup::KeyLookup;
use crate::crypto::secret_store::SecretStore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const SECRET_KEY: &str = "remote-pairing-token";

/// Everything about a pairing except the bearer token itself — safe to
/// write to disk as plain JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingRecord {
    pub device_id: String,
    pub device_name: String,
    pub host_addr: String,
    pub port: u16,
    /// SHA-256 DER fingerprint (lowercase hex) captured at pairing time —
    /// pinned for every request from then on (`super::tls::PinnedVerifier`).
    pub fingerprint_hex: String,
    pub paired_at_ms: i64,
}

fn load_record(path: &Path) -> Option<RemotePairingRecord> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Atomic write, matching `crate::account`'s own `save_cache` pattern.
fn save_record(path: &Path, record: &RemotePairingRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

pub(super) struct Store {
    path: PathBuf,
    secret_store: Arc<dyn SecretStore>,
}

impl Store {
    pub fn new(path: PathBuf, secret_store: Arc<dyn SecretStore>) -> Self {
        Self { path, secret_store }
    }

    /// The persisted record and its bearer token, if a complete pairing is
    /// on disk/in the keychain. A record with no matching token (keychain
    /// entry deleted out from under us, or never written) is treated as "no
    /// pairing" — there's nothing usable without the token.
    pub fn load(&self) -> Option<(RemotePairingRecord, String)> {
        let record = load_record(&self.path)?;
        match self.secret_store.load(SECRET_KEY) {
            KeyLookup::Found(token) => Some((record, token)),
            KeyLookup::Absent => None,
            KeyLookup::Unavailable(reason) => {
                tracing::warn!(
                    "remote client: couldn't reach the OS keychain to load the pairing token: {reason}"
                );
                None
            }
        }
    }

    pub fn save(&self, record: &RemotePairingRecord, token: &str) -> Result<(), String> {
        self.secret_store.store(SECRET_KEY, token)?;
        save_record(&self.path, record)
    }

    /// Best-effort: an already-absent record/token isn't a failure the
    /// caller needs to act on, mirroring `SecretStore::delete`'s own
    /// contract.
    pub fn forget(&self) {
        let _ = self.secret_store.delete(SECRET_KEY);
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// An in-memory `SecretStore` test double — mirrors
    /// `crate::account`'s own tests' reliance on a real `KeyringSecretStore`
    /// backend, except here a fake avoids touching the real OS keychain in
    /// unit tests that don't need to.
    #[derive(Default)]
    struct FakeSecretStore {
        entries: Mutex<std::collections::HashMap<String, String>>,
    }

    impl SecretStore for FakeSecretStore {
        fn store(&self, key: &str, value: &str) -> Result<(), String> {
            self.entries
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        fn load(&self, key: &str) -> KeyLookup<String> {
            match self.entries.lock().unwrap().get(key) {
                Some(v) => KeyLookup::Found(v.clone()),
                None => KeyLookup::Absent,
            }
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            self.entries.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn sample_record() -> RemotePairingRecord {
        RemotePairingRecord {
            device_id: "dev-1".to_string(),
            device_name: "Ana's Desktop".to_string(),
            host_addr: "192.168.1.20".to_string(),
            port: 54321,
            fingerprint_hex: "a".repeat(64),
            paired_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn round_trips_record_and_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(
            dir.path().join("remote-pairing.json"),
            Arc::new(FakeSecretStore::default()),
        );
        assert!(store.load().is_none());

        store.save(&sample_record(), "the-token").unwrap();
        let (record, token) = store.load().unwrap();
        assert_eq!(record, sample_record());
        assert_eq!(token, "the-token");
    }

    #[test]
    fn a_record_with_no_matching_token_is_treated_as_unpaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-pairing.json");
        save_record(&path, &sample_record()).unwrap();
        // No token ever stored in the secret store.
        let store = Store::new(path, Arc::new(FakeSecretStore::default()));
        assert!(store.load().is_none());
    }

    #[test]
    fn forget_clears_both_the_record_and_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(
            dir.path().join("remote-pairing.json"),
            Arc::new(FakeSecretStore::default()),
        );
        store.save(&sample_record(), "the-token").unwrap();
        assert!(store.load().is_some());

        store.forget();
        assert!(store.load().is_none());
    }
}
