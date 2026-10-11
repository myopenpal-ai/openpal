//! The inbound HTTP(S) gateway a paired device talks to: an authenticated,
//! TLS-terminated reverse proxy in front of the local engine's
//! OpenAI-compatible API, plus the pairing/status endpoints. This is the
//! one explicit, gated exception to "the engine only ever listens on
//! loopback" (`crate::engine`) — bound to `0.0.0.0`, but only while the
//! user has turned Remote Access on in Settings → Devices, and only ever
//! reachable with a valid paired-device bearer token (or, for
//! `/remote/v1/pair` itself, a fresh, single-use, short-lived pairing
//! code).
//!
//! Chat completions are proxied as raw bytes straight to the engine's
//! `/chat/completions` endpoint (`EngineManager::endpoint`) — deliberately
//! bypassing `chat_stream`/`ChatEvent` entirely. A remote client speaks
//! plain OpenAI-style SSE, not OpenPal's internal desktop-UI event shape,
//! so there's nothing to translate; see this module's own handler for why.
//!
//! **Known v1 gap, disclosed rather than silent**: no per-device rate
//! limiting. TLS + a per-device bearer token + a short-lived single-use
//! pairing code are the only defenses against abuse once a device is
//! paired — acceptable for a LAN/overlay-network feature aimed at "my own
//! devices", not yet hardened against a malicious paired device sending a
//! flood of requests. See `docs/REMOTE_INFERENCE.md`.

use super::{pairing, store, Shared};
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// A bound, listening gateway. Dropping/`.stop()`-ing tears down both the
/// HTTP(S) server and its mDNS advertisement — a paired client should never
/// be able to discover or reach a gateway the user just turned off.
pub struct RunningGateway {
    pub port: u16,
    handle: axum_server::Handle<SocketAddr>,
    join: tokio::task::JoinHandle<()>,
    advertisement: Option<super::discovery::Advertisement>,
}

impl RunningGateway {
    pub async fn stop(self) {
        self.handle.graceful_shutdown(Some(Duration::from_secs(2)));
        let _ = self.join.await;
        if let Some(advertisement) = self.advertisement {
            advertisement.stop().await;
        }
    }
}

/// Pick a free port on every interface (not just loopback) — the port this
/// resolves to is what the gateway will actually bind, so availability
/// must be checked the same way.
pub fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("0.0.0.0:0")?;
    Ok(listener.local_addr()?.port())
}

/// The route table, independent of TLS/binding — split out so tests can
/// drive it directly (`tower::ServiceExt::oneshot`) without a real
/// certificate or open socket.
fn build_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/remote/v1/status", get(status_handler))
        .route("/remote/v1/pair", post(pair_handler))
        .route("/v1/chat/completions", post(chat_completions_handler))
        .with_state(shared)
}

