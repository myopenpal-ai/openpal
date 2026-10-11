//! Device pairing, key derivations, and revocation/rotation — the protocol
//! specified in `docs/SYNC_PAIRING_PROTOCOL.md`, adversarially reviewed
//! (5 independent attacker lenses) before this was written. Read that
//! document first; this module's doc comments cross-reference it but don't
//! repeat its rationale.
//!
//! **The one rule that matters most in this file**: a `join_request`,
//! `join_challenge`, `join_confirm`, or `rotate` envelope that doesn't
//! carry a valid MAC (verified via [`parse_mac_envelope`]) must never be
//! acted on and never rendered to a user — the MAC gate is what makes
//! forgery by a malicious relay or a network attacker who doesn't know
//! `group_secret` structurally impossible, not the human fingerprint
//! comparison (which is defense in depth, not the primary defense — see
//! the protocol doc's "Red team findings and fixes", `malicious-relay`
//! Finding A/B).
//!
//! **Device ids are deterministic, not randomly assigned**: every id in
//! this module is `device_id_for(identity_sign_pub)`, computed
//! independently by whichever side needs it from a public key both sides
//! already have. Assigning a random id on one side and hoping the other
//! side learns the same value led to real inconsistency bugs while writing
//! this — deriving it instead means there is nothing to transmit or agree
//! on.

