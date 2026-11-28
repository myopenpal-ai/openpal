//! Wire types for the durable vault.
//!
//! The vault is the account-scoped, end-to-end encrypted store for
//! conversations, projects, settings and inline artifacts. The server keeps
//! opaque ciphertext plus the minimum headers needed to sync; these types
//! describe that exchange, not the cryptography inside it.
//!
//! # Additive-only
//!
//! The vault wire is additive-only:
//!
//! - a new optional field may be added at any time; readers ignore fields they
//!   do not know and writers omit optional fields they do not use;
//! - an existing field is never renamed, removed or given a new meaning;
//! - a new [`VaultItemKind`] or a new [`error_codes`] value is a wire change
//!   that needs a fixture and a client parser test before a server emits it;
//! - [`crate::VAULT_PROTOCOL_VERSION`] changes only for a breaking change,
//!   which means a new route or a shape old readers cannot parse. Additive
//!   fields and new error codes do not bump it.
//!
//! Every request and response has a fixture under `fixtures/vault/`, and
//! `tests/vault_fixtures.rs` round-trips each one and fails if a fixture is
//! not referenced. A wire change therefore fails a test instead of surprising
//! a client at runtime.
//!
//! See `CHANGELOG.md` next to this crate.
//!
//! # Casing and encoding
//!
//! JSON field names are camelCase. Enum values and error codes are
//! snake_case. `nonce`, `ciphertext` and `wrappedVaultKey` are standard base64
//! on the wire; this crate converts them to and from bytes so callers never
//! hand-encode them. `kdf` is an opaque JSON object this crate neither
//! interprets nor validates beyond being an object.

use serde::{Deserialize, Serialize};

/// Known keyring rotation reasons. The field is a plain string so a reason
/// added later still parses on an older client; these constants are the
/// values this version knows.
pub mod keyring_reasons {
    /// The first keyring for a vault.
    pub const INITIAL: &str = "initial";
    /// A device was added and contributed a wrapper.
    pub const DEVICE_ADDED: &str = "device_added";
    /// A device was removed and the vault key generation changed.
    pub const DEVICE_REMOVED: &str = "device_removed";
    /// The recovery code was rotated; the same vault key was re-wrapped.
    pub const CODE_ROTATED: &str = "code_rotated";
    /// A keyring was re-wrapped while restoring from a recovery code.
    pub const RECOVERY_USED: &str = "recovery_used";
}

/// Known error codes. `code` is a plain string rather than an enum so an
/// unknown code from a newer server survives parsing verbatim — a client can
/// log and display it instead of collapsing every future code into one
/// `Unknown` bucket. These constants are the values this version knows.
pub mod error_codes {
    /// The write would exceed the account's vault quota.
    pub const SYNC_QUOTA_EXCEEDED: &str = "sync_quota_exceeded";
    /// The pull cursor was malformed, truncated or issued for another vault.
    pub const INVALID_CURSOR: &str = "invalid_cursor";
    /// The request body or one item exceeded the protocol limit.
    pub const PAYLOAD_TOO_LARGE: &str = "payload_too_large";
    /// The vault does not exist for this account.
    pub const VAULT_NOT_FOUND: &str = "vault_not_found";
    /// The caller is over the per-account request rate.
    pub const RATE_LIMITED: &str = "rate_limited";
    /// The bearer token is missing, expired or invalid.
    pub const INVALID_TOKEN: &str = "invalid_token";
    /// An unexpected server failure.
    pub const INTERNAL_ERROR: &str = "internal_error";
}

/// A keyring descriptor object. This crate treats it as opaque: the client's
/// vault implementation interprets the fields, and the server validates only
/// that it is a JSON object of bounded size.
pub type OpaqueJsonObject = serde_json::Map<String, serde_json::Value>;

/// Request body for an idempotent vault create.
///
/// This version sends no fields — the vault identifier comes from the
/// authenticated account. The body exists so a later version can add an
/// optional field without inventing a new route.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVaultRequest {}

