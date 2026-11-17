//! The optional-password model: `<data>/keys/password.wrap` is the one
//! primary slot when a password is set, replacing the OS-keychain-sealed
//! `keychain.wrap` that used to be (see `docs/SPACES_AUTHORING.md` §9). The
//! OS keychain no longer participates in the master key at all — `keyring`
//! remains only for the account refresh token, the remote-pairing token
//! (`secret_store`), and the **launch slot** this module adds (below). An
//! install with **no password** is tracked by an explicit
//! `keys/nopass.marker` file (written on skip/remove, deleted on set) —
//! see [`nopass_marker_path`] and [`BootstrapStatus::NoPassword`] — so
//! legacy `keychain.wrap`/pre-password detection paths stay byte-for-byte
//! unchanged.
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
//! ## The launch slot (OS-keychain "remember me", installed builds only)
//!
//! On an **installed** (non-Portable) desktop build, the OS keychain is the
//! trusted unlocker on the user's own machine: `detect` classifies a normal
//! launch of a password-protected install as
//! [`BootstrapStatus::UnlockWithKeychain`] when the keychain can produce
//! the key, and the frontend unlocks with no password screen (see
//! `commands::unlock_with_keychain`). This is **not** "no silent unlock"
//! being walked back into the boot path — `AppState` is still only built by
//! an explicit unlock command; the keychain simply replaces the typed
//! password as the holder of the key on that command's happy path.
//!
//! The slot is the same construction the pre-password model used, kept in
//! two halves:
//!
//! - a **dedicated keychain entry** (`launch-key`, see
//!   `openpal_core::crypto::keychain::store_launch_key`/`load_launch_key`),
//!   holding a random 32-byte KEK — the keychain access itself is the
//!   authentication, no KDF runs;
//! - **`<data>/keys/launch.wrap`**, sealing the *same items key* as
//!   `password.wrap` under that KEK (a `wrap::WrapHeader::keychain_backed()`
//!   file, byte-for-byte the legacy `keychain.wrap` shape).
//!
//! The items key is never stored in the clear anywhere; the raw DEK still
//! only ever lives in memory (`AppState.dek`), and the SQLCipher keying and
//! per-conversation envelopes are untouched. The password stays
//! authoritative: change/remove still work through their existing screens,
//! and the launch slot is kept consistent with them —
//! [`provision_launch_key`] runs on every flow that re-verifies the items
//! key (create/set/change password, a typed unlock, a recovery unlock), and
//! [`remove_launch_key`] on remove-password (so after removal the next
//! launch skips the gate and no keychain entry is left behind). When the
//! keychain is absent, unreachable, or holds nothing, `detect` still
//! classifies the launch as [`BootstrapStatus::EnterPassword`] — a keychain
//! failure never opens the database unencrypted and never falls back to the
//! no-password layout. A **Portable** build of the same data dir still
//! prompts for the password: the slot is scoped to installed builds (see
//! `AppPaths::portable`'s "leaves no trace on the host" promise).
//!
//! Existing installs that set their password before this feature are
//! migrated invisibly: whichever flow re-verifies the items key next — a
//! typed unlock, a recovery unlock, a change — re-provisions the slot, so
//! nothing has to be re-entered or re-set beyond the password the user was
//! already typing each launch. Where the install migrated from the
//! pre-password model, the still-present `keychain.wrap` duo is adopted
//! the very first launch after this lands, with no password at all
//! ([`adopt_legacy_launch_key`], from `BootState::initialize`).
//!
//! ## The boot gates
//!
//! `detect` classifies a launch into one of five frontend-gated states
//! (`no_password` → straight in with no encryption, `needs_password` →
//! create, `needs_unlock` → enter, `unlock_with_keychain` → the OS keychain
//! unlocks silently, `needs_recovery` → a legacy pre-password install whose
//! OS keychain can't produce the old DEK), and `AppState` is not built until
//! `start_without_password`/`create_password`/`unlock_with_password`/
//! `unlock_with_keychain`/`attempt_recovery_unlock` calls
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
    #[error("no `launch.wrap` (or its OS keychain slot) exists for this install — enter the password instead")]
    NoLaunchSlot,
    #[error("the password can't be empty")]
    EmptyPassword,
    #[error("{0}")]
    Vault(#[from] VaultError),
    #[error("{0}")]
    Wrap(#[from] wrap::WrapError),
}

/// Which of the five boot gates a launch is behind — computed once at boot
/// (`state::BootState::initialize`) and surfaced to the frontend via
/// `commands::get_app_info`'s `password_set`/`needs_password`/
/// `needs_unlock`/`needs_recovery`/`can_unlock_with_keychain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    /// The `keys/nopass.marker` exists and no `password.wrap` does: an
    /// install that explicitly chose "continue without a password". Nothing
    /// is encrypted at rest — the database, per-conversation rows, logs,
    /// backups and exports are all plaintext. The frontend goes straight
    /// in (no gate).
    NoPassword,
    /// No `password.wrap` yet: a genuinely first-ever run, or a pre-password
    /// install's first post-upgrade launch (its migration happens inside the
    /// create-password run). The frontend shows the create-password screen.
    CreatePassword,
    /// `password.wrap` exists but the OS keychain can't produce the items
    /// key — the keychain is absent/unreachable, holds nothing, or what it
    /// holds no longer opens `launch.wrap` (e.g. it was handed to a
    /// different data dir, or the keychain itself was wiped). The frontend
    /// shows the enter-password screen (recovery is the fallback from
    /// there). This is also `password.wrap`'s classification on a Portable
    /// build and on mobile — the launch slot is scoped to installed desktop
    /// builds, and a keychain failure must never decrypt anything itself.
    EnterPassword,
    /// `password.wrap` exists and the OS keychain's `launch-key` slot opens
    /// `launch.wrap` to the items key: installed desktop build, keychain
    /// happy path. The frontend unlocks via `commands::unlock_with_keychain`
    /// with no password screen — the OS keychain is the trusted unlocker on
    /// this machine. The PIN gate (`AppLockGate`), where enabled, still
    /// applies after; the password stays authoritative for change/remove
    /// and as the recovery fallback.
    UnlockWithKeychain,
    /// Legacy migration window: a pre-password install whose `keychain.wrap`
    /// the OS keychain can no longer open, but which has `recovery.wrap` to
    /// fall back to — the exact case the old `mint_or_unlock` surfaced as
    /// `NeedsRecovery`. The recovery code proves the old DEK; the recovery
    /// command then re-mints the keychain slot so the create-password run can
    /// adopt the DEK (see `commands::attempt_recovery_unlock`).
    UseRecoveryCode,
}

