//! Stores and loads the keychain slot's KEK — the doc's `keychain.wrap`
//! primary on desktop: "a random 32-byte secret in the OS keychain
//! (`kdf_id = 0`)" (`docs/SPACES_AUTHORING.md` §9). No KDF runs for this
//! slot; the keychain access itself is the authentication.
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

pub fn store(store: &dyn SecretStore, kek: &DataKey) -> Result<(), String> {
    store
        .store(KEYCHAIN_KEY, &encode(kek))
        .map_err(|e| format!("couldn't save the master key to the OS keychain: {e}"))
}

/// Removes the keychain entry entirely — factory reset only. Best-effort:
/// an already-absent entry isn't a failure the caller needs to act on.
pub fn delete(store: &dyn SecretStore) -> Result<(), String> {
    store
        .delete(KEYCHAIN_KEY)
        .map_err(|e| format!("couldn't remove the saved master key: {e}"))
}

pub fn load(store: &dyn SecretStore) -> KeyLookup<DataKey> {
    match store.load(KEYCHAIN_KEY) {
        KeyLookup::Found(encoded) => match decode(&Zeroizing::new(encoded)) {
            Some(kek) => KeyLookup::Found(kek),
            None => KeyLookup::Unavailable("the OS keychain entry was not a valid key".to_string()),
        },
        KeyLookup::Absent => KeyLookup::Absent,
        KeyLookup::Unavailable(reason) => KeyLookup::Unavailable(reason),
    }
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