pub async fn start(
    shared: Arc<Shared>,
    port: u16,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    device_id: &str,
    fingerprint: &str,
) -> Result<RunningGateway, String> {
    // `axum-server`'s `tls-rustls-no-provider` feature deliberately doesn't
    // install a process-wide crypto provider (see `crates/core/Cargo.toml`'s
    // comment on this dependency) — `ring`, matching the rest of the
    // workspace. Safe to call more than once: a second call just returns an
    // `Err` this build ignores, since "already installed" isn't a failure.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = axum_server::tls_rustls::RustlsConfig::from_der(vec![cert_der], key_der)
        .await
        .map_err(|e| {
            tracing::error!("remote access: tls config failed: {e}");
            "Couldn't start the remote access server.".to_string()
        })?;

    let app = build_router(shared);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let handle = axum_server::Handle::new();
    let serve_handle = handle.clone();
    let join = tokio::spawn(async move {
        if let Err(e) = axum_server::bind_rustls(addr, config)
            .handle(serve_handle)
            .serve(app.into_make_service())
            .await
        {
            tracing::error!("remote access: gateway server error: {e}");
        }
    });

    // Wait for the socket to actually be bound and accepting connections
    // before reporting success — otherwise a caller (the Tauri command, a
    // test, the UI) could see "enabled" a moment before it's actually
    // reachable. `None` means the serve task above already exited (e.g. the
    // port turned out to be unavailable after all).
    if handle.listening().await.is_none() {
        let _ = join.await;
        return Err("Couldn't start the remote access server.".to_string());
    }

    let advertisement = match super::discovery::Advertisement::start(
        device_id,
        "OpenPal",
        &fingerprint[..12.min(fingerprint.len())],
        port,
    ) {
        Ok(a) => Some(a),
        Err(e) => {
            // LAN discovery failing is a degraded experience (a client
            // falls back to typing an IP manually), not a reason to
            // refuse to serve at all.
            tracing::warn!("remote access: {e}");
            None
        }
    };

    Ok(RunningGateway {
        port,
        handle,
        join,
        advertisement,
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Validate the `Authorization: Bearer <token>` header against the paired
/// devices table. Every route except `/remote/v1/pair` requires this.
/// Returns a small `(StatusCode, &str)` rather than a full `Response` —
/// `Response` is too large for a `Result` error variant (clippy
/// `result_large_err`); callers convert via `.into_response()`.
fn authenticate(
    shared: &Shared,
    headers: &HeaderMap,
) -> Result<store::PairedDeviceInfo, (StatusCode, &'static str)> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or((StatusCode::UNAUTHORIZED, "Missing bearer token."))?;
    let now = now_ms();
    let conn = shared.app_db.lock().unwrap();
    let info = store::authenticate(&conn, token, now)
        .map_err(|e| {
            tracing::error!("remote access: auth lookup failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal error.")
        })?
        .ok_or((StatusCode::UNAUTHORIZED, "Invalid or revoked token."))?;
    // Distinct from "invalid/revoked" — a legitimate device just needs to
    // re-pair, not troubleshoot a bad token. `store::authenticate` itself
    // stays a pure token-to-device lookup; expiry is a route-level policy.
    if now >= info.expires_at_ms {
        return Err((
            StatusCode::UNAUTHORIZED,
            "This device's access has expired. Pair it again from Settings.",
        ));
    }
    Ok(info)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusResponse {
    engine: crate::types::EngineStatus,
}

async fn status_handler(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if let Err(resp) = authenticate(&shared, &headers) {
        return resp.into_response();
    }
    let engine = shared.engine.status().await;
    Json(StatusResponse { engine }).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairRequest {
    code: String,
    #[serde(default)]
    device_name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairResponse {
    device_id: String,
    device_name: String,
    token: String,
}

async fn pair_handler(
    State(shared): State<Arc<Shared>>,
    Json(body): Json<PairRequest>,
) -> Response {
    let mut guard = shared.pending_pairing.lock().await;
    let Some(mut session) = guard.take() else {
        return (StatusCode::NOT_FOUND, "No pairing session is open.").into_response();
    };

    let now = now_ms();
    if now > session.expires_at_ms {
        return (StatusCode::GONE, "This pairing session has expired.").into_response();
    }
    if session.code != body.code {
        session.attempts += 1;
        if session.attempts >= pairing::MAX_PAIRING_ATTEMPTS {
            // Cap hit: invalidate the session outright — don't reinsert
            // `session` (its embedded `tx` drops here), forcing a fresh
            // "Add a device" instead of leaving an open slot for an
            // attacker to keep guessing.
            return (
                StatusCode::UNAUTHORIZED,
                "Too many incorrect attempts. Ask to pair again from the other device.",
            )
                .into_response();
        }
        // Wrong code, still under the cap: keep the session open so the
        // user can retry within the remaining window, rather than forcing
        // "Add a device" again.
        let response = (StatusCode::UNAUTHORIZED, "That code didn't match.").into_response();
        *guard = Some(session);
        return response;
    }
    let Some(tx) = session.tx.take() else {
        return (
            StatusCode::CONFLICT,
            "This pairing session was already used.",
        )
            .into_response();
    };
    drop(guard); // session consumed either way from here — nothing left to reinsert

    let device_name = body
        .device_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "New device".to_string());
    let device_id = store::new_device_id();
    let token = store::new_device_token();
    let salt = store::new_salt();
    let hash = store::hash_token(&token, &salt);
    {
        let conn = shared.app_db.lock().unwrap();
        if let Err(e) =
            store::insert_paired_device(&conn, &device_id, &device_name, &hash, &salt, now)
        {
            tracing::error!("remote access: failed to persist paired device: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Couldn't finish pairing.",
            )
                .into_response();
        }
    }
    let _ = tx.send(device_name.clone());

    Json(PairResponse {
        device_id,
        device_name,
        token,
    })
    .into_response()
}

async fn chat_completions_handler(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(resp) = authenticate(&shared, &headers) {
        return resp.into_response();
    }
    let Some(endpoint) = shared.engine.endpoint().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "The engine isn't running on this desktop. Open OpenPal there, or turn on \
             Settings > AI model > \"Load model on startup\".",
        )
            .into_response();
    };

    let url = format!("{}/chat/completions", endpoint.base_url);
    let upstream = shared
        .http
        .post(&url)
        .bearer_auth(&endpoint.api_key)
        .header(header::CONTENT_TYPE, "application/json")
        .timeout(Duration::from_secs(endpoint.request_timeout_secs))
        .body(body)
        .send()
        .await;
    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("remote access: proxying to engine failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                "Couldn't reach the engine on this desktop.",
            )
                .into_response();
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let stream = upstream.bytes_stream();

    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Internal error.").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::EngineManager;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_shared() -> Arc<Shared> {
        let dir = tempfile::tempdir().unwrap().keep();
        let app_db_conn = crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0;
        let engine = EngineManager::new(dir.join("logs"));
        Arc::new(Shared {
            app_db: Arc::new(std::sync::Mutex::new(app_db_conn)),
            engine: Arc::new(engine),
            http: reqwest::Client::new(),
            pending_pairing: tokio::sync::Mutex::new(None),
        })
    }

    async fn body_string(response: Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[tokio::test]
    async fn chat_completions_without_token_is_unauthorized() {
        let shared = test_shared();
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn chat_completions_with_invalid_token_is_unauthorized() {
        let shared = test_shared();
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/v1/chat/completions")
                    .header(header::AUTHORIZATION, "Bearer not-a-real-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn chat_completions_with_expired_token_is_unauthorized() {
        let shared = test_shared();
        let token = "an-old-token";
        let salt = store::new_salt();
        let hash = store::hash_token(token, &salt);
        {
            let conn = shared.app_db.lock().unwrap();
            // `created_at_ms = 0` (the Unix epoch) puts `expires_at_ms` (90
            // days later) far in the past relative to any real `now_ms()`.
            store::insert_paired_device(&conn, "dev-1", "Old Device", &hash, &salt, 0).unwrap();
        }
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body_string(response).await.contains("expired"));
    }

    #[tokio::test]
    async fn chat_completions_with_valid_token_and_stopped_engine_is_service_unavailable() {
        let shared = test_shared();
        let token = "a-valid-token";
        let salt = store::new_salt();
        let hash = store::hash_token(token, &salt);
        {
            let conn = shared.app_db.lock().unwrap();
            store::insert_paired_device(&conn, "dev-1", "Test Device", &hash, &salt, now_ms())
                .unwrap();
        }
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/v1/chat/completions")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // The test engine was constructed but never started, so `endpoint()`
        // is `None` — the gateway must report this clearly rather than
        // erroring opaquely or silently hanging.
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(body_string(response).await.contains("isn't running"));
    }

    #[tokio::test]
    async fn status_requires_auth() {
        let shared = test_shared();
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::get("/remote/v1/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn pair_with_no_open_session_is_not_found() {
        let shared = test_shared();
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"123456"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn pair_wrong_code_keeps_session_open_for_retry() {
        let shared = test_shared();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = shared.pending_pairing.lock().await;
            *pending = Some(super::super::PendingPairing {
                code: "111111".to_string(),
                expires_at_ms: now_ms() + 60_000,
                attempts: 0,
                tx: Some(tx),
            });
        }
        let app = build_router(shared.clone());
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"000000"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        // The session must still be open for a retry with the right code.
        assert!(shared.pending_pairing.lock().await.is_some());
    }

    #[tokio::test]
    async fn pair_expired_session_is_gone() {
        let shared = test_shared();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = shared.pending_pairing.lock().await;
            *pending = Some(super::super::PendingPairing {
                code: "111111".to_string(),
                expires_at_ms: now_ms() - 1,
                attempts: 0,
                tx: Some(tx),
            });
        }
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"111111"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[tokio::test]
    async fn pair_correct_code_completes_and_persists_device() {
        let shared = test_shared();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = shared.pending_pairing.lock().await;
            *pending = Some(super::super::PendingPairing {
                code: "222222".to_string(),
                expires_at_ms: now_ms() + 60_000,
                attempts: 0,
                tx: Some(tx),
            });
        }
        let app = build_router(shared.clone());
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"code":"222222","deviceName":"Sarah's Phone"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The waiting `start_pairing` caller wakes up with the device name.
        assert_eq!(rx.await.unwrap(), "Sarah's Phone");
        // The session is consumed — a second attempt finds nothing open.
        assert!(shared.pending_pairing.lock().await.is_none());
        assert_eq!(
            store::list_paired_devices(&shared.app_db.lock().unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn pair_nth_wrong_attempt_invalidates_session_even_for_correct_code_next_time() {
        let shared = test_shared();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = shared.pending_pairing.lock().await;
            *pending = Some(super::super::PendingPairing {
                code: "111111".to_string(),
                expires_at_ms: now_ms() + 60_000,
                attempts: pairing::MAX_PAIRING_ATTEMPTS - 1,
                tx: Some(tx),
            });
        }
        let app = build_router(shared.clone());
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"000000"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body_string(response).await.contains("Too many"));
        assert!(shared.pending_pairing.lock().await.is_none());

        // Even the *correct* code now finds nothing open.
        let app = build_router(shared);
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"code":"111111"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn pair_correct_code_still_succeeds_just_under_the_attempt_cap() {
        let shared = test_shared();
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = shared.pending_pairing.lock().await;
            *pending = Some(super::super::PendingPairing {
                code: "222222".to_string(),
                expires_at_ms: now_ms() + 60_000,
                attempts: pairing::MAX_PAIRING_ATTEMPTS - 1,
                tx: Some(tx),
            });
        }
        let app = build_router(shared.clone());
        let response = app
            .oneshot(
                axum::http::Request::post("/remote/v1/pair")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"code":"222222","deviceName":"Sarah's Phone"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(rx.await.unwrap(), "Sarah's Phone");
    }
}
