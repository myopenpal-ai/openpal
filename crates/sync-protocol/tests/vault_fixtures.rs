//! Fixture gate for the vault wire.
//!
//! Every request and response in `fixtures/vault/` must parse into its type
//! and serialize back to the same JSON. A fixture no test references fails
//! [`every_fixture_file_is_referenced`], so adding a type or changing a field
//! without a fixture is a test failure rather than a runtime surprise.

use openpal_sync_protocol::vault::{
    CreateVaultRequest, CreateVaultResponse, DeleteItemRequest, DeleteItemResponse, ItemConflict,
    KeyringConflict, PullItemsRequest, PullItemsResponse, PutItemRequest, PutItemResponse,
    PutKeyringRequest, PutKeyringResponse, VaultErrorEnvelope, VaultKeyring, VaultUsage,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};

type RoundTrip = fn(&str) -> Result<(), String>;

/// Every fixture, paired with the type it pins. Keep this list and the
/// directory in step; the two tests below enforce both directions.
const FIXTURES: &[(&str, RoundTrip)] = &[
    ("create-vault-request.json", roundtrip::<CreateVaultRequest>),
    (
        "create-vault-response.json",
        roundtrip::<CreateVaultResponse>,
    ),
    ("keyring-get-response.json", roundtrip::<VaultKeyring>),
    ("put-keyring-request.json", roundtrip::<PutKeyringRequest>),
    ("put-keyring-response.json", roundtrip::<PutKeyringResponse>),
    ("keyring-conflict.json", roundtrip::<KeyringConflict>),
    ("put-item-create-request.json", roundtrip::<PutItemRequest>),
    ("put-item-update-request.json", roundtrip::<PutItemRequest>),
    ("put-item-response.json", roundtrip::<PutItemResponse>),
    ("item-conflict.json", roundtrip::<ItemConflict>),
    ("delete-item-request.json", roundtrip::<DeleteItemRequest>),
    ("delete-item-response.json", roundtrip::<DeleteItemResponse>),
    ("pull-items-request.json", roundtrip::<PullItemsRequest>),
    ("pull-items-response.json", roundtrip::<PullItemsResponse>),
    (
        "pull-items-last-page-response.json",
        roundtrip::<PullItemsResponse>,
    ),
    ("vault-usage.json", roundtrip::<VaultUsage>),
    ("vault-error.json", roundtrip::<VaultErrorEnvelope>),
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("vault")
}

fn read_fixture(name: &str) -> String {
    let path = fixture_dir().join(name);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()))
}

fn load<T: DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(&read_fixture(name))
        .unwrap_or_else(|err| panic!("{name} does not parse into its type: {err}"))
}

fn roundtrip<T>(raw: &str) -> Result<(), String>
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let before: Value =
        serde_json::from_str(raw).map_err(|err| format!("not valid JSON: {err}"))?;
    let parsed: T = serde_json::from_value(before.clone())
        .map_err(|err| format!("does not parse into its type: {err}"))?;
    let after =
        serde_json::to_value(&parsed).map_err(|err| format!("does not serialize: {err}"))?;
    if after != before {
        return Err(format!(
            "round trip changed the JSON\n  before: {before}\n  after:  {after}"
        ));
    }
    Ok(())
}

fn assert_camel_case(fixture: &str, value: &Value, path: &str) {
    match value {
        Value::Object(map) => {
            // `kdf` and `details` are opaque JSON owned by another layer, so
            // their inner keys are deliberately not constrained here.
            if path.ends_with(".kdf") || path.ends_with(".details") {
                return;
            }
            for (key, child) in map {
                assert!(
                    !key.contains('_'),
                    "{fixture}: field {path}.{key} is snake_case, not camelCase"
                );
                assert_camel_case(fixture, child, &format!("{path}.{key}"));
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                assert_camel_case(fixture, child, &format!("{path}[{index}]"));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[test]
fn every_fixture_round_trips() {
    for (name, check) in FIXTURES {
        check(&read_fixture(name)).unwrap_or_else(|err| panic!("{name}: {err}"));
    }
}

#[test]
fn every_fixture_file_is_referenced() {
    let referenced: HashSet<&str> = FIXTURES.iter().map(|(name, _)| *name).collect();

    let mut on_disk = HashSet::new();
    for entry in fs::read_dir(fixture_dir()).expect("read fixtures/vault") {
        let entry = entry.expect("read a fixtures/vault entry");
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            on_disk.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }

    let mut unreferenced: Vec<&String> = on_disk
        .iter()
        .filter(|name| !referenced.contains(name.as_str()))
        .collect();
    unreferenced.sort();
    assert!(
        unreferenced.is_empty(),
        "fixtures/vault has JSON files no test references: {unreferenced:?}"
    );

    for name in referenced {
        assert!(
            on_disk.contains(name),
            "{name} is referenced by the test but missing from fixtures/vault"
        );
    }
}

#[test]
fn every_fixture_is_camel_case() {
    for (name, _) in FIXTURES {
        let value: Value =
            serde_json::from_str(&read_fixture(name)).expect("fixture is valid JSON");
        assert_camel_case(name, &value, "");
    }
}

#[test]
fn base_version_zero_means_create() {
    let create: PutItemRequest = load("put-item-create-request.json");
    assert_eq!(create.base_version, 0);
    assert!(
        create.is_create(),
        "baseVersion 0 must mean create, not update"
    );

    let update: PutItemRequest = load("put-item-update-request.json");
    assert_eq!(update.base_version, 2);
    assert!(!update.is_create());
}

#[test]
fn a_tombstone_is_deleted_with_empty_ciphertext() {
    let page: PullItemsResponse = load("pull-items-response.json");

    let tombstone = page
        .items
        .iter()
        .find(|item| item.deleted)
        .expect("the pull fixture carries a tombstone");
    assert!(
        tombstone.ciphertext.is_empty(),
        "a tombstone must not carry ciphertext"
    );
    assert!(
        tombstone.nonce.is_empty(),
        "a tombstone must not carry a nonce"
    );

    let live = page
        .items
        .iter()
        .find(|item| !item.deleted)
        .expect("the pull fixture carries a live item");
    assert!(!live.ciphertext.is_empty());
}

#[test]
fn the_cursor_is_carried_as_an_uninterpreted_string() {
    let page: PullItemsResponse = load("pull-items-response.json");
    let cursor = page
        .next_cursor
        .as_deref()
        .expect("the first page carries a cursor");
    assert_eq!(cursor, "opaque-cursor/3:not-json");

    let request: PullItemsRequest = load("pull-items-request.json");
    assert_eq!(request.cursor.as_deref(), Some(cursor));
    assert_eq!(
        request.query_pairs(),
        vec![("cursor", cursor.to_string()), ("limit", "100".to_string())],
        "the cursor must reach the query string byte for byte"
    );
}

#[test]
fn the_last_page_has_no_cursor() {
    let page: PullItemsResponse = load("pull-items-last-page-response.json");
    assert!(page.items.is_empty());
    assert!(!page.has_more);
    assert_eq!(page.next_cursor, None);
}
