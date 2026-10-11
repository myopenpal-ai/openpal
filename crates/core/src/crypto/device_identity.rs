//! `DeviceIdentity` — a device's persistent Ed25519 signing keypair + X25519
//! static ("box") keypair, per `docs/SYNC_PAIRING_PROTOCOL.md`'s "Key
//! derivations" section (the `sync_identity.wrap` row). This module is
//! deliberately free of any mailbox/relay/sync-group concept: its only job
//! is "a device has a persistent signable/ECDH-capable identity," reusable
//! by any future feature that needs the same primitive, not just sync
//! pairing (a peer feature — desktop-as-inference-host-for-mobile — wants
//! to reuse this exact type). Sealing this to disk (via `super::wrap`) and
//! everything protocol-specific that *uses* the ECDH output (the
//! `pairing_key`/`rotation_key` derivations) is the caller's job, not this
//! module's.
//!
//! The **signing** key authenticates control-plane messages a device sends;
//! the **box** key is a separate persistent X25519 keypair used only for
//! static-static ECDH. Splitting them (rather than one key doing both jobs)
//! mirrors the "one wrap file, one secret" convention this crate already
//! uses for `keychain.wrap` vs. `recovery.wrap`, and keeps the signing key's
//! only job being "prove this message came from me," never key agreement.
//!
//! Same redaction/no-`Clone` discipline as [`super::key::DataKey`]: neither
//! private half is ever printed, and `DeviceIdentity` cannot be casually
//! copied. `ed25519_dalek::SigningKey` already hand-writes a redacted
//! `Debug` upstream (it excludes the raw secret bytes via
//! `finish_non_exhaustive`), but `x25519_dalek::StaticSecret` derives no
//! `Debug` at all — so `DeviceIdentity` as a whole gets its own hand-written
//! `Debug` below rather than relying on either upstream impl.

use ed25519_dalek::{Signature, SignatureError, Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use std::fmt;
use x25519_dalek::{PublicKey as BoxPublicKey, StaticSecret as BoxSecret};

/// A device's persistent identity. Generated once per device
/// (`DeviceIdentity::generate`), never rotated — device removal invalidates
/// trust in it by dropping it from the roster (`crate::sync::roster`), not
/// by changing it (see the protocol doc's "Group creation" step 2 and
/// "Revocation and rotation" section).
pub struct DeviceIdentity {
    signing_key: SigningKey,
    box_key: BoxSecret,
}

/// The two PUBLIC halves of a [`DeviceIdentity`] — what gets sent to other
/// devices during pairing and stored in the roster
/// (`crate::sync::roster::RosterEntry`). Each half round-trips through a
/// fixed 32-byte array via the underlying crates' own `to_bytes`/`as_bytes`
/// (no hand-rolled serialization).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceIdentityPublic {
    signing: VerifyingKey,
    box_public: BoxPublicKey,
}

impl DeviceIdentity {
    /// A fresh identity: an `OsRng`-generated Ed25519 signing keypair and an
    /// `OsRng`-generated X25519 static keypair. Per this crate's rule, all
    /// randomness comes from the OS CSPRNG, never `thread_rng`.
    pub fn generate() -> Self {
        Self {
            signing_key: SigningKey::generate(&mut OsRng),
            box_key: BoxSecret::random_from_rng(OsRng),
        }
    }

    /// The public halves of this identity — safe to send over the network
    /// or store in the roster.
    pub fn public(&self) -> DeviceIdentityPublic {
        DeviceIdentityPublic {
            signing: self.signing_key.verifying_key(),
            box_public: BoxPublicKey::from(&self.box_key),
        }
    }

    /// Sign `message` with this device's Ed25519 signing key. Used to
    /// authenticate control-plane messages (roster updates, rotation) —
    /// see the protocol doc's `Rotate`/`DeviceAdded`/`JoinConfirm` message
    /// shapes.
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// This device's static X25519 secret, for callers that need to run
    /// ECDH directly (e.g. `their_public_key.diffie_hellman(&their_public)`
    /// — see `x25519_dalek::StaticSecret::diffie_hellman`). The actual
    /// key-derivation formulas that consume the resulting `SharedSecret`
    /// (`pairing_key`, `rotation_key`) live in `crate::sync::pairing`, not
    /// here.
    pub fn box_secret(&self) -> &BoxSecret {
        &self.box_key
    }

