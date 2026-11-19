//! The Standard Notes-004-style key hierarchy behind per-conversation
//! encryption (`docs/SPACES_AUTHORING.md` §9 and the "per-conversation →
//! items key → password-derived root" model that
//! [feat(security): adopt Standard Notes-style per-conversation encryption
//! under a mandatory password] records as binding).
//!
//! Three levels, mirroring Standard Notes' own envelope exactly:
//!
//! - **Password-derived root key.** `derive_root` stretches a passphrase
//!   with Argon2id (parameters clamped to the compiled-in floor in
//!   [`super::kdf`]) and then HKDF-splits the result into two 256-bit
//!   halves: the *root key* (which wraps the items key) and a *server half*
//!   (Standard Notes' `serverPassword` — used there for server
//!   authentication; here it is **reserved and never emitted or stored**,
//!   until the self-hostable backend that would use it exists). Only the
//!   root key is ever used by this build.
//! - **Items key.** One random 256-bit key per install — in this app it is
//!   the same key that keys the SQLCipher database (`AppState.dek`), so the
//!   whole tree is unlockable with exactly one secret. It is sealed under
//!   the password-derived root in `<data>/keys/password.wrap` (an ordinary
//!   [`super::wrap`] file whose header records the Argon2id salt and
//!   parameters, so an unlock re-derives what the creation run actually
//!   used and a downgraded header is rejected, never silently weakened).
//!   The recovery slot (`crate::crypto::recovery` + `commands.rs`'s
//!   `confirm_recovery_code`) seals the same items key under the
//!   recovery-code-derived KEK — that's why a lost password still unlocks
//!   data, and why a pre-upgrade recovery code keeps working untouched
//!   (a migrated install adopts its existing DEK as the items key).
//! - **Per-conversation keys.** One random 256-bit key per conversation,
//!   never shared. It is stored — wrapped under the items key — inside the
//!   conversation's own stored payload ([`ConversationEnvelope`]), and the
//!   conversation's content is sealed under it.
//!
//! ## The wire format ([`ConversationEnvelope`])
//!
//! Stored in the `conversations.data` column (see `crate::store`), and the
//! exact unit a future dumb backend can persist and return without ever
//! seeing plaintext or key material:
//!
//! ```json
//! { "v": 1,
//!   "key":     { "alg": "AES-256-GCM", "nonce": "<b64>", "ct": "<b64>" },
//!   "content": { "alg": "AES-256-GCM", "nonce": "<b64>", "ct": "<b64>" } }
//! ```
//!
//! `key` is the per-conversation key sealed under the items key; `content`
//! is the conversation JSON sealed under the per-conversation key. `v`
//! records the wire-format version — readers reject anything they don't
//! know — and "which items key wraps which conversation key" is recorded by
//! `v` itself plus the AEAD label on `key`: there is exactly one items key
//! per install, and the `alg` field states the wrapping construction, so a
//! store carrying these payloads never needs to guess.
//!
//! Derived from [`super::wrap`]'s conventions (AES-256-GCM, nonces from
//! `SysRng`, plaintext buffers zeroized on every exit path, `DataKey` never
//! displayed), but as a small per-use-case seal/open pair rather than a
//! shared generic function — this crate's established crypto style
//! (`wrap.rs`/`pairing.rs`/`sync::envelope.rs` each already do this).
//!
//! ## Rotation
//!
//! Changing the password re-wraps **only this key-encryption layer**:
//! `write_password_file` under the new root. The items key, every
//! per-conversation key, and every conversation's content are untouched —
//! a password change never re-encrypts conversation content. (The
//! per-conversation `v`/`key`/`content` payloads are independent of the
//! password once sealed.)

use super::key::DataKey;
use super::{kdf, wrap};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::rngs::SysRng;
use rand::{rand_core::UnwrapErr, Rng};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use serde::{Deserialize, Serialize};
use std::path::Path;
use zeroize::{Zeroize, Zeroizing};

/// The one wire-format version this build reads and writes. Bump (and add a
/// reader branch) if the envelope's framing, its key-wrap construction, or
/// the semantics of what `content` holds ever change.
pub const ENVELOPE_VERSION: u8 = 1;

/// The AEAD every `Sealed` blob in an envelope uses. Recorded in each blob
/// so a dumb store carrying these payloads never has to guess — and so a
/// future format that switches constructions is visibly different.
const AEAD_ALG: &str = "AES-256-GCM";

