//! HTTP routes for the mailbox skeleton: push (`POST`), pull (`GET`), ack
//! (`DELETE`). Wire encoding is plain JSON with the ciphertext base64-encoded
//! (`base64` is already a workspace dependency elsewhere in this repo) —
//! Phase 2 can swap this for `sync_protocol::Envelope` directly once real
//! crypto exists; Phase 1 keeps the wire shape simple and inspectable.

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    middleware,
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::rate_limit::{self, RateLimiter};
use crate::store::MailboxStore;

/// Explicit replacement for axum's implicit, undocumented 2 MiB default
/// body-size limit (`axum_core`'s `Bytes`/`Json` extractors fall back to
/// exactly `2 * 1024 * 1024` bytes when no `DefaultBodyLimit` layer is
/// configured) — this relay never set one, so it was relying on that
/// accident rather than an intentional, documented bound. Sized with
/// headroom above `openpal_core::sync::artifact_sweep::
/// ARTIFACT_SYNC_MAX_BYTES` (1 MiB) once JSON+AEAD+base64 framing overhead
/// is accounted for: ~2.67 MiB worst-case (pathological JSON escaping of
/// the content), ~1.4 MiB realistic (ordinary text/code) — see Phase 4's
/// plan for the arithmetic. Applies to every route, not just artifact
/// pushes — this is the relay's only defense against an oversized request
/// body, so it stays a real, bounded number rather than "large enough it
/// might as well be unlimited," same style as `MAX_PENDING_PER_MAILBOX`
/// below.
const MAX_MAILBOX_REQUEST_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Takes an already-constructed `Arc<RateLimiter>` rather than building one
/// internally from a policy number — `main.rs` holds the same instance to
/// pass to its periodic sweep loop for pruning, and constructing two
/// separate limiters here (one enforcing, one being pruned) would make the
/// pruning meaningless. Tests construct a small-limit instance directly to
/// exercise the 429 path without firing hundreds of requests.
pub fn router(store: MailboxStore, limiter: Arc<RateLimiter>) -> Router {
    let mailbox_routes = Router::new()
        .route(
            "/mailbox/{mailbox_id}",
            post(push_envelope).get(pull_envelopes),
        )
        .route("/mailbox/{mailbox_id}/{envelope_id}", delete(ack_envelope))
        .layer(DefaultBodyLimit::max(MAX_MAILBOX_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            limiter,
            rate_limit::rate_limit_middleware,
        ));

    // `/health` is deliberately outside the rate-limited/body-limited
    // mailbox tree — an orchestrator's health-poll cadence has nothing to
    // do with the mailbox-traffic budget, and coupling them would only add
    // a way for infra-layer polling to interact with abuse-hardening it
    // isn't part of.
    Router::new()
        .route("/health", get(health_check))
        .merge(mailbox_routes)
        .with_state(store)
}

#[derive(Debug, Deserialize)]
pub struct PushEnvelopeRequest {
    /// Base64-encoded opaque ciphertext bytes. Phase 1 never puts real user
    /// data here — synthetic/test payloads only.
    pub ciphertext: String,
    #[serde(default = "default_protocol_version")]
    pub version: u32,
    /// Server time is used if omitted.
    pub created_at_ms: Option<i64>,
    /// Requested retention, in seconds from `created_at_ms`. Server-clamped
    /// to `[MIN_TTL_SECONDS, MAX_TTL_SECONDS]`; defaults to
    /// `DEFAULT_TTL_SECONDS` if omitted. Purely a resource-exhaustion
    /// mitigation (see `docs/SYNC_PAIRING_PROTOCOL.md`'s "Relay hardening")
    /// — it grants the relay no new authentication or inspection power.
    pub ttl_seconds: Option<u64>,
}

fn default_protocol_version() -> u32 {
    openpal_sync_protocol::PROTOCOL_VERSION
}

/// Floor of the server-side `ttl_seconds` clamp: 60 seconds.
const MIN_TTL_SECONDS: u64 = 60;
/// Ceiling of the server-side `ttl_seconds` clamp: 90 days. Matches the
/// rotation-delivery envelope's long retention need
/// (`docs/SYNC_PAIRING_PROTOCOL.md`'s "Revocation and rotation") — bumped
/// from 30 to 90 days when periodic (not just removal-triggered) rotation
/// was added, since periodic rotation applies "must reconnect within the
/// TTL or get stranded" to every long-idle device, not just rare manual
/// removals.
const MAX_TTL_SECONDS: u64 = 7_776_000;
/// Default `ttl_seconds` when the caller omits it — matches the
/// pairing-envelope default in `docs/SYNC_PAIRING_PROTOCOL.md` (10 minutes).
/// A caller doing a long-lived rotation delivery must pass an explicit
/// larger value.
const DEFAULT_TTL_SECONDS: u64 = 600;
/// Per-mailbox cap on non-expired pending envelopes. Generous enough for a
/// plausible rotation fan-out across many survivors, tight enough to bound
/// worst-case `GET` payload size and parse cost.
const MAX_PENDING_PER_MAILBOX: i64 = 200;

