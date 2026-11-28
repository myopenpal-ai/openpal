//! Replays the vendored cloud `/v1` contract fixtures through the shipped
//! parsers, against a local stub that also asserts what this app sends.
//!
//! The fixtures live in `crates/core/tests/contract/` — a byte-for-byte copy
//! of the private backend repo's versioned contract set (`2026-11-25`, see
//! that directory's `README.md` and `PIN.toml`). Keeping the copy in-tree is
//! what lets this run with no credential, network call, or repository secret,
//! including from a fork's `pull_request` run.
//!
//! The suite is driven by the backend's own `MANIFEST.toml`: every entry is
//! replayed or named in [`SKIPPED`] with a reason, so a newly vendored fixture
//! can't sit unexercised. `MANIFEST.toml` and `v1/` must also agree file for
//! file, and `PIN.toml`'s digest is recomputed from the bytes on disk.
//!
//! Two things this proves that the hand-written unit tests could not:
//! - a recorded response from a real backend parses with *these* types (the
//!   snake_case poll tokens, a `quota`-less account, the bare cloud-model
//!   array);
//! - the request this app actually sends matches the contract's shape (the
//!   refresh body, the bearer headers, the full llama.cpp sampling set with
//!   its `-1` sentinels), not just the response it knows how to read.

use crate::account::{self, Quota, SessionPoll};
use crate::cloud::{self, SamplingParamKey};
use crate::sse::STREAM_ERROR_MESSAGE;
use crate::types::{new_message_id, ChatEvent, ChatMessage, SamplingParams};
use crate::Error;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CONTRACT_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract");

/// Fixtures the manifest lists but no shipped client calls yet. Every entry
/// needs a reason; an unexercised, unlisted fixture fails
/// [`every_manifested_fixture_is_replayed_or_skipped`].
const SKIPPED: &[(&str, &str)] = &[(
    "v1/search_results.json",
    "no shipped client calls GET /v1/managed-search/search (issue #138, Not in scope)",
)];

// ---------------------------------------------------------------------------
// Manifest, pin and fixture loading
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Manifest {
    contract: ContractMeta,
    fixture: Vec<Entry>,
}