/// HKDF `info` label binding the root key's first half to its role and no
/// other — see [`derive_root`].
const ROOT_INFO: &[u8] = b"openpal-root-key-v1";
/// HKDF `info` label for the reserved `serverPassword` half (Standard
/// Notes' `serverPassword`) — derived from the same password-stretched
/// material but never communicated by anything in this build.
const SERVER_HALF_INFO: &[u8] = b"openpal-server-half-v1";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("a conversation envelope with wire version {0} can't be opened by this build")]
    UnsupportedVersion(u8),
    #[error("a conversation envelope uses an unsupported AEAD ({0})")]
    UnsupportedAlg(String),
    #[error("the conversation envelope is malformed")]
    Malformed,
    #[error("wrong key, or the conversation envelope was tampered with")]
    WrongKey,
    #[error("this key can't be used for AES-256-GCM")]
    UnusableKey,
    #[error("{0}")]
    Kdf(#[from] kdf::KdfError),
    #[error("{0}")]
    Wrap(#[from] wrap::WrapError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

/// The password-derived root. `server_half` is returned (so a future
/// backend ticket can authenticate with it) but must never be stored,
/// logged, or otherwise communicated by this build — see the module doc.
pub fn derive_root(
    passphrase: &[u8],
    salt: &[u8; 16],
    m_cost_kib: u32,
    t_cost: u32,
) -> Result<(DataKey, DataKey), kdf::KdfError> {
    let root_material = kdf::derive(passphrase, salt, m_cost_kib, t_cost)?;
    let root = kdf::derive_subkey(&root_material, ROOT_INFO);
    let server_half = kdf::derive_subkey(&root_material, SERVER_HALF_INFO);
    Ok((root, server_half))
}

/// One sealed blob inside an envelope: a base64 nonce and the base64
/// ciphertext (which for `open_in_place` output includes the trailing GCM
/// tag). `alg` records the construction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Sealed {
    pub alg: String,
    pub nonce: String,
    /// Base64 of `ciphertext || tag`.
    pub ct: String,
}

/// A stored conversation payload — see the module doc for the wire format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConversationEnvelope {
    /// [`ENVELOPE_VERSION`]. Readers reject anything else.
    pub v: u8,
    /// The per-conversation key, sealed under the items key.
    pub key: Sealed,
    /// The conversation JSON, sealed under the per-conversation key.
    pub content: Sealed,
}

impl ConversationEnvelope {
    /// Constructs a fresh envelope for `plaintext` under `items_key`,
    /// minting a brand-new per-conversation key — no two conversations
    /// share one, because each [`seal_conversation`] call mints its own
    /// [`DataKey::random`] and never reuses or stores it in the clear.
    pub fn seal(items_key: &DataKey, plaintext: &[u8]) -> Result<Self, VaultError> {
        let conversation_key = DataKey::random();
        let key = seal_blob(items_key, conversation_key.as_bytes())?;
        let content = seal_blob(&conversation_key, plaintext)?;
        Ok(Self {
            v: ENVELOPE_VERSION,
            key,
            content,
        })
    }

    /// Unseals `content` (the conversation JSON) using `items_key` to first
    /// unwrap this envelope's own per-conversation key. `Err` on a wrong
    /// items key, a tampered blob, or an unknown version — never a partial
    /// trust.
    pub fn open(&self, items_key: &DataKey) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if self.v != ENVELOPE_VERSION {
            return Err(VaultError::UnsupportedVersion(self.v));
        }
        let conversation_key_bytes = open_blob(&self.key, items_key)?;
        let conversation_key: [u8; 32] = conversation_key_bytes
            .as_slice()
            .try_into()
            .map_err(|_| VaultError::Malformed)?;
        let conversation_key = DataKey::from_bytes(conversation_key);
        open_blob(&self.content, &conversation_key)
    }
}

/// Whether `data` looks like a stored envelope rather than a pre-envelope
/// plaintext `Conversation` JSON blob — used by the once-per-install
/// migration (`crate::store::rewrite_rows_as_envelopes`) to leave an
/// already-migrated row alone. A plaintext `Conversation` serialization
/// can't structurally match: it has no `v`/`key`/`content` triple, and the
/// fields are camelCase strings of different shapes.
pub fn is_envelope(data: &str) -> bool {
    serde_json::from_str::<ConversationEnvelope>(data)
        .map(|env| env.v == ENVELOPE_VERSION)
        .unwrap_or(false)
}

fn seal_blob(key_material: &DataKey, plaintext: &[u8]) -> Result<Sealed, VaultError> {
    let unbound = UnboundKey::new(&AES_256_GCM, key_material.as_bytes())
        .map_err(|_| VaultError::UnusableKey)?;
    let key = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    UnwrapErr(SysRng).fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    // `in_out` briefly holds the raw plaintext before
    // `seal_in_place_append_tag` overwrites it with ciphertext — zeroized
    // unconditionally below, on every exit path, like `wrap::seal`'s own.
    let mut in_out = plaintext.to_vec();
    let result = key
        .seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| VaultError::WrongKey)
        .map(|()| Sealed {
            alg: AEAD_ALG.to_string(),
            nonce: B64.encode(nonce_bytes),
            ct: B64.encode(&in_out),
        });
    in_out.zeroize();
    result
}

