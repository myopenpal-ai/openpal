//! Drives this crate's real account/cloud client against the backend's
//! contract double.
//!
//! [`crate::contract_tests`] proves this client reads the recorded `/v1`
//! bytes. This module proves the other half of the pair: the dev-only axum
//! server in `myopenpal-ai/openpal-cloud` emits bytes this client's shipped
//! parsers accept, through the code that actually ships — a full
//! `AccountManager::sign_in` (create → browser-handoff code → approval →
//! poll completion), `/v1/account/me`, the catalog, one streamed reply, and
//! the 401/403 copy the UI shows. A fixture rename that lands on both sides
//! in one commit is green in the replay; only this run catches the pair
//! disagreeing.
//!
//! The suite is `#[ignore]`d out of the default `cargo test` run because it
//! needs a live process. Point this crate at one with
//! [`OPENPAL_ACCOUNT_API`](crate::account::api_base) — the same override the
//! app already supports:
//!
//! ```sh
//! OPENPAL_ACCOUNT_API=http://127.0.0.1:8788 \
//!   cargo test -p openpal-core --lib contract_smoke -- --ignored
//! ```
//!
//! CI's `contract-smoke` job starts the digest-pinned double and runs exactly
//! that command, so neither this repo nor a fork PR needs a credential or a
//! repository secret. The double's `x-openpal-contract` header must match the
//! version recorded in `tests/contract/PIN.toml`; the failure message names
//! the version it ran, and the job emits it to its summary.

use crate::account::{self, Account, AccountEvent, AccountManager, Quota};
use crate::cloud;
use crate::crypto::lookup::KeyLookup;
use crate::crypto::secret_store::SecretStore;
use crate::types::{ChatEvent, ChatMessage, SamplingParams};
use crate::Error;
use futures_util::FutureExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const PIN_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract/PIN.toml");
/// `sign_in`'s poll loop ticks every 2 s (`account::POLL_INTERVAL`). This only
/// bounds a broken run so CI fails instead of hanging; a healthy one costs
/// two ticks.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Test plumbing
// ---------------------------------------------------------------------------