use crate::crypto::device_identity::{DeviceIdentity, DeviceIdentityPublic};
use crate::crypto::kdf;
use crate::crypto::key::DataKey;
#[cfg(not(test))]
use crate::crypto::keychain;
#[cfg(not(test))]
use crate::crypto::lookup::KeyLookup;
use crate::crypto::recovery;
use crate::crypto::secret_store::SecretStore;
use crate::crypto::sync_key::SyncKey;
use crate::crypto::wrap::{self, WrapHeader};
use crate::sync::key_state;
use crate::sync::mailbox_client::MailboxTransport;
use crate::sync::roster::{self, RosterEntry};
use crate::{Error, Result};
use ed25519_dalek::{Signature as EdSignature, Verifier as _, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::hmac;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use x25519_dalek::{EphemeralSecret, PublicKey as BoxPublicKey};
use zeroize::Zeroize;

/// A generic, non-recoverable pairing failure message — every internal
/// error in this module (bad MAC, failed AEAD open, bad signature, bad
/// wire format, transport error) collapses to text this vague on purpose.
/// Distinguishing "forged" from "the network hiccuped" in anything shown
/// to a user would only help an attacker calibrate their next attempt.
const GENERIC_FAILURE: &str = "That pairing didn't go through. Please try again.";
#[cfg(not(test))]
const GENERIC_KEYCHAIN_FAILURE: &str =
    "Couldn't reach the secure key storage on this device. Please try again.";

fn pairing_err(_detail: &str) -> Error {
    // `_detail` exists for a future `tracing::warn!` at call sites, not for
    // the user-facing message — see `GENERIC_FAILURE`'s doc comment.
    Error::Pairing(GENERIC_FAILURE.to_string())
}

// ---------------------------------------------------------------------------
// Key derivations — docs/SYNC_PAIRING_PROTOCOL.md's "Key derivations" table
// ---------------------------------------------------------------------------

/// `group_secret` as produced by `recovery.rs:35-39`'s `generate()` is a
/// hyphen-grouped Crockford Base32 string. HKDF's input keying material
/// here is that string's *normalized* (uppercased, hyphen-stripped) UTF-8
/// bytes, not the original 16 raw entropy bytes — `recovery::encode` is a
/// deterministic, injective (lossless) encoding, so using the canonical
/// string form costs no entropy versus decoding back to raw bytes, and
/// avoids needing a decode function that doesn't otherwise exist in
/// `recovery.rs`. Every derivation below that takes `group_secret` calls
/// this first.
fn group_secret_ikm(group_secret: &str) -> Vec<u8> {
    recovery::normalize(group_secret).into_bytes()
}

/// `control_mailbox_id = HKDF(group_secret, info="openpal-sync-control-mailbox-v1")`,
/// hex-encoded. Fixed forever for a sync group — see the protocol doc for
/// why this deliberately never rotates.
pub fn control_mailbox_id(group_secret: &str) -> String {
    let key = kdf::derive_subkey_from_bytes(
        &group_secret_ikm(group_secret),
        b"openpal-sync-control-mailbox-v1",
    );
    hex_encode(key.as_bytes())
}

/// `mailbox_auth = HKDF(group_secret, info="openpal-sync-mailbox-auth-v1")` —
/// the HMAC-SHA256 key every pairing/rotation envelope's MAC is computed
/// and verified under. Never sent anywhere; both sides derive it
/// independently from `group_secret`.
fn mailbox_auth_key(group_secret: &str) -> DataKey {
    kdf::derive_subkey_from_bytes(
        &group_secret_ikm(group_secret),
        b"openpal-sync-mailbox-auth-v1",
    )
}

/// `data_mailbox_id_n = HKDF(group_secret, info="openpal-sync-data-mailbox-v1" || gen_n)`.
/// Phase 3's conversation-data mailbox — generation-scoped so a removed
/// device can never derive a future generation's address. Implemented here
/// (unused until Phase 3 wires up data sync) because it's one of the six
/// derivations this module owns, and `gen_n` scoping is exactly what
/// [`remove_device`]'s revocation logic depends on being correct.
pub fn data_mailbox_id(group_secret: &str, gen_n: u32) -> String {
    let mut info = b"openpal-sync-data-mailbox-v1".to_vec();
    info.extend_from_slice(&gen_n.to_be_bytes());
    let key = kdf::derive_subkey_from_bytes(&group_secret_ikm(group_secret), &info);
    hex_encode(key.as_bytes())
}

/// `pairing_key = HKDF(ECDH(a_eph_priv, b_eph_pub) || group_secret, info="openpal-pairing-v1" || control_mailbox_id || request_id)`.
/// The AEAD key `join_confirm`'s `encrypted_sync_key` is sealed under.
/// Binding `group_secret` into the key itself (not just the mailbox
/// address) is the fix for red-team finding `malicious-relay`/A — a party
/// who only observes the relay traffic (and therefore knows
/// `control_mailbox_id` and both ephemeral public keys, but not
/// `group_secret`) cannot derive this key even if it also somehow computed
/// the correct ECDH shared secret.
fn pairing_key(
    shared_secret: &x25519_dalek::SharedSecret,
    group_secret: &str,
    control_mailbox_id: &str,
    request_id: &[u8; 16],
) -> DataKey {
    let mut ikm = shared_secret.as_bytes().to_vec();
    ikm.extend_from_slice(&group_secret_ikm(group_secret));
    let mut info = b"openpal-pairing-v1".to_vec();
    info.extend_from_slice(control_mailbox_id.as_bytes());
    info.extend_from_slice(request_id);
    let key = kdf::derive_subkey_from_bytes(&ikm, &info);
    ikm.zeroize();
    key
}

/// `fingerprint = truncate(SHA-256(a_eph_pub || b_eph_pub || control_mailbox_id), 50 bits)`,
/// Crockford-Base32-encoded via `recovery.rs`'s `encode`/`group` helpers
/// (reused, not reimplemented — see that module), 2 groups of 5. Both
/// sides compute this independently from the same transcript once they've
/// both seen `a_eph_pub`/`b_eph_pub` (over the MAC-gated channel) and
/// display it for the user to compare — defense in depth on top of the MAC
/// gate, not the primary defense (see this module's top doc comment).
pub fn fingerprint(
    a_eph_pub: &BoxPublicKey,
    b_eph_pub: &BoxPublicKey,
    control_mailbox_id: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(a_eph_pub.as_bytes());
    hasher.update(b_eph_pub.as_bytes());
    hasher.update(control_mailbox_id.as_bytes());
    let digest = hasher.finalize();

    // 50 bits: the first 7 bytes (56 bits) of the digest, with the low 6
    // bits of the 7th byte zeroed out before Crockford-encoding, so the
    // encoded form is deterministic regardless of how `encode` treats a
    // partial-byte tail.
    let mut truncated = [0u8; 7];
    truncated.copy_from_slice(&digest[..7]);
    truncated[6] &= 0b1100_0000;

    let full = recovery::encode(&truncated);
    // `encode` emits one Crockford char per 5 bits; 50 bits is exactly 10
    // chars (56 bits of input produces up to 12, so take the first 10 —
    // the trailing encoded chars came only from the zeroed-out padding
    // above and carry no entropy).
    let code: String = full.chars().take(10).collect();
    recovery::group(&code)
}

/// `rotation_key = HKDF(ECDH(initiator_box_priv, survivor_box_pub), info="openpal-sync-rotation-v1" || control_mailbox_id || target_device_id || gen_n)`.
/// Per-survivor AEAD key for delivering a rotated `SyncKey`, either during
/// device removal (`target_device_id: Some(...)`) or a periodic rotation
/// that isn't removing anyone (`None`, see [`rotate_key_periodically`]).
/// Static-static ECDH (both box keys are the devices' persistent identity
/// keys, not ephemeral) — secure against anyone who doesn't hold one of the
/// two private halves, independent of `group_secret` (unlike `pairing_key`,
/// this doesn't need `group_secret` bound in: it's already bound to two
/// specific, roster-verified long-term identities, which `group_secret`
/// alone can't forge — see [`verify_roster_signature`]).
fn rotation_key(
    shared_secret: &x25519_dalek::SharedSecret,
    control_mailbox_id: &str,
    target_device_id: Option<&str>,
    gen_n: u32,
) -> DataKey {
    let mut info = b"openpal-sync-rotation-v1".to_vec();
    info.extend_from_slice(control_mailbox_id.as_bytes());
    encode_optional_target(&mut info, target_device_id);
    info.extend_from_slice(&gen_n.to_be_bytes());
    kdf::derive_subkey_from_bytes(shared_secret.as_bytes(), &info)
}

/// Appends a 1-byte presence tag (`0x00` = none, `0x01` = present, then the
/// UTF-8 bytes) for `target_device_id` — used identically by
/// [`rotation_key`] and [`Rotate::aead_aad`] so both sides derive the same
/// key/AAD regardless of which case they're in. An explicit tag (rather
/// than, say, an empty-string sentinel) keeps "periodic rotation, no
/// target" unambiguous by construction rather than by the coincidence of
/// which strings `roster::validate_id` happens to allow.
fn encode_optional_target(buf: &mut Vec<u8>, target_device_id: Option<&str>) {
    match target_device_id {
        None => buf.push(0x00),
        Some(id) => {
            buf.push(0x01);
            buf.extend_from_slice(id.as_bytes());
        }
    }
}

/// A device's roster/mailbox id: deterministic, not randomly assigned —
/// see this module's top doc comment for why. `identity_sign_pub`'s own
/// bytes are already public (transmitted in every pairing message), so
/// hashing them costs no confidentiality; it just gives a shorter, more
/// roster-friendly identifier than the full 32-byte key.
pub fn device_id_for(identity_sign_pub: &[u8; 32]) -> String {
    let digest = Sha256::digest(identity_sign_pub);
    hex_encode(&digest[..16])
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> std::result::Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

// ---------------------------------------------------------------------------
// MAC gate
// ---------------------------------------------------------------------------

fn compute_mac(group_secret: &str, canonical_bytes: &[u8]) -> Vec<u8> {
    let key_bytes = mailbox_auth_key(group_secret);
    let key = hmac::Key::new(hmac::HMAC_SHA256, key_bytes.as_bytes());
    hmac::sign(&key, canonical_bytes).as_ref().to_vec()
}

/// Constant-time MAC verification via `ring::hmac::verify` — never `==` on
/// the raw bytes.
fn verify_mac(group_secret: &str, canonical_bytes: &[u8], mac: &[u8]) -> bool {
    let key_bytes = mailbox_auth_key(group_secret);
    let key = hmac::Key::new(hmac::HMAC_SHA256, key_bytes.as_bytes());
    hmac::verify(&key, canonical_bytes, mac).is_ok()
}

/// Wire envelope: a message plus the MAC computed over that message's own
/// JSON serialization. `#[serde(flatten)]` merges `message`'s fields into
/// the same JSON object as `mac_hex` on the wire — round-tripping a
/// concrete `T` through this (serialize → deserialize → re-serialize just
/// `message`) reproduces byte-identical JSON both times, since `derive`d
/// `Serialize` always emits a struct's fields in declaration order
/// regardless of the flatten wrapper — that determinism is what makes
/// re-deriving the MAC input at verification time sound.
#[derive(Serialize, Deserialize)]
struct MacEnvelope<T> {
    #[serde(flatten)]
    message: T,
    mac_hex: String,
}

fn build_mac_envelope<T: Serialize>(
    message: T,
    group_secret: &str,
) -> std::result::Result<Vec<u8>, String> {
    let canonical = serde_json::to_vec(&message).map_err(|e| e.to_string())?;
    let mac_hex = hex_encode(&compute_mac(group_secret, &canonical));
    serde_json::to_vec(&MacEnvelope { message, mac_hex }).map_err(|e| e.to_string())
}

/// Parses a MAC-enveloped message and verifies its MAC before returning
/// it. **This is the MAC gate.** Nothing calling this function should ever
/// treat its `Err` case as anything other than "ignore this envelope, it
/// isn't a legitimate message" — never logged as a pending request, never
/// rendered, never partially acted on.
fn parse_mac_envelope<M: for<'de> Deserialize<'de> + Serialize>(
    bytes: &[u8],
    group_secret: &str,
) -> std::result::Result<M, ()> {
    let envelope: MacEnvelope<M> = serde_json::from_slice(bytes).map_err(|_| ())?;
    let canonical = serde_json::to_vec(&envelope.message).map_err(|_| ())?;
    let mac = hex_decode(&envelope.mac_hex).map_err(|_| ())?;
    if !verify_mac(group_secret, &canonical, &mac) {
        return Err(());
    }
    Ok(envelope.message)
}

async fn push_mac_envelope<T: MailboxTransport, M: Serialize>(
    transport: &T,
    mailbox_id: &str,
    message: M,
    group_secret: &str,
    ttl_seconds: u64,
) -> std::result::Result<(), String> {
    let enveloped = build_mac_envelope(message, group_secret)?;
    transport
        .push(mailbox_id, &enveloped, MESSAGE_VERSION, Some(ttl_seconds))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// AEAD (SyncKey / rotation-key delivery) — mirrors crypto::wrap's exact
// ring::aead usage (nonce via OsRng, AES-256-GCM, zeroize plaintext buffers
// on every exit path).
// ---------------------------------------------------------------------------

/// Seals `plaintext` (a `SyncKey`'s 32 bytes) under `key`, with `aad` bound
/// in as AEAD associated data. Output is `nonce || ciphertext || tag`.
fn aead_seal(key: &DataKey, aad: &[u8], plaintext: &[u8; 32]) -> Vec<u8> {
    let unbound = UnboundKey::new(&AES_256_GCM, key.as_bytes())
        .expect("32-byte key is valid for AES-256-GCM");
    let sealing_key = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    let mut in_out = plaintext.to_vec();
    sealing_key
        .seal_in_place_append_tag(nonce, Aad::from(aad), &mut in_out)
        .expect("AES-256-GCM seal with a validated key/nonce cannot fail");
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&in_out);
    in_out.zeroize();
    out
}

/// Opens a blob produced by [`aead_seal`]. `Err` on any failure — wrong
/// key, tampered ciphertext/tag, or wrong `aad` — deliberately without
/// distinguishing which: an AEAD failure here means a forged/tampered
/// envelope slipped past the MAC gate, treated as a hard failure, never a
/// silent retry (protocol doc step 6).
fn aead_open(key: &DataKey, aad: &[u8], sealed: &[u8]) -> Result<[u8; 32]> {
    if sealed.len() < NONCE_LEN {
        return Err(pairing_err("sealed blob shorter than one nonce"));
    }
    let (nonce_bytes, ciphertext_and_tag) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes)
        .map_err(|_| pairing_err("malformed nonce"))?;
    let unbound = UnboundKey::new(&AES_256_GCM, key.as_bytes())
        .expect("32-byte key is valid for AES-256-GCM");
    let opening_key = LessSafeKey::new(unbound);

    let mut buf = ciphertext_and_tag.to_vec();
    let result = opening_key
        .open_in_place(nonce, Aad::from(aad), &mut buf)
        .map_err(|_| pairing_err("AEAD open failed"))
        .and_then(|plaintext| {
            let bytes: [u8; 32] = plaintext
                .try_into()
                .map_err(|_| pairing_err("unexpected plaintext length"))?;
            Ok(bytes)
        });
    buf.zeroize();
    result
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A random 16-byte request id — `rand::rngs::OsRng`, explicitly, per the
/// protocol doc's fix for replay-race-dos/Fix-4 ("`request_id` is now an
/// explicit 16-byte `OsRng` value, not just 'random'").
fn generate_request_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    OsRng.fill_bytes(&mut id);
    id
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    pub control_mailbox_id: String,
    #[serde(with = "hex_bytes16")]
    pub request_id: [u8; 16],
    pub b_eph_pub: [u8; 32],
    pub b_identity_pub: [u8; 32],
    pub b_box_pub: [u8; 32],
    pub b_device_label: String,
    pub requested_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinChallenge {
    pub control_mailbox_id: String,
    #[serde(with = "hex_bytes16")]
    pub request_id: [u8; 16],
    pub a_eph_pub: [u8; 32],
    pub challenged_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinConfirm {
    pub control_mailbox_id: String,
    #[serde(with = "hex_bytes16")]
    pub request_id: [u8; 16],
    pub a_identity_pub: [u8; 32],
    pub a_box_pub: [u8; 32],
    pub gen_n: u32,
    #[serde(with = "hex_bytes_vec")]
    pub encrypted_sync_key: Vec<u8>,
    #[serde(with = "hex_bytes64")]
    pub a_sig: [u8; 64],
}

impl JoinConfirm {
    /// Everything except `a_sig` — the signed payload per the protocol
    /// doc's step 5: `Ed25519(a_identity_sign_priv, (control_mailbox_id,
    /// request_id, a_identity_pub, a_box_pub, gen_n))`. Signs the
    /// long-term identity keys (the recipient's only authentic way to
    /// learn them), not the ephemeral `a_eph_pub` — that's already bound
    /// into `pairing_key` via the AEAD associated data below, so signing
    /// it again would be redundant, not stronger.
    fn signed_fields(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Signed<'a> {
            control_mailbox_id: &'a str,
            #[serde(with = "hex_bytes16")]
            request_id: [u8; 16],
            a_identity_pub: [u8; 32],
            a_box_pub: [u8; 32],
            gen_n: u32,
        }
        serde_json::to_vec(&Signed {
            control_mailbox_id: &self.control_mailbox_id,
            request_id: self.request_id,
            a_identity_pub: self.a_identity_pub,
            a_box_pub: self.a_box_pub,
            gen_n: self.gen_n,
        })
        .expect("always serializes")
    }

    /// AEAD associated data for `encrypted_sync_key` —
    /// `(type="join_confirm", control_mailbox_id, request_id)` per the
    /// protocol doc, so a captured ciphertext can't be replayed into a
    /// different pairing event (red-team fix `malicious-relay`/Fix-2).
    fn aead_aad(control_mailbox_id: &str, request_id: &[u8; 16]) -> Vec<u8> {
        let mut aad = b"join_confirm".to_vec();
        aad.extend_from_slice(control_mailbox_id.as_bytes());
        aad.extend_from_slice(request_id);
        aad
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAdded {
    pub identity_pub: [u8; 32],
    pub box_pub: [u8; 32],
    pub device_label: String,
    pub gen_n: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rotate {
    pub control_mailbox_id: String,
    /// `Some(id)` for a device-removal rotation (the device being removed);
    /// `None` for a periodic rotation that isn't removing anyone (see
    /// [`rotate_key_periodically`]).
    pub target_device_id: Option<String>,
    pub gen_n_new: u32,
    pub for_device_id: String,
    pub initiator_device_id: String,
    #[serde(with = "hex_bytes_vec")]
    pub ciphertext: Vec<u8>,
    #[serde(with = "hex_bytes64")]
    pub initiator_sig: [u8; 64],
}

impl Rotate {
    /// The signed payload: `(control_mailbox_id, target_device_id,
    /// gen_n_new, for_device_id, ciphertext)`. `initiator_device_id` isn't
    /// included — the verifier already knows who it's checking the
    /// signature against (it looked the roster entry up by that id before
    /// calling this), so signing it again would be redundant.
    fn signed_fields(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Signed<'a> {
            control_mailbox_id: &'a str,
            target_device_id: Option<&'a str>,
            gen_n_new: u32,
            for_device_id: &'a str,
            #[serde(with = "hex_bytes_vec")]
            ciphertext: &'a [u8],
        }
        serde_json::to_vec(&Signed {
            control_mailbox_id: &self.control_mailbox_id,
            target_device_id: self.target_device_id.as_deref(),
            gen_n_new: self.gen_n_new,
            for_device_id: &self.for_device_id,
            ciphertext: &self.ciphertext,
        })
        .expect("always serializes")
    }

    fn aead_aad(
        control_mailbox_id: &str,
        target_device_id: Option<&str>,
        gen_n_new: u32,
        for_device_id: &str,
    ) -> Vec<u8> {
        let mut aad = b"rotate".to_vec();
        aad.extend_from_slice(control_mailbox_id.as_bytes());
        encode_optional_target(&mut aad, target_device_id);
        aad.extend_from_slice(&gen_n_new.to_be_bytes());
        aad.extend_from_slice(for_device_id.as_bytes());
        aad
    }
}

mod hex_bytes16 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        bytes: &[u8; 16],
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex_encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<[u8; 16], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = super::hex_decode(&s).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 16 bytes"))
    }
}

mod hex_bytes64 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(
        bytes: &[u8; 64],
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex_encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<[u8; 64], D::Error> {
        let s = String::deserialize(d)?;
        let bytes = super::hex_decode(&s).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 64 bytes"))
    }
}

mod hex_bytes_vec {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex_encode(bytes))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        super::hex_decode(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Wrap-file sealing for the three new keychain-backed secrets
// ---------------------------------------------------------------------------

/// Real OS keychain in production. In `cfg(test)`, sidesteps it entirely
/// rather than mocking it — `crate::crypto::keychain`'s own doc comment
/// establishes "no keychain mocking anywhere in this repo, a real-OS-
/// keychain round trip isn't something to exercise in `cargo test`," and a
/// headless CI runner has no Secret Service/keyring daemon for the real
/// path to reach anyway. This module's tests exercise real wrap-file
/// sealing/opening across multiple simulated "devices" (separate temp
/// dirs), which needs *some* KEK to round-trip through, so every simulated
/// device within one test binary run shares one process-wide random KEK —
/// this actually matches production more closely than it might look: two
/// real OpenPal installs on the same physical machine already share one
/// real OS-keychain-derived KEK today, differing only in their own
/// separate wrap files under different data directories, exactly the
/// `dir_a`/`dir_b` tempdir pattern these tests already use.
fn load_kek(store: &dyn SecretStore) -> Result<DataKey> {
    #[cfg(test)]
    {
        let _ = store; // bypassed entirely below — never touched in test mode
        use std::sync::OnceLock;
        static TEST_KEK_BYTES: OnceLock<[u8; 32]> = OnceLock::new();
        let bytes = *TEST_KEK_BYTES.get_or_init(|| *DataKey::random().as_bytes());
        Ok(DataKey::from_bytes(bytes))
    }
    #[cfg(not(test))]
    {
        match keychain::load(store) {
            KeyLookup::Found(kek) => Ok(kek),
            _ => Err(Error::Pairing(GENERIC_KEYCHAIN_FAILURE.to_string())),
        }
    }
}

/// `group_secret`'s normalized form is always exactly 26 uppercase
/// Crockford-alphabet ASCII characters (`recovery.rs`'s `generate`/
/// `normalize`) — well under 32 bytes and never containing a NUL byte
/// (the Crockford alphabet excludes byte 0), so zero-padding it into a
/// `DataKey`-shaped 32-byte slot and trimming at the first NUL on read-back
/// round-trips safely without needing a new wrap-file format for a
/// variable-length secret.
fn seal_group_secret(path: &Path, group_secret: &str, kek: &DataKey) -> Result<()> {
    let normalized = recovery::normalize(group_secret);
    let secret_bytes = normalized.as_bytes();
    debug_assert!(
        secret_bytes.len() <= 32,
        "normalized recovery codes are always <= 32 bytes"
    );
    let mut bytes = [0u8; 32];
    let len = secret_bytes.len().min(32);
    bytes[..len].copy_from_slice(&secret_bytes[..len]);
    let as_key = DataKey::from_bytes(bytes);
    wrap::write_wrap(path, &as_key, kek, &WrapHeader::keychain_backed())
        .map_err(|_| pairing_err("couldn't save this device's sync-group secret"))
}

fn open_group_secret(path: &Path, kek: &DataKey) -> Result<String> {
    let data_key = wrap::read_wrap(path, kek)
        .map_err(|_| pairing_err("couldn't unlock this device's sync-group secret"))?;
    let bytes = data_key.as_bytes();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(32);
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| pairing_err("corrupt sync-group secret"))
}

fn seal_sync_key(path: &Path, sync_key: &SyncKey, kek: &DataKey) -> Result<()> {
    let as_key = DataKey::from_bytes(*sync_key.as_bytes());
    wrap::write_wrap(path, &as_key, kek, &WrapHeader::keychain_backed())
        .map_err(|_| pairing_err("couldn't save this device's sync key"))
}

fn open_sync_key(path: &Path, kek: &DataKey) -> Result<SyncKey> {
    let data_key = wrap::read_wrap(path, kek)
        .map_err(|_| pairing_err("couldn't unlock this device's sync key"))?;
    Ok(SyncKey::from_bytes(*data_key.as_bytes()))
}

/// Two 32-byte secrets (signing + box) packed into two wrap files derived
/// from `path` (`<path>.signing`/`<path>.box`) rather than one call —
/// `wrap::write_wrap` seals a single fixed-size `DataKey`, and Phase 2 is
/// the only caller with a two-secret identity to seal, so two calls
/// against two paths is simpler than widening `wrap.rs`'s API for one
/// caller.
fn seal_identity(path: &Path, identity: &DeviceIdentity, kek: &DataKey) -> Result<()> {
    let (signing_path, box_path) = identity_wrap_paths(path);
    let signing_key = DataKey::from_bytes(identity.signing_key_bytes());
    let box_key = DataKey::from_bytes(identity.box_key_bytes());
    wrap::write_wrap(
        &signing_path,
        &signing_key,
        kek,
        &WrapHeader::keychain_backed(),
    )
    .map_err(|_| pairing_err("couldn't save this device's identity"))?;
    wrap::write_wrap(&box_path, &box_key, kek, &WrapHeader::keychain_backed())
        .map_err(|_| pairing_err("couldn't save this device's identity"))
}

fn open_identity(path: &Path, kek: &DataKey) -> Result<DeviceIdentity> {
    let (signing_path, box_path) = identity_wrap_paths(path);
    let signing_key = wrap::read_wrap(&signing_path, kek)
        .map_err(|_| pairing_err("couldn't unlock this device's identity"))?;
    let box_key = wrap::read_wrap(&box_path, kek)
        .map_err(|_| pairing_err("couldn't unlock this device's identity"))?;
    Ok(DeviceIdentity::from_raw_bytes(
        *signing_key.as_bytes(),
        *box_key.as_bytes(),
    ))
}

fn identity_wrap_paths(path: &Path) -> (PathBuf, PathBuf) {
    (
        path.with_extension("signing.wrap"),
        path.with_extension("box.wrap"),
    )
}

// ---------------------------------------------------------------------------
// PairingManager
// ---------------------------------------------------------------------------

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const JOIN_REQUEST_TTL: Duration = Duration::from_secs(300);
const REPOST_INTERVAL: Duration = Duration::from_secs(15);
/// Pairing envelopes get the relay's short default TTL (600s, matching
/// `crates/relay`'s own default); rotation envelopes ask for the relay's
/// 90-day maximum (`crates/relay/src/routes.rs::MAX_TTL_SECONDS`) so they
/// survive until an offline survivor next comes online. 90 days, not the
/// original 30: periodic rotation (unlike rare, human-triggered removal)
/// applies "must reconnect within the TTL or get stranded" to every
/// long-idle device, not just ones a human just removed — a device idle
/// longer than that falls back to the existing re-pair flow, already the
/// only recovery path for any "fell too far behind" case.
const PAIRING_ENVELOPE_TTL_SECONDS: u64 = 600;
const ROTATION_ENVELOPE_TTL_SECONDS: u64 = 90 * 24 * 3600;
/// Pairing-message schema version — distinct from `crates/sync-protocol`'s
/// own `Envelope::version` (the mailbox wire format).
const MESSAGE_VERSION: u32 = 1;

/// Events streamed to the UI during pairing — mirrors `crate::account`'s
/// `AccountEvent` shape (a `FnMut` callback the Tauri command layer forwards
/// through a `tauri::ipc::Channel`), not a new async pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PairingEvent {
    /// Device A: a device asking to join is showing this fingerprint —
    /// render it as the visually dominant element of the confirm card
    /// (protocol doc's UI requirement), never pre-confirmed, never a
    /// default-focused button. `listen_for_join` returns immediately after
    /// emitting this; the Tauri command layer calls `confirm_incoming`
    /// separately once the user taps.
    IncomingRequest {
        device_label: String,
        fingerprint: String,
    },
    /// Device B: posted the join request, waiting for Device A to respond.
    AwaitingChallenge,
    /// Device B: got a challenge back — show this fingerprint for the user
    /// to compare against Device A's screen. Not yet installable.
    AwaitingConfirmation { fingerprint: String },
    /// Device B: successfully decrypted a `join_confirm` — the SyncKey is
    /// in hand but NOT YET sealed to disk. `request_join` returns after
    /// this; the Tauri command layer calls `finish_join` separately once
    /// the user gives their own explicit final tap (protocol doc step 6:
    /// implicit trust-on-decrypt is not enough).
    ReadyToInstall,
    /// Pairing completed and the SyncKey/roster were installed locally.
    Complete { device_id: String },
    /// The other side (or this side) explicitly rejected the pairing.
    Rejected,
    /// No response within the request's TTL.
    Expired,
    /// A MAC verification, AEAD open, signature, or transport failure.
    /// `message` is safe to show to a non-technical user — see
    /// `GENERIC_FAILURE`'s doc comment for why this is deliberately
    /// generic.
    Failed { message: String },
}

/// State that has to survive between two separate Tauri command
/// invocations: `listen_for_join` (which finds+challenges a request) and
/// `confirm_incoming` (triggered by a later, separate user tap) can't be
/// one blocking call — a Tauri command can't hold the UI thread waiting on
/// a different command's future invocation. So the ephemeral key material
/// and pending message live here instead, guarded by the same
/// generation counter `cancel`/a new operation invalidates.
struct PendingIncoming {
    generation: u64,
    request: JoinRequest,
    a_eph_secret: EphemeralSecret,
    control_mailbox_id: String,
}

struct PendingInstall {
    generation: u64,
    group_secret: String,
    my_identity: DeviceIdentity,
    sync_key: SyncKey,
    confirm: JoinConfirm,
    a_identity: DeviceIdentityPublic,
}

struct Inner {
    generation: u64,
    pending_incoming: Option<PendingIncoming>,
    pending_install: Option<PendingInstall>,
}

/// Ties the pure crypto/message functions above to a real transport, the
/// local roster (`crate::sync::roster`, in `app_db`), and the three wrap
/// files (sealed via `crate::crypto::wrap` under the local KEK — re-fetched
/// from the OS keychain on demand for each operation, never cached in this
/// struct's own state, the same way `crates/app/src/bootstrap.rs`
/// re-fetches it on every launch rather than keeping it resident longer
/// than necessary).
pub struct PairingManager<T: MailboxTransport> {
    transport: T,
    keys_dir: PathBuf,
    secret_store: Arc<dyn SecretStore>,
    inner: tokio::sync::Mutex<Inner>,
}

impl<T: MailboxTransport> PairingManager<T> {
    pub fn new(transport: T, keys_dir: PathBuf, secret_store: Arc<dyn SecretStore>) -> Self {
        Self {
            transport,
            keys_dir,
            secret_store,
            inner: tokio::sync::Mutex::new(Inner {
                generation: 0,
                pending_incoming: None,
                pending_install: None,
            }),
        }
    }

    fn sync_group_wrap_path(&self) -> PathBuf {
        self.keys_dir.join("sync_group.wrap")
    }
    fn sync_key_wrap_path(&self) -> PathBuf {
        self.keys_dir.join("sync.wrap")
    }
    fn sync_identity_wrap_path(&self) -> PathBuf {
        self.keys_dir.join("sync_identity.wrap")
    }

    /// Bumps the generation counter, invalidating any in-flight poll loop
    /// and any pending confirm/install state — mirrors
    /// `AccountManager::sign_in`'s own `sign_in_generation` guard against a
    /// superseded operation.
    async fn next_generation(&self) -> u64 {
        let mut inner = self.inner.lock().await;
        inner.generation += 1;
        inner.pending_incoming = None;
        inner.pending_install = None;
        inner.generation
    }

    async fn is_current(&self, generation: u64) -> bool {
        self.inner.lock().await.generation == generation
    }

    pub async fn cancel(&self) {
        self.next_generation().await;
    }

    pub fn has_local_identity(&self) -> bool {
        self.sync_identity_wrap_path()
            .with_extension("signing.wrap")
            .exists()
    }

    /// The underlying transport — for callers (e.g. `sync_device_remove`)
    /// that need to call a free function like [`deliver_rotations`]
    /// directly rather than through a `PairingManager` method.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// **Group creation** — protocol doc's "Group creation (Device A, first
    /// device — no pairing)". Mints `group_secret`, this device's identity,
    /// and the initial `SyncKey`; seals all three; writes the `is_self`
    /// roster row at `gen_n = 0`. Returns `group_secret` (shown to the user
    /// once, the out-of-band pairing secret).
    /// Not `async`: nothing here does network I/O (protocol doc: "the relay
    /// learns nothing at this step"), and keeping it synchronous means a
    /// caller holding a non-`Send` `std::sync::MutexGuard<Connection>`
    /// never has to worry about it crossing an `.await` — `rusqlite::Connection`
    /// is not `Sync`, so `&Connection` is not `Send`, and Tauri command
    /// futures must be `Send` (only enforced once a command is registered
    /// in `generate_handler!`, which is how this was caught).
    pub fn create_group(&self, conn: &Connection, this_device_label: &str) -> Result<String> {
        let group_secret = recovery::generate();
        let identity = DeviceIdentity::generate();
        let sync_key = SyncKey::random();

        let kek = load_kek(self.secret_store.as_ref())?;
        seal_group_secret(&self.sync_group_wrap_path(), &group_secret, &kek)?;
        seal_sync_key(&self.sync_key_wrap_path(), &sync_key, &kek)?;
        seal_identity(&self.sync_identity_wrap_path(), &identity, &kek)?;

        let public = identity.public();
        roster::save(
            conn,
            &RosterEntry {
                device_id: device_id_for(&public.identity_sign_pub_bytes()),
                identity_sign_pub: public.identity_sign_pub_bytes(),
                box_pub: public.box_pub_bytes(),
                label: this_device_label.to_string(),
                gen_added: 0,
                is_self: true,
            },
        )?;
        // Establishes this device's real `key_installed_at_ms` — see
        // `key_state::set_current_gen`'s doc comment for why the very
        // first call must happen here rather than being left to seed
        // lazily at `0` (the Unix epoch).
        key_state::set_current_gen(conn, 0, now_ms())?;

        Ok(group_secret)
    }

    /// **Device A's side** ("Add a device" open): polls for one
    /// MAC-verified `join_request` that arrived after this call started,
    /// challenges it, and returns after emitting `IncomingRequest` — it
    /// does not block waiting for the user's tap (see `PendingIncoming`'s
    /// doc comment). One call = one pairing session, per the protocol
    /// doc's "each open of that screen is one pairing session" rule.
    pub async fn listen_for_join(
        &self,
        group_secret: &str,
        mut on_event: impl FnMut(PairingEvent) + Send,
    ) -> Result<()> {
        let generation = self.next_generation().await;
        let control_mailbox_id = control_mailbox_id(group_secret);
        let session_started_ms = now_ms();
        let deadline = tokio::time::Instant::now() + JOIN_REQUEST_TTL;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            if !self.is_current(generation).await {
                return Ok(()); // superseded by cancel() or a new operation
            }
            if tokio::time::Instant::now() >= deadline {
                on_event(PairingEvent::Expired);
                return Ok(());
            }

            let pulled = match self.transport.pull(&control_mailbox_id).await {
                Ok(p) => p,
                Err(_) => continue, // transient network error: keep polling within the deadline
            };

            let mut found = None;
            for envelope in pulled {
                let Some(bytes) = envelope.ciphertext() else {
                    continue;
                };
                // ---- MAC gate: nothing past this point runs for an
                // envelope whose MAC doesn't verify. ----
                let Ok(candidate) = parse_mac_envelope::<JoinRequest>(&bytes, group_secret) else {
                    continue;
                };
                if candidate.requested_at_ms < session_started_ms {
                    continue; // arrived before this session's button press
                }
                found = Some(candidate);
                break;
            }

            let Some(request) = found else { continue };

            let a_eph_secret = EphemeralSecret::random_from_rng(OsRng);
            let a_eph_pub = BoxPublicKey::from(&a_eph_secret);
            let b_eph_pub = BoxPublicKey::from(request.b_eph_pub);
            let fp = fingerprint(&a_eph_pub, &b_eph_pub, &control_mailbox_id);

            let challenge = JoinChallenge {
                control_mailbox_id: control_mailbox_id.clone(),
                request_id: request.request_id,
                a_eph_pub: a_eph_pub.to_bytes(),
                challenged_at_ms: now_ms(),
            };
            if let Err(e) = push_mac_envelope(
                &self.transport,
                &control_mailbox_id,
                challenge,
                group_secret,
                PAIRING_ENVELOPE_TTL_SECONDS,
            )
            .await
            {
                on_event(PairingEvent::Failed { message: e });
                return Ok(());
            }

            {
                let mut inner = self.inner.lock().await;
                if inner.generation != generation {
                    return Ok(()); // superseded while we were pushing the challenge
                }
                inner.pending_incoming = Some(PendingIncoming {
                    generation,
                    request: request.clone(),
                    a_eph_secret,
                    control_mailbox_id: control_mailbox_id.clone(),
                });
            }

            on_event(PairingEvent::IncomingRequest {
                device_label: request.b_device_label,
                fingerprint: fp,
            });
            return Ok(());
        }
    }

    /// Called by the Tauri command layer once Device A's user taps
    /// Confirm/Reject on the rendered card — a separate call from
    /// `listen_for_join` (see `PendingIncoming`'s doc comment), never
    /// auto-triggered by a poll tick (protocol doc step 5: "never
    /// auto-approve").
    /// Takes `gen_n: u32` — already read via [`next_gen_n`] by the caller
    /// — rather than `conn: &Connection`. `rusqlite::Connection` is not
    /// `Sync`, so `&Connection` is not `Send`; a `std::sync::MutexGuard`
    /// derived from `crates/app`'s `app_db` is not `Send` either, and
    /// Tauri command futures must be `Send` (only enforced once a command
    /// is registered in `generate_handler!`, which is how this was
    /// caught). It turns out that isn't just about *where* `&Connection`
    /// is read relative to an internal `.await` — merely being a parameter
    /// to any `async fn` that awaits internally taints the whole
    /// function's generated future, regardless of read order (confirmed
    /// empirically: reading `conn` as the very first statement, strictly
    /// before this method's own first `.await`, still failed to compile
    /// once registered). The only fix that actually works is to never let
    /// `&Connection`/its guard cross an `async fn` boundary that awaits
    /// internally at all — the caller does 100% of the database work
    /// (reading `gen_n`, and later saving the returned roster entry) in
    /// its own synchronous scopes, never inside a method it then awaits.
    pub async fn confirm_incoming(
        &self,
        gen_n: u32,
        group_secret: &str,
        approve: bool,
    ) -> Result<(PairingEvent, Option<RosterEntry>)> {
        let pending = {
            let mut inner = self.inner.lock().await;
            let Some(pending) = inner.pending_incoming.take() else {
                return Err(pairing_err("no pending incoming request"));
            };
            if pending.generation != inner.generation {
                return Err(pairing_err("this pairing session is no longer current"));
            }
            pending
        };

        if !approve {
            return Ok((PairingEvent::Rejected, None));
        }

        let PendingIncoming {
            request,
            a_eph_secret,
            control_mailbox_id,
            ..
        } = pending;
        let b_eph_pub = BoxPublicKey::from(request.b_eph_pub);
        let shared = a_eph_secret.diffie_hellman(&b_eph_pub);

        let kek = load_kek(self.secret_store.as_ref())?;
        let sync_key = open_sync_key(&self.sync_key_wrap_path(), &kek)?;
        let identity = open_identity(&self.sync_identity_wrap_path(), &kek)?;

        let key = pairing_key(
            &shared,
            group_secret,
            &control_mailbox_id,
            &request.request_id,
        );
        let aad = JoinConfirm::aead_aad(&control_mailbox_id, &request.request_id);
        let encrypted_sync_key = aead_seal(&key, &aad, sync_key.as_bytes());

        let mut confirm = JoinConfirm {
            control_mailbox_id: control_mailbox_id.clone(),
            request_id: request.request_id,
            a_identity_pub: identity.public().identity_sign_pub_bytes(),
            a_box_pub: identity.public().box_pub_bytes(),
            gen_n,
            encrypted_sync_key,
            a_sig: [0u8; 64],
        };
        confirm.a_sig = identity.sign(&confirm.signed_fields()).to_bytes();

        push_mac_envelope(
            &self.transport,
            &control_mailbox_id,
            confirm,
            group_secret,
            PAIRING_ENVELOPE_TTL_SECONDS,
        )
        .await
        .map_err(|_| pairing_err("couldn't deliver the pairing confirmation"))?;

        let device_added = DeviceAdded {
            identity_pub: request.b_identity_pub,
            box_pub: request.b_box_pub,
            device_label: request.b_device_label.clone(),
            gen_n,
        };
        // Best-effort broadcast so other already-online survivors learn
        // about the new device too — a delivery failure here doesn't fail
        // the pairing itself (Device B already has what it needs once
        // `join_confirm` lands), so this is intentionally not `?`.
        let _ = push_mac_envelope(
            &self.transport,
            &control_mailbox_id,
            device_added,
            group_secret,
            PAIRING_ENVELOPE_TTL_SECONDS,
        )
        .await;

        let new_device_id = device_id_for(&request.b_identity_pub);
        let entry = RosterEntry {
            device_id: new_device_id.clone(),
            identity_sign_pub: request.b_identity_pub,
            box_pub: request.b_box_pub,
            label: request.b_device_label,
            gen_added: gen_n,
            is_self: false,
        };

        Ok((
            PairingEvent::Complete {
                device_id: new_device_id,
            },
            Some(entry),
        ))
    }

    /// **Device B's side**: posts a `join_request`, waits for a
    /// MAC-verified `join_challenge`, shows the fingerprint, then keeps
    /// waiting/polling for the `join_confirm` that only arrives once
    /// Device A's user has tapped Confirm there. Returns after a
    /// successful AEAD decrypt (`ReadyToInstall`) without sealing anything
    /// yet — `finish_join` (a separate call, gated on this device's own
    /// user tapping confirm) does the actual install.
    pub async fn request_join(
        &self,
        group_secret: &str,
        device_label: &str,
        mut on_event: impl FnMut(PairingEvent) + Send,
    ) -> Result<()> {
        let generation = self.next_generation().await;
        let control_mailbox_id = control_mailbox_id(group_secret);

        let identity = match self.load_identity_for_join() {
            Ok(identity) => identity,
            Err(_) => {
                // First pairing attempt on this device: no identity sealed
                // yet. Generate and seal it now, before any network I/O, so
                // a crash mid-pairing never leaves key material only in
                // memory (protocol doc step 2).
                let kek = match load_kek(self.secret_store.as_ref()) {
                    Ok(kek) => kek,
                    Err(e) => {
                        on_event(PairingEvent::Failed {
                            message: format!("{e}"),
                        });
                        return Ok(());
                    }
                };
                let identity = DeviceIdentity::generate();
                if let Err(e) = seal_identity(&self.sync_identity_wrap_path(), &identity, &kek) {
                    on_event(PairingEvent::Failed {
                        message: format!("{e}"),
                    });
                    return Ok(());
                }
                identity
            }
        };

        // `Option`-wrapped so it can be `.take()`n exactly once, on
        // whichever loop iteration below finds the matching `join_confirm`
        // — `EphemeralSecret::diffie_hellman` consumes `self` by value
        // (intentionally not `Clone`, to enforce single use).
        let mut b_eph_secret = Some(EphemeralSecret::random_from_rng(OsRng));
        let b_eph_pub = BoxPublicKey::from(b_eph_secret.as_ref().unwrap());
        let request_id = generate_request_id();
        let mut request = JoinRequest {
            control_mailbox_id: control_mailbox_id.clone(),
            request_id,
            b_eph_pub: b_eph_pub.to_bytes(),
            b_identity_pub: identity.public().identity_sign_pub_bytes(),
            b_box_pub: identity.public().box_pub_bytes(),
            b_device_label: device_label.to_string(),
            requested_at_ms: now_ms(),
        };

        if let Err(e) = push_mac_envelope(
            &self.transport,
            &control_mailbox_id,
            request.clone(),
            group_secret,
            PAIRING_ENVELOPE_TTL_SECONDS,
        )
        .await
        {
            on_event(PairingEvent::Failed { message: e });
            return Ok(());
        }
        on_event(PairingEvent::AwaitingChallenge);

        let deadline = tokio::time::Instant::now() + JOIN_REQUEST_TTL;
        let mut last_repost = tokio::time::Instant::now();
        let mut a_eph_pub_seen: Option<BoxPublicKey> = None;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            if !self.is_current(generation).await {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                on_event(PairingEvent::Expired);
                return Ok(());
            }
            // Re-POST the identical request (same `request_id`) every
            // ~15s while still waiting for a challenge, so a
            // selectively-deleted request (network-attacker/Attack B —
            // an attacker who knows `control_mailbox_id` races to DELETE
            // it before Device A's poll sees it) becomes a detectable
            // repeated failure instead of a silent one. Stops once a
            // challenge has arrived — nothing left to repost for.
            if a_eph_pub_seen.is_none() && last_repost.elapsed() >= REPOST_INTERVAL {
                // Refresh `requested_at_ms` on every repost — reusing the
                // original timestamp verbatim would mean a request whose
                // first push happened to land before the far side's
                // session-start filter (`listen_for_join`'s
                // `requested_at_ms < session_started_ms` check) stays
                // invisible forever, since every "repost" would carry the
                // same stale, already-rejected timestamp. This was a real
                // bug caught by this module's own round-trip test hanging.
                request.requested_at_ms = now_ms();
                let _ = push_mac_envelope(
                    &self.transport,
                    &control_mailbox_id,
                    request.clone(),
                    group_secret,
                    PAIRING_ENVELOPE_TTL_SECONDS,
                )
                .await;
                last_repost = tokio::time::Instant::now();
            }

            let pulled = match self.transport.pull(&control_mailbox_id).await {
                Ok(p) => p,
                Err(_) => continue,
            };

            for envelope in &pulled {
                let Some(bytes) = envelope.ciphertext() else {
                    continue;
                };

                if a_eph_pub_seen.is_none() {
                    if let Ok(challenge) =
                        parse_mac_envelope::<JoinChallenge>(bytes.as_slice(), group_secret)
                    {
                        if challenge.request_id == request_id {
                            let a_pub = BoxPublicKey::from(challenge.a_eph_pub);
                            let fp = fingerprint(&a_pub, &b_eph_pub, &control_mailbox_id);
                            on_event(PairingEvent::AwaitingConfirmation { fingerprint: fp });
                            a_eph_pub_seen = Some(a_pub);
                        }
                        continue;
                    }
                }

                let Some(a_pub) = a_eph_pub_seen else {
                    continue;
                };
                let Ok(confirm) = parse_mac_envelope::<JoinConfirm>(bytes.as_slice(), group_secret)
                else {
                    continue;
                };
                if confirm.request_id != request_id {
                    continue;
                }

                // Consumed here — this is the one loop iteration that ever
                // reaches this line, since the function returns
                // unconditionally right after (success or failure below).
                let shared = b_eph_secret
                    .take()
                    .expect("b_eph_secret is only ever taken once, on the single loop iteration that finds a matching join_confirm")
                    .diffie_hellman(&a_pub);
                let key = pairing_key(&shared, group_secret, &control_mailbox_id, &request_id);
                let aad = JoinConfirm::aead_aad(&control_mailbox_id, &request_id);
                let plaintext = match aead_open(&key, &aad, &confirm.encrypted_sync_key) {
                    Ok(p) => p,
                    Err(e) => {
                        on_event(PairingEvent::Failed {
                            message: format!("{e}"),
                        });
                        return Ok(());
                    }
                };
                let Ok(a_identity) =
                    DeviceIdentityPublic::from_bytes(confirm.a_identity_pub, confirm.a_box_pub)
                else {
                    on_event(PairingEvent::Failed {
                        message: GENERIC_FAILURE.to_string(),
                    });
                    return Ok(());
                };
                let sig = EdSignature::from_bytes(&confirm.a_sig);
                if a_identity
                    .signing_key()
                    .verify(&confirm.signed_fields(), &sig)
                    .is_err()
                {
                    on_event(PairingEvent::Failed {
                        message: GENERIC_FAILURE.to_string(),
                    });
                    return Ok(());
                }

                let sync_key = SyncKey::from_bytes(plaintext);
                {
                    let mut inner = self.inner.lock().await;
                    if inner.generation != generation {
                        return Ok(());
                    }
                    inner.pending_install = Some(PendingInstall {
                        generation,
                        group_secret: group_secret.to_string(),
                        my_identity: identity,
                        sync_key,
                        confirm,
                        a_identity,
                    });
                }
                on_event(PairingEvent::ReadyToInstall);
                return Ok(());
            }
        }
    }

    /// Called by the Tauri command layer once Device B's user gives the
    /// second, explicit "codes match, finish pairing?" confirmation
    /// (protocol doc step 6 — implicit trust-on-decrypt is not enough).
    /// Seals `SyncKey`/`group_secret` and records Device A's roster entry.
    /// Takes no `&Connection` at all (unlike `confirm_incoming`, which
    /// needs one for a quick pre-await read) — every roster row this
    /// method would have saved is returned instead, for the caller to save
    /// with a fresh lock after this method's `.await`s (the seal
    /// operations are synchronous, but `self.inner.lock().await` still
    /// counts) have completed. See `confirm_incoming`'s doc comment for
    /// why a database guard can never be alive across an `.await` here.
    pub async fn finish_join(&self, approve: bool) -> Result<(PairingEvent, Vec<RosterEntry>)> {
        let pending = {
            let mut inner = self.inner.lock().await;
            let Some(pending) = inner.pending_install.take() else {
                return Err(pairing_err("no pending pairing install"));
            };
            if pending.generation != inner.generation {
                return Err(pairing_err("this pairing session is no longer current"));
            }
            pending
        };

        if !approve {
            return Ok((PairingEvent::Rejected, Vec::new()));
        }

        let kek = load_kek(self.secret_store.as_ref())?;
        seal_sync_key(&self.sync_key_wrap_path(), &pending.sync_key, &kek)?;
        seal_group_secret(&self.sync_group_wrap_path(), &pending.group_secret, &kek)?;
        seal_identity(&self.sync_identity_wrap_path(), &pending.my_identity, &kek)?;

        let my_device_id = device_id_for(&pending.my_identity.public().identity_sign_pub_bytes());
        let entries = vec![
            RosterEntry {
                device_id: my_device_id.clone(),
                identity_sign_pub: pending.my_identity.public().identity_sign_pub_bytes(),
                box_pub: pending.my_identity.public().box_pub_bytes(),
                label: String::new(),
                gen_added: pending.confirm.gen_n,
                is_self: true,
            },
            RosterEntry {
                device_id: device_id_for(&pending.a_identity.identity_sign_pub_bytes()),
                identity_sign_pub: pending.confirm.a_identity_pub,
                box_pub: pending.confirm.a_box_pub,
                label: String::new(),
                gen_added: pending.confirm.gen_n,
                is_self: false,
            },
        ];

        Ok((
            PairingEvent::Complete {
                device_id: my_device_id,
            },
            entries,
        ))
    }

    fn load_identity_for_join(&self) -> Result<DeviceIdentity> {
        let kek = load_kek(self.secret_store.as_ref())?;
        open_identity(&self.sync_identity_wrap_path(), &kek)
    }

    /// Reads this device's currently-sealed `group_secret`, if any — used
    /// by the Tauri command layer to know whether "Add a device"/removal
    /// commands can run at all without the caller having to separately
    /// track "is sync set up yet."
    pub fn load_group_secret(&self) -> Result<String> {
        let kek = load_kek(self.secret_store.as_ref())?;
        open_group_secret(&self.sync_group_wrap_path(), &kek)
    }

    /// This device's current `SyncKey` — for `crates/app/src/sync_loop.rs`
    /// (Phase 3) to encrypt outbound change envelopes and decrypt inbound
    /// ones. `Err` if this device hasn't created/joined a sync group yet
    /// (same "not set up" signal `load_group_secret` gives).
    pub fn load_sync_key(&self) -> Result<SyncKey> {
        let kek = load_kek(self.secret_store.as_ref())?;
        open_sync_key(&self.sync_key_wrap_path(), &kek)
    }
}