fn open_blob(sealed: &Sealed, key_material: &DataKey) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    if sealed.alg != AEAD_ALG {
        return Err(VaultError::UnsupportedAlg(sealed.alg.clone()));
    }
    let nonce_bytes: Zeroizing<Vec<u8>> = Zeroizing::new(
        B64.decode(&sealed.nonce)
            .map_err(|_| VaultError::Malformed)?,
    );
    if nonce_bytes.len() != NONCE_LEN {
        return Err(VaultError::Malformed);
    }
    let nonce =
        Nonce::try_assume_unique_for_key(&nonce_bytes).map_err(|_| VaultError::Malformed)?;

    // `buf` holds the raw plaintext once `open_in_place` succeeds —
    // zeroized unconditionally below, on every exit path, like
    // `wrap::open`'s own. A plain `Vec<u8>` drop would only deallocate.
    let mut buf: Zeroizing<Vec<u8>> =
        Zeroizing::new(B64.decode(&sealed.ct).map_err(|_| VaultError::Malformed)?);
    let unbound = UnboundKey::new(&AES_256_GCM, key_material.as_bytes())
        .map_err(|_| VaultError::UnusableKey)?;
    let key = LessSafeKey::new(unbound);
    let result = key
        .open_in_place(nonce, Aad::empty(), &mut buf)
        .map_err(|_| VaultError::WrongKey)
        .map(|plaintext| Zeroizing::new(plaintext.to_vec()));
    result
}

/// The primary password slot's wrap file: `<data>/keys/password.wrap`.
pub fn password_wrap_path(keys_dir: &Path) -> std::path::PathBuf {
    keys_dir.join("password.wrap")
}

/// Writes (or, on a password change, re-writes — see the module doc's
/// "Rotation" section) `<data>/keys/password.wrap`: a fresh random salt, the
/// root key derived from `password`, and `items_key` sealed under it as an
/// ordinary [`super::wrap`] file. The wrap file's header records the salt
/// and KDF parameters, so the matching [`open_password_file`] re-derives
/// exactly what this call used, and a tampered field fails decrypting
/// rather than silently weakening a rewrap.
pub fn write_password_file(
    path: &Path,
    items_key: &DataKey,
    password: &[u8],
) -> Result<(), VaultError> {
    let mut salt = [0u8; 16];
    UnwrapErr(SysRng).fill_bytes(&mut salt);
    let (root, _server_half) = derive_root(password, &salt, kdf::M_COST_KIB, kdf::T_COST)?;
    wrap::write_wrap(path, items_key, &root, &wrap::WrapHeader::new(salt))?;
    Ok(())
}

