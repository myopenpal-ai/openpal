//! Thin `reqwest` client for `crates/relay`'s push/pull/ack HTTP API
//! (`crates/relay/src/routes.rs`). Nothing in `crates/core` talked to the
//! relay before this module — Phase 1 only built the server side.
//!
//! Request/response shapes here intentionally mirror
//! `crates/relay/src/routes.rs`'s `PushEnvelopeRequest`/`StoredEnvelope`
//! field-for-field (base64 `ciphertext`, `version`, `created_at_ms`,
//! `ttl_seconds`) rather than importing them, since `crates/core` (a
//! library other things embed) shouldn't depend on `crates/relay` (a
//! standalone server binary) — `crates/sync-protocol`'s `Envelope` is the
//! actual shared wire type; these are the HTTP-layer DTOs around it.  A
//! version drift here would show up immediately as a JSON
//! serialize/deserialize mismatch against a real relay, caught by this
//! module's own integration test against an in-memory `crates/relay`
//! router (see `crate::sync::pairing`'s tests).
//!
//! No authentication, no TLS pinning here — the relay itself is
//! deliberately blind and unauthenticated (`docs/SYNC_PAIRING_PROTOCOL.md`'s
//! "Problem statement"); this client is exactly as trusting of the network
//! as that design already accounts for. [`validate_relay_url`] does gate
//! *transport* confidentiality against other observers on the path to a
//! non-local relay, though — see its own doc comment.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};

/// Where to reach the relay. `base_url` is runtime-mutable
/// ([`MailboxClient::set_base_url`]) — `crates/app`'s Settings → Devices →
/// Sync "Server" field can repoint a live client, not just set what a
/// freshly-constructed one starts with.
#[derive(Debug, Clone)]
pub struct MailboxClient {
    base_url: Arc<RwLock<String>>,
    client: reqwest::Client,
}

/// The sync relay's base URL. No "OpenPal default instance" is deployed
/// yet (that's Phase 5) — this defaults to `crates/relay`'s own local dev
/// port so the app has something to construct a [`MailboxClient`] against.
/// Overridable via `OPENPAL_SYNC_RELAY`, mirroring
/// `crate::account::api_base`'s env-override convention for pointing at a
/// self-hosted or dev instance. An override that fails
/// [`validate_relay_url`] is logged and ignored (falls back to the
/// compiled-in default) rather than crashing startup — this is a dev/ops
/// env var, not a beginner-facing setting, so "loud warning, don't brick
/// the app" is the right posture; `crates/app`'s Settings UI path instead
/// hard-rejects an invalid value at save time (see that command).
pub fn relay_base_url() -> String {
    const DEFAULT: &str = "http://127.0.0.1:8787";
    match std::env::var("OPENPAL_SYNC_RELAY") {
        Ok(url) => {
            match validate_relay_url(&url) {
                Ok(()) => url,
                Err(e) => {
                    tracing::error!("OPENPAL_SYNC_RELAY={url:?} is invalid ({e}); using the default relay instead");
                    DEFAULT.to_string()
                }
            }
        }
        Err(_) => DEFAULT.to_string(),
    }
}

