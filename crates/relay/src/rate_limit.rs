//! Per-source rate limiting for the mailbox routes — a floor against one
//! hammering client, not a defense against a distributed abuser (that's
//! explicitly out of scope, see `docs/RELAY_SELF_HOSTING.md`'s "what this
//! is not"). Fixed-window counter, hand-picked over pulling in
//! `tower_governor` for the same reason this crate already avoids most
//! dependencies (see `Cargo.toml`'s inline justifications elsewhere in this
//! crate) — this is well under 100 lines of counter logic, not worth a new
//! dependency subtree on a workspace already pinned to bleeding-edge
//! axum/hyper. Implemented as an `axum::middleware::from_fn_with_state`
//! function rather than a hand-rolled `tower::Layer`/`Service` pair — axum
//! already ships exactly this "stateful middleware, plain async fn"
//! primitive, so implementing `Service::poll_ready`/pinning by hand would
//! be more code for the same result.
//!
//! **Key extraction and its trust assumption**: every non-loopback
//! deployment of this relay requires a reverse proxy in front (enforced
//! client-side by `openpal_core::sync::mailbox_client::validate_relay_url`,
//! which refuses to save a plain-`http://` relay URL for anything but
//! loopback). Behind a reverse proxy, `ConnectInfo<SocketAddr>` reflects the
//! PROXY's address, not the real client's — collapsing "per-source" into one
//! shared bucket for every user behind that proxy, which would both
//! understate the limit for legitimate multi-device traffic and overstate
//! protection against a single real abuser. So this reads the first
//! `X-Forwarded-For` value when present (the standard convention Caddy/
//! nginx both set by default), falling back to `ConnectInfo` only when
//! absent (the direct-loopback, no-proxy case). This is only safe when the
//! relay is NOT reachable except through that configured proxy — a relay
//! exposed directly alongside a proxy would let a client spoof this header
//! and evade the limit entirely. `docs/RELAY_SELF_HOSTING.md` states this
//! trust assumption explicitly for self-hosters.

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

struct Bucket {
    window_start: Instant,
    count: u32,
}

/// Shared limiter state — one per relay process, handed to
/// `axum::middleware::from_fn_with_state`. `Arc`-wrapped so `main.rs` can
/// also hold a clone to call [`RateLimiter::prune_stale`] from the existing
/// housekeeping sweep loop.
pub struct RateLimiter {
    per_window: u32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new(requests_per_minute: u32) -> Arc<Self> {
        Arc::new(Self {
            per_window: requests_per_minute,
            buckets: Mutex::new(HashMap::new()),
        })
    }

    /// `Ok(())` if `key` is still under the limit for its current window
    /// (and records this request against it); `Err(retry_after)` if the
    /// window's cap is already spent.
    fn check(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");
        let bucket = buckets.entry(key.to_string()).or_insert_with(|| Bucket {
            window_start: now,
            count: 0,
        });
        if now.duration_since(bucket.window_start) >= WINDOW {
            bucket.window_start = now;
            bucket.count = 0;
        }
        if bucket.count >= self.per_window {
            return Err(WINDOW - now.duration_since(bucket.window_start));
        }
        bucket.count += 1;
        Ok(())
    }

    /// Evicts any bucket whose window has already elapsed — called from
    /// `main.rs`'s existing periodic sweep loop (same cadence already used
    /// to purge expired mailbox envelopes) so this map never grows
    /// unboundedly over a long-running process.
    pub fn prune_stale(&self) {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter mutex poisoned");
        buckets.retain(|_, bucket| now.duration_since(bucket.window_start) < WINDOW);
    }
}

/// First `X-Forwarded-For` value if present, else the immediate peer's
/// address from `ConnectInfo`, else a shared fallback bucket (no proxy
/// header and no connection info at all — e.g. a router built without
/// `into_make_service_with_connect_info`, which includes this file's own
/// unit tests and every existing `routes.rs` test). See this module's top
/// doc comment for the trust assumption behind preferring the
/// forwarded-for header.
fn rate_limit_key(headers: &HeaderMap, connect_info: Option<SocketAddr>) -> String {
    if let Some(value) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = value.split(',').map(str::trim).find(|s| !s.is_empty()) {
            return first.to_string();
        }
    }
    match connect_info {
        Some(addr) => addr.ip().to_string(),
        None => "unidentified".to_string(),
    }
}

pub async fn rate_limit_middleware(
    State(limiter): State<Arc<RateLimiter>>,
    req: Request,
    next: Next,
) -> Response {
    let connect_info = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let key = rate_limit_key(req.headers(), connect_info);
    match limiter.check(&key) {
        Ok(()) => next.run(req).await,
        Err(retry_after) => {
            let mut response = StatusCode::TOO_MANY_REQUESTS.into_response();
            if let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().max(1).to_string()) {
                response
                    .headers_mut()
                    .insert(axum::http::header::RETRY_AFTER, value);
            }
            response
        }
    }
}

/// Reads `RELAY_RATE_LIMIT_PER_MINUTE`, defaulting to 600 — see
/// `docs/RELAY_SELF_HOSTING.md`'s configuration reference for the
/// rationale (a single device polls its own mailbox every 5s per
/// `crates/app/src/sync_loop.rs`'s `TICK_INTERVAL`, so even a generous
/// multi-device household stays two orders of magnitude under this).
pub fn requests_per_minute_from_env() -> u32 {
    const DEFAULT: u32 = 600;
    std::env::var("RELAY_RATE_LIMIT_PER_MINUTE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_under_the_limit() {
        let limiter = RateLimiter::new(3);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
    }

    #[test]
    fn rejects_once_the_window_s_cap_is_spent() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
    }

    #[test]
    fn different_keys_have_independent_budgets() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.check("a").is_ok());
        assert!(limiter.check("a").is_err());
        assert!(
            limiter.check("b").is_ok(),
            "a different key must have its own, unspent budget"
        );
    }

    #[test]
    fn prune_stale_does_not_evict_a_still_live_window() {
        let limiter = RateLimiter::new(1);
        limiter.check("a").unwrap();
        limiter.prune_stale();
        assert!(
            limiter.check("a").is_err(),
            "a window that hasn't elapsed yet must survive a prune pass, count intact"
        );
    }

    #[test]
    fn rate_limit_key_prefers_forwarded_for_over_connect_info() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let connect_info = Some("10.0.0.1:12345".parse::<SocketAddr>().unwrap());
        assert_eq!(rate_limit_key(&headers, connect_info), "203.0.113.7");
    }

    #[test]
    fn rate_limit_key_falls_back_to_connect_info_when_no_forwarded_for_header() {
        let headers = HeaderMap::new();
        let connect_info = Some("198.51.100.9:9999".parse::<SocketAddr>().unwrap());
        assert_eq!(rate_limit_key(&headers, connect_info), "198.51.100.9");
    }

    #[test]
    fn rate_limit_key_falls_back_to_a_shared_bucket_when_nothing_is_available() {
        let headers = HeaderMap::new();
        assert_eq!(rate_limit_key(&headers, None), "unidentified");
    }
}