/// `pub`: `crates/app`'s `sync_pair_confirm` command needs this to compute
/// `gen_n` synchronously, under its own lock, before calling
/// `PairingManager::confirm_incoming` — see that method's doc comment for
/// why it takes `gen_n: u32` rather than `conn: &Connection`.
///
/// Delegates to `key_state::get` — the authoritative, persisted current
/// generation — rather than recomputing `max(roster.gen_added)` itself.
/// `gen_added` is write-once pairing metadata (the generation a device was
/// *added* in), not a live counter, so recomputing it here would never
/// reflect a rotation that happened after the last device joined; see
/// `crate::sync::key_state`'s own doc comment for the full history.
pub fn next_gen_n(conn: &Connection) -> u32 {
    key_state::get(conn).current_gen_n
}

// ---------------------------------------------------------------------------
// Revocation and rotation
// ---------------------------------------------------------------------------

/// Removes `target_device_id` from the local roster and delivers a
/// signed, per-survivor-encrypted `SyncKey_new` to every other currently
/// known device. Called by the Tauri `sync_device_remove` command — see
/// protocol doc "Revocation and rotation": authenticated by this device's
/// persistent identity signature, verified by each survivor against its
/// own roster, **never by the SyncKey being rotated** (the bug the red
/// team called "the sharpest bug in the original draft").
/// One signed, encrypted `Rotate` message ready to push — everything about
/// it is already computed; delivering it is pure network I/O.
pub struct PendingRotate {
    control_mailbox_id: String,
    message: Rotate,
}