/// Linux runners have no keychain. This is the same stand-in shape the rest
/// of the crate's network-boundary tests use: `store` succeeding is what makes
/// `AccountEvent::Complete { persisted: true }`.
#[derive(Default)]
struct MemorySecretStore {
    entries: Mutex<HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn store(&self, key: &str, value: &str) -> std::result::Result<(), String> {
        self.entries
            .lock()
            .expect("secret store lock")
            .insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn load(&self, key: &str) -> KeyLookup<String> {
        match self.entries.lock().expect("secret store lock").get(key) {
            Some(value) => KeyLookup::Found(value.clone()),
            None => KeyLookup::Absent,
        }
    }

    fn delete(&self, key: &str) -> std::result::Result<(), String> {
        self.entries.lock().expect("secret store lock").remove(key);
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct PinFile {
    contract: PinContract,
}

#[derive(Debug, Deserialize)]
struct PinContract {
    version: String,
}

fn pinned_contract_version() -> String {
    let raw = std::fs::read_to_string(PIN_PATH).unwrap_or_else(|e| panic!("read {PIN_PATH}: {e}"));
    let pin: PinFile = toml::from_str(&raw).unwrap_or_else(|e| panic!("parse {PIN_PATH}: {e}"));
    pin.contract.version
}

fn base_url() -> String {
    std::env::var("OPENPAL_ACCOUNT_API")
        .unwrap_or_else(|_| {
            panic!(
                "contract smoke test needs OPENPAL_ACCOUNT_API pointed at the double, e.g. \
                 OPENPAL_ACCOUNT_API=http://127.0.0.1:8788"
            )
        })
        .trim_end_matches('/')
        .to_string()
}

/// The `?code=` from a `verificationUrl` — the only pre-completion value the
/// shipped `AccountEvent::AwaitingBrowser` exposes. The double's dev route
/// accepts it in place of the session id precisely because the client never
/// learns the id before completion.
fn handoff_code(verification_url: &str) -> String {
    verification_url
        .split_once("?code=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or(rest))
        .filter(|code| !code.is_empty())
        .unwrap_or_else(|| {
            panic!("contract double's verificationUrl carries no ?code=: {verification_url}")
        })
        .to_string()
}

async fn served_contract_version(client: &reqwest::Client, base: &str) -> String {
    let url = format!("{base}/health");
    let response = client
        .get(&url)
        .send()
        .await
        .unwrap_or_else(|e| panic!("the contract double isn't reachable at {url}: {e}"));
    assert!(
        response.status().is_success(),
        "the contract double's {url} answered {}",
        response.status(),
    );
    response
        .headers()
        .get("x-openpal-contract")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("the contract double's {url} has no x-openpal-contract header"))
        .to_string()
}

fn user_message(content: &str) -> ChatMessage {
    ChatMessage {
        id: crate::types::new_message_id(),
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

fn streamed_text(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic".to_string()
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

/// Sign in through the real `AccountManager`, approving from the
/// `verificationUrl`'s code the way a browser handoff would.
async fn sign_in_once(manager: &AccountManager, base: &str, version: &str) -> Account {
    let (approval_tx, mut approval_rx) = mpsc::unbounded_channel::<String>();
    let approval_base = base.to_string();
    let approval_version = version.to_string();
    let approval = tokio::spawn(async move {
        let verification_url = approval_rx
            .recv()
            .await
            .expect("sign_in emitted AwaitingBrowser before finishing");
        let code = handoff_code(&verification_url);
        let url = format!("{approval_base}/dev/session/code/{code}/approve");
        let response = reqwest::Client::new()
            .post(&url)
            .send()
            .await
            .unwrap_or_else(|e| {
                panic!("contract {approval_version}: approval POST {url} failed: {e}")
            });
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NO_CONTENT,
            "contract {approval_version}: approval POST {url}",
        );
    });

    let events: Arc<Mutex<Vec<AccountEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&events);
    let sign_in = tokio::time::timeout(
        SIGN_IN_TIMEOUT,
        manager.sign_in(move |event| {
            if let AccountEvent::AwaitingBrowser { verification_url } = &event {
                approval_tx
                    .send(verification_url.clone())
                    .expect("approval task is still listening");
            }
            recorded.lock().expect("events lock").push(event);
        }),
    )
    .await;

    let result = match sign_in {
        Ok(result) => result,
        Err(_) => {
            approval.abort();
            panic!(
                "contract {version}: sign_in did not complete within {SIGN_IN_TIMEOUT:?} \
                 (the approval route or the poll loop disagreed)"
            );
        }
    };
    approval.await.expect("approval task finished");

    result.unwrap_or_else(|e| panic!("contract {version}: sign_in failed: {e}"));
    let events = events.lock().expect("events lock");

    assert!(
        events
            .iter()
            .any(|event| matches!(event, AccountEvent::AwaitingBrowser { .. })),
        "contract {version}: no AwaitingBrowser event: {events:?}",
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AccountEvent::Failed { .. } | AccountEvent::Expired)),
        "contract {version}: sign-in failed before completion: {events:?}",
    );

    let complete = events.iter().find_map(|event| match event {
        AccountEvent::Complete { account, persisted } => Some((account.clone(), *persisted)),
        _ => None,
    });
    let (account, persisted) =
        complete.unwrap_or_else(|| panic!("contract {version}: no Complete event: {events:?}"));
    assert!(
        persisted,
        "contract {version}: the refresh token wasn't persisted to the (in-memory) store",
    );
    account
}

async fn stream_reply(
    client: &reqwest::Client,
    access_token: &str,
    model: &str,
    version: &str,
) -> (Result<(), Error>, Vec<ChatEvent>) {
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        HTTP_TIMEOUT,
        cloud::chat_stream(
            client,
            access_token,
            model,
            &[user_message("hi")],
            &SamplingParams::default(),
            Arc::new(AtomicBool::new(false)),
            |event| events.push(event),
        ),
    )
    .await
    .unwrap_or_else(|_| {
        panic!("contract {version}: streamed reply from {model} did not finish within {HTTP_TIMEOUT:?}")
    });
    (result, events)
}

async fn run(client: &reqwest::Client, base: &str, version: &str) {
    let expected_account = Account {
        email: "person@example.com".to_string(),
        tier: "Plus".to_string(),
        capabilities: vec![
            "cloud-models".to_string(),
            "managed-web-search".to_string(),
            "sync-managed".to_string(),
        ],
        quota: Some(Quota {
            used: 37,
            limit: 500,
        }),
    };

    let dir = tempfile::tempdir().expect("tempdir");
    let secret_store: Arc<MemorySecretStore> = Arc::new(MemorySecretStore::default());
    let manager = AccountManager::new(dir.path().join("account.json"), secret_store.clone());

    // 1. The whole browser-handoff flow: create, approve by the URL's code,
    //    poll to Complete, persist the refresh token.
    let account = sign_in_once(&manager, base, version).await;
    assert_eq!(
        account, expected_account,
        "contract {version}: the signed-in account snapshot drifted",
    );
    assert_eq!(
        manager.cached().await,
        Some(expected_account.clone()),
        "contract {version}: the on-disk cache didn't get the completed account",
    );

    // 2. Boot-time refresh + GET /v1/account/me, driven through a fresh
    //    manager with an empty cache: only a real refresh + account fetch can
    //    populate it, so a silent failure here can't pass on stale state.
    let relaunched = AccountManager::new(dir.path().join("relaunch.json"), secret_store);
    assert_eq!(
        relaunched.cached().await,
        None,
        "contract {version}: the relaunch cache should start empty",
    );
    relaunched.refresh().await;
    assert_eq!(
        relaunched.cached().await,
        Some(expected_account),
        "contract {version}: refresh or /v1/account/me returned a different account",
    );
    let access_token = relaunched
        .access_token()
        .await
        .expect("a successful refresh leaves an access token in memory");

    // 3. The public catalog, parsed by the shipped type.
    let models = tokio::time::timeout(HTTP_TIMEOUT, cloud::list_models(client))
        .await
        .unwrap_or_else(|_| panic!("contract {version}: the catalog request timed out"))
        .unwrap_or_else(|e| panic!("contract {version}: the catalog failed: {e}"));
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        ["openpal/fast-cloud", "openpal/pro-cloud"],
        "contract {version}: the catalog's model ids drifted",
    );
    assert_eq!(models[0].required_tier, "Plus");
    assert_eq!(models[1].required_tier, "Pro");
    assert_eq!(
        models[0].supported_params.len(),
        10,
        "contract {version}: the fast model's supported sampling set drifted",
    );