/// The marker file whose existence means "this install deliberately runs
/// with no password (no encryption at rest)". Written by the
/// continue-without-a-password and remove-password flows; deleted by the
/// create/change-password flows. Kept as its own file rather than inferred
/// from `password.wrap`'s absence so that a *pre-password* (un-migrated)
/// install — which also has no `password.wrap` — is still detected through
/// the unchanged legacy `keychain.wrap` path.
pub fn nopass_marker_path(keys_dir: &Path) -> std::path::PathBuf {
    keys_dir.join("nopass.marker")
}

/// Record that this install runs with no password. Idempotent.
pub fn write_no_password_marker(keys_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(keys_dir)?;
    std::fs::write(
        nopass_marker_path(keys_dir),
        b"openpal no-password mode: this install has no password set, so data is stored \
          unencrypted on this device.\n",
    )
}

/// Remove the no-password marker (when a password is created). Missing file
/// is not an error.
pub fn remove_no_password_marker(keys_dir: &Path) {
    let _ = std::fs::remove_file(nopass_marker_path(keys_dir));
}

/// Classifies what this launch must ask the user for. Never errors — every
/// state is one the frontend can render. The only side effects are keychain
/// reads: the `launch-key` slot (a password-protected installed build) and,
/// only on a pre-password (un-migrated) install, the `master-key` slot.
///
/// `keychain_launch` is whether this build/launch may use the OS keychain
/// as the launch unlocker at all — false on a Portable build (the slot is
/// scoped to installed builds, and a Portable run of a shared data dir must
/// keep prompting) and on mobile (its unlock UX is a separate ticket). It
/// only gates the new `password.wrap`-exists branch below; the legacy
/// pre-password path keeps its historical behavior unconditionally.
pub fn detect(
    secret_store: &dyn SecretStore,
    keys_dir: &Path,
    keychain_launch: bool,
) -> BootstrapStatus {
    if vault::password_wrap_path(keys_dir).exists() {
        if keychain_launch && launch_slot_usable(secret_store, keys_dir) {
            return BootstrapStatus::UnlockWithKeychain;
        }
        return BootstrapStatus::EnterPassword;
    }
    // A pre-password install: `keychain.wrap` is still the only holder of
    // its DEK until the migration writes `password.wrap`, so a keychain
    // that can't produce the old KEK routes to the recovery-code path
    // exactly as the old `mint_or_unlock` did. Checked *before* the
    // no-password marker on purpose: such an install's data is already
    // encrypted under the keychain-sealed DEK, so the UI never offers (and
    // never writes a marker for) "continue without a password" on it —
    // were a stray marker to appear, routing to [`BootstrapStatus::NoPassword`]
    // would open that keyed database without a key and fail, whereas
    // create-password's adopt path recovers gracefully.
    if keys_dir.join("keychain.wrap").exists() {
        let keychain_ok = matches!(keychain::load(secret_store), KeyLookup::Found(_));
        if !keychain_ok && keys_dir.join("recovery.wrap").exists() {
            return BootstrapStatus::UseRecoveryCode;
        }
        return BootstrapStatus::CreatePassword;
    }
    if nopass_marker_path(keys_dir).exists() {
        return BootstrapStatus::NoPassword;
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

// ---------------------------------------------------------------------------
// The launch slot — see the module doc's "The launch slot" section.
// ---------------------------------------------------------------------------

/// Whether this build supports keychain-backed launch unlock at all: desktop
/// only. Mobile's unlock UX is a separate ticket (`docs/design/MOBILE.md`),
/// and Android's `SecretStore` is a different implementation that this
/// feature deliberately hasn't been reasoned about for. The Portable build
/// is *not* excluded here — that's done per-launch by the caller passing
/// `keychain_launch: false`, so the same installed build can't be tricked by
/// pointing it at a portable data dir.
pub fn keychain_unlock_supported() -> bool {
    !cfg!(any(target_os = "android", target_os = "ios"))
}

/// The launch slot's on-disk half: `<data>/keys/launch.wrap`, sealing the
/// same items key as `password.wrap` under a random KEK the OS keychain
/// holds (`launch-key`).
pub fn launch_wrap_path(keys_dir: &Path) -> std::path::PathBuf {
    keys_dir.join("launch.wrap")
}

/// Whether `launch.wrap` exists *and* the OS keychain's `launch-key` slot
/// actually opens it to a `DataKey`. That's the definition of a "usable
/// keychain slot" for boot classification: a present-but-unopenable slot
/// (stale key from a different data dir, a wiped keychain) must classify as
/// `EnterPassword`, never as a silent unlock that would then fail to open
/// the database.
fn launch_slot_usable(secret_store: &dyn SecretStore, keys_dir: &Path) -> bool {
    let path = launch_wrap_path(keys_dir);
    if !path.exists() {
        return false;
    }
    match keychain::load_launch_key(secret_store) {
        KeyLookup::Found(kek) => wrap::read_wrap(&path, &kek).is_ok(),
        _ => false,
    }
}

/// Writes (or re-writes — a password change re-verifies the same items key,
/// so the slot is simply refreshed) the launch slot: a fresh random KEK goes
/// to the OS keychain's `launch-key` entry, and `items_key` is sealed under
/// it in `launch.wrap` as a `keychain_backed()` file — byte-for-byte the
/// legacy pre-password construction, no KDF (the keychain access itself is
/// the authentication). Idempotent.
///
/// The caller gates this on installed-desktop builds (`!portable &&
/// keychain_unlock_supported()`); the slot's whole point is that it only
/// ever exists on a build where the keychain is meaningful.
pub fn provision_launch_key(
    secret_store: &dyn SecretStore,
    keys_dir: &Path,
    items_key: &DataKey,
) -> Result<(), BootstrapError> {
    let kek = DataKey::random();
    keychain::store_launch_key(secret_store, &kek).map_err(BootstrapError::Keychain)?;
    let path = launch_wrap_path(keys_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(wrap::WrapError::from)?;
    }
    wrap::write_wrap(&path, items_key, &kek, &wrap::WrapHeader::keychain_backed())?;
    Ok(())
}

/// Unlocks the items key from the launch slot: reads the `launch-key` KEK
/// from the OS keychain and opens `launch.wrap` with it. `NoLaunchSlot` when
/// either half is missing; `Keychain` when the OS keychain itself can't be
/// reached. Nothing is decrypted unless the keychain cooperates — a
/// keychain failure is *not* a path to unencrypted data, it's the
/// enter-password gate's turn.
pub fn unlock_with_keychain(
    secret_store: &dyn SecretStore,
    keys_dir: &Path,
) -> Result<DataKey, BootstrapError> {
    let path = launch_wrap_path(keys_dir);
    if !path.exists() {
        return Err(BootstrapError::NoLaunchSlot);
    }
    match keychain::load_launch_key(secret_store) {
        KeyLookup::Found(kek) => Ok(wrap::read_wrap(&path, &kek)?),
        KeyLookup::Absent => Err(BootstrapError::NoLaunchSlot),
        KeyLookup::Unavailable(reason) => Err(BootstrapError::Keychain(reason)),
    }
}

/// Deletes both halves of the launch slot — the remove-password flow's
/// cleanup, so that after the password is gone "the next launch skips the
/// gate and no keychain entry is left behind" (the wrapper file and the
/// keychain entry are each best-effort; one failing must not strand the
/// other). Also drops the stale pre-password `keychain.wrap`: once no
/// password exists it seals nothing meaningful, and leaving it would poison
/// a later re-set's silent adoption with a mismatched items key.
pub fn remove_launch_key(secret_store: &dyn SecretStore, keys_dir: &Path) {
    let _ = std::fs::remove_file(launch_wrap_path(keys_dir));
    let _ = keychain::delete_launch_key(secret_store);
    let _ = std::fs::remove_file(keys_dir.join("keychain.wrap"));
}

/// One-time, silent migration for an install that set its password *before*
/// this feature: `password.wrap` exists but no launch slot does, yet the
/// pre-password `keychain.wrap` + `master-key` duo is still on disk. When
/// that pair was the source of the migrated items key (the case after the
/// mandatory-password migration adopted the old DEK), it can be re-derived
/// with no password at all — which is what makes an *existing* install stop
/// asking on the very first launch after this lands.
///
/// Two hard guards keep this from ever trusting the wrong key:
///
/// - the candidate is verified against the live `app.sqlite` (a
///   remove-password → re-set-password cycle leaves stale `keychain.wrap`
///   material behind whose DEK no longer matches the database — that must
///   never be adopted);
/// - a wrong-anywhere result returns `None` and the launch simply stays on
///   the enter-password gate (typing the password then re-provisions).
///
/// On success the launch slot is provisioned and the items key returned —
/// the caller (`BootState::initialize`) flips the launch to
/// `UnlockWithKeychain`.
pub fn adopt_legacy_launch_key(secret_store: &dyn SecretStore, keys_dir: &Path) -> Option<DataKey> {
    let keychain_wrap = keys_dir.join("keychain.wrap");
    if !keychain_wrap.exists() {
        return None;
    }
    let kek = match keychain::load(secret_store) {
        KeyLookup::Found(kek) => kek,
        _ => return None,
    };
    let candidate = wrap::read_wrap(&keychain_wrap, &kek).ok()?;
    // `keys_dir` is `<data>/keys` — the database lives at `<data>/app.sqlite`.
    let db = keys_dir.parent()?.join("app.sqlite");
    if !db.exists() {
        return None;
    }
    // Running the full (usually no-op) migration ladder with the candidate
    // key is the honest proof it IS this install's key: SQLCipher's open
    // fails on the very first page otherwise.
    if openpal_core::app_db::open(&db, Some(&candidate)).is_err() {
        tracing::warn!(
            "legacy keychain material no longer matches this install's database — \
             not provisioning a launch key (enter the password to re-arm the keychain unlock)"
        );
        return None;
    }
    if let Err(e) = provision_launch_key(secret_store, keys_dir, &candidate) {
        tracing::warn!("couldn't provision the launch key from legacy material: {e}");
        return None;
    }
    tracing::info!(
        "adopted the legacy keychain material as the launch key — \
         this install now unlocks straight into the app"
    );
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A `SecretStore` that answers nothing — the "OS keychain holds
    /// nothing" posture, the convention this module's tests use for the
    /// keychain-absent side of every classification.
    struct EmptyStore;

    impl SecretStore for EmptyStore {
        fn store(&self, _key: &str, _value: &str) -> Result<(), String> {
            Ok(())
        }
        fn load(&self, key: &str) -> KeyLookup<String> {
            let _ = key;
            KeyLookup::Absent
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            let _ = key;
            Ok(())
        }
    }

    /// A `SecretStore` reified as a map — substitution at the `SecretStore`
    /// trait (this module's established convention: never mock the OS
    /// keychain itself, mirror the same `EmptyStore` trick), so the
    /// launch-slot round trip is exercised with real wrap-file sealing and
    /// an honest `Found`/`Absent`/delete cycle.
    #[derive(Default)]
    struct InMemoryStore {
        map: Mutex<HashMap<String, String>>,
    }

    impl SecretStore for InMemoryStore {
        fn store(&self, key: &str, value: &str) -> Result<(), String> {
            self.map
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        fn load(&self, key: &str) -> KeyLookup<String> {
            match self.map.lock().unwrap().get(key) {
                Some(value) => KeyLookup::Found(value.clone()),
                None => KeyLookup::Absent,
            }
        }
        fn delete(&self, key: &str) -> Result<(), String> {
            self.map.lock().unwrap().remove(key);
            Ok(())
        }
    }

    /// The launch-slot round trip, provisioned exactly the way the password
    /// flows do it — used by several tests below. Returns the items key.
    fn provisioned_install(dir: &std::path::Path, store: &InMemoryStore) -> DataKey {
        let keys = dir.join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        let items_key = DataKey::from_bytes([0x42; 32]);
        create_password_file(&keys, &items_key, b"hunter2").unwrap();
        provision_launch_key(store, &keys, &items_key).unwrap();
        assert!(launch_wrap_path(&keys).exists());
        assert!(matches!(
            keychain::load_launch_key(store),
            KeyLookup::Found(_)
        ));
        items_key
    }

    #[test]
    fn marker_routes_a_launch_to_no_password_mode() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        assert_eq!(
            detect(&EmptyStore, &keys, true),
            BootstrapStatus::CreatePassword,
            "a keys dir that doesn't exist yet is a first-ever run"
        );

        write_no_password_marker(&keys).unwrap();
        assert_eq!(
            detect(&EmptyStore, &keys, true),
            BootstrapStatus::NoPassword
        );
        assert!(nopass_marker_path(&keys).exists());

        // A password created later overrides the marker.
        let items_key = DataKey::from_bytes([0x42; 32]);
        create_password_file(&keys, &items_key, b"hunter2").unwrap();
        assert_eq!(
            detect(&EmptyStore, &keys, true),
            BootstrapStatus::EnterPassword,
            "password.wrap wins over the marker once a password exists"
        );

        // And removing the password restores no-password mode.
        std::fs::remove_file(vault::password_wrap_path(&keys)).unwrap();
        assert_eq!(
            detect(&EmptyStore, &keys, true),
            BootstrapStatus::NoPassword
        );
    }

    #[test]
    fn the_marker_leaves_legacy_detection_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        // A pre-password install (keychain.wrap, no password.wrap, no
        // marker): still classified through the unchanged legacy path.
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::write(keys.join("keychain.wrap"), b"legacy").unwrap();
        std::fs::write(keys.join("recovery.wrap"), b"legacy").unwrap();
        assert_eq!(
            detect(&EmptyStore, &keys, true),
            BootstrapStatus::UseRecoveryCode,
            "the marker must not change pre-password install detection"
        );
        // Writing a marker on top of a legacy install must not route to
        // NoPassword — a legacy encrypted store can't be opened plaintext.
        write_no_password_marker(&keys).unwrap();
        assert_eq!(
            detect(&EmptyStore, &keys, true),
            BootstrapStatus::UseRecoveryCode,
            "keychain.wrap detection stays ahead of the marker"
        );
    }

    #[test]
    fn a_usable_keychain_slot_classifies_as_keychain_unlock_otherwise_enter_password() {
        let dir = tempfile::tempdir().unwrap();
        let store = InMemoryStore::default();

        // Password set, launch slot provisioned: the trusted-unlocker happy
        // path — `detect` hands the launch to the keychain, no password
        // screen.
        let items_key = provisioned_install(dir.path(), &store);
        assert_eq!(
            detect(&store, &dir.path().join("keys"), true),
            BootstrapStatus::UnlockWithKeychain,
            "password.wrap + a usable launch-key slot = keychain unlock"
        );
        // And the slot really does round-trip to exactly the items key.
        assert_eq!(
            unlock_with_keychain(&store, &dir.path().join("keys")).unwrap(),
            items_key
        );

        // The keychain flat-out holds nothing: enter-password gate.
        let other = EmptyStore;
        assert_eq!(
            detect(&other, &dir.path().join("keys"), true),
            BootstrapStatus::EnterPassword,
            "a password-protected install whose keychain holds nothing still asks"
        );

        // The slot's on-disk half goes missing: same gate — nothing silently
        // unlocks without both halves.
        std::fs::remove_file(launch_wrap_path(&dir.path().join("keys"))).unwrap();
        assert_eq!(
            detect(&store, &dir.path().join("keys"), true),
            BootstrapStatus::EnterPassword,
            "launch.wrap gone => no silent unlock, even with the keychain entry still present"
        );

        // A stale on-disk half sealed under a key the keychain doesn't hold
        // must also stay on the gate — never classify a slot that can't open.
        let keys = dir.path().join("keys");
        let stale_kek = DataKey::random();
        std::fs::create_dir_all(&keys).unwrap();
        wrap::write_wrap(
            &launch_wrap_path(&keys),
            &items_key,
            &stale_kek,
            &wrap::WrapHeader::keychain_backed(),
        )
        .unwrap();
        assert_eq!(
            detect(&store, &keys, true),
            BootstrapStatus::EnterPassword,
            "a present-but-unopenable slot is not a usable keychain slot"
        );
    }

    #[test]
    fn portable_and_unsupported_builds_ignore_the_keychain_unlock() {
        let dir = tempfile::tempdir().unwrap();
        let store = InMemoryStore::default();
        provisioned_install(dir.path(), &store);

        let keys = dir.path().join("keys");
        assert_eq!(
            detect(&store, &keys, false),
            BootstrapStatus::EnterPassword,
            "a Portable build of the same data dir still prompts for the password"
        );
    }

    #[test]
    fn removing_the_launch_key_leaves_no_trace_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = InMemoryStore::default();
        provisioned_install(dir.path(), &store);
        let keys = dir.path().join("keys");

        remove_launch_key(&store, &keys);
        assert!(!launch_wrap_path(&keys).exists());
        assert!(matches!(
            keychain::load_launch_key(&store),
            KeyLookup::Absent
        ));
        // Back on the password gate, exactly like a pre-feature install.
        assert_eq!(detect(&store, &keys, true), BootstrapStatus::EnterPassword);
        assert!(matches!(
            unlock_with_keychain(&store, &keys),
            Err(BootstrapError::NoLaunchSlot)
        ));
    }

    #[test]
    fn a_matching_legacy_keychain_wrap_is_adopted_silently() {
        let dir = tempfile::tempdir().unwrap();
        let store = InMemoryStore::default();
        // An install that migrated from the pre-password model: the items
        // key is the old DEK, still sealed in keychain.wrap under the
        // keychain's master-key, and app.sqlite is keyed under it.
        let items_key = DataKey::from_bytes([0x11; 32]);
        let master_kek = DataKey::from_bytes([0x22; 32]);
        keychain::store(&store, &master_kek).unwrap();
        let keys = dir.path().join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        wrap::write_wrap(
            &keys.join("keychain.wrap"),
            &items_key,
            &master_kek,
            &wrap::WrapHeader::keychain_backed(),
        )
        .unwrap();
        create_password_file(&keys, &items_key, b"hunter2").unwrap();
        openpal_core::app_db::open(&dir.path().join("app.sqlite"), Some(&items_key)).unwrap();

        // First post-feature launch: password.wrap exists but no launch slot
        // yet -> no silent unlock through `detect` alone...
        assert_eq!(detect(&store, &keys, true), BootstrapStatus::EnterPassword);
        // ...until `BoostState::initialize`'s adoption step runs, which
        // verifies the legacy material against the database and provisions.
        let adopted = adopt_legacy_launch_key(&store, &keys).expect("legacy material matches");
        assert_eq!(adopted, items_key);
        assert_eq!(
            detect(&store, &keys, true),
            BootstrapStatus::UnlockWithKeychain
        );
        assert_eq!(unlock_with_keychain(&store, &keys).unwrap(), items_key);
    }

    #[test]
    fn stale_legacy_keychain_material_is_never_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let store = InMemoryStore::default();
        // The remove-password -> re-set-password trap: keychain.wrap +
        // master-key still hold the OLD items key, but the live database
        // (and password.wrap) belong to a fresh one.
        let old_key = DataKey::from_bytes([0x33; 32]);
        let master_kek = DataKey::from_bytes([0x44; 32]);
        keychain::store(&store, &master_kek).unwrap();
        let keys = dir.path().join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        wrap::write_wrap(
            &keys.join("keychain.wrap"),
            &old_key,
            &master_kek,
            &wrap::WrapHeader::keychain_backed(),
        )
        .unwrap();
        let live_key = DataKey::from_bytes([0x55; 32]);
        create_password_file(&keys, &live_key, b"hunter2").unwrap();
        openpal_core::app_db::open(&dir.path().join("app.sqlite"), Some(&live_key)).unwrap();

        // The adoption refuses the mismatched key and provisions nothing.
        assert!(adopt_legacy_launch_key(&store, &keys).is_none());
        assert!(!launch_wrap_path(&keys).exists());
        assert_eq!(
            detect(&store, &keys, true),
            BootstrapStatus::EnterPassword,
            "stale legacy material must not unlock a database it doesn't key"
        );
    }
}