/// Every synchronous step of device removal: mint+seal the new SyncKey,
/// remove the target from the LOCAL roster, and pre-build one signed
/// `Rotate` message per surviving device. Split from delivery
/// ([`deliver_rotations`]) so a caller holding a non-`Send` database guard
/// (e.g. `crates/app`'s `std::sync::MutexGuard<Connection>`) never needs to
/// hold it across an `.await` — the same split-lock shape
/// `crate::export::read_export_data`/`write_export_zip` already uses for
/// the same reason. Returns the messages to deliver; the caller is
/// expected to drop its database guard before awaiting
/// [`deliver_rotations`].
pub fn remove_device_locally(
    conn: &Connection,
    keys_dir: &Path,
    secret_store: &dyn SecretStore,
    group_secret: &str,
    target_device_id: &str,
) -> Result<Vec<PendingRotate>> {
    let control_mailbox = control_mailbox_id(group_secret);
    let kek = load_kek(secret_store)?;
    let my_identity = open_identity(&keys_dir.join("sync_identity.wrap"), &kek)?;
    let my_roster = roster::list(conn);
    let me = my_roster
        .iter()
        .find(|r| r.is_self)
        .ok_or_else(|| pairing_err("this device isn't part of a sync group yet"))?;

    let gen_n_new = next_gen_n(conn) + 1;
    let sync_key_new = SyncKey::random();
    seal_sync_key(&keys_dir.join("sync.wrap"), &sync_key_new, &kek)?;
    roster::remove(conn, target_device_id)?;
    // Persist the advance — previously nothing recorded `gen_n_new`
    // anywhere, so `next_gen_n` never actually reflected a rotation. See
    // `key_state`'s own doc comment for the full history of this gap.
    key_state::set_current_gen(conn, gen_n_new, now_ms())?;

    Ok(build_rotate_messages(
        &my_identity,
        me,
        &my_roster,
        &control_mailbox,
        Some(target_device_id),
        gen_n_new,
        &sync_key_new,
        &[target_device_id],
    ))
}

