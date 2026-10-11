//! Crypto primitives for the SQLCipher store (`docs/SPACES_AUTHORING.md`
//! §9 "Store and encryption"). [`keychain`] is the one piece of this
//! module actually wired into a running app (`crates/app`'s
//! `bootstrap::mint_or_unlock`, called from `lib.rs`'s `setup()`) — the
//! rest stay pure library, not yet reachable from `setup()` or any
//! command.
//!
//! [`key::DataKey`] is the one type that ever holds real key material — a
//! newtype whose `Debug`/`Display` always print `<redacted>`, so no format
//! string anywhere can leak it. [`lookup::KeyLookup`] is the enum that
//! replaces `.ok()?`-style collapsing of "no entry" and "backend
//! unreachable" (see [`lookup::classify_keyring_error`] and
//! [`keychain::load`] for its real uses). [`kdf`] derives a key from a
//! passphrase (Argon2id). [`wrap`] reads and writes one wrap file — an
//! AES-256-GCM-sealed [`key::DataKey`] under another [`key::DataKey`] used
//! as a KEK, with every weakenable KDF parameter authenticated in the
//! AEAD's associated data. [`keychain`] is the desktop primary slot's KEK
//! source: a random secret held in the OS keychain, no KDF involved.
//! [`recovery`] generates the *second* slot's KEK source instead — an
//! app-generated (never user-typed), shown-once recovery code — so a lost
//! keychain entry doesn't mean permanently unrecoverable data.

pub mod device_identity;
pub mod kdf;
pub mod key;
pub mod keychain;
pub mod logframe;
pub mod lookup;
pub mod recovery;
pub mod secret_store;
pub(crate) mod sqlcipher;
pub mod sync_key;
pub mod wrap;
