//! Crypto primitives for the SQLCipher store (`docs/SPACES_AUTHORING.md`
//! §9 "Store and encryption"). The mandatory-password model wires a full
//! hierarchy into the running app: [`kdf`] derives the password-derived
//! root from the user's password (Argon2id, compiled-in floors), [`wrap`]
//! seals the items key under that root as `keys/password.wrap`
//! (`crates/app::bootstrap::{create_password_file, unlock_with_password}`,
//! invoked by the frontend's `PasswordGate` via the `create_password` /
//! `unlock_with_password` commands), and [`recovery`] adds the shown-once
//! code slot (`recovery.wrap`) that survives a forgotten password.
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
//! AEAD's associated data. [`keychain`] is the legacy (pre-password) and
//! sync idle KEK source: a random secret held in the OS keychain, no KDF
//! involved — still read on the create-password migration path to adopt a
//! pre-password install's DEK, and by the sync group's wrap files.
//! [`recovery`] generates a *second* slot's KEK source instead — an
//! app-generated (never user-typed), shown-once recovery code — so a
//! forgotten password doesn't mean permanently unrecoverable data.

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
pub mod vault;
pub mod wrap;
