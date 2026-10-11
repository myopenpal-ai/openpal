//! `KeyLookup` — replaces the `.ok()?`-shaped pattern that collapses "no
//! entry" and "backend unreachable" into one `None`. `crate::account`'s
//! `load_refresh_token` did exactly this
//! (`keyring::Entry::new(...).ok()?.get_password().ok()`) before it was
//! rewritten against this type; see `crate::account::classify` for the
//! first real use. The distinction matters more the higher the stakes get:
//! for a wrapped data-encryption key, reading `Unavailable` (the keychain
//! backend itself is unreachable — common on a headless Linux session with
//! no secret-service) as `Absent` (go ahead and mint a new key) would be
//! silently catastrophic.

use std::convert::Infallible;
use std::fmt;

pub enum KeyLookup<T> {
    /// The entry exists and was read successfully.
    Found(T),
    /// No entry — a normal, expected state (e.g. never signed in, never
    /// minted).
    Absent,
    /// The backend itself couldn't be reached or answer trusted — never
    /// treat this the same as `Absent`. The `String` is a human-readable
    /// reason, safe to log.
    Unavailable(String),
}

impl<T> KeyLookup<T> {
    /// Recasts a lookup that's provably never `Found` (e.g. the result of
    /// classifying an error, where there's no value to have found) into any
    /// other `T` — for a helper like `crate::account::classify` that only
    /// ever needs to express `Absent`/`Unavailable`, without every caller
    /// having to re-match a variant that can't occur.
    pub fn from_infallible(other: KeyLookup<Infallible>) -> Self {
        match other {
            KeyLookup::Found(never) => match never {},
            KeyLookup::Absent => KeyLookup::Absent,
            KeyLookup::Unavailable(reason) => KeyLookup::Unavailable(reason),
        }
    }
}

/// Deliberately doesn't require `T: Debug` and never prints a `Found`
/// payload's contents — `KeyLookup<T>` is meant to stay safe to log even
/// when `T` itself is sensitive (a token, eventually a `DataKey`).
impl<T> fmt::Debug for KeyLookup<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyLookup::Found(_) => f.write_str("Found(..)"),
            KeyLookup::Absent => f.write_str("Absent"),
            KeyLookup::Unavailable(reason) => f.debug_tuple("Unavailable").field(reason).finish(),
        }
    }
}

/// "No entry" (never signed in, never minted) and "the keychain backend
/// itself is unreachable" (e.g. no secret-service on a headless Linux
/// session) must not collapse into the same outcome. Pure and
/// unit-testable without a real keychain, since `keyring::Error`'s
/// variants are plain, constructible values. Shared by every
/// `keyring`-backed lookup in this crate — `crate::account::load_refresh_token`
/// first, `crate::crypto::keychain::load` second.
pub fn classify_keyring_error(e: keyring::Error) -> KeyLookup<Infallible> {
    match e {
        keyring::Error::NoEntry => KeyLookup::Absent,
        other => KeyLookup::Unavailable(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_found_payload() {
        let lookup = KeyLookup::Found("super-secret-token".to_string());
        assert!(!format!("{lookup:?}").contains("super-secret-token"));
    }

    #[test]
    fn debug_of_absent_and_unavailable_is_readable() {
        assert_eq!(format!("{:?}", KeyLookup::<String>::Absent), "Absent");
        assert_eq!(
            format!(
                "{:?}",
                KeyLookup::<String>::Unavailable("no secret-service".to_string())
            ),
            "Unavailable(\"no secret-service\")"
        );
    }

    #[test]
    fn classify_maps_no_entry_to_absent() {
        assert!(matches!(
            classify_keyring_error(keyring::Error::NoEntry),
            KeyLookup::Absent
        ));
    }

    #[test]
    fn classify_maps_every_other_error_to_unavailable_with_a_reason() {
        let errors = vec![
            keyring::Error::PlatformFailure(Box::new(std::io::Error::other("boom"))),
            keyring::Error::NoStorageAccess(Box::new(std::io::Error::other("locked"))),
            keyring::Error::BadEncoding(vec![0xFF]),
            keyring::Error::TooLong("attr".to_string(), 10),
            keyring::Error::Invalid("attr".to_string(), "reason".to_string()),
            keyring::Error::Ambiguous(vec![]),
        ];
        for e in errors {
            match classify_keyring_error(e) {
                KeyLookup::Unavailable(reason) => assert!(!reason.is_empty()),
                other => panic!("expected Unavailable, got {other:?}"),
            }
        }
    }
}