fn clamp_ttl_seconds(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(DEFAULT_TTL_SECONDS)
        .clamp(MIN_TTL_SECONDS, MAX_TTL_SECONDS)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushEnvelopeResponse {
    pub envelope_id: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredEnvelope {
    pub envelope_id: i64,
    pub version: u32,
    pub mailbox_id: String,
    /// Base64-encoded opaque ciphertext bytes.
    pub ciphertext: String,
    pub created_at_ms: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn internal_error(err: rusqlite::Error) -> (StatusCode, String) {
    tracing::error!(?err, "relay mailbox store error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal error".to_string(),
    )
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

/// Unauthenticated by necessity — a reverse proxy or orchestrator polling
/// this has no bearer token, and the relay has zero authentication on any
/// route already, so this adds no new gap. Checks a real dependency
/// (`MailboxStore::health_check`'s `SELECT 1`), not just "the process is
/// alive," which would miss the actual failure mode that matters (SQLite
/// unwritable/disk-full while the TCP listener is still accepting).
async fn health_check(
    State(store): State<MailboxStore>,
) -> Result<Json<HealthResponse>, StatusCode> {
    store.health_check().map_err(|err| {
        tracing::error!(?err, "relay health check failed");
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    Ok(Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    }))
}

async fn push_envelope(
    State(store): State<MailboxStore>,
    Path(mailbox_id): Path<String>,
    Json(req): Json<PushEnvelopeRequest>,
) -> Result<(StatusCode, Json<PushEnvelopeResponse>), (StatusCode, String)> {
    let ciphertext = STANDARD.decode(&req.ciphertext).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid base64 ciphertext: {e}"),
        )
    })?;
    let created_at_ms = req.created_at_ms.unwrap_or_else(now_ms);
    let ttl_seconds = clamp_ttl_seconds(req.ttl_seconds);

    let pending = store.pending_count(&mailbox_id).map_err(internal_error)?;
    if pending >= MAX_PENDING_PER_MAILBOX {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "mailbox has too many pending envelopes".to_string(),
        ));
    }

    let envelope_id = store
        .push(
            &mailbox_id,
            req.version,
            &ciphertext,
            created_at_ms,
            ttl_seconds,
        )
        .map_err(internal_error)?;

    Ok((
        StatusCode::CREATED,
        Json(PushEnvelopeResponse { envelope_id }),
    ))
}

async fn pull_envelopes(
    State(store): State<MailboxStore>,
    Path(mailbox_id): Path<String>,
) -> Result<Json<Vec<StoredEnvelope>>, (StatusCode, String)> {
    let rows = store.pull(&mailbox_id).map_err(internal_error)?;

    let envelopes = rows
        .into_iter()
        .map(|row| StoredEnvelope {
            envelope_id: row.envelope_id,
            version: row.version,
            mailbox_id: mailbox_id.clone(),
            ciphertext: STANDARD.encode(row.ciphertext),
            created_at_ms: row.created_at_ms,
        })
        .collect();

    Ok(Json(envelopes))
}

