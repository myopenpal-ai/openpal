//! The desktop happy path's mint-or-unlock: `<data>/keys/keychain.wrap`
//! doesn't exist yet on first launch (mint a DEK and a random KEK, store
//! the KEK in the OS keychain, seal the DEK under it) or it does (fetch
//! the KEK back from the keychain, unseal the DEK). Called synchronously
//! during startup — see `crates/app/src/lib.rs`'s `setup()` split for
//! where this fits between the pre-unlock and post-unlock phases.
//!
//! The resulting `DataKey` is verified (`write_wrap`/`read_wrap` already
//! do this) and returned via `KeyStatus::Ready`, so the caller can carry
//! it into `AppState.dek` and from there into the SQLCipher connections
//! that need `PRAGMA key`.
//!
//! A keychain failure on an existing install (`Unavailable` — backend
//! unreachable, or `Absent` — the wrap file exists but the keychain entry
//! is gone; likewise a stored KEK that fails to open its own wrap file) is
//! only a hard error when there's genuinely nothing to fall back to. If
//! `<data>/keys/recovery.wrap` exists — the one-time-shown recovery code
//! ceremony (`crates/app/src/commands.rs`'s `confirm_recovery_code`) ran at
//! some earlier launch — this returns [`KeyStatus::NeedsRecovery`] instead
//! of an `Err`, so `setup()` sets `BootState::needs_recovery` and defers
//! building `AppState` entirely rather than failing; the frontend reads
//! that flag (via `commands::get_app_info`) and prompts the user to type
//! their recovery code back in via `commands::attempt_recovery_unlock`,
//! which finishes the same `AppState`-building step on success. Only when
//! no recovery slot exists at all does this still hard-error, same as
//! before this phase.

use openpal_core::crypto::key::DataKey;
use openpal_core::crypto::keychain;
use openpal_core::crypto::lookup::KeyLookup;
use openpal_core::crypto::secret_store::SecretStore;
use openpal_core::crypto::wrap::{self, WrapHeader};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("couldn't reach the OS keychain: {0}")]
    Keychain(String),
    #[error("the master key file exists, but the OS keychain no longer has the key to open it")]
    KeychainEntryGone,
    #[error("{0}")]
    Wrap(#[from] wrap::WrapError),
}

#[derive(Debug)]
pub enum KeyStatus {
    /// The DEK is available (freshly minted, or unlocked via the keychain).
    Ready(DataKey),
    /// The keychain couldn't produce the KEK, but a recovery slot exists to
    /// fall back to — the caller should surface this to the user rather
    /// than treating it as fatal.
    NeedsRecovery,
}

pub fn mint_or_unlock(
    secret_store: &dyn SecretStore,
    keys_dir: &Path,
) -> Result<KeyStatus, BootstrapError> {
    std::fs::create_dir_all(keys_dir).map_err(wrap::WrapError::from)?;
    let path = keys_dir.join("keychain.wrap");

    if path.exists() {
        let keychain_result = match keychain::load(secret_store) {
            KeyLookup::Found(kek) => wrap::read_wrap(&path, &kek).map_err(BootstrapError::from),
            KeyLookup::Absent => Err(BootstrapError::KeychainEntryGone),
            KeyLookup::Unavailable(reason) => Err(BootstrapError::Keychain(reason)),
        };
        match keychain_result {
            Ok(dek) => {
                tracing::info!("unlocked the master key via the OS keychain");
                Ok(KeyStatus::Ready(dek))
            }
            Err(err) => {
                if keys_dir.join("recovery.wrap").exists() {
                    tracing::warn!(
                        "OS keychain couldn't produce the master key ({err}), but a recovery \
                         slot exists — falling back to asking the user for their recovery code"
                    );
                    Ok(KeyStatus::NeedsRecovery)
                } else {
                    Err(err)
                }
            }
        }
    } else {
        let kek = DataKey::random();
        let dek = DataKey::random();
        keychain::store(secret_store, &kek).map_err(BootstrapError::Keychain)?;
        // `m_cost_kib`/`t_cost` of 0: an honest signal that no KDF ran for
        // this slot — the keychain access itself is the authentication,
        // per the doc's `kdf_id = 0` framing for this exact slot.
        let header = WrapHeader {
            m_cost_kib: 0,
            t_cost: 0,
            ..WrapHeader::new([0u8; 16])
        };
        wrap::write_wrap(&path, &dek, &kek, &header)?;
        tracing::info!("minted a new master key, sealed under a keychain-held secret");
        Ok(KeyStatus::Ready(dek))
    }
}
