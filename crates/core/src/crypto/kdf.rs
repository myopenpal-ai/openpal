//! Argon2id key derivation (`docs/SPACES_AUTHORING.md` §9 "The portable
//! passphrase — decided"). `m_cost`/`t_cost` are read from a wrap file's
//! header (untrusted input, though authenticated — see `super::wrap`) and
//! clamped to a compiled-in floor here, so a downgraded header can weaken
//! a rewrap's *parameters* only down to a bound this binary still enforces,
//! never below it.
//!
//! [`derive_subkey`] is a different kind of derivation living in the same
//! module: HKDF-SHA256 key *separation*, not password stretching. It turns
//! the one DEK into as many independent, purpose-scoped keys as callers
//! need (the app log, the engine log, ...) so unrelated ciphertext
//! structures never share an AEAD key. No salt — this is deliberately
//! cheap, unlike [`derive`] above, because the input is already a
//! uniformly-random 256-bit key, not a low-entropy passphrase.

use super::key::DataKey;
use argon2::{Algorithm, Argon2, Params, Version};
use ring::hkdf;

/// 64 MiB — the doc's default, chosen because the security lives in the
/// passphrase's word count, not the memory parameter (quadrupling memory
/// buys ~2 bits; one extra word buys ~11).
pub const M_COST_KIB: u32 = 64 * 1024;
pub const T_COST: u32 = 3;
/// Neither Rust Argon2id implementation parallelizes lanes, and `p > 1`
/// measured *slower* than `p = 1` for identical work — a pure gift to a GPU
/// attacker, per the doc's own measurement.
pub const P_COST: u32 = 1;

/// Compiled-in floor: no derivation this binary performs ever uses less
/// than this, regardless of what a wrap file's (authenticated, but
/// possibly old or malicious) header claims.
pub const M_COST_FLOOR_KIB: u32 = 32 * 1024;
pub const T_COST_FLOOR: u32 = 2;

#[derive(Debug, thiserror::Error)]
pub enum KdfError {
    #[error("invalid Argon2 parameters: {0}")]
    InvalidParams(argon2::Error),
    #[error("key derivation failed: {0}")]
    DerivationFailed(argon2::Error),
}

/// Derives a 32-byte key from `passphrase` and `salt`. `m_cost_kib`/`t_cost`
/// are clamped to the compiled-in floor above before use.
pub fn derive(
    passphrase: &[u8],
    salt: &[u8; 16],
    m_cost_kib: u32,
    t_cost: u32,
) -> Result<DataKey, KdfError> {
    let m_cost = m_cost_kib.max(M_COST_FLOOR_KIB);
    let t_cost = t_cost.max(T_COST_FLOOR);
    let params = Params::new(m_cost, t_cost, P_COST, Some(32)).map_err(KdfError::InvalidParams)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(passphrase, salt, &mut out)
        .map_err(KdfError::DerivationFailed)?;
    Ok(DataKey::from_bytes(out))
}

/// Derive a purpose-scoped 256-bit subkey from `dek`, via HKDF-SHA256 with
/// no salt and `info` as the context-binding label (e.g.
/// `b"openpal-app-log-v1"`). Two different `info` values applied to the
/// same `dek` produce independent, unrelated keys — the domain-separation
/// property that lets the app log and engine log each get their own key
/// while both trace back to the same DEK, without ever reusing one AEAD
/// key across unrelated ciphertext structures.
pub fn derive_subkey(dek: &DataKey, info: &[u8]) -> DataKey {
    derive_subkey_from_bytes(dek.as_bytes(), info)
}

/// Same construction as [`derive_subkey`], generalized to any-length input
/// keying material rather than only a 32-byte [`DataKey`] — needed for
/// `docs/SYNC_PAIRING_PROTOCOL.md`'s derivations, whose `group_secret` is a
/// 128-bit `crate::crypto::recovery` code (26 Crockford-Base32 characters),
/// not a `DataKey`. `derive_subkey` is defined in terms of this function
/// rather than duplicating the HKDF call, so there is exactly one place
/// this construction lives.
///
/// `ikm` should already be uniformly random (or, as with a recovery code,
/// a deterministic injective encoding of uniformly random bytes — encoding
/// format doesn't cost entropy) — this is expansion only, no salt, same
/// rationale as `derive_subkey`'s own doc comment.
pub fn derive_subkey_from_bytes(ikm: &[u8], info: &[u8]) -> DataKey {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]);
    let prk = salt.extract(ikm);
    let info_parts = [info];
    let okm = prk
        .expand(&info_parts, hkdf::HKDF_SHA256)
        .expect("HKDF-SHA256 expand of one 32-byte key cannot exceed its output limit");
    let mut out = [0u8; 32];
    okm.fill(&mut out)
        .expect("32 bytes is exactly HKDF-SHA256's output length");
    DataKey::from_bytes(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_derive_the_same_key() {
        let salt = [1u8; 16];
        let a = derive(b"correct horse battery staple", &salt, M_COST_KIB, T_COST).unwrap();
        let b = derive(b"correct horse battery staple", &salt, M_COST_KIB, T_COST).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn different_passphrases_derive_different_keys() {
        let salt = [1u8; 16];
        let a = derive(b"correct horse battery staple", &salt, M_COST_KIB, T_COST).unwrap();
        let b = derive(b"correct horse battery staplf", &salt, M_COST_KIB, T_COST).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn different_salts_derive_different_keys() {
        let a = derive(b"same passphrase", &[1u8; 16], M_COST_KIB, T_COST).unwrap();
        let b = derive(b"same passphrase", &[2u8; 16], M_COST_KIB, T_COST).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn params_below_the_floor_are_clamped_not_rejected() {
        let salt = [1u8; 16];
        let clamped = derive(b"pw", &salt, 1, 1).unwrap();
        let floor = derive(b"pw", &salt, M_COST_FLOOR_KIB, T_COST_FLOOR).unwrap();
        assert_eq!(clamped, floor);
    }

    #[test]
    fn derive_subkey_is_deterministic_for_the_same_dek_and_info() {
        let dek = DataKey::random();
        let a = derive_subkey(&dek, b"openpal-app-log-v1");
        let b = derive_subkey(&dek, b"openpal-app-log-v1");
        assert_eq!(a, b);
    }

    #[test]
    fn derive_subkey_domain_separates_by_info() {
        let dek = DataKey::random();
        let app_key = derive_subkey(&dek, b"openpal-app-log-v1");
        let engine_key = derive_subkey(&dek, b"openpal-engine-log-v1");
        assert_ne!(app_key, engine_key);
    }

    #[test]
    fn derive_subkey_differs_from_the_dek_itself() {
        let dek = DataKey::random();
        let subkey = derive_subkey(&dek, b"openpal-app-log-v1");
        assert_ne!(subkey.as_bytes(), dek.as_bytes());
    }

    #[test]
    fn derive_subkey_differs_across_deks() {
        let a = derive_subkey(&DataKey::random(), b"same-info");
        let b = derive_subkey(&DataKey::random(), b"same-info");
        assert_ne!(a, b);
    }
}
