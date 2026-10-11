//! `SyncKey` — the shared secret every device in a sync group holds a copy
//! of once paired (`docs/SYNC_PAIRING_PROTOCOL.md`'s "Key derivations" and
//! "Group creation" sections). Constructed exactly like
//! [`super::key::DataKey`] (32 random `OsRng` bytes) but kept as a distinct
//! newtype rather than a type alias, so a `SyncKey` can never be handed to
//! code expecting a local data-encryption key by accident — the two secrets
//! protect different things (one device's local store vs. a whole sync
//! group's shared material) and mixing them up would be exactly the kind of
//! bug a type system should catch at compile time.
//!
//! Same redaction discipline as `DataKey` (see that module's own doc
//! comment for the full rationale): `Debug`/`Display` are hand-written and
//! always print `"<redacted>"`, never derived from `Zeroizing`, which still
//! derives a real (leaky) `Debug` on its own.

use rand::rngs::OsRng;
use rand::RngCore;
use std::fmt;
use zeroize::Zeroizing;

#[derive(PartialEq, Eq)]
pub struct SyncKey(Zeroizing<[u8; 32]>);

impl SyncKey {
    /// A fresh, random key. `OsRng` per this crate's rule: all randomness
    /// in `crate::crypto` comes from the OS CSPRNG, never `thread_rng`.
    pub fn random() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self::from_bytes(bytes)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for SyncKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SyncKey").field(&"<redacted>").finish()
    }
}

impl fmt::Display for SyncKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_keys_are_not_all_zero_and_differ() {
        let a = SyncKey::random();
        let b = SyncKey::random();
        assert_ne!(a.as_bytes(), &[0u8; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn from_bytes_roundtrips() {
        let bytes = [7u8; 32];
        let key = SyncKey::from_bytes(bytes);
        assert_eq!(key.as_bytes(), &bytes);
    }

    #[test]
    fn debug_and_display_never_contain_the_real_bytes() {
        let key = SyncKey::from_bytes([0xAB; 32]);
        let debug = format!("{key:?}");
        let display = format!("{key}");
        // "ab" repeated would be the hex-ish giveaway if a format string
        // ever leaked the raw bytes through a naive {:x?}-style render.
        assert!(!debug.contains("ab"));
        assert!(!display.contains("ab"));
        assert!(debug.contains("redacted"));
        assert!(display.contains("redacted"));
    }
}
