//! The mandatory-first-run-password model: `<data>/keys/password.wrap` is
//! the one primary slot, replacing the OS-keychain-sealed `keychain.wrap`
//! that used to be (see `docs/SPACES_AUTHORING.md` §9). The OS keychain no
//! longer participates in the master key at all — `keyring` remains only for
//! the account refresh token and the remote-pairing token (`secret_store`).
//!
//! The shape is Standard Notes-pure: a password-derived root key wraps the
//! items key (which also keys the SQLCipher database — `AppState.dek`), and
//! that items key wraps every conversation's own per-conversation key (see
//! `openpal_core::crypto::vault`). The root derivation is Argon2id at the
//! compiled-in KDF floor (`openpal_core::crypto::kdf`), and the wrap file's
//! own header records the salt and parameters actually used, so an unlock
//! re-derives exactly what the creation run used and a downgraded header is
//! rejected, never silently weakened. See
//! `crates/app/src/lib.rs`'s `setup()` for where these functions fit
//! between the pre-unlock and post-unlock phases.
//!
//! There is **no silent unlock**. `detect` classifies a launch into one of
//! three frontend-gated states (`needs_password` → create, `needs_unlock` →
//! enter, `needs_recovery` → a legacy pre-password install whose OS keychain
//! can't produce the old DEK), and `AppState` is not built until
//! `create_password`/`unlock_with_password`/`attempt_recovery_unlock` calls
//! `state::finish_unlock`.
//!
//! ## Existing installs (the one-time migration)
//!
//! A pre-password install has `keychain.wrap` (and optionally
//! `recovery.wrap`) but no `password.wrap`, so its first post-upgrade launch
//! falls into `CreatePassword`. The create-password run then **adopts the
//! existing DEK as the items key** rather than minting a new one — that is
//! what keeps every old `recovery.wrap` code working untouched, and what
//! lets the app open the already-SQLCipher-keyed `app.sqlite` unchanged.
//! `password.wrap` is written *last* in that run, after the data migration
//! (plaintext→keyed for legacy portable, plaintext→envelope per-conversation
//! rows) has succeeded — its existence is the install's "migrated" marker.
//! See `commands.rs`'s `create_password` and `openpal_core::app_db::
//! encrypt_legacy_plaintext`/`openpal_core::store::
//! rewrite_rows_as_envelopes`.

use openpal_core::crypto::key::DataKey;
use openpal_core::crypto::keychain;
use openpal_core::crypto::lookup::KeyLookup;
use openpal_core::crypto::secret_store::SecretStore;
use openpal_core::crypto::vault::{self, VaultError};
use openpal_core::crypto::wrap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error("couldn't reach the OS keychain: {0}")]
    Keychain(String),
    #[error("a password has already been created for this install")]
    AlreadyHasPassword,
    #[error("the pre-password master key file exists, but the OS keychain no longer has the key to open it")]
    KeychainEntryGone,
    #[error("no `password.wrap` exists for this install — create a password first")]
    NoPasswordFile,
    #[error("the password can't be empty")]
    EmptyPassword,
    #[error("{0}")]
    Vault(#[from] VaultError),
    #[error("{0}")]
    Wrap(#[from] wrap::WrapError),
}

/// Which of the three boot gates a launch is behind — computed once at boot
/// (`state::BootState::initialize`) and surfaced to the frontend via
/// `commands::get_app_info`'s `needs_password`/`needs_unlock`/
/// `needs_recovery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    /// No `password.wrap` yet: a genuinely first-ever run, or a pre-password
    /// install's first post-upgrade launch (its migration happens inside the
    /// create-password run). The frontend shows the create-password screen.
    CreatePassword,
    /// `password.wrap` exists: a normal launch. The frontend shows the
    /// enter-password screen (recovery is the fallback from there).
    EnterPassword,
    /// Legacy migration window: a pre-password install whose `keychain.wrap`
    /// the OS keychain can no longer open, but which has `recovery.wrap` to
    /// fall back to — the exact case the old `mint_or_unlock` surfaced as
    /// `NeedsRecovery`. The recovery code proves the old DEK; the recovery
    /// command then re-mints the keychain slot so the create-password run can
    /// adopt the DEK (see `commands::attempt_recovery_unlock`).
    UseRecoveryCode,
}

