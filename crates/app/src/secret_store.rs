//! Desktop and iOS: `keyring` has a real OS-keychain backend on both, so
//! `crate::secret_store::store()` is just `KeyringSecretStore`. Android's
//! `secret_store_android.rs` is the platform-selected alternative — see
//! `lib.rs`'s `#[path]` selection.

use openpal_core::crypto::secret_store::{KeyringSecretStore, SecretStore};
use std::sync::Arc;

pub fn store() -> Arc<dyn SecretStore> {
    Arc::new(KeyringSecretStore)
}
