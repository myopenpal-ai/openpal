//! Stores and loads keychain slots' KEKs — the doc's `keychain.wrap`
//! primary on desktop: "a random 32-byte secret in the OS keychain
//! (`kdf_id = 0`)" (`docs/SPACES_AUTHORING.md` §9). No KDF runs for these
//! slots; the keychain access itself is the authentication.
//!
//! Two slots share this module:
//!
//! - `master-key` ([`store`]/[`load`]/[`delete`]) — the legacy pre-password
//!   slot: the KEK that opens `keys/keychain.wrap`. Still load-bearing
//!   today on the create-password migration path (adopting a pre-password
//!   install's DEK), for legacy recovery re-minting, and as the shared KEK
//!   behind the sync pairing wrap files (`crate::sync::pairing`).
//! - `launch-key` ([`store_launch_key`]/[`load_launch_key`]/
//!   [`delete_launch_key`]) — the password model's "remember me" slot: the
//!   KEK that opens `keys/launch.wrap`, which seals the same items key as
//!   `password.wrap`. Its presence is what lets an installed build open
//!   straight into the app at launch, the OS keychain standing in for the
//!   typed password (see `crates/app/src/bootstrap.rs`).
//!
//! Backed by a [`SecretStore`] (see that module) rather than `keyring`
//! directly — desktop/iOS and Android need different implementations, and
//! this crate only has the former. The encode/decode-to-a-storable-string
//! logic is pure and unit-tested here; the actual `SecretStore` calls are
//! thin, deliberately untested wrappers — there's no keychain mocking
//! anywhere in this repo, and a real-OS-keychain round trip isn't something
//! to exercise in `cargo test`.

use super::key::DataKey;
use super::lookup::KeyLookup;
use super::secret_store::SecretStore;
use base64::Engine as _;
use zeroize::Zeroizing;

const KEYCHAIN_KEY: &str = "master-key";
/// The password model's launch-unlock slot — see the module doc.
const LAUNCH_KEY: &str = "launch-key";

// Both wrapped in `Zeroizing` — the base64 encoding of the raw KEK is just
// as sensitive as the KEK itself, and neither `String` nor `Vec<u8>` scrubs
// its contents on an ordinary drop.

fn encode(kek: &DataKey) -> Zeroizing<String> {
    Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(kek.as_bytes()))
}

fn decode(s: &str) -> Option<DataKey> {
    let bytes: Zeroizing<Vec<u8>> =
        Zeroizing::new(base64::engine::general_purpose::STANDARD.decode(s).ok()?);
    let bytes: [u8; 32] = bytes.as_slice().try_into().ok()?;
    Some(DataKey::from_bytes(bytes))
}

/// Named-slot core shared by the `master-key` and `launch-key` wrappers
/// below — each public function is a thin, distinctive-error-message veneer
/// over it, so the two slots never drift in shape.
fn store_slot(
    store: &dyn SecretStore,
    slot: &str,
    kek: &DataKey,
    verb: &str,
) -> Result<(), String> {
    store
        .store(slot, &encode(kek))
        .map_err(|e| format!("couldn't {verb} to the OS keychain: {e}"))
}

fn delete_slot(store: &dyn SecretStore, slot: &str, verb: &str) -> Result<(), String> {
    store
        .delete(slot)
        .map_err(|e| format!("couldn't {verb}: {e}"))
}

fn load_slot(store: &dyn SecretStore, slot: &str, bad_entry: &str) -> KeyLookup<DataKey> {
    match store.load(slot) {
        KeyLookup::Found(encoded) => match decode(&Zeroizing::new(encoded)) {
            Some(kek) => KeyLookup::Found(kek),
            None => KeyLookup::Unavailable(bad_entry.to_string()),
        },
        KeyLookup::Absent => KeyLookup::Absent,
        KeyLookup::Unavailable(reason) => KeyLookup::Unavailable(reason),
    }
}

pub fn store(store: &dyn SecretStore, kek: &DataKey) -> Result<(), String> {
    store_slot(store, KEYCHAIN_KEY, kek, "save the master key")
}

/// Removes the keychain entry entirely — factory reset only. Best-effort:
/// an already-absent entry isn't a failure the caller needs to act on.
pub fn delete(store: &dyn SecretStore) -> Result<(), String> {
    delete_slot(store, KEYCHAIN_KEY, "remove the saved master key")
}

pub fn load(store: &dyn SecretStore) -> KeyLookup<DataKey> {
    load_slot(
        store,
        KEYCHAIN_KEY,
        "the OS keychain entry was not a valid key",
    )
}

/// The launch slot is written (or re-written, on the flows that re-verify
/// the items key: create/set/change password, a typed unlock, a recovery
/// unlock) and deleted (on remove-password) from `crates/app`. Deliberately
/// a separate slot from `master-key`: that one is still load-bearing for the
/// legacy pre-password path and for sync pairing, and must never be
/// overwritten by the password model's "remember me" key.
pub fn store_launch_key(store: &dyn SecretStore, kek: &DataKey) -> Result<(), String> {
    store_slot(store, LAUNCH_KEY, kek, "save the launch key")
}

/// Best-effort, same shape as [`delete`].
pub fn delete_launch_key(store: &dyn SecretStore) -> Result<(), String> {
    delete_slot(store, LAUNCH_KEY, "remove the saved launch key")
}

pub fn load_launch_key(store: &dyn SecretStore) -> KeyLookup<DataKey> {
    load_slot(
        store,
        LAUNCH_KEY,
        "the OS keychain entry was not a valid launch key",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrips() {
        let kek = DataKey::random();
        let decoded = decode(&encode(&kek)).unwrap();
        assert_eq!(decoded, kek);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode("not base64 at all !!!").is_none());
    }

    #[test]
    fn decode_rejects_the_wrong_length() {
        // Valid base64, wrong byte count once decoded.
        let short = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        assert!(decode(&short).is_none());
    }
}
