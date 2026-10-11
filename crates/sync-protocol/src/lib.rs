//! Wire envelope shared between OpenPal desktop/mobile clients and the sync
//! relay (`crates/relay`). Both sides depend on this crate so the envelope
//! shape can never drift out of sync between them.
//!
//! Phase 1 (docs/design/MULTI_DEVICE_SYNC.md): pure plumbing, no cryptography. The
//! relay never sees real user data in this phase — `ciphertext` here is
//! opaque bytes, and Phase 1 callers only ever put synthetic/test data in
//! it. Phase 2 wires real AES-256-GCM sealing under a SyncKey through this
//! same field; this crate does not know or care about that yet.

use serde::{Deserialize, Serialize};

/// Wire format version. Bump when the envelope shape changes; the relay and
/// any client must agree on this before Phase 2+ wires real crypto through it.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub version: u32,
    /// Opaque routing key. In Phase 1 this is caller-supplied and meaningless;
    /// Phase 2 will derive it via HKDF from a real SyncKey — this crate does
    /// not know or care how it's derived.
    pub mailbox_id: String,
    /// Opaque bytes — Phase 1 sends synthetic/test data, never real user data.
    pub ciphertext: Vec<u8>,
    pub created_at_ms: i64,
    /// How long the relay should retain this envelope, in seconds from
    /// `created_at_ms`. `None` means "let the relay pick its default" — see
    /// `docs/SYNC_PAIRING_PROTOCOL.md`'s "Relay hardening" section. Purely a
    /// resource-exhaustion mitigation (bounds relay storage); it carries no
    /// authentication meaning and the relay still never inspects `ciphertext`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_seconds: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_json_round_trips() {
        let envelope = Envelope {
            version: PROTOCOL_VERSION,
            mailbox_id: "test-mailbox-id".to_string(),
            ciphertext: vec![1, 2, 3, 4, 5],
            created_at_ms: 1_724_800_000_000,
            ttl_seconds: Some(600),
        };

        let json = serde_json::to_string(&envelope).expect("serialize");
        let round_tripped: Envelope = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(envelope, round_tripped);
    }

    #[test]
    fn envelope_round_trips_with_empty_ciphertext() {
        let envelope = Envelope {
            version: PROTOCOL_VERSION,
            mailbox_id: String::new(),
            ciphertext: Vec::new(),
            created_at_ms: 0,
            ttl_seconds: None,
        };

        let json = serde_json::to_string(&envelope).expect("serialize");
        let round_tripped: Envelope = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(envelope, round_tripped);
    }

    #[test]
    fn envelope_ttl_seconds_is_omitted_from_json_when_none() {
        let envelope = Envelope {
            version: PROTOCOL_VERSION,
            mailbox_id: "test-mailbox-id".to_string(),
            ciphertext: vec![1, 2, 3],
            created_at_ms: 1_724_800_000_000,
            ttl_seconds: None,
        };

        let json = serde_json::to_string(&envelope).expect("serialize");
        assert!(
            !json.contains("ttl_seconds"),
            "ttl_seconds should be omitted from the wire when None, got: {json}"
        );
    }

    #[test]
    fn envelope_deserializes_without_ttl_seconds_field() {
        // Backward compatibility: anything that doesn't send `ttl_seconds`
        // at all (e.g. an older client, or Phase 1 test payloads) must still
        // deserialize, with `ttl_seconds` defaulting to `None`.
        let json = serde_json::json!({
            "version": PROTOCOL_VERSION,
            "mailbox_id": "test-mailbox-id",
            "ciphertext": [1, 2, 3, 4, 5],
            "created_at_ms": 1_724_800_000_000i64,
        })
        .to_string();

        let envelope: Envelope = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(envelope.ttl_seconds, None);
    }
}