/// Classifies what this launch must ask the user for. Never errors — every
/// state is one the frontend can render. The only side effect is the
/// keychain read, and only on a pre-password (un-migrated) install: once
/// `password.wrap` exists the keychain is never consulted at boot again.
pub fn detect(secret_store: &dyn SecretStore, keys_dir: &Path) -> BootstrapStatus {
    if vault::password_wrap_path(keys_dir).exists() {
        return BootstrapStatus::EnterPassword;
    }
    // A pre-password install: `keychain.wrap` is still the only holder of
    // its DEK until the migration writes `password.wrap`, so a keychain
    // that can't produce the old KEK routes to the recovery-code path
    // exactly as the old `mint_or_unlock` did.
    if keys_dir.join("keychain.wrap").exists() {
        let keychain_ok = matches!(keychain::load(secret_store), KeyLookup::Found(_));
        if !keychain_ok && keys_dir.join("recovery.wrap").exists() {
            return BootstrapStatus::UseRecoveryCode;
        }
    }
    BootstrapStatus::CreatePassword
}

/// The items key a create-password run should adopt: the existing DEK of a
/// pre-password install (so old recovery codes keep working and the already-
/// SQLCipher-keyed database opens unchanged), or a freshly minted key for a
/// genuinely new install. Deliberately does **not** write `password.wrap` —
/// the caller migrates the database first and only then calls
/// [`create_password_file`], so a crash mid-migration never leaves a
/// `password.wrap`-with-unmigrated-data split (its existence is the
/// migration's completion marker).
pub fn items_key_for_creation(
    secret_store: &dyn SecretStore,
    keys_dir: &Path,
) -> Result<DataKey, BootstrapError> {
    std::fs::create_dir_all(keys_dir).map_err(wrap::WrapError::from)?;
    if vault::password_wrap_path(keys_dir).exists() {
        return Err(BootstrapError::AlreadyHasPassword);
    }
    if keys_dir.join("keychain.wrap").exists() {
        match keychain::load(secret_store) {
            KeyLookup::Found(kek) => {
                wrap::read_wrap(&keys_dir.join("keychain.wrap"), &kek).map_err(BootstrapError::from)
            }
            KeyLookup::Absent => Err(BootstrapError::KeychainEntryGone),
            KeyLookup::Unavailable(reason) => Err(BootstrapError::Keychain(reason)),
        }
    } else {
        Ok(DataKey::random())
    }
}

/// Writes `<data>/keys/password.wrap`, sealing `items_key` under a root key
/// derived from `password`. **Must be the very last step of a create-password
/// run** — the file's existence is what marks the install as migrated.
/// Re-writeable for a password change (only this key-encryption layer is
/// touched; the items key and every conversation envelope are re-sealed
/// under nothing — they're all already sealed under the same items key).
pub fn create_password_file(
    keys_dir: &Path,
    items_key: &DataKey,
    password: &[u8],
) -> Result<(), BootstrapError> {
    if password.is_empty() {
        return Err(BootstrapError::EmptyPassword);
    }
    vault::write_password_file(&vault::password_wrap_path(keys_dir), items_key, password)?;
    Ok(())
}

/// Alias of [`create_password_file`] used by the password-change command to
/// make the "same write, new password" nature of a change explicit. The
/// caller must already have verified the current password (re-deriving the
/// items key) before calling this.
pub fn change_password_file(
    keys_dir: &Path,
    items_key: &DataKey,
    new_password: &[u8],
) -> Result<(), BootstrapError> {
    create_password_file(keys_dir, items_key, new_password)
}

/// Unlocks the items key from `password.wrap` with `password`. A wrong
/// password lands on `VaultError::Wrap(WrongPassphrase)` (via the wrap
/// file's own header-recorded salt/parameters) — nothing is decrypted, per
/// "a wrong password decrypts nothing".
pub fn unlock_with_password(keys_dir: &Path, password: &[u8]) -> Result<DataKey, BootstrapError> {
    let password_path = vault::password_wrap_path(keys_dir);
    if !password_path.exists() {
        return Err(BootstrapError::NoPasswordFile);
    }
    Ok(vault::open_password_file(&password_path, password)?)
}