/// Response to an idempotent vault create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVaultResponse {
    /// Stable vault identifier for this account.
    pub vault_id: String,
    /// `true` when this call created the vault, `false` when it already
    /// existed.
    pub created: bool,
}

/// The current keyring for a vault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultKeyring {
    /// Monotonic generation of the keyring record.
    pub keyring_version: u32,
    /// Why this generation exists; one of [`keyring_reasons`].
    pub reason: String,
    /// Server time when this generation was written, in epoch milliseconds.
    pub rotated_at_ms: i64,
    /// Opaque key-derivation descriptor.
    pub kdf: OpaqueJsonObject,
    /// Opaque wrapped vault key.
    #[serde(with = "base64_bytes")]
    pub wrapped_vault_key: Vec<u8>,
}

/// Conditional keyring write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutKeyringRequest {
    /// The keyring version this write is based on. `0` creates the first one.
    pub base_keyring_version: u32,
    /// Why this generation exists; one of [`keyring_reasons`].
    pub reason: String,
    /// Opaque key-derivation descriptor.
    pub kdf: OpaqueJsonObject,
    /// Opaque wrapped vault key.
    #[serde(with = "base64_bytes")]
    pub wrapped_vault_key: Vec<u8>,
}

/// Result of a successful keyring write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutKeyringResponse {
    /// The new keyring generation.
    pub keyring_version: u32,
    /// Why this generation exists; one of [`keyring_reasons`].
    pub reason: String,
    /// Server time when this generation was written, in epoch milliseconds.
    pub rotated_at_ms: i64,
}

/// Body of a `409` when the keyring moved under the writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyringConflict {
    /// The keyring generation the server currently holds.
    pub current_keyring_version: u32,
}

/// The closed set of item kinds in this protocol version.
///
/// Adding a kind is a wire change: it needs a fixture, a parser test in the
/// client, and a server that does not emit it until both have landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VaultItemKind {
    /// One conversation.
    Conversation,
    /// A project or folder grouping.
    Project,
    /// Preferences and local UI state.
    Settings,
    /// A generated file, inlined in the item.
    Artifact,
    /// A saved model identity and its parameters.
    ModelRef,
}

/// One item as returned by a pull.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultItem {
    /// Client-generated stable object identifier.
    pub object_id: String,
    /// Per-vault monotonic sequence number assigned by the server.
    pub seq: u64,
    /// Compare-and-swap counter for this object.
    pub version: u32,
    /// Coarse item kind.
    pub kind: VaultItemKind,
    /// Client-declared creation time, in epoch milliseconds.
    pub created_at_ms: i64,
    /// Client-declared update time, in epoch milliseconds.
    pub updated_at_ms: i64,
    /// `true` for a tombstone.
    pub deleted: bool,
    /// GCM nonce; empty on a tombstone.
    #[serde(with = "base64_bytes")]
    pub nonce: Vec<u8>,
    /// Opaque ciphertext; empty on a tombstone.
    #[serde(with = "base64_bytes")]
    pub ciphertext: Vec<u8>,
}

/// Conditional item write. `baseVersion = 0` creates the object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutItemRequest {
    /// The version this write is based on. `0` creates the object.
    pub base_version: u32,
    /// Coarse item kind.
    pub kind: VaultItemKind,
    /// Client-declared creation time, in epoch milliseconds.
    pub created_at_ms: i64,
    /// Client-declared update time, in epoch milliseconds.
    pub updated_at_ms: i64,
    /// `true` writes a tombstone; prefer the delete route for deletions.
    pub deleted: bool,
    /// GCM nonce; empty on a tombstone.
    #[serde(with = "base64_bytes")]
    pub nonce: Vec<u8>,
    /// Opaque ciphertext; empty on a tombstone.
    #[serde(with = "base64_bytes")]
    pub ciphertext: Vec<u8>,
}

impl PutItemRequest {
    /// Whether this write creates a new object rather than updating one.
    #[must_use]
    pub fn is_create(&self) -> bool {
        self.base_version == 0
    }
}

