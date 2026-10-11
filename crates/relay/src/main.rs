//! OpenPal sync relay — Phase 1 skeleton (docs/design/MULTI_DEVICE_SYNC.md).
//!
//! A bare-bones store-and-forward mailbox: push an opaque ciphertext blob to
//! a mailbox, pull pending blobs for a mailbox, ack (delete) one by id. It
//! exists to prove the client/relay plumbing works before any real
//! cryptography is wired through it — `openpal_sync_protocol::Envelope`'s
//! `ciphertext` is genuinely opaque bytes here, and this server is never
//! sent real user data in this phase (synthetic/test payloads only).
//!
//! # This is NOT production-ready as-is
//! There is **no authentication and no TLS termination** in this relay —
//! anyone who can reach it can push, read, and delete any mailbox by
//! guessing/enumerating its id. It must run behind a reverse proxy that
//! terminates TLS (`docs/RELAY_SELF_HOSTING.md`'s deploy guide covers both
//! a Docker Compose and a bare-metal + systemd path, plus why TLS is
//! mandatory — `openpal_core::sync::mailbox_client::validate_relay_url`
//! enforces this from the client side too). Phase 5
//! (`docs/design/MULTI_DEVICE_SYNC.md`) added a `/health` endpoint and per-source
//! rate limiting (`rate_limit.rs`) on top of this skeleton; it deliberately
//! did NOT add in-process auth or TLS — the reverse-proxy boundary remains
//! the access-control layer, matching every other self-hosted relay of
//! this shape (Standard Notes/Anytype's own self-hosting docs, cited in
//! the design doc, make the same choice).

mod rate_limit;
mod routes;
mod store;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use rate_limit::RateLimiter;
use store::MailboxStore;

/// How often the background sweep checks for expired envelopes. Purely
/// housekeeping (reclaims storage) — `store.rs`'s `pull`/`pending_count`
/// already filter expired rows out at read time regardless of whether the
/// sweep has run yet, so this interval only affects how promptly storage is
/// reclaimed, not correctness.
const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Periodically deletes expired envelopes across all mailboxes, and prunes
/// the rate limiter's stale per-source windows — two housekeeping jobs on
/// one loop, proportional to the scope of each. Sweeping mailbox rows is a
/// resource-exhaustion mitigation (bounding relay storage against
/// `ttl_seconds`-expired rows that were never acked); neither this nor the
/// rate-limiter pruning is an authentication mechanism — the relay stays
/// exactly as content-blind as before.
async fn run_sweep_loop(store: MailboxStore, rate_limiter: Arc<RateLimiter>) {
    let mut interval = tokio::time::interval(SWEEP_INTERVAL);
    // The first tick fires immediately; skip it so we don't sweep an
    // empty/fresh store the instant the relay starts.
    interval.tick().await;
    loop {
        interval.tick().await;
        match store.sweep_expired() {
            Ok(deleted) if deleted > 0 => {
                tracing::info!(deleted, "swept expired mailbox envelopes");
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!(?err, "failed to sweep expired mailbox envelopes");
            }
        }
        rate_limiter.prune_stale();
    }
}

fn configured_port() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787)
}

/// `openpal-relay --healthcheck`: an in-process exit-code probe for
/// environments without a general-purpose HTTP client available (the
/// `debian:bookworm-slim` Docker runtime image ships neither `curl` nor
/// `wget`, and adding one just for this narrow purpose would mean a new
/// piece of real attack surface in an otherwise-minimal image). Exercises
/// the actual `/health` handler over a raw TCP connection to this same
/// process's own port — not a bare TCP-connect probe, which would report
/// healthy even if the app layer (e.g. the SQLite check inside `/health`)
/// were wedged. Exit code only; deliberately prints nothing on success (a
/// Docker/systemd healthcheck only inspects the exit code).
async fn run_healthcheck() -> std::process::ExitCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let port = configured_port();
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return std::process::ExitCode::FAILURE;
    };
    let request = b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if stream.write_all(request).await.is_err() {
        return std::process::ExitCode::FAILURE;
    }
    let mut response = [0u8; 32];
    match stream.read(&mut response).await {
        Ok(n) if response[..n].starts_with(b"HTTP/1.1 200") => std::process::ExitCode::SUCCESS,
        _ => std::process::ExitCode::FAILURE,
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        return run_healthcheck().await;
    }

    tracing_subscriber::fmt::init();

    let db_path = std::env::var("RELAY_DB_PATH").unwrap_or_else(|_| "./relay.sqlite".to_string());
    let store = MailboxStore::open(&db_path).expect("failed to open relay mailbox store");
    tracing::info!(db_path = %db_path, "relay mailbox store ready");

    let rate_limit_per_minute = rate_limit::requests_per_minute_from_env();
    let rate_limiter = RateLimiter::new(rate_limit_per_minute);
    tracing::info!(rate_limit_per_minute, "relay rate limit configured");

    tokio::spawn(run_sweep_loop(store.clone(), rate_limiter.clone()));

    let port = configured_port();
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let app = routes::router(store, rate_limiter);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind relay listener");
    tracing::info!(%addr, "relay listening (no auth, no TLS — see module docs)");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("relay server error");
    std::process::ExitCode::SUCCESS
}