/// Builds one signed, per-survivor-encrypted `Rotate` message for each
/// current roster member other than `excluded_ids` (and, always, `me`
/// itself) — the step shared by device-removal rotation
/// ([`remove_device_locally`]) and periodic rotation
/// ([`rotate_key_periodically`]); the only difference between the two
/// callers is `target_device_id` and whether anyone is excluded.
#[allow(clippy::too_many_arguments)]
fn build_rotate_messages(
    my_identity: &DeviceIdentity,
    me: &RosterEntry,
    roster_entries: &[RosterEntry],
    control_mailbox: &str,
    target_device_id: Option<&str>,
    gen_n_new: u32,
    sync_key_new: &SyncKey,
    excluded_ids: &[&str],
) -> Vec<PendingRotate> {
    let mut pending = Vec::new();
    for survivor in roster_entries
        .iter()
        .filter(|r| !r.is_self && !excluded_ids.contains(&r.device_id.as_str()))
    {
        let survivor_box = BoxPublicKey::from(survivor.box_pub);
        let shared = my_identity.box_secret().diffie_hellman(&survivor_box);
        let key = rotation_key(&shared, control_mailbox, target_device_id, gen_n_new);
        let aad = Rotate::aead_aad(
            control_mailbox,
            target_device_id,
            gen_n_new,
            &survivor.device_id,
        );
        let ciphertext = aead_seal(&key, &aad, sync_key_new.as_bytes());

        let mut rotate = Rotate {
            control_mailbox_id: control_mailbox.to_string(),
            target_device_id: target_device_id.map(str::to_string),
            gen_n_new,
            for_device_id: survivor.device_id.clone(),
            initiator_device_id: me.device_id.clone(),
            ciphertext,
            initiator_sig: [0u8; 64],
        };
        rotate.initiator_sig = my_identity.sign(&rotate.signed_fields()).to_bytes();
        pending.push(PendingRotate {
            control_mailbox_id: control_mailbox.to_string(),
            message: rotate,
        });
    }
    pending
}