/// Result of a successful item write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutItemResponse {
    /// The new compare-and-swap version.
    pub version: u32,
    /// The server sequence number assigned to this write.
    pub seq: u64,
    /// Ciphertext length in bytes, as counted against the quota.
    pub bytes: u64,
}

/// Body of a `409` when an item moved under the writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemConflict {
    /// The version the server currently holds.
    pub current_version: u32,
    /// The sequence number the server currently holds.
    pub seq: u64,
}

/// Conditional item delete. The server writes a tombstone, never a physical
/// row removal, so every device can observe the deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteItemRequest {
    /// The version this delete is based on.
    pub base_version: u32,
}

/// Result of a successful item delete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteItemResponse {
    /// The version of the tombstone the server wrote.
    pub version: u32,
    /// The server sequence number assigned to the tombstone.
    pub seq: u64,
}

/// Query for an item pull.
///
/// On the wire these are URL query parameters, not a JSON body; the type is
/// also `Serialize`/`Deserialize` so the same shape can be exercised as a
/// fixture. The cursor is opaque: carry it unchanged, never parse or build
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullItemsRequest {
    /// Opaque cursor returned by the previous page. Omit for a full restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Requested page size; the server may clamp it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl PullItemsRequest {
    /// Query parameters in wire order, with the cursor carried verbatim.
    #[must_use]
    pub fn query_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = Vec::new();
        if let Some(cursor) = &self.cursor {
            pairs.push(("cursor", cursor.clone()));
        }
        if let Some(limit) = self.limit {
            pairs.push(("limit", limit.to_string()));
        }
        pairs
    }
}

/// One page of items. The cursor is opaque and must be sent back unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullItemsResponse {
    /// Items in server sequence order, including tombstones.
    pub items: Vec<VaultItem>,
    /// Cursor for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Whether another page exists.
    pub has_more: bool,
}

/// Account usage against the vault quota.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultUsage {
    /// Ciphertext bytes currently stored.
    pub bytes_used: u64,
    /// Ciphertext byte limit for this account.
    pub bytes_limit: u64,
    /// Item count currently stored.
    pub objects_used: u64,
    /// Item count limit for this account.
    pub objects_limit: u64,
}

/// Stable error envelope shared by every vault route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultErrorEnvelope {
    /// The error body.
    pub error: VaultErrorBody,
}

/// The error body. `code` is machine-readable; `message` is server-provided
/// and is not a stable API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultErrorBody {
    /// Machine-readable snake_case code; see [`error_codes`].
    pub code: String,
    /// Server-provided, user-safe sentence. Do not match on it.
    pub message: String,
    /// Request identifier for support.
    pub request_id: String,
    /// Optional machine-readable details.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Standard-base64 serde for `Vec<u8>` fields.