    /// Raw signing-key bytes, for sealing to disk via `crate::crypto::wrap`
    /// (which seals a fixed 32-byte [`super::key::DataKey`], so the caller
    /// needs the raw bytes to construct one). `pub(crate)`, not `pub`: this
    /// is exactly the kind of raw-private-key access this module's own doc
    /// comment says stays the caller's job — but only a caller *inside this
    /// crate* (`crate::sync::pairing`'s wrap-file sealing) should ever
    /// reach for it; an external consumer of this reusable type gets the
    /// opaque, misuse-resistant API only.
    pub(crate) fn signing_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Raw box-key bytes — same rationale as [`Self::signing_key_bytes`].
    pub(crate) fn box_key_bytes(&self) -> [u8; 32] {
        self.box_key.to_bytes()
    }

    /// Reconstructs a [`DeviceIdentity`] from raw bytes previously obtained
    /// via [`Self::signing_key_bytes`]/[`Self::box_key_bytes`] (i.e. after
    /// unsealing them from disk). `pub(crate)` for the same reason those
    /// two accessors are — this is the unsealing counterpart to sealing,
    /// not a general-purpose "build an identity from arbitrary bytes" API.
    pub(crate) fn from_raw_bytes(signing_key_bytes: [u8; 32], box_key_bytes: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&signing_key_bytes),
            box_key: BoxSecret::from(box_key_bytes),
        }
    }
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("signing_key", &"<redacted>")
            .field("box_key", &"<redacted>")
            .finish()
    }
}

impl DeviceIdentityPublic {
    /// Reconstruct from the raw bytes stored in the roster or received over
    /// the wire. Fails if `identity_sign_pub` doesn't decode to a valid
    /// Ed25519 point — `box_pub` can never fail to construct (X25519
    /// accepts any 32 bytes as input, clamping happens inside
    /// Diffie-Hellman itself).
    pub fn from_bytes(
        identity_sign_pub: [u8; 32],
        box_pub: [u8; 32],
    ) -> Result<Self, SignatureError> {
        Ok(Self {
            signing: VerifyingKey::from_bytes(&identity_sign_pub)?,
            box_public: BoxPublicKey::from(box_pub),
        })
    }

    pub fn signing_key(&self) -> &VerifyingKey {
        &self.signing
    }

    pub fn box_public(&self) -> &BoxPublicKey {
        &self.box_public
    }

    pub fn identity_sign_pub_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn box_pub_bytes(&self) -> [u8; 32] {
        *self.box_public.as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Verifier;

    #[test]
    fn sign_and_verify_round_trips() {
        let identity = DeviceIdentity::generate();
        let message = b"openpal-sync-test-message";
        let signature = identity.sign(message);
        assert!(identity
            .public()
            .signing_key()
            .verify(message, &signature)
            .is_ok());
    }

    #[test]
    fn verify_rejects_a_tampered_message() {
        let identity = DeviceIdentity::generate();
        let signature = identity.sign(b"original message");
        assert!(identity
            .public()
            .signing_key()
            .verify(b"tampered message", &signature)
            .is_err());
    }

    #[test]
    fn ecdh_between_two_identities_produces_the_same_shared_secret_from_both_sides() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();

        let shared_from_a = a.box_secret().diffie_hellman(b.public().box_public());
        let shared_from_b = b.box_secret().diffie_hellman(a.public().box_public());

        assert_eq!(shared_from_a.as_bytes(), shared_from_b.as_bytes());
    }

    #[test]
    fn two_generated_identities_differ() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        assert_ne!(a.public(), b.public());
    }

    #[test]
    fn debug_never_contains_raw_private_bytes() {
        let identity = DeviceIdentity::generate();
        let signing_bytes = identity.signing_key.to_bytes();
        let box_bytes = identity.box_key.to_bytes();

        let debug = format!("{identity:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains(&hex_encode(&signing_bytes)));
        assert!(!debug.contains(&hex_encode(&box_bytes)));
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn public_bytes_round_trip_through_from_bytes() {
        let identity = DeviceIdentity::generate();
        let public = identity.public();
        let reconstructed = DeviceIdentityPublic::from_bytes(
            public.identity_sign_pub_bytes(),
            public.box_pub_bytes(),
        )
        .unwrap();
        assert_eq!(public, reconstructed);
    }
}