/// A periodic (or count-triggered) rotation that isn't removing anyone —
/// every current roster member, including the initiator, ends up on the
/// new key. Mint+seal the new `SyncKey` locally and pre-build one signed
/// `Rotate` message (`target_device_id: None`) per OTHER roster member;
/// mirrors [`remove_device_locally`] minus the excluded target. Callers:
/// see [`is_designated_initiator`] and `docs/SYNC_PAIRING_PROTOCOL.md`'s
/// "Revocation and rotation" for the trigger policy and why a periodic
/// rotation is safe under the same idempotency rule removal already
/// relies on.
pub fn rotate_key_periodically(
    conn: &Connection,
    keys_dir: &Path,
    secret_store: &dyn SecretStore,
    group_secret: &str,
) -> Result<Vec<PendingRotate>> {
    let control_mailbox = control_mailbox_id(group_secret);
    let kek = load_kek(secret_store)?;
    let my_identity = open_identity(&keys_dir.join("sync_identity.wrap"), &kek)?;
    let my_roster = roster::list(conn);
    let me = my_roster
        .iter()
        .find(|r| r.is_self)
        .ok_or_else(|| pairing_err("this device isn't part of a sync group yet"))?;

    let gen_n_new = next_gen_n(conn) + 1;
    let sync_key_new = SyncKey::random();
    seal_sync_key(&keys_dir.join("sync.wrap"), &sync_key_new, &kek)?;
    key_state::set_current_gen(conn, gen_n_new, now_ms())?;

    Ok(build_rotate_messages(
        &my_identity,
        me,
        &my_roster,
        &control_mailbox,
        None,
        gen_n_new,
        &sync_key_new,
        &[],
    ))
}

/// True if `own_device_id` is the lowest device id currently in `roster` —
/// the single-initiator rule for periodic rotation. Evaluated identically
/// and independently by every device each tick, with no presence/heartbeat
/// system: if the lowest-id device happens to be offline, rotation is
/// simply late (the trigger check the caller applies is `>=`, not `==`),
/// and `apply_incoming_rotate`'s existing stale-`gen_n_new` rejection
/// absorbs the rare race where two devices briefly disagree on who's
/// lowest (e.g. right after a pairing/removal changes the roster).
pub fn is_designated_initiator(roster_entries: &[RosterEntry], own_device_id: &str) -> bool {
    roster_entries.iter().map(|r| r.device_id.as_str()).min() == Some(own_device_id)
}

/// Delivers every [`PendingRotate`] `remove_device_locally` built. Pure
/// network I/O — no database access, so this is safe to `.await` from
/// anywhere, including across a point where the caller's database guard
/// has already been dropped.
pub async fn deliver_rotations<T: MailboxTransport>(
    transport: &T,
    pending: Vec<PendingRotate>,
    group_secret: &str,
) {
    for rotate in pending {
        // Best-effort per survivor: one unreachable survivor shouldn't
        // block removal for the others, or for the (already-completed)
        // local rotation `remove_device_locally` already applied.
        let _ = push_mac_envelope(
            transport,
            &rotate.control_mailbox_id,
            rotate.message,
            group_secret,
            ROTATION_ENVELOPE_TTL_SECONDS,
        )
        .await;
    }
}