pub mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{Deserialize, Deserializer, Serializer};

    /// Encode bytes as a standard-base64 JSON string.
    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    /// Decode a standard-base64 JSON string into bytes.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD
            .decode(encoded.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_item() -> VaultItem {
        VaultItem {
            object_id: "object-1".to_string(),
            seq: 1,
            version: 1,
            kind: VaultItemKind::Conversation,
            created_at_ms: 1,
            updated_at_ms: 2,
            deleted: false,
            nonce: vec![0; 12],
            ciphertext: vec![1, 2, 3],
        }
    }

    #[test]
    fn base64_fields_use_strings_on_the_wire() {
        let value = serde_json::to_value(sample_item()).expect("serialize");
        assert_eq!(value["nonce"], json!("AAAAAAAAAAAAAAAA"));
        assert_eq!(value["ciphertext"], json!("AQID"));
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let value = json!({
            "objectId": "object-1",
            "seq": 1,
            "version": 1,
            "kind": "conversation",
            "createdAtMs": 1,
            "updatedAtMs": 2,
            "deleted": false,
            "nonce": "not base64",
            "ciphertext": "AQID"
        });
        assert!(serde_json::from_value::<VaultItem>(value).is_err());
    }

    #[test]
    fn unknown_keyring_reason_parses() {
        let value = json!({
            "keyringVersion": 2,
            "reason": "a_reason_from_the_future",
            "rotatedAtMs": 1,
            "kdf": { "alg": "opaque" },
            "wrappedVaultKey": "AQID"
        });
        let keyring: VaultKeyring = serde_json::from_value(value).expect("parse");
        assert_eq!(keyring.reason, "a_reason_from_the_future");
    }

    #[test]
    fn unknown_error_code_parses_verbatim() {
        let value = json!({
            "error": {
                "code": "an_error_from_the_future",
                "message": "Something changed.",
                "requestId": "request-1"
            }
        });
        let envelope: VaultErrorEnvelope = serde_json::from_value(value).expect("parse");
        assert_eq!(envelope.error.code, "an_error_from_the_future");
        assert_eq!(envelope.error.details, None);
    }

    #[test]
    fn kdf_is_an_opaque_object_that_round_trips() {
        let kdf = json!({
            "alg": "argon2id",
            "params": { "mCostKib": 65536, "tCost": 3 },
            "labels": ["vault", "test"]
        });
        let value = json!({
            "keyringVersion": 1,
            "reason": "initial",
            "rotatedAtMs": 1,
            "kdf": kdf,
            "wrappedVaultKey": "AQID"
        });
        let keyring: VaultKeyring = serde_json::from_value(value.clone()).expect("parse");
        assert_eq!(
            serde_json::to_value(keyring).expect("serialize"),
            value,
            "kdf must be carried through without interpretation"
        );
    }

    #[test]
    fn unknown_additive_fields_are_ignored() {
        let value = json!({
            "baseVersion": 0,
            "kind": "conversation",
            "createdAtMs": 1,
            "updatedAtMs": 2,
            "deleted": false,
            "nonce": "",
            "ciphertext": "",
            "aFieldFromTheFuture": { "nested": true }
        });
        let request: PutItemRequest = serde_json::from_value(value).expect("parse");
        assert!(request.is_create());
        assert!(request.ciphertext.is_empty());
    }

    #[test]
    fn base_version_zero_means_create() {
        let mut request = PutItemRequest {
            base_version: 0,
            kind: VaultItemKind::Settings,
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted: false,
            nonce: Vec::new(),
            ciphertext: Vec::new(),
        };
        assert!(request.is_create());

        request.base_version = 1;
        assert!(!request.is_create());
    }

    #[test]
    fn tombstone_serializes_with_empty_ciphertext() {
        let tombstone = VaultItem {
            deleted: true,
            nonce: Vec::new(),
            ciphertext: Vec::new(),
            ..sample_item()
        };
        let value = serde_json::to_value(tombstone).expect("serialize");
        assert_eq!(value["deleted"], json!(true));
        assert_eq!(value["nonce"], json!(""));
        assert_eq!(value["ciphertext"], json!(""));
    }

    #[test]
    fn query_pairs_carry_the_cursor_verbatim() {
        let query = PullItemsRequest {
            cursor: Some("v1.not-a-real-cursor".to_string()),
            limit: None,
        };
        assert_eq!(
            query.query_pairs(),
            vec![("cursor", "v1.not-a-real-cursor".to_string())]
        );

        let query = PullItemsRequest {
            cursor: None,
            limit: Some(100),
        };
        assert_eq!(query.query_pairs(), vec![("limit", "100".to_string())]);
    }

    #[test]
    fn absent_optionals_are_omitted_and_parse_as_none() {
        let page = PullItemsResponse {
            items: Vec::new(),
            next_cursor: None,
            has_more: false,
        };
        let value = serde_json::to_value(&page).expect("serialize");
        assert!(value.get("nextCursor").is_none());
        let parsed: PullItemsResponse = serde_json::from_value(value).expect("parse");
        assert_eq!(parsed.next_cursor, None);

        let query: PullItemsRequest = serde_json::from_value(json!({})).expect("parse empty query");
        assert_eq!(query, PullItemsRequest::default());
    }
}