    // 4. One streamed reply through the same SSE parser local chat uses.
    let (result, events) = stream_reply(client, &access_token, "openpal/fast-cloud", version).await;
    result.unwrap_or_else(|e| panic!("contract {version}: the streamed reply failed: {e}"));
    assert_eq!(
        streamed_text(&events),
        "Hello from the fake cloud.",
        "contract {version}: the assembled reply drifted: {events:?}",
    );
    match events.last() {
        Some(ChatEvent::Done { stats }) => {
            assert_eq!(
                stats.total_tokens, 5,
                "contract {version}: timings.predicted_n"
            );
            assert_eq!(
                stats.tokens_per_sec, 31.5,
                "contract {version}: timings.predicted_per_second"
            );
        }
        other => panic!("contract {version}: expected Done with timings stats, got {other:?}"),
    }

    // 5. 403 — a model outside this account's plan must keep the UI copy.
    let (result, events) = stream_reply(client, &access_token, "openpal/pro-cloud", version).await;
    match result {
        Err(Error::Cloud(message)) => assert_eq!(
            message, "Your plan doesn't include this model.",
            "contract {version}: the 403 copy drifted",
        ),
        other => panic!("contract {version}: expected the 403 cloud error, got {other:?}"),
    }
    assert!(
        matches!(
            events.last(),
            Some(ChatEvent::Error { message }) if message == "Your plan doesn't include this model."
        ),
        "contract {version}: the 403 UI event drifted: {events:?}",
    );

    // 6. Sign out revokes the access token server-side; the same token now
    //    gets a real 401 from the double, on both surfaces the UI shows.
    relaunched.sign_out().await;
    let message = account::fetch_account(client, &access_token)
        .await
        .expect_err("a revoked access token must fail /v1/account/me");
    assert_eq!(
        message, "Your session expired. Please sign in again.",
        "contract {version}: the account 401 copy drifted",
    );

    let (result, events) = stream_reply(client, &access_token, "openpal/fast-cloud", version).await;
    match result {
        Err(Error::Cloud(message)) => assert_eq!(
            message, "Your sign-in expired. Please sign in again.",
            "contract {version}: the chat 401 copy drifted",
        ),
        other => panic!("contract {version}: expected the 401 cloud error, got {other:?}"),
    }
    assert!(
        matches!(
            events.last(),
            Some(ChatEvent::Error { message }) if message == "Your sign-in expired. Please sign in again."
        ),
        "contract {version}: the 401 UI event drifted: {events:?}",
    );
}

/// The one opt-in run. The outer panic wraps every inner assertion with the
/// contract version and base URL, so a red CI job names what it ran.
#[tokio::test]
#[ignore = "needs the contract double; set OPENPAL_ACCOUNT_API and pass -- --ignored"]
async fn contract_smoke_against_the_cloud_double() {
    let base = base_url();
    let client = reqwest::Client::new();
    let served = served_contract_version(&client, &base).await;
    let pinned = pinned_contract_version();
    assert_eq!(
        served, pinned,
        "contract {served} at {base} does not match the version vendored in tests/contract/PIN.toml \
         ({pinned}); re-vendor the fixture set before pinning this image",
    );
    eprintln!("contract {served} at {base}");

    let outcome = AssertUnwindSafe(run(&client, &base, &served))
        .catch_unwind()
        .await;
    if let Err(payload) = outcome {
        panic!("contract {served} at {base}: {}", panic_message(payload));
    }
}