/// A survivor processing an incoming `Rotate` envelope it pulled from
/// `control_mailbox_id`. Verifies, in order: the MAC (is this from someone
/// who knows `group_secret` at all — via `parse_mac_envelope`), whether
/// `for_device_id` matches this device, and the initiator's signature
/// **against this device's own current roster** (never against the key
/// being replaced — see this module's revocation doc comment). Only then
/// opens the AEAD ciphertext. Rejects a signature from a device already
/// absent from the verifier's roster outright, no exceptions. Returns
/// `Ok(true)` if this envelope was a rotation addressed to (and applied
/// for) this device, `Ok(false)` if it was ignored (not addressed here, or
/// failed verification — both cases are deliberately indistinguishable to
/// the caller, same "never soften a crypto failure" posture as everything
/// else in this module).
pub fn apply_incoming_rotate(
    conn: &Connection,
    keys_dir: &Path,
    secret_store: &dyn SecretStore,
    group_secret: &str,
    my_device_id: &str,
    raw_envelope: &[u8],
) -> Result<bool> {
    let Ok(rotate) = parse_mac_envelope::<Rotate>(raw_envelope, group_secret) else {
        return Ok(false);
    };
    if rotate.for_device_id != my_device_id {
        return Ok(false);
    }

    let Ok(initiator) = roster::get(conn, &rotate.initiator_device_id) else {
        // Signer isn't in my roster at all — either never was, or was
        // already removed in an earlier generation. Reject outright.
        return Ok(false);
    };
    let Ok(signing_key) = VerifyingKey::from_bytes(&initiator.identity_sign_pub) else {
        return Ok(false);
    };
    let signature = EdSignature::from_bytes(&rotate.initiator_sig);
    if signing_key
        .verify(&rotate.signed_fields(), &signature)
        .is_err()
    {
        return Ok(false);
    }

    // Idempotency guard: a `gen_n_new` that isn't strictly newer than what
    // this device already has installed is a stale replay or a duplicate
    // copy of a rotation already applied (or lost a race to a competing
    // one) — discard it here, before touching any key material, rather
    // than relying on `key_state::set_current_gen`'s own monotonic no-op
    // further down. That guard alone would leave `sync.wrap` clobbered
    // with OLDER key bytes while the persisted generation counter stayed
    // at the newer value it already had — an inconsistency between the
    // two, not just a no-op. See `docs/SYNC_PAIRING_PROTOCOL.md`'s
    // "Residual risk" for why this is the same idempotency rule both
    // removal- and periodic-triggered rotation rely on.
    if rotate.gen_n_new <= key_state::get(conn).current_gen_n {
        return Ok(false);
    }

    let kek = load_kek(secret_store)?;
    let my_identity = open_identity(&keys_dir.join("sync_identity.wrap"), &kek)?;
    let initiator_box = BoxPublicKey::from(initiator.box_pub);
    let shared = my_identity.box_secret().diffie_hellman(&initiator_box);
    let control_mailbox = control_mailbox_id(group_secret);
    let target = rotate.target_device_id.as_deref();
    let key = rotation_key(&shared, &control_mailbox, target, rotate.gen_n_new);
    let aad = Rotate::aead_aad(&control_mailbox, target, rotate.gen_n_new, my_device_id);
    let plaintext = aead_open(&key, &aad, &rotate.ciphertext)?;

    // Only a removal rotation (`target: Some`) drops anyone from the
    // roster — a periodic rotation (`None`) just installs the new key.
    if let Some(target_device_id) = target {
        roster::remove(conn, target_device_id)?;
    }
    let sync_key_new = SyncKey::from_bytes(plaintext);
    seal_sync_key(&keys_dir.join("sync.wrap"), &sync_key_new, &kek)?;
    key_state::set_current_gen(conn, rotate.gen_n_new, now_ms())?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::mailbox_client::{MailboxClientError, PulledEnvelope};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // -----------------------------------------------------------------
    // In-memory transport double — see `mailbox_client::MailboxTransport`'s
    // doc comment for why this exists instead of a real relay.
    // -----------------------------------------------------------------

    /// One stored row: `(envelope_id, ciphertext, version, created_at_ms)`.
    type StoredRow = (i64, Vec<u8>, u32, i64);

    #[derive(Clone, Default)]
    struct InMemoryRelay {
        mailboxes: Arc<Mutex<HashMap<String, Vec<StoredRow>>>>,
        next_id: Arc<Mutex<i64>>,
    }

    impl MailboxTransport for InMemoryRelay {
        async fn push(
            &self,
            mailbox_id: &str,
            ciphertext: &[u8],
            version: u32,
            _ttl_seconds: Option<u64>,
        ) -> std::result::Result<i64, MailboxClientError> {
            let mut next_id = self.next_id.lock().unwrap();
            *next_id += 1;
            let id = *next_id;
            self.mailboxes
                .lock()
                .unwrap()
                .entry(mailbox_id.to_string())
                .or_default()
                .push((id, ciphertext.to_vec(), version, now_ms()));
            Ok(id)
        }

        async fn pull(
            &self,
            mailbox_id: &str,
        ) -> std::result::Result<Vec<PulledEnvelope>, MailboxClientError> {
            use base64::{engine::general_purpose::STANDARD, Engine as _};
            let mailboxes = self.mailboxes.lock().unwrap();
            let Some(rows) = mailboxes.get(mailbox_id) else {
                return Ok(Vec::new());
            };
            Ok(rows
                .iter()
                .map(|(id, ct, version, created_at_ms)| {
                    PulledEnvelope::new(
                        *id,
                        *version,
                        mailbox_id.to_string(),
                        STANDARD.encode(ct),
                        *created_at_ms,
                    )
                })
                .collect())
        }

        async fn ack(
            &self,
            mailbox_id: &str,
            envelope_id: i64,
        ) -> std::result::Result<(), MailboxClientError> {
            if let Some(rows) = self.mailboxes.lock().unwrap().get_mut(mailbox_id) {
                rows.retain(|(id, ..)| *id != envelope_id);
            }
            Ok(())
        }
    }

    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    /// `load_kek`'s `#[cfg(test)]` branch bypasses the real keychain
    /// entirely (see its own doc comment) and never actually touches the
    /// `SecretStore` it's handed — this double exists only to satisfy that
    /// function's signature in test builds. `unreachable!()` on every method
    /// so a real call here (which would mean `load_kek`'s test bypass broke)
    /// fails loudly in the test that triggered it, rather than silently.
    struct UnreachableSecretStore;

    impl SecretStore for UnreachableSecretStore {
        fn store(&self, _key: &str, _value: &str) -> std::result::Result<(), String> {
            unreachable!("load_kek's test bypass never touches the SecretStore")
        }
        fn load(&self, _key: &str) -> crate::crypto::lookup::KeyLookup<String> {
            unreachable!("load_kek's test bypass never touches the SecretStore")
        }
        fn delete(&self, _key: &str) -> std::result::Result<(), String> {
            unreachable!("load_kek's test bypass never touches the SecretStore")
        }
    }

    fn test_secret_store() -> Arc<dyn SecretStore> {
        Arc::new(UnreachableSecretStore)
    }

    // -----------------------------------------------------------------
    // Derivation + MAC + AEAD unit tests
    // -----------------------------------------------------------------

    #[test]
    fn control_mailbox_id_is_deterministic_and_group_secret_scoped() {
        let secret_a = recovery::generate();
        let secret_b = recovery::generate();
        assert_eq!(control_mailbox_id(&secret_a), control_mailbox_id(&secret_a));
        assert_ne!(control_mailbox_id(&secret_a), control_mailbox_id(&secret_b));
    }

    #[test]
    fn data_mailbox_id_differs_per_generation() {
        let secret = recovery::generate();
        assert_ne!(data_mailbox_id(&secret, 0), data_mailbox_id(&secret, 1));
    }

    #[test]
    fn device_id_for_is_deterministic_from_public_key_bytes() {
        let a = DeviceIdentity::generate();
        let id1 = device_id_for(&a.public().identity_sign_pub_bytes());
        let id2 = device_id_for(&a.public().identity_sign_pub_bytes());
        assert_eq!(id1, id2);
        let b = DeviceIdentity::generate();
        assert_ne!(id1, device_id_for(&b.public().identity_sign_pub_bytes()));
    }

    #[test]
    fn mac_verifies_only_under_the_correct_group_secret_and_exact_bytes() {
        let secret_a = recovery::generate();
        let secret_b = recovery::generate();
        let bytes = b"some canonical message bytes";
        let mac = compute_mac(&secret_a, bytes);

        assert!(verify_mac(&secret_a, bytes, &mac));
        assert!(
            !verify_mac(&secret_b, bytes, &mac),
            "a different group_secret must not verify"
        );
        assert!(
            !verify_mac(&secret_a, b"tampered bytes!!!!!!!!!!!!!", &mac),
            "tampered bytes must not verify"
        );
    }

    #[test]
    fn fingerprint_is_deterministic_and_scoped_to_the_transcript() {
        let a = EphemeralSecret::random_from_rng(OsRng);
        let a_pub = BoxPublicKey::from(&a);
        let b = EphemeralSecret::random_from_rng(OsRng);
        let b_pub = BoxPublicKey::from(&b);

        let control_id = "deadbeef";
        let fp1 = fingerprint(&a_pub, &b_pub, control_id);
        let fp2 = fingerprint(&a_pub, &b_pub, control_id);
        assert_eq!(fp1, fp2);
        assert_ne!(fp1, fingerprint(&a_pub, &b_pub, "different-mailbox"));
    }

    #[test]
    fn aead_seal_open_round_trips_and_rejects_wrong_group_secret_binding() {
        let a_eph = EphemeralSecret::random_from_rng(OsRng);
        let b_identity = DeviceIdentity::generate();
        let shared = a_eph.diffie_hellman(&BoxPublicKey::from(b_identity.box_secret()));

        let secret_real = recovery::generate();
        let secret_forged = recovery::generate();
        let control_id = "control-mailbox";
        let request_id = [7u8; 16];
        let plaintext = [42u8; 32];

        let key_real = pairing_key(&shared, &secret_real, control_id, &request_id);
        let key_forged = pairing_key(&shared, &secret_forged, control_id, &request_id);
        let aad = JoinConfirm::aead_aad(control_id, &request_id);

        let sealed = aead_seal(&key_real, &aad, &plaintext);
        assert_eq!(aead_open(&key_real, &aad, &sealed).unwrap(), plaintext);

        // Even with the IDENTICAL ECDH shared secret, a party that derived
        // pairing_key under the wrong group_secret (e.g. a malicious relay
        // that observed the ephemeral keys on the wire but never learned
        // group_secret) cannot open this — the core fix for red-team
        // finding malicious-relay/A.
        assert!(aead_open(&key_forged, &aad, &sealed).is_err());
    }

    #[test]
    fn parse_mac_envelope_rejects_a_forged_join_request() {
        let real_secret = recovery::generate();
        let attacker_secret = recovery::generate();

        let request = JoinRequest {
            control_mailbox_id: control_mailbox_id(&real_secret),
            request_id: generate_request_id(),
            b_eph_pub: [1u8; 32],
            b_identity_pub: [2u8; 32],
            b_box_pub: [3u8; 32],
            b_device_label: "Attacker's phone".to_string(),
            requested_at_ms: now_ms(),
        };
        // Attacker doesn't know real_secret, only its own guess.
        let enveloped = build_mac_envelope(request, &attacker_secret).unwrap();

        let parsed = parse_mac_envelope::<JoinRequest>(&enveloped, &real_secret);
        assert!(parsed.is_err(), "a request MAC'd under the wrong group_secret must be rejected before it's ever parsed as legitimate");
    }

    #[test]
    fn mac_envelope_round_trips() {
        let secret = recovery::generate();
        let request = JoinRequest {
            control_mailbox_id: control_mailbox_id(&secret),
            request_id: generate_request_id(),
            b_eph_pub: [9u8; 32],
            b_identity_pub: [8u8; 32],
            b_box_pub: [7u8; 32],
            b_device_label: "Real device".to_string(),
            requested_at_ms: now_ms(),
        };
        let enveloped = build_mac_envelope(request.clone(), &secret).unwrap();
        let parsed = parse_mac_envelope::<JoinRequest>(&enveloped, &secret).unwrap();
        assert_eq!(parsed.request_id, request.request_id);
        assert_eq!(parsed.b_device_label, request.b_device_label);
    }

    // -----------------------------------------------------------------
    // Full pairing round trip, exercised through the real PairingManager
    // methods a Tauri command layer would call.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn full_pairing_round_trip_installs_matching_synckey_and_roster_on_both_devices() {
        let relay = InMemoryRelay::default();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let conn_a = test_conn();
        let conn_b = test_conn();

        let mgr_a = PairingManager::new(
            relay.clone(),
            dir_a.path().to_path_buf(),
            test_secret_store(),
        );
        let mgr_b = PairingManager::new(
            relay.clone(),
            dir_b.path().to_path_buf(),
            test_secret_store(),
        );

        let group_secret = mgr_a.create_group(&conn_a, "Device A").unwrap();

        let events_a: Arc<Mutex<Vec<PairingEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_a_clone = events_a.clone();
        let secret_for_a = group_secret.clone();
        let listen_task = tokio::spawn(async move {
            mgr_a
                .listen_for_join(&secret_for_a, move |e| {
                    events_a_clone.lock().unwrap().push(e)
                })
                .await
                .unwrap();
            mgr_a
        });

        let events_b: Arc<Mutex<Vec<PairingEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_b_clone = events_b.clone();
        let secret_for_b = group_secret.clone();
        let request_task = tokio::spawn(async move {
            mgr_b
                .request_join(&secret_for_b, "Device B", move |e| {
                    events_b_clone.lock().unwrap().push(e)
                })
                .await
                .unwrap();
            mgr_b
        });

        // Give both poll loops time to find each other's envelopes.
        tokio::time::sleep(Duration::from_millis(2_500)).await;

        let mgr_a = listen_task.await.unwrap();
        assert!(
            events_a
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, PairingEvent::IncomingRequest { .. })),
            "Device A should have surfaced the incoming request; got {:?}",
            events_a.lock().unwrap()
        );

        let gen_n = next_gen_n(&conn_a);
        let (confirm_event, roster_entry) = mgr_a
            .confirm_incoming(gen_n, &group_secret, true)
            .await
            .unwrap();
        assert!(matches!(confirm_event, PairingEvent::Complete { .. }));
        // Mirrors what the real Tauri command layer does: save the
        // returned entry with a fresh lock, after confirm_incoming's own
        // `.await`s have completed (see that method's doc comment).
        roster::save(
            &conn_a,
            &roster_entry.expect("approved confirm always returns an entry"),
        )
        .unwrap();

        // Give Device B's poll loop time to see the join_confirm.
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        let mgr_b = request_task.await.unwrap();
        assert!(
            events_b
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, PairingEvent::ReadyToInstall)),
            "Device B should have reached ReadyToInstall; got {:?}",
            events_b.lock().unwrap()
        );

        let (finish_event, roster_entries) = mgr_b.finish_join(true).await.unwrap();
        assert!(matches!(finish_event, PairingEvent::Complete { .. }));
        for entry in &roster_entries {
            roster::save(&conn_b, entry).unwrap();
        }

        let roster_a = roster::list(&conn_a);
        let roster_b = roster::list(&conn_b);
        assert_eq!(roster_a.len(), 2, "A's roster: itself + B");
        assert_eq!(roster_b.len(), 2, "B's roster: itself + A");

        // Both devices must agree on each other's device ids (the bug this
        // rewrite specifically fixed: independently-generated random ids
        // never matching across devices).
        let a_self_id = roster_a
            .iter()
            .find(|r| r.is_self)
            .unwrap()
            .device_id
            .clone();
        let a_as_seen_by_b = roster_b
            .iter()
            .find(|r| !r.is_self)
            .unwrap()
            .device_id
            .clone();
        assert_eq!(a_self_id, a_as_seen_by_b);

        let b_self_id = roster_b
            .iter()
            .find(|r| r.is_self)
            .unwrap()
            .device_id
            .clone();
        let b_as_seen_by_a = roster_a
            .iter()
            .find(|r| !r.is_self)
            .unwrap()
            .device_id
            .clone();
        assert_eq!(b_self_id, b_as_seen_by_a);
    }

    #[tokio::test]
    async fn a_join_request_with_a_forged_mac_is_never_surfaced_to_the_user() {
        let relay = InMemoryRelay::default();
        let dir_a = tempfile::tempdir().unwrap();
        let conn_a = test_conn();
        let mgr_a = PairingManager::new(
            relay.clone(),
            dir_a.path().to_path_buf(),
            test_secret_store(),
        );
        let group_secret = mgr_a.create_group(&conn_a, "Device A").unwrap();
        let control_id = control_mailbox_id(&group_secret);

        // Attacker posts a JoinRequest MAC'd under a secret it made up.
        let forged = JoinRequest {
            control_mailbox_id: control_id.clone(),
            request_id: generate_request_id(),
            b_eph_pub: [1u8; 32],
            b_identity_pub: [2u8; 32],
            b_box_pub: [3u8; 32],
            b_device_label: "Attacker".to_string(),
            requested_at_ms: now_ms(),
        };
        let attacker_secret = recovery::generate();
        push_mac_envelope(&relay, &control_id, forged, &attacker_secret, 600)
            .await
            .unwrap();

        let events: Arc<Mutex<Vec<PairingEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let handle = tokio::spawn(async move {
            // Short-lived listen: cancel it shortly after giving it time to
            // poll, rather than waiting out the full 5-minute TTL.
            mgr_a
                .listen_for_join(&group_secret, move |e| events_clone.lock().unwrap().push(e))
                .await
        });
        tokio::time::sleep(Duration::from_millis(2_500)).await;
        handle.abort();

        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .all(|e| !matches!(e, PairingEvent::IncomingRequest { .. })),
            "a forged join_request must never surface as IncomingRequest"
        );
    }

    // -----------------------------------------------------------------
    // Rotation (removal-triggered and periodic)
    // -----------------------------------------------------------------

    #[test]
    fn lowest_device_id_is_the_designated_initiator() {
        let entries = vec![
            roster_entry("bbb", &DeviceIdentity::generate(), false),
            roster_entry("aaa", &DeviceIdentity::generate(), true),
            roster_entry("ccc", &DeviceIdentity::generate(), false),
        ];
        assert!(is_designated_initiator(&entries, "aaa"));
        assert!(!is_designated_initiator(&entries, "bbb"));
        assert!(!is_designated_initiator(&entries, "ccc"));
    }

    #[test]
    fn rotate_with_no_target_round_trips_through_the_same_aead_construction_as_removal() {
        let a_identity = DeviceIdentity::generate();
        let b_identity = DeviceIdentity::generate();
        let shared_a = a_identity
            .box_secret()
            .diffie_hellman(&BoxPublicKey::from(b_identity.public().box_pub_bytes()));
        let shared_b = b_identity
            .box_secret()
            .diffie_hellman(&BoxPublicKey::from(a_identity.public().box_pub_bytes()));

        let control_id = "control-mailbox";
        let gen_n = 7u32;
        let sync_key_new = SyncKey::random();

        // Periodic rotation: no target.
        let key_a = rotation_key(&shared_a, control_id, None, gen_n);
        let aad = Rotate::aead_aad(control_id, None, gen_n, "device-b");
        let sealed = aead_seal(&key_a, &aad, sync_key_new.as_bytes());

        let key_b = rotation_key(&shared_b, control_id, None, gen_n);
        let opened = aead_open(&key_b, &aad, &sealed).unwrap();
        assert_eq!(&opened, sync_key_new.as_bytes());

        // The `None`/`Some` cases must never be interchangeable — opening
        // a periodic-rotation ciphertext under a removal-shaped AAD (or
        // vice versa) must fail, proving the 1-byte presence tag actually
        // separates the two key/AAD spaces rather than colliding.
        let removal_aad = Rotate::aead_aad(control_id, Some("device-c"), gen_n, "device-b");
        assert!(aead_open(&key_b, &removal_aad, &sealed).is_err());
    }

    #[test]
    fn apply_incoming_rotate_rejects_a_stale_or_duplicate_gen_n() {
        let conn = test_conn();
        key_state::set_current_gen(&conn, 5, 1_000).unwrap();

        let initiator = DeviceIdentity::generate();
        let me = DeviceIdentity::generate();
        let my_id = device_id_for(&me.public().identity_sign_pub_bytes());
        let initiator_id = device_id_for(&initiator.public().identity_sign_pub_bytes());
        roster::save(&conn, &roster_entry(&my_id, &me, true)).unwrap();
        roster::save(&conn, &roster_entry(&initiator_id, &initiator, false)).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let kek = load_kek(&UnreachableSecretStore).unwrap();
        seal_identity(&dir.path().join("sync_identity.wrap"), &me, &kek).unwrap();
        seal_sync_key(&dir.path().join("sync.wrap"), &SyncKey::random(), &kek).unwrap();

        let group_secret = recovery::generate();
        let control_id = control_mailbox_id(&group_secret);
        let shared = initiator
            .box_secret()
            .diffie_hellman(&BoxPublicKey::from(me.public().box_pub_bytes()));

        // gen_n_new == 5 (not-newer than the already-recorded 5) must be
        // rejected outright — a stale replay or duplicate copy.
        let stale_gen_n = 5u32;
        let key = rotation_key(&shared, &control_id, None, stale_gen_n);
        let aad = Rotate::aead_aad(&control_id, None, stale_gen_n, &my_id);
        let ciphertext = aead_seal(&key, &aad, SyncKey::random().as_bytes());
        let mut rotate = Rotate {
            control_mailbox_id: control_id.clone(),
            target_device_id: None,
            gen_n_new: stale_gen_n,
            for_device_id: my_id.clone(),
            initiator_device_id: initiator_id.clone(),
            ciphertext,
            initiator_sig: [0u8; 64],
        };
        rotate.initiator_sig = initiator.sign(&rotate.signed_fields()).to_bytes();
        let enveloped = build_mac_envelope(rotate, &group_secret).unwrap();

        let sync_key_before = open_sync_key(&dir.path().join("sync.wrap"), &kek).unwrap();
        let applied = apply_incoming_rotate(
            &conn,
            dir.path(),
            &UnreachableSecretStore,
            &group_secret,
            &my_id,
            &enveloped,
        )
        .unwrap();
        assert!(!applied, "a non-newer gen_n_new must be rejected");
        assert_eq!(
            key_state::get(&conn).current_gen_n,
            5,
            "must not regress or no-op-advance the counter"
        );
        let sync_key_after = open_sync_key(&dir.path().join("sync.wrap"), &kek).unwrap();
        assert_eq!(
            sync_key_before.as_bytes(),
            sync_key_after.as_bytes(),
            "a rejected stale rotation must never touch sync.wrap"
        );
    }

    fn roster_entry(device_id: &str, identity: &DeviceIdentity, is_self: bool) -> RosterEntry {
        RosterEntry {
            device_id: device_id.to_string(),
            identity_sign_pub: identity.public().identity_sign_pub_bytes(),
            box_pub: identity.public().box_pub_bytes(),
            label: device_id.to_string(),
            gen_added: 0,
            is_self,
        }
    }

    /// Directly seeds one device's on-disk key material and local roster —
    /// bypasses the full async pairing protocol (already covered by
    /// `full_pairing_round_trip_...`) so rotation-focused tests can set up
    /// a multi-device group cheaply and deterministically. `group_secret`
    /// itself is never sealed to disk here since none of
    /// `remove_device_locally`/`rotate_key_periodically`/
    /// `apply_incoming_rotate` read it from disk — it's always passed in
    /// directly by the caller (`crates/app`'s command layer loads it once
    /// per call via `PairingManager::load_group_secret`).
    fn seed_device(
        keys_dir: &Path,
        conn: &Connection,
        identity: &DeviceIdentity,
        sync_key: &SyncKey,
        roster_entries: &[RosterEntry],
    ) {
        let kek = load_kek(&UnreachableSecretStore).unwrap();
        seal_identity(&keys_dir.join("sync_identity.wrap"), identity, &kek).unwrap();
        seal_sync_key(&keys_dir.join("sync.wrap"), sync_key, &kek).unwrap();
        for entry in roster_entries {
            roster::save(conn, entry).unwrap();
        }
    }

    #[tokio::test]
    async fn removal_then_periodic_rotation_converge_all_survivors_on_the_same_key() {
        let relay = InMemoryRelay::default();
        let group_secret = recovery::generate();
        let control_mailbox = control_mailbox_id(&group_secret);
        let sync_key_v0 = SyncKey::random();

        let identity_a = DeviceIdentity::generate();
        let identity_b = DeviceIdentity::generate();
        let identity_c = DeviceIdentity::generate();
        let id_a = device_id_for(&identity_a.public().identity_sign_pub_bytes());
        let id_b = device_id_for(&identity_b.public().identity_sign_pub_bytes());
        let id_c = device_id_for(&identity_c.public().identity_sign_pub_bytes());

        let full_roster = |self_id: &str| {
            vec![
                roster_entry(&id_a, &identity_a, self_id == id_a),
                roster_entry(&id_b, &identity_b, self_id == id_b),
                roster_entry(&id_c, &identity_c, self_id == id_c),
            ]
        };

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let conn_a = test_conn();
        let conn_b = test_conn();
        seed_device(
            dir_a.path(),
            &conn_a,
            &identity_a,
            &sync_key_v0,
            &full_roster(&id_a),
        );
        seed_device(
            dir_b.path(),
            &conn_b,
            &identity_b,
            &sync_key_v0,
            &full_roster(&id_b),
        );
        // Device C only needs a roster on the other two — it's never
        // brought online in this test, only removed.

        // --- A removes C. ---
        let pending = remove_device_locally(
            &conn_a,
            dir_a.path(),
            &UnreachableSecretStore,
            &group_secret,
            &id_c,
        )
        .unwrap();
        assert_eq!(
            pending.len(),
            1,
            "only the one other survivor (B) gets a Rotate message"
        );
        deliver_rotations(&relay, pending, &group_secret).await;

        let pulled = relay.pull(&control_mailbox).await.unwrap();
        assert_eq!(pulled.len(), 1);
        let raw = pulled[0].ciphertext().unwrap();
        let applied = apply_incoming_rotate(
            &conn_b,
            dir_b.path(),
            &UnreachableSecretStore,
            &group_secret,
            &id_b,
            &raw,
        )
        .unwrap();
        assert!(applied, "B should apply the removal rotation addressed to it — previously nothing in the app ever called this");
        relay
            .ack(&control_mailbox, pulled[0].envelope_id)
            .await
            .unwrap();

        assert_eq!(
            roster::list(&conn_b).len(),
            2,
            "C must be gone from B's roster too"
        );
        assert_eq!(
            key_state::get(&conn_a).current_gen_n,
            key_state::get(&conn_b).current_gen_n,
            "both sides must agree on the new generation — previously the initiator's own counter never advanced either"
        );

        let kek = load_kek(&UnreachableSecretStore).unwrap();
        let sync_key_gen1_a = open_sync_key(&dir_a.path().join("sync.wrap"), &kek).unwrap();
        let sync_key_gen1_b = open_sync_key(&dir_b.path().join("sync.wrap"), &kek).unwrap();
        assert_eq!(
            sync_key_gen1_a.as_bytes(),
            sync_key_gen1_b.as_bytes(),
            "A and B must converge on the same rotated key"
        );
        assert_ne!(sync_key_gen1_a.as_bytes(), sync_key_v0.as_bytes());

        // --- Periodic rotation between the two survivors, initiated by
        // whichever of {A, B} has the lower device id. ---
        let roster_now = roster::list(&conn_a);
        let lowest_is_a = is_designated_initiator(&roster_now, &id_a);
        assert_ne!(
            lowest_is_a,
            is_designated_initiator(&roster_now, &id_b),
            "exactly one of the two survivors is designated"
        );

        let (initiator_conn, initiator_dir, survivor_conn, survivor_dir, survivor_id) =
            if lowest_is_a {
                (&conn_a, dir_a.path(), &conn_b, dir_b.path(), id_b.as_str())
            } else {
                (&conn_b, dir_b.path(), &conn_a, dir_a.path(), id_a.as_str())
            };

        let pending2 = rotate_key_periodically(
            initiator_conn,
            initiator_dir,
            &UnreachableSecretStore,
            &group_secret,
        )
        .unwrap();
        assert_eq!(
            pending2.len(),
            1,
            "the single other survivor gets exactly one Rotate"
        );
        deliver_rotations(&relay, pending2, &group_secret).await;

        let pulled2 = relay.pull(&control_mailbox).await.unwrap();
        assert_eq!(pulled2.len(), 1);
        let raw2 = pulled2[0].ciphertext().unwrap();
        let applied2 = apply_incoming_rotate(
            survivor_conn,
            survivor_dir,
            &UnreachableSecretStore,
            &group_secret,
            survivor_id,
            &raw2,
        )
        .unwrap();
        assert!(
            applied2,
            "the surviving non-initiator must apply the periodic rotation"
        );

        assert_eq!(
            roster::list(survivor_conn).len(),
            2,
            "periodic rotation must not remove anyone from the roster"
        );

        let sync_key_gen2_initiator =
            open_sync_key(&initiator_dir.join("sync.wrap"), &kek).unwrap();
        let sync_key_gen2_survivor = open_sync_key(&survivor_dir.join("sync.wrap"), &kek).unwrap();
        assert_eq!(
            sync_key_gen2_initiator.as_bytes(),
            sync_key_gen2_survivor.as_bytes(),
            "both sides must converge again"
        );
        assert_ne!(
            sync_key_gen2_initiator.as_bytes(),
            sync_key_gen1_a.as_bytes(),
            "periodic rotation must install yet another new key, not repeat the removal-rotation one"
        );
    }
}