#[derive(Debug, Deserialize)]
struct ContractMeta {
    version: String,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct Entry {
    path: String,
    method: String,
    route: String,
    status: u16,
    content_type: String,
    parse_as: String,
    #[serde(default)]
    placeholders: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Pin {
    contract: PinMeta,
}

#[derive(Debug, Deserialize)]
struct PinMeta {
    version: String,
    digest: String,
}

fn contract_dir() -> PathBuf {
    PathBuf::from(CONTRACT_DIR)
}

fn manifest() -> Manifest {
    let raw = std::fs::read_to_string(contract_dir().join("MANIFEST.toml"))
        .expect("read the vendored MANIFEST.toml");
    toml::from_str(&raw).expect("parse the vendored MANIFEST.toml")
}

fn pin() -> Pin {
    let raw = std::fs::read_to_string(contract_dir().join("PIN.toml"))
        .expect("read the vendored PIN.toml");
    toml::from_str(&raw).expect("parse the vendored PIN.toml")
}

fn fixture(path: &str) -> String {
    std::fs::read_to_string(contract_dir().join(path))
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

/// SHA-256 over every `v1/` file in byte-sorted path order, each hashed as
/// `path + NUL + bytes + NUL` — the same recipe documented in `PIN.toml`.
fn fixture_digest() -> String {
    let root = contract_dir().join("v1");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("read v1/")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    let mut hasher = Sha256::new();
    for file in files {
        let name = file
            .file_name()
            .expect("fixture file name")
            .to_string_lossy();
        hasher.update(format!("v1/{name}").as_bytes());
        hasher.update([0]);
        hasher.update(std::fs::read(&file).expect("read fixture bytes"));
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

fn contract_file(name: &str) -> PathBuf {
    contract_dir().join(name)
}

// ---------------------------------------------------------------------------
// Inventory gates
// ---------------------------------------------------------------------------

/// The vendored set is pinned: its recorded version is the manifest's own, and
/// its digest still matches the bytes on disk. Editing a fixture by hand (or
/// half-revendoring a set) fails here rather than silently changing what the
/// replay proves.
#[test]
fn contract_pin_matches_the_manifest_and_fixture_bytes() {
    let manifest = manifest();
    let pin = pin();

    assert_eq!(pin.contract.version, manifest.contract.version);
    assert_eq!(manifest.contract.base_url, "https://api.openpal.app");
    assert_eq!(
        pin.contract.digest,
        format!("sha256:{}", fixture_digest()),
        "fixtures changed without re-vendoring and updating PIN.toml",
    );

    let changelog = std::fs::read_to_string(contract_file("CHANGELOG.md")).expect("read changelog");
    assert!(
        changelog.contains(&manifest.contract.version),
        "CHANGELOG.md does not mention the pinned version {}",
        manifest.contract.version,
    );
}

/// Every manifest entry has its file, and every file has an entry — so a new
/// fixture cannot be added to the v1/ directory without also describing it.
#[test]
fn manifest_and_disk_agree() {
    let manifest = manifest();
    assert!(!manifest.fixture.is_empty());

    let mut manifested = BTreeSet::new();
    for entry in &manifest.fixture {
        let full = contract_dir().join(&entry.path);
        assert!(
            full.is_file(),
            "manifest names a missing fixture: {}",
            entry.path
        );
        assert!(
            entry.route.starts_with('/'),
            "{}: route must start with /",
            entry.path
        );
        assert!(
            entry.status >= 200,
            "{}: status {}",
            entry.path,
            entry.status
        );
        assert!(
            matches!(
                entry.content_type.as_str(),
                "application/json" | "text/event-stream"
            ),
            "{}: unexpected content type {}",
            entry.path,
            entry.content_type,
        );

        let raw = fixture(&entry.path);
        for placeholder in &entry.placeholders {
            assert!(
                raw.contains(placeholder),
                "{}: manifest names placeholder `{placeholder}` but the fixture does not contain it",
                entry.path,
            );
        }

        assert!(
            manifested.insert(entry.path.clone()),
            "{} is listed more than once in MANIFEST.toml",
            entry.path,
        );
    }

    let mut on_disk: Vec<String> = std::fs::read_dir(contract_dir().join("v1"))
        .expect("read v1/")
        .filter_map(|entry| {
            let entry = entry.expect("dir entry");
            entry
                .path()
                .is_file()
                .then(|| format!("v1/{}", entry.file_name().to_string_lossy()))
        })
        .collect();
    on_disk.sort();

    let manifested: Vec<String> = manifested.into_iter().collect();
    assert_eq!(on_disk, manifested, "v1/ and MANIFEST.toml drifted");
}

/// The coverage gate: a manifest entry is either replayed here or listed in
/// [`SKIPPED`] with a reason. There is no third state.
#[tokio::test]
async fn every_manifested_fixture_is_replayed_or_skipped() {
    for entry in manifest().fixture {
        if let Some((_, reason)) = SKIPPED.iter().find(|(path, _)| *path == entry.path) {
            assert!(!reason.is_empty(), "{}: a skip needs a reason", entry.path);
            // Still prove the vendored bytes are well formed, even where no
            // client reads them yet.
            if entry.parse_as == "json" {
                let raw = fixture(&entry.path);
                serde_json::from_str::<Value>(&raw)
                    .unwrap_or_else(|e| panic!("{}: invalid JSON: {e}", entry.path));
            }
            continue;
        }

        replay(&entry)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", entry.path));
    }
}

async fn replay(entry: &Entry) -> Result<(), String> {
    match entry.parse_as.as_str() {
        "session_created" => replay_session_created(entry).await,
        "session_poll" => replay_session_poll(entry).await,
        "refresh_request" | "refresh_response" => replay_refresh(entry).await,
        "account" => replay_account(entry).await,
        "cloud_models" => replay_cloud_models(entry).await,
        "sse" => replay_sse(entry).await,
        "error" => replay_error(entry).await,
        other => Err(format!("manifest names unknown parse_as `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// The stub: one route per replay, with request capture
// ---------------------------------------------------------------------------

struct Stub {
    server: MockServer,
    /// Points `OPENPAL_ACCOUNT_API` here, while holding the process-wide lock
    /// that keeps another test from repointing it mid-request.
    _base: account::test_support::TestApiBase,
}

impl Stub {
    async fn serving(verb: &str, route: &str, status: u16, content_type: &str, body: &str) -> Self {
        let server = MockServer::start().await;
        Mock::given(method(verb))
            .and(path(route.to_string()))
            .respond_with(
                ResponseTemplate::new(status).set_body_raw(body.to_string(), content_type),
            )
            .mount(&server)
            .await;
        let base = account::test_support::TestApiBase::point_at(&server.uri()).await;
        Self {
            server,
            _base: base,
        }
    }

    async fn only_request(&self) -> wiremock::Request {
        let mut requests = self
            .server
            .received_requests()
            .await
            .expect("the stub records requests");
        assert_eq!(
            requests.len(),
            1,
            "expected exactly one request, got {}",
            requests.len(),
        );
        requests.remove(0)
    }
}

fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

fn bearer(request: &wiremock::Request) -> Option<&str> {
    request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
}

fn request_json(request: &wiremock::Request) -> Value {
    serde_json::from_slice(&request.body)
        .unwrap_or_else(|e| panic!("request body was not JSON: {e}; {:?}", request.body))
}

// ---------------------------------------------------------------------------
// POST /v1/desktop/session
// ---------------------------------------------------------------------------

async fn replay_session_created(entry: &Entry) -> Result<(), String> {
    let stub = Stub::serving(
        &entry.method,
        &entry.route,
        entry.status,
        &entry.content_type,
        &fixture(&entry.path),
    )
    .await;

    let session = account::start_device_session(&http_client())
        .await
        .map_err(|e| format!("start_device_session failed: {e}"))?;
    assert_eq!(session.session_id, "01J00000000000000000000000");
    assert_eq!(
        session.verification_url,
        "https://openpal.app/link?code=ABCD-1234"
    );
    assert_eq!(session.expires_in, 600);

    let request = stub.only_request().await;
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/v1/desktop/session");
    assert!(
        request.body.is_empty(),
        "starting a session sends no body, got {:?}",
        request.body,
    );
    assert_eq!(bearer(&request), None, "starting a session needs no token");
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /v1/desktop/session/{id}
// ---------------------------------------------------------------------------

async fn replay_session_poll(entry: &Entry) -> Result<(), String> {
    const SESSION_ID: &str = "01J00000000000000000000000";
    let route = format!("/v1/desktop/session/{SESSION_ID}");
    let stub = Stub::serving(
        &entry.method,
        &route,
        entry.status,
        &entry.content_type,
        &fixture(&entry.path),
    )
    .await;

    let poll = account::poll_device_session(&http_client(), SESSION_ID)
        .await
        .map_err(|e| format!("poll_device_session failed: {e}"))?;

    match (entry.path.as_str(), poll) {
        ("v1/session_poll_pending.json", SessionPoll::Pending) => {}
        ("v1/session_poll_expired.json", SessionPoll::Expired) => {}
        ("v1/session_poll_denied.json", SessionPoll::Denied) => {}
        (
            "v1/session_poll_complete.json",
            SessionPoll::Complete {
                access_token,
                refresh_token,
                account,
            },
        ) => {
            // The one exchange whose tokens are snake_case on the wire.
            assert_eq!(access_token, "at_placeholder");
            assert_eq!(refresh_token, "rt_placeholder");
            assert_eq!(account.email, "person@example.com");
            assert_eq!(account.tier, "Plus");
            assert_eq!(
                account.capabilities,
                vec!["cloud-models", "managed-web-search", "sync-managed"],
            );
            assert_eq!(
                account.quota,
                Some(Quota {
                    used: 37,
                    limit: 500,
                }),
            );
        }
        (_path, other) => return Err(format!("unexpected poll variant {other:?}")),
    }

    let request = stub.only_request().await;
    assert_eq!(request.method.as_str(), "GET");
    assert_eq!(request.url.path(), route);
    assert_eq!(bearer(&request), None, "polling needs no token");
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /v1/auth/refresh
// ---------------------------------------------------------------------------

async fn replay_refresh(entry: &Entry) -> Result<(), String> {
    let response = fixture("v1/auth_refresh_response.json");
    let stub = Stub::serving(
        "POST",
        "/v1/auth/refresh",
        200,
        "application/json",
        &response,
    )
    .await;

    let refreshed = account::refresh_access_token(&http_client(), "rt_test")
        .await
        .map_err(|e| format!("refresh_access_token failed: {e}"))?;
    assert_eq!(refreshed.access_token, "at_placeholder");

    let request = stub.only_request().await;
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/v1/auth/refresh");
    assert_eq!(bearer(&request), None, "refresh authenticates by body");

    // The request fixture is the contract; only its placeholder value differs.
    let mut expected: Value =
        serde_json::from_str(&fixture("v1/auth_refresh_request.json")).expect("request fixture");
    expected["refreshToken"] = Value::String("rt_test".to_string());
    assert_eq!(
        request_json(&request),
        expected,
        "{}: refresh request shape drifted",
        entry.path,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /v1/account/me
// ---------------------------------------------------------------------------

async fn replay_account(entry: &Entry) -> Result<(), String> {
    let raw = fixture(&entry.path);
    let stub = Stub::serving(
        &entry.method,
        &entry.route,
        entry.status,
        &entry.content_type,
        &raw,
    )
    .await;

    let account = account::fetch_account(&http_client(), "at_test_token")
        .await
        .map_err(|e| format!("fetch_account failed: {e}"))?;
    assert_eq!(account.email, "person@example.com");

    match entry.path.as_str() {
        "v1/account_me_plus.json" => {
            assert_eq!(account.tier, "Plus");
            assert_eq!(
                account.capabilities,
                vec!["cloud-models", "managed-web-search", "sync-managed"],
            );
            assert_eq!(
                account.quota,
                Some(Quota {
                    used: 37,
                    limit: 500,
                }),
            );
        }
        "v1/account_me_free.json" => {
            assert_eq!(account.tier, "Free");
            assert!(account.capabilities.is_empty());
            assert_eq!(account.quota, None);
            assert!(
                !raw.contains("\"quota\""),
                "a plan without a quota must omit the key, not send 0/0",
            );
        }
        other => return Err(format!("unexpected account fixture {other}")),
    }

    let request = stub.only_request().await;
    assert_eq!(request.method.as_str(), "GET");
    assert_eq!(request.url.path(), "/v1/account/me");
    assert_eq!(bearer(&request), Some("Bearer at_test_token"));
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /v1/cloud-models
// ---------------------------------------------------------------------------

async fn replay_cloud_models(entry: &Entry) -> Result<(), String> {
    let stub = Stub::serving(
        &entry.method,
        &entry.route,
        entry.status,
        &entry.content_type,
        &fixture(&entry.path),
    )
    .await;

    let models = cloud::list_models(&http_client())
        .await
        .map_err(|e| format!("list_models failed: {e}"))?;
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "openpal/fast-cloud");
    assert_eq!(models[0].required_tier, "Plus");
    assert_eq!(models[1].id, "openpal/pro-cloud");
    assert_eq!(models[1].required_tier, "Pro");
    for model in &models {
        // Every key in the fixture maps to a known SamplingParamKey; an
        // unknown one would have failed to deserialize above. Assert the full
        // launch set so a dropped field is caught here too.
        assert_eq!(model.supported_params.len(), 10, "{}", model.id);
        for key in [
            SamplingParamKey::Temperature,
            SamplingParamKey::TopP,
            SamplingParamKey::TopK,
            SamplingParamKey::MinP,
            SamplingParamKey::RepeatPenalty,
            SamplingParamKey::RepeatLastN,
            SamplingParamKey::PresencePenalty,
            SamplingParamKey::FrequencyPenalty,
            SamplingParamKey::MaxTokens,
            SamplingParamKey::Seed,
        ] {
            assert!(
                model.supported_params.contains(&key),
                "{} does not declare {key:?}",
                model.id,
            );
        }
    }

    let request = stub.only_request().await;
    assert_eq!(request.method.as_str(), "GET");
    assert_eq!(request.url.path(), "/v1/cloud-models");
    assert_eq!(
        bearer(&request),
        None,
        "the catalog is public — sending a token would be a contract change",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// POST /v1/chat/completions (SSE)
// ---------------------------------------------------------------------------

fn user_message(content: &str) -> ChatMessage {
    ChatMessage {
        id: new_message_id(),
        role: "user".to_string(),
        content: content.to_string(),
        created_at_ms: None,
        model: None,
        citations: None,
        thinking: None,
        steps: None,
        attachments: None,
        artifacts: None,
        stats: None,
        display_content: None,
        injection_notices: None,
        withheld_from_model: None,
    }
}

async fn run_chat_stream() -> (Result<(), Error>, Vec<ChatEvent>) {
    let mut events = Vec::new();
    let result = cloud::chat_stream(
        &http_client(),
        "at_test_token",
        "openpal/fast-cloud",
        &[user_message("hi")],
        &SamplingParams::default(),
        Arc::new(AtomicBool::new(false)),
        |event| events.push(event),
    )
    .await;
    (result, events)
}

async fn replay_sse(entry: &Entry) -> Result<(), String> {
    let stub = Stub::serving(
        "POST",
        "/v1/chat/completions",
        200,
        "text/event-stream",
        &fixture(&entry.path),
    )
    .await;

    let (result, events) = run_chat_stream().await;
    assert_chat_request(&stub.only_request().await);

    match entry.path.as_str() {
        "v1/chat_stream.sse" => {
            result.map_err(|e| format!("plain stream failed: {e}"))?;
            assert_eq!(streamed_tokens(&events), "Hello from the fake cloud.");
            assert_eq!(streamed_reasoning(&events), "");
            assert_done_stats(&events, 5, 31.5);
        }
        "v1/chat_stream_reasoning.sse" => {
            result.map_err(|e| format!("reasoning stream failed: {e}"))?;
            assert_eq!(streamed_reasoning(&events), "Thinking about it.");
            assert_eq!(streamed_tokens(&events), "Hello from the fake cloud.");
            assert_done_stats(&events, 5, 31.5);
        }
        "v1/chat_stream_error.sse" => {
            assert!(
                matches!(result, Err(Error::Cloud(_))),
                "finish_reason: error must fail the call",
            );
            assert_eq!(streamed_tokens(&events), "Hello from the fake cloud.");
            assert!(
                matches!(
                    events.last(),
                    Some(ChatEvent::Error { message }) if message == STREAM_ERROR_MESSAGE
                ),
                "the error stream must end in ChatEvent::Error, got {events:?}",
            );
            assert!(
                !events
                    .iter()
                    .any(|event| matches!(event, ChatEvent::Done { .. })),
                "a failed stream must not also report success",
            );
        }
        other => return Err(format!("unexpected SSE fixture {other}")),
    }
    Ok(())
}

/// The outbound side of `chat_stream`: bearer, model, `stream`, and the whole
/// llama.cpp sampling set under its snake_case wire names — including the
/// `-1` sentinels that must reach this client's own contract unchanged.
fn assert_chat_request(request: &wiremock::Request) {
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/v1/chat/completions");
    assert_eq!(bearer(request), Some("Bearer at_test_token"));

    let body = request_json(request);
    assert_eq!(body["model"], "openpal/fast-cloud");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["messages"],
        serde_json::json!([{ "role": "user", "content": "hi" }]),
    );

    let sampling = SamplingParams::default();
    assert_eq!(body["temperature"], serde_json::json!(sampling.temperature));
    assert_eq!(body["top_p"], serde_json::json!(sampling.top_p));
    assert_eq!(body["top_k"], serde_json::json!(sampling.top_k));
    assert_eq!(body["min_p"], serde_json::json!(sampling.min_p));
    assert_eq!(
        body["repeat_penalty"],
        serde_json::json!(sampling.repeat_penalty),
    );
    assert_eq!(
        body["repeat_last_n"],
        serde_json::json!(sampling.repeat_last_n),
    );
    assert_eq!(
        body["presence_penalty"],
        serde_json::json!(sampling.presence_penalty),
    );
    assert_eq!(
        body["frequency_penalty"],
        serde_json::json!(sampling.frequency_penalty),
    );
    assert_eq!(body["max_tokens"], -1);
    assert_eq!(body["seed"], -1);

    // The struct's own camelCase Serialize must never leak onto this wire.
    for camel in [
        "topP",
        "topK",
        "minP",
        "repeatPenalty",
        "repeatLastN",
        "presencePenalty",
        "frequencyPenalty",
        "maxTokens",
    ] {
        assert!(
            body.get(camel).is_none(),
            "{camel} belongs to the IPC shape, not the chat wire",
        );
    }
}

fn streamed_tokens(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn streamed_reasoning(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Reasoning { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn assert_done_stats(events: &[ChatEvent], total_tokens: u64, tokens_per_sec: f64) {
    match events.last() {
        Some(ChatEvent::Done { stats }) => {
            assert_eq!(stats.total_tokens, total_tokens);
            assert_eq!(stats.tokens_per_sec, tokens_per_sec);
        }
        other => panic!("expected Done with timings stats, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Error envelopes (401, 403, 429)
// ---------------------------------------------------------------------------

async fn replay_error(entry: &Entry) -> Result<(), String> {
    let stub = Stub::serving(
        &entry.method,
        &entry.route,
        entry.status,
        &entry.content_type,
        &fixture(&entry.path),
    )
    .await;

    match entry.status {
        401 => {
            let message = account::fetch_account(&http_client(), "at_test_token")
                .await
                .expect_err("a 401 must fail the account fetch");
            assert_eq!(message, "Your session expired. Please sign in again.");
            let request = stub.only_request().await;
            assert_eq!(request.url.path(), "/v1/account/me");
            assert_eq!(bearer(&request), Some("Bearer at_test_token"));
        }
        403 | 429 => {
            let expected = if entry.status == 403 {
                "Your plan doesn't include this model."
            } else {
                "Your cloud model couldn't process this message. Please try again."
            };
            let (result, events) = run_chat_stream().await;
            match result {
                Err(Error::Cloud(message)) => assert_eq!(message, expected),
                other => return Err(format!("expected a cloud error, got {other:?}")),
            }
            assert!(
                matches!(
                    events.last(),
                    Some(ChatEvent::Error { message }) if message == expected
                ),
                "the UI copy must be pinned, got {events:?}",
            );
        }
        status => return Err(format!("unexpected error status {status}")),
    }
    Ok(())
}

/// The 401 path through `chat_stream` (the manifest's own 401 fixture is the
/// account route). A 429 deliberately falls through to the generic copy.
#[tokio::test]
async fn chat_error_copy_is_pinned() {
    for (fixture_name, status, expected) in [
        (
            "v1/error_unauthorized.json",
            401,
            "Your sign-in expired. Please sign in again.",
        ),
        (
            "v1/error_model_not_in_plan.json",
            403,
            "Your plan doesn't include this model.",
        ),
        (
            "v1/error_quota_exceeded.json",
            429,
            "Your cloud model couldn't process this message. Please try again.",
        ),
    ] {
        let _stub = Stub::serving(
            "POST",
            "/v1/chat/completions",
            status,
            "application/json",
            &fixture(fixture_name),
        )
        .await;

        let (result, events) = run_chat_stream().await;
        match result {
            Err(Error::Cloud(message)) => assert_eq!(message, expected, "{fixture_name}"),
            other => panic!("{fixture_name}: expected a cloud error, got {other:?}"),
        }
        assert!(
            matches!(
                events.last(),
                Some(ChatEvent::Error { message }) if message == expected
            ),
            "{fixture_name}: UI copy drifted, got {events:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// POST /v1/auth/signout (no fixture: any 2xx is accepted)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn signout_posts_a_bearer_and_ignores_the_response() {
    let stub = Stub::serving("POST", "/v1/auth/signout", 204, "application/json", "").await;

    account::revoke_session(&http_client(), "at_test_token").await;

    let request = stub.only_request().await;
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.url.path(), "/v1/auth/signout");
    assert_eq!(bearer(&request), Some("Bearer at_test_token"));
    assert!(
        request.body.is_empty(),
        "signout carries the token in the header only",
    );
}

// ---------------------------------------------------------------------------
// Fixture hygiene
// ---------------------------------------------------------------------------

/// Placeholder values, never real-looking secrets: these files are public.
#[test]
fn placeholder_fixtures_do_not_contain_real_looking_secrets() {
    let complete = fixture("v1/session_poll_complete.json");
    assert!(complete.contains("at_placeholder"));
    assert!(complete.contains("rt_placeholder"));
    assert!(!complete.contains("eyJ"), "no JWTs in fixtures");
}
