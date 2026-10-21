//! The change-envelope wire schema for Phase 3 data sync, and its
//! encryption under the sync group's `SyncKey`. This is the one place a
//! `crate::sync::conversations::AppliedLocal` (or a projects/artifacts/
//! settings/custom_models change) turns into bytes that leave the device —
//! `crate::sync::outbox` only ever stores/forwards opaque ciphertext it
//! never inspects, same "relay stays blind" principle
//! `docs/SYNC_PAIRING_PROTOCOL.md` already established for pairing.
//!
//! AEAD construction mirrors `crate::crypto::wrap`'s and
//! `crate::sync::pairing`'s exactly (nonce via `OsRng`, AES-256-GCM,
//! zeroize plaintext buffers on every exit path) — this project's existing
//! convention is a small, local seal/open pair per use case rather than one
//! shared generic function (`wrap.rs` and `pairing.rs` each already do
//! this independently), so this module follows suit rather than
//! introducing a fourth abstraction.

use crate::crypto::sync_key::SyncKey;
use crate::sync::hlc::Hlc;
use crate::types::{ChatMessage, Conversation, ModelSpec, Project};
use crate::{Error, Result};
use rand::rngs::OsRng;
use rand::RngCore;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// One change, in the shape it travels the wire. `mailbox_id` isn't part of
/// this payload — the caller already knows which mailbox to push
/// to/pulled from (`pairing::data_mailbox_id` for every variant here; the
/// control-mailbox-only variants from Phase 2 live in `pairing`'s own
/// message types, untouched by this module).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChangeEnvelope {
    AppendMessage {
        conversation_id: String,
        hlc: Hlc,
        message: ChatMessage,
    },
    ConversationSnapshot {
        hlc: Hlc,
        conversation: Conversation,
    },
    ConversationTitle {
        conversation_id: String,
        hlc: Hlc,
        title: String,
    },
    ConversationProject {
        conversation_id: String,
        hlc: Hlc,
        project_id: Option<String>,
    },
    /// The sidebar's "New reply" hint, set or cleared — its own LWW
    /// register (`field_versions` scope `"conversation_unread"`), so reading
    /// a conversation on one device never races a rename or a move made on
    /// another.
    ConversationUnread {
        conversation_id: String,
        hlc: Hlc,
        unread: bool,
    },
    /// A tombstone, never a physical-delete signal to blindly replay —
    /// `docs/design/MULTI_DEVICE_SYNC.md`'s "Deletes are tombstones, never physical
    /// row removal": the receiving side records this HLC in
    /// `field_versions` (scope `"conversation_deleted"`) so a
    /// causally-earlier `AppendMessage`/`ConversationTitle`/
    /// `ConversationProject` that arrives afterward is recognized as stale
    /// and doesn't resurrect the conversation via
    /// `crates/app/src/sync_loop.rs`'s placeholder-creation step — see that
    /// file's own doc comment on the resurrection guard.
    ConversationDeleted {
        conversation_id: String,
        hlc: Hlc,
    },
    ProjectRow {
        hlc: Hlc,
        project: Project,
    },
    ProjectDeleted {
        row_key: String,
        hlc: Hlc,
    },
    ArtifactRow {
        hlc: Hlc,
        conversation_id: String,
        artifact_id: String,
        filename: String,
        content: String,
    },
    /// The entire `Settings` blob, already serialized — this module doesn't
    /// depend on `crate::types::Settings` directly so a settings-shape
    /// change never has to touch the wire schema, matching how
    /// `crate::settings::save` already treats it as an opaque whole-blob
    /// write (see `crate::sync::field_versions`'s doc comment).
    SettingsRow {
        hlc: Hlc,
        settings_json: String,
    },
    /// The whole `custom_models` blob, matching `crate::custom::save`'s own
    /// whole-array persistence (it serializes `CustomModels::models:
    /// Vec<ModelSpec>` directly, not the `CustomModels` wrapper struct,
    /// which doesn't itself implement `Serialize`/`Deserialize`).
    CustomModelsRow {
        hlc: Hlc,
        models: Vec<ModelSpec>,
    },
}

/// Encrypts `envelope` under `sync_key`. Output is `nonce || ciphertext ||
/// tag`, ready to hand to `crate::sync::outbox::enqueue`/
/// `crate::sync::mailbox_client::MailboxClient::push`.
pub fn encrypt(sync_key: &SyncKey, envelope: &ChangeEnvelope) -> Result<Vec<u8>> {
    let plaintext = serde_json::to_vec(envelope)?;
    let unbound = UnboundKey::new(&AES_256_GCM, sync_key.as_bytes())
        .map_err(|_| Error::Pairing("couldn't prepare this device's sync key".into()))?;
    let key = LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_bytes);

    let mut in_out = plaintext;
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| Error::Pairing("couldn't encrypt a sync change".into()))?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&in_out);
    in_out.zeroize();
    Ok(out)
}

/// Decrypts a blob produced by [`encrypt`]. `Err` on any failure — wrong
/// key, tampered ciphertext/tag, or malformed JSON — deliberately without
/// distinguishing which (same "never soften a crypto failure" posture
/// `crate::sync::pairing`'s AEAD functions already use): a relay is
/// untrusted by design, so a garbled or foreign envelope must never be
/// partially trusted, only discarded.
pub fn decrypt(sync_key: &SyncKey, sealed: &[u8]) -> Result<ChangeEnvelope> {
    if sealed.len() < NONCE_LEN {
        return Err(Error::Pairing("received a malformed sync change".into()));
    }
    let (nonce_bytes, ciphertext_and_tag) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce_bytes)
        .map_err(|_| Error::Pairing("received a malformed sync change".into()))?;
    let unbound = UnboundKey::new(&AES_256_GCM, sync_key.as_bytes())
        .map_err(|_| Error::Pairing("couldn't prepare this device's sync key".into()))?;
    let key = LessSafeKey::new(unbound);

    let mut buf = ciphertext_and_tag.to_vec();
    let result = key
        .open_in_place(nonce, Aad::empty(), &mut buf)
        .map_err(|_| Error::Pairing("received a sync change that couldn't be decrypted".into()))
        .and_then(|plaintext| {
            serde_json::from_slice::<ChangeEnvelope>(plaintext).map_err(Error::from)
        });
    buf.zeroize();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::hlc::HlcClock;

    fn sample_envelope() -> ChangeEnvelope {
        ChangeEnvelope::ConversationTitle {
            conversation_id: "conv-1".to_string(),
            hlc: HlcClock::new("device-a".to_string()).next(),
            title: "Trip planning".to_string(),
        }
    }

    #[test]
    fn encrypt_decrypt_round_trips() {
        let key = SyncKey::random();
        let envelope = sample_envelope();
        let sealed = encrypt(&key, &envelope).unwrap();
        let opened = decrypt(&key, &sealed).unwrap();
        match opened {
            ChangeEnvelope::ConversationTitle { title, .. } => assert_eq!(title, "Trip planning"),
            other => panic!("wrong variant round-tripped: {other:?}"),
        }
    }

    #[test]
    fn decrypt_rejects_the_wrong_key() {
        let key_a = SyncKey::random();
        let key_b = SyncKey::random();
        let sealed = encrypt(&key_a, &sample_envelope()).unwrap();
        assert!(decrypt(&key_b, &sealed).is_err());
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext() {
        let key = SyncKey::random();
        let mut sealed = encrypt(&key, &sample_envelope()).unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(decrypt(&key, &sealed).is_err());
    }
}