/// Accepts `https://` unconditionally; accepts `http://` only when the host
/// is a loopback address (`127.0.0.0/8`, `::1`) or the literal hostname
/// `localhost` — anything else (a LAN IP, a real hostname) is exactly the
/// traffic TLS is meant to protect from other observers on that network
/// path, so it must use `https://`. `crates/relay` (the relay this project
/// ships) never terminates TLS itself by design — self-hosting docs say to
/// put a reverse proxy in front (`crates/relay/src/main.rs`'s own module
/// doc) — so this validates the URL a caller points AT, not anything this
/// crate itself terminates. Deliberately NOT relaxed to accept any
/// RFC1918/private-LAN address without `https://`: a LAN address is still
/// traffic other devices on that LAN could observe, which is the exact risk
/// requiring TLS in the first place.
pub fn validate_relay_url(url: &str) -> std::result::Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "Not a valid URL.".to_string())?;
    let is_loopback = parsed.host_str() == Some("localhost")
        || parsed
            .host_str()
            // `Url::host_str` renders an IPv6 host with its `[...]`
            // brackets (e.g. `"[::1]"`) — `IpAddr`'s `FromStr` doesn't
            // accept those, so strip them before parsing. A no-op for an
            // IPv4 host or `localhost`, neither of which has brackets.
            .map(|h| h.trim_start_matches('[').trim_end_matches(']'))
            .and_then(|h| h.parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if parsed.scheme() == "https" || is_loopback {
        Ok(())
    } else {
        Err("Non-local relay URLs must use https:// (plain http:// is only allowed for 127.0.0.1/localhost).".to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MailboxClientError {
    #[error("couldn't reach the sync server: {0}")]
    Network(#[from] reqwest::Error),
    #[error("the sync server rejected this mailbox (too many pending envelopes)")]
    TooManyPending,
    #[error("the sync server returned an unexpected response: {0}")]
    Malformed(String),
}

type Result<T> = std::result::Result<T, MailboxClientError>;

#[derive(Debug, Serialize)]
struct PushRequest {
    ciphertext: String,
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PushResponse {
    envelope_id: i64,
}

/// One envelope as returned by a pull — ciphertext still base64, same wire
/// shape as `crates/relay/src/routes.rs::StoredEnvelope`.
/// Never deserialized directly from wire JSON (its field names don't match
/// the relay's — `pull()` deserializes into the private `Wire` struct
/// first, then maps field-by-field) — constructed either by `pull()` or,
/// for tests exercising a [`MailboxTransport`] double that doesn't go
/// through real HTTP/JSON at all, via [`Self::new`].
#[derive(Debug, Clone)]
pub struct PulledEnvelope {
    pub envelope_id: i64,
    pub version: u32,
    pub mailbox_id: String,
    ciphertext_b64: String,
    pub created_at_ms: i64,
}

impl PulledEnvelope {
    /// `pub(crate)`, test-only: for `MailboxTransport` test doubles
    /// elsewhere in this crate (see `crate::sync::pairing`'s tests) to
    /// construct a `PulledEnvelope` without needing to fake real HTTP/JSON
    /// — not a general-purpose constructor for external consumers.
    /// `#[cfg(test)]` because its only caller is itself test-only code;
    /// without it, `cargo clippy -- -D warnings` (the non-test lib target)
    /// sees zero callers and errors on `dead_code`.
    #[cfg(test)]
    pub(crate) fn new(
        envelope_id: i64,
        version: u32,
        mailbox_id: String,
        ciphertext_b64: String,
        created_at_ms: i64,
    ) -> Self {
        Self {
            envelope_id,
            version,
            mailbox_id,
            ciphertext_b64,
            created_at_ms,
        }
    }

    /// Decodes the base64 wire payload. `None` on malformed base64 — the
    /// relay never validates ciphertext content (it's blind by design), so
    /// a corrupt/foreign payload is a client-side data problem, not a
    /// transport error; treated as "skip this envelope," same posture
    /// `crate::import`'s "one bad entry is skipped, never fatal" rule.
    pub fn ciphertext(&self) -> Option<Vec<u8>> {
        STANDARD.decode(&self.ciphertext_b64).ok()
    }
}

/// What `crate::sync::pairing::PairingManager` needs from a mailbox
/// transport — implemented for real by [`MailboxClient`] (HTTP against a
/// real `crates/relay`), and by an in-memory test double in
/// `pairing`'s own test module. `crates/core` deliberately doesn't depend
/// on the `crates/relay` binary crate (a library shouldn't depend on a
/// server binary) even for tests, so this trait — not a real relay
/// instance — is what makes the pairing protocol's logic testable without
/// a network.
pub trait MailboxTransport: Send + Sync {
    fn push(
        &self,
        mailbox_id: &str,
        ciphertext: &[u8],
        version: u32,
        ttl_seconds: Option<u64>,
    ) -> impl std::future::Future<Output = Result<i64>> + Send;

    fn pull(
        &self,
        mailbox_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<PulledEnvelope>>> + Send;

    fn ack(
        &self,
        mailbox_id: &str,
        envelope_id: i64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}

impl MailboxClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: Arc::new(RwLock::new(base_url.into())),
            client: reqwest::Client::new(),
        }
    }

    /// Repoints this client (and every clone sharing its handle — `Clone`
    /// on [`MailboxClient`] clones the `Arc`, not the string) at a new
    /// relay, live. `crates/app`'s `update_settings` command calls this
    /// after validating a new `Settings::sync_relay_url` via
    /// [`validate_relay_url`], so `AppState.pairing`'s single
    /// long-lived `PairingManager<MailboxClient>` doesn't need to be
    /// rebuilt (which would risk pairing and this client ending up pointed
    /// at two different relays).
    pub fn set_base_url(&self, url: String) {
        *self.base_url.write().unwrap() = url;
    }

    fn base_url(&self) -> String {
        self.base_url.read().unwrap().clone()
    }

    /// Pushes an already-sealed `ciphertext` blob to `mailbox_id`, with an
    /// explicit `ttl_seconds` (the relay clamps to its own
    /// `[60, 2_592_000]` range and defaults to 600 if this is `None` — see
    /// `crates/relay/src/routes.rs`'s `clamp_ttl_seconds`). Returns the
    /// assigned `envelope_id`. `429` (mailbox over its 200-pending cap) is
    /// surfaced as [`MailboxClientError::TooManyPending`], not folded into
    /// the generic network-error case, since a caller (the pairing poll
    /// loop) may want to react differently to it.
    pub async fn push(
        &self,
        mailbox_id: &str,
        ciphertext: &[u8],
        version: u32,
        ttl_seconds: Option<u64>,
    ) -> Result<i64> {
        let body = PushRequest {
            ciphertext: STANDARD.encode(ciphertext),
            version,
            created_at_ms: None,
            ttl_seconds,
        };
        let resp = self
            .client
            .post(format!("{}/mailbox/{mailbox_id}", self.base_url()))
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(MailboxClientError::TooManyPending);
        }
        let resp = resp
            .error_for_status()
            .map_err(MailboxClientError::Network)?;
        let parsed: PushResponse = resp
            .json()
            .await
            .map_err(|e| MailboxClientError::Malformed(e.to_string()))?;
        Ok(parsed.envelope_id)
    }

    /// Pulls every currently-pending (non-expired) envelope for
    /// `mailbox_id`, oldest first.
    pub async fn pull(&self, mailbox_id: &str) -> Result<Vec<PulledEnvelope>> {
        let resp = self
            .client
            .get(format!("{}/mailbox/{mailbox_id}", self.base_url()))
            .send()
            .await?
            .error_for_status()
            .map_err(MailboxClientError::Network)?;
        #[derive(Deserialize)]
        struct Wire {
            envelope_id: i64,
            version: u32,
            mailbox_id: String,
            ciphertext: String,
            created_at_ms: i64,
        }
        let wire: Vec<Wire> = resp
            .json()
            .await
            .map_err(|e| MailboxClientError::Malformed(e.to_string()))?;
        Ok(wire
            .into_iter()
            .map(|w| PulledEnvelope {
                envelope_id: w.envelope_id,
                version: w.version,
                mailbox_id: w.mailbox_id,
                ciphertext_b64: w.ciphertext,
                created_at_ms: w.created_at_ms,
            })
            .collect())
    }

    /// Acks (deletes) one envelope. Idempotent at the relay — acking an
    /// already-gone envelope is not an error here either.
    pub async fn ack(&self, mailbox_id: &str, envelope_id: i64) -> Result<()> {
        self.client
            .delete(format!(
                "{}/mailbox/{mailbox_id}/{envelope_id}",
                self.base_url()
            ))
            .send()
            .await?
            .error_for_status()
            .map_err(MailboxClientError::Network)?;
        Ok(())
    }
}

impl MailboxTransport for MailboxClient {
    async fn push(
        &self,
        mailbox_id: &str,
        ciphertext: &[u8],
        version: u32,
        ttl_seconds: Option<u64>,
    ) -> Result<i64> {
        MailboxClient::push(self, mailbox_id, ciphertext, version, ttl_seconds).await
    }

    async fn pull(&self, mailbox_id: &str) -> Result<Vec<PulledEnvelope>> {
        MailboxClient::pull(self, mailbox_id).await
    }

    async fn ack(&self, mailbox_id: &str, envelope_id: i64) -> Result<()> {
        MailboxClient::ack(self, mailbox_id, envelope_id).await
    }
}

#[cfg(test)]
mod tests {
    // `push`/`pull`/`ack` themselves only wrap HTTP calls to a real relay —
    // nothing meaningful to unit-test there without a live server. Coverage
    // of the wire format (base64 round-trip, field shapes) comes for free
    // from `crate::sync::pairing`'s tests, which exercise this client's
    // `MailboxTransport` impl indirectly through an in-memory double that
    // implements the same trait — see that module. `validate_relay_url` is
    // pure and worth testing directly, below.
    use super::*;

    #[test]
    fn validate_relay_url_accepts_loopback_over_plain_http() {
        assert!(validate_relay_url("http://127.0.0.1:8787").is_ok());
        assert!(validate_relay_url("http://localhost:8787").is_ok());
        assert!(validate_relay_url("http://[::1]:8787").is_ok());
    }

    #[test]
    fn validate_relay_url_accepts_any_host_over_https() {
        assert!(validate_relay_url("https://relay.example.com").is_ok());
        assert!(validate_relay_url("https://192.168.1.5:8787").is_ok());
        assert!(validate_relay_url("https://127.0.0.1:8787").is_ok());
    }

    #[test]
    fn validate_relay_url_rejects_non_loopback_plain_http() {
        assert!(validate_relay_url("http://relay.example.com").is_err());
        assert!(
            validate_relay_url("http://192.168.1.5:8787").is_err(),
            "a LAN IP is exactly the traffic https:// is meant to protect from other observers on that network"
        );
    }

    #[test]
    fn validate_relay_url_rejects_malformed_urls() {
        assert!(validate_relay_url("not a url").is_err());
        assert!(validate_relay_url("").is_err());
    }
}