async fn ack_envelope(
    State(store): State<MailboxStore>,
    Path((mailbox_id, envelope_id)): Path<(String, i64)>,
) -> Result<StatusCode, (StatusCode, String)> {
    store
        .ack(&mailbox_id, envelope_id)
        .map_err(internal_error)?;
    // 204 whether or not the row existed: the ack is idempotent by design.
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// A generous rate limit — high enough that none of the existing
    /// mailbox-behavior tests below (including the 200-push pending-cap
    /// test) trip it incidentally. The rate limiter's own behavior is
    /// tested separately, in `rate_limit.rs` and via `test_router_with_rate_limit`.
    const GENEROUS_TEST_RATE_LIMIT: u32 = 10_000;

    fn test_router() -> Router {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        router(store, RateLimiter::new(GENEROUS_TEST_RATE_LIMIT))
    }

    fn test_router_with_rate_limit(requests_per_minute: u32) -> Router {
        let store = MailboxStore::open_in_memory().expect("open in-memory store");
        router(store, RateLimiter::new(requests_per_minute))
    }

    async fn body_json<T: serde::de::DeserializeOwned>(response: axum::response::Response) -> T {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn push_pull_ack_round_trip() {
        let app = test_router();
        let mailbox_id = "test-mailbox";

        // Push a synthetic envelope (never real user data, per Phase 1 scope).
        let push_body = serde_json::json!({
            "ciphertext": STANDARD.encode(b"synthetic test payload"),
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/mailbox/{mailbox_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&push_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let push_response: PushEnvelopeResponse = body_json(response).await;
        let envelope_id = push_response.envelope_id;

        // Pull it back and confirm it's present with the id we were given.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/mailbox/{mailbox_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let envelopes: Vec<StoredEnvelope> = body_json(response).await;
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0].envelope_id, envelope_id);
        assert_eq!(
            STANDARD.decode(&envelopes[0].ciphertext).unwrap(),
            b"synthetic test payload"
        );

        // Ack (delete) it.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/mailbox/{mailbox_id}/{envelope_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Pull again: the mailbox is now empty.
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/mailbox/{mailbox_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let envelopes: Vec<StoredEnvelope> = body_json(response).await;
        assert!(envelopes.is_empty());
    }

    #[tokio::test]
    async fn push_is_rejected_with_413_when_body_exceeds_the_configured_limit() {
        let app = test_router();
        // Comfortably over MAX_MAILBOX_REQUEST_BODY_BYTES (4 MiB) — the
        // relay's only defense against an oversized request body, added
        // when Phase 4 discovered this router had never configured one
        // (silently relying on axum's implicit, undocumented 2 MiB default).
        let oversized_ciphertext = STANDARD.encode(vec![0u8; MAX_MAILBOX_REQUEST_BODY_BYTES + 1]);
        let push_body = serde_json::json!({ "ciphertext": oversized_ciphertext });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mailbox/test-mailbox")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&push_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn ack_of_nonexistent_envelope_is_idempotent() {
        let app = test_router();
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/mailbox/some-mailbox/999999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn pull_on_empty_mailbox_returns_empty_array() {
        let app = test_router();
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/mailbox/never-pushed-to")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let envelopes: Vec<StoredEnvelope> = body_json(response).await;
        assert!(envelopes.is_empty());
    }

    #[test]
    fn clamp_ttl_seconds_defaults_when_omitted() {
        assert_eq!(clamp_ttl_seconds(None), DEFAULT_TTL_SECONDS);
    }

    #[test]
    fn clamp_ttl_seconds_clamps_below_floor_and_above_ceiling() {
        assert_eq!(clamp_ttl_seconds(Some(1)), MIN_TTL_SECONDS);
        assert_eq!(clamp_ttl_seconds(Some(u64::MAX)), MAX_TTL_SECONDS);
        assert_eq!(clamp_ttl_seconds(Some(1_000)), 1_000);
    }

    #[tokio::test]
    async fn push_is_rejected_with_429_once_mailbox_hits_the_pending_cap() {
        let app = test_router();
        let mailbox_id = "flooded-mailbox";

        // Fill the mailbox up to the cap (MAX_PENDING_PER_MAILBOX = 200).
        for _ in 0..MAX_PENDING_PER_MAILBOX {
            let push_body = serde_json::json!({
                "ciphertext": STANDARD.encode(b"synthetic test payload"),
            });
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/mailbox/{mailbox_id}"))
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&push_body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED);
        }

        // The 200th-and-beyond push is rejected with 429.
        let push_body = serde_json::json!({
            "ciphertext": STANDARD.encode(b"one too many"),
        });
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/mailbox/{mailbox_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&push_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // A different mailbox is unaffected by the first one's cap.
        let push_body = serde_json::json!({
            "ciphertext": STANDARD.encode(b"different mailbox"),
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/mailbox/another-mailbox")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&push_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn health_check_reports_ok_against_a_working_store() {
        let app = test_router();
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = body_json(response).await;
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn requests_are_rejected_with_429_and_retry_after_once_the_rate_limit_is_spent() {
        let app = test_router_with_rate_limit(2);
        let mailbox_id = "rate-limited-mailbox";

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(format!("/mailbox/{mailbox_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/mailbox/{mailbox_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response
            .headers()
            .contains_key(axum::http::header::RETRY_AFTER));
    }

    #[tokio::test]
    async fn rate_limit_is_scoped_per_forwarded_for_source_not_globally() {
        let app = test_router_with_rate_limit(1);
        let mailbox_id = "shared-mailbox";

        let request_from = |ip: &str| {
            Request::builder()
                .method("GET")
                .uri(format!("/mailbox/{mailbox_id}"))
                .header("x-forwarded-for", ip)
                .body(Body::empty())
                .unwrap()
        };

        let response = app
            .clone()
            .oneshot(request_from("203.0.113.1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Same source, second request within the window: exceeds its budget of 1.
        let response = app
            .clone()
            .oneshot(request_from("203.0.113.1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

        // A different source has its own, unspent budget.
        let response = app.oneshot(request_from("203.0.113.2")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_check_is_not_subject_to_the_mailbox_rate_limit() {
        let app = test_router_with_rate_limit(1);
        // Spend the (very small) mailbox rate limit's single request.
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/mailbox/some-mailbox")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // /health must still succeed — it's outside the mailbox route tree
        // the rate limiter is layered on.
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