/// Opens `<data>/keys/password.wrap` with `password`: reads the salt and
/// KDF parameters the file was actually sealed with (never assumed),
/// re-derives the root from `password`, and unseals the items key. A wrong
/// password lands on [`wrap::WrapError::WrongPassphrase`] — nothing is
/// decrypted, matching "a wrong password decrypts nothing".
pub fn open_password_file(path: &Path, password: &[u8]) -> Result<DataKey, VaultError> {
    let header = wrap::read_header(path)?;
    let (root, _server_half) =
        derive_root(password, &header.salt, header.m_cost_kib, header.t_cost)?;
    let items_key = wrap::read_wrap(path, &root)?;
    Ok(items_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_root_is_deterministic_and_domain_separated() {
        let salt = [7u8; 16];
        let (root_a, server_a) = derive_root(b"correct horse battery staple", &salt, 1, 1).unwrap();
        let (root_b, server_b) = derive_root(b"correct horse battery staple", &salt, 1, 1).unwrap();
        assert_eq!(root_a, root_b, "same password + salt ⇒ same root");
        assert_eq!(
            server_a, server_b,
            "same password + salt ⇒ same server half"
        );
        assert_ne!(root_a, server_a, "the two halves must never coincide");
    }

    #[test]
    fn derive_root_differs_across_passwords_and_salts() {
        let a = derive_root(b"password one", &[1u8; 16], 1, 1).unwrap().0;
        let b = derive_root(b"password two", &[1u8; 16], 1, 1).unwrap().0;
        let c = derive_root(b"password one", &[2u8; 16], 1, 1).unwrap().0;
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn derive_root_params_below_the_floor_are_clamped() {
        // Same posture as `kdf::derive`'s own clamp test: a header claiming
        // 1 MiB must derive the same root as the compiled-in floor.
        let flung = derive_root(b"pw", &[1u8; 16], 1, 1).unwrap().0;
        let floor = derive_root(b"pw", &[1u8; 16], kdf::M_COST_FLOOR_KIB, kdf::T_COST_FLOOR)
            .unwrap()
            .0;
        assert_eq!(flung, floor);
    }

    #[test]
    fn envelope_roundtrips_and_never_reuses_per_conversation_keys() {
        let items_key = DataKey::random();
        let plaintext = br#"{"title":"Trip planning","messages":[]}"#;

        let envelope = ConversationEnvelope::seal(&items_key, plaintext).unwrap();
        let opened = envelope.open(&items_key).unwrap();
        assert_eq!(opened.as_slice(), plaintext);

        // A second seal of the SAME content must produce a different
        // ciphertext — each call mints a fresh per-conversation key, so no
        // two conversations share one and no two renders of the same
        // content are byte-identical.
        let again = ConversationEnvelope::seal(&items_key, plaintext).unwrap();
        assert_ne!(envelope.content.ct, again.content.ct);
    }

    #[test]
    fn envelope_rejects_the_wrong_items_key() {
        let items_key = DataKey::random();
        let envelope = ConversationEnvelope::seal(&items_key, b"secret").unwrap();
        let wrong = DataKey::random();
        assert!(matches!(envelope.open(&wrong), Err(VaultError::WrongKey)));
    }

    #[test]
    fn envelope_rejects_tampered_ciphertext() {
        let items_key = DataKey::random();
        let mut envelope = ConversationEnvelope::seal(&items_key, b"secret").unwrap();
        let mut bytes = B64.decode(&envelope.content.ct).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        envelope.content.ct = B64.encode(&bytes);
        assert!(matches!(
            envelope.open(&items_key),
            Err(VaultError::WrongKey)
        ));
    }

    #[test]
    fn envelope_rejects_an_unknown_version() {
        let items_key = DataKey::random();
        let mut envelope = ConversationEnvelope::seal(&items_key, b"secret").unwrap();
        envelope.v = ENVELOPE_VERSION + 1;
        assert!(matches!(
            envelope.open(&items_key),
            Err(VaultError::UnsupportedVersion(v)) if v == ENVELOPE_VERSION + 1
        ));
    }

    #[test]
    fn envelope_rejects_an_unknown_aead_label() {
        let items_key = DataKey::random();
        let mut envelope = ConversationEnvelope::seal(&items_key, b"secret").unwrap();
        envelope.content.alg = "AES-128-GCM".to_string();
        assert!(matches!(
            envelope.open(&items_key),
            Err(VaultError::UnsupportedAlg(_))
        ));
    }

    #[test]
    fn is_envelope_distinguishes_envelopes_from_plaintext() {
        let items_key = DataKey::random();
        let envelope = ConversationEnvelope::seal(&items_key, b"{}").unwrap();
        assert!(is_envelope(&serde_json::to_string(&envelope).unwrap()));
        assert!(!is_envelope(r#"{"id":"conv-1","title":"old chat"}"#));
        assert!(!is_envelope("not json at all"));
    }

    #[test]
    fn password_file_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password.wrap");
        let items_key = DataKey::random();
        write_password_file(&path, &items_key, b"hunter2 correct horse battery staple").unwrap();
        let opened = open_password_file(&path, b"hunter2 correct horse battery staple").unwrap();
        assert_eq!(opened, items_key);
    }

    #[test]
    fn password_file_rejects_a_wrong_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password.wrap");
        write_password_file(&path, &DataKey::random(), b"right password").unwrap();
        let err = open_password_file(&path, b"wrong password").unwrap_err();
        assert!(matches!(
            err,
            VaultError::Wrap(wrap::WrapError::WrongPassphrase)
        ));
    }

    #[test]
    fn rewrapping_a_password_file_rotates_only_the_key_encryption_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password.wrap");
        let items_key = DataKey::random();
        write_password_file(&path, &items_key, b"old password").unwrap();

        // A password change is the same write under a new password: the
        // items key it seals is the SAME key, so everything wrapped under
        // that items key (every conversation envelope) is untouched. The
        // old generation is preserved as `.bak` by `wrap::write_wrap`.
        write_password_file(&path, &items_key, b"new password").unwrap();

        assert!(open_password_file(&path, b"old password").is_err());
        assert_eq!(
            open_password_file(&path, b"new password").unwrap(),
            items_key
        );
    }
}
