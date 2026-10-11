//! `DataKey` — the one type in this crate that ever holds real key
//! material (a data encryption key or a key-encryption key; the wrap file
//! format in `super::wrap` is what tells them apart on disk, not the type).
//!
//! The doc's own "redaction prerequisite, before any unlock code is
//! written" (`docs/SPACES_AUTHORING.md` §9): `Zeroizing<[u8; 32]>` on its
//! own still derives a real `Debug` impl (confirmed against the `zeroize`
//! crate's own source — it prints the bytes), so `DataKey` never derives or
//! delegates to it. `Debug`/`Display` are hand-written below and always
//! print `<redacted>`, so no format string anywhere — a `tracing::debug!`,
//! a panic payload, an `.unwrap()` on a `Result<_, DataKey>` — can ever
//! print real key material.

use rand::rngs::OsRng;
use rand::RngCore;
use std::fmt;
use zeroize::Zeroizing;

#[derive(PartialEq, Eq)]
pub struct DataKey(Zeroizing<[u8; 32]>);

impl DataKey {
    /// A fresh, random key. `OsRng` per the doc's rule: all randomness in
    /// this module comes from the OS CSPRNG, never `thread_rng`.
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

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DataKey").field(&"<redacted>").finish()
    }
}

impl fmt::Display for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_keys_are_not_all_zero_and_differ() {
        let a = DataKey::random();
        let b = DataKey::random();
        assert_ne!(a.as_bytes(), &[0u8; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn from_bytes_roundtrips() {
        let bytes = [7u8; 32];
        let key = DataKey::from_bytes(bytes);
        assert_eq!(key.as_bytes(), &bytes);
    }

    #[test]
    fn debug_and_display_never_contain_the_real_bytes() {
        let key = DataKey::from_bytes([0xAB; 32]);
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
