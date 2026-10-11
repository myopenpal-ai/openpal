//! Account state and premium entitlement caching.
//!
//! Sign-in is a browser-handoff "device session" flow: the app asks the
//! account backend for a session id and a URL, the app layer opens that URL
//! in the system browser (the login form itself lives there, not in the
//! webview — see `commands::sign_in`), and this module polls until the
//! session completes. This mirrors the GitHub CLI / Docker Desktop
//! device-login shape and needs no embedded webview auth form, no local
//! redirect server, and no new Tauri ACL.
//!
//! The refresh token is the only long-lived secret and lives in the OS
//! keychain — never on disk, never sent to the frontend, the same trust
//! boundary [`crate::engine::EngineEndpoint`]'s api key already uses. The
//! short-lived access token stays in memory for this run only. A small
//! non-secret cache (`account.json`: email/tier/capabilities) is written to
//! disk so the UI can show plan status instantly at boot, before any network
//! round trip.
//!
//! Entitlement checks anywhere in this app ([`AccountManager::has`],
//! `commands::require_entitlement`) are a UX guard, not a security boundary
//! — this is a public, MIT-licensed client, so the only real gate is the
//! backend refusing a cloud request without a valid access token.

use crate::crypto::secret_store::SecretStore;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECRET_KEY: &str = "account-refresh-token";

/// How long to poll a device session before giving up if the backend didn't
/// say how long it's good for.
const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Types sent to the UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub email: String,
    pub tier: String,
    pub capabilities: Vec<String>,
    /// Cloud request usage for the current billing period, when the backend
    /// reports one — some tiers (or the backend's early days) may not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota: Option<Quota>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    /// Cloud requests used in the current billing period.
    pub used: u64,
    /// Cloud requests included in the current plan.
    pub limit: u64,
}

/// Streamed by `sign_in` while a device session is in flight.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AccountEvent {
    /// A device session is ready; the app layer opens `verification_url` in
    /// the system browser. The user finishes signing in there.
    AwaitingBrowser {
        verification_url: String,
    },
    /// `persisted` is false when the refresh token couldn't be saved to the
    /// OS keychain (e.g. no Secret Service provider on a headless/minimal
    /// Linux desktop) — the user is signed in for this run, but will need to
    /// sign in again next launch. The UI should say so.
    Complete {
        account: Account,
        persisted: bool,
    },
    /// `message` is safe to show to a non-technical user.
    Failed {
        message: String,
    },
    Expired,
}

// ---------------------------------------------------------------------------
// On-disk cache (non-secret)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedAccount {
    account: Option<Account>,
    #[serde(default)]
    checked_at_ms: u64,
}

fn load_cache(path: &Path) -> CachedAccount {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Best-effort, atomic like `settings::save` — a failed write just means the
/// next boot re-fetches instead of showing a stale badge; never a hard error.
fn save_cache(path: &Path, cache: &CachedAccount) {
    let Ok(json) = serde_json::to_string_pretty(cache) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Keyring (secret: refresh token only)
// ---------------------------------------------------------------------------

/// Returns whether the token was actually saved — `sign_in` uses this to
/// warn the user their session won't survive a relaunch (see
/// `AccountEvent::Complete`'s `persisted` field) rather than failing silently.
fn store_refresh_token(store: &dyn SecretStore, token: &str) -> bool {
    match store.store(SECRET_KEY, token) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("couldn't save the sign-in session to the OS keychain: {e}");
            false
        }
    }
}

fn load_refresh_token(store: &dyn SecretStore) -> crate::crypto::lookup::KeyLookup<String> {
    store.load(SECRET_KEY)
}

fn clear_refresh_token(store: &dyn SecretStore) {
    let _ = store.delete(SECRET_KEY);
}

// ---------------------------------------------------------------------------
// Backend client — contract only; the account/billing service itself lives
// outside this repo (see docs/design/PORTABLE.md-style split: this is behavior, not
// something to fork the client over).
// ---------------------------------------------------------------------------

const UNREACHABLE_MSG: &str =
    "Couldn't reach the sign-in server. Check your connection and try again.";
const EXPIRED_MSG: &str = "Your session expired. Please sign in again.";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceSession {
    session_id: String,
    verification_url: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SessionPoll {
    Pending,
    Complete {
        access_token: String,
        refresh_token: String,
        account: Account,
    },
    Expired,
    Denied,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshedToken {
    access_token: String,
}

/// The account/billing backend's base URL. Overridable via
/// `OPENPAL_ACCOUNT_API` for local development against a stub server.
/// `pub(crate)`: `crate::cloud` reuses it — cloud chat and account/billing
/// share one backend (see that module's docs).
pub(crate) fn api_base() -> String {
    std::env::var("OPENPAL_ACCOUNT_API").unwrap_or_else(|_| "https://api.openpal.app".to_string())
}

async fn start_device_session(client: &reqwest::Client) -> Result<DeviceSession, String> {
    let resp = client
        .post(format!("{}/v1/desktop/session", api_base()))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("starting sign-in failed: {e}");
            UNREACHABLE_MSG.to_string()
        })?;
    resp.error_for_status()
        .map_err(|e| {
            tracing::warn!("starting sign-in failed: {e}");
            UNREACHABLE_MSG.to_string()
        })?
        .json()
        .await
        .map_err(|e| {
            tracing::warn!("sign-in response was malformed: {e}");
            "Something went wrong starting sign-in. Please try again.".to_string()
        })
}

async fn poll_device_session(
    client: &reqwest::Client,
    session_id: &str,
) -> Result<SessionPoll, String> {
    let resp = client
        .get(format!("{}/v1/desktop/session/{session_id}", api_base()))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("checking sign-in status failed: {e}");
            UNREACHABLE_MSG.to_string()
        })?;
    resp.error_for_status()
        .map_err(|e| {
            tracing::warn!("checking sign-in status failed: {e}");
            UNREACHABLE_MSG.to_string()
        })?
        .json()
        .await
        .map_err(|e| {
            tracing::warn!("sign-in status response was malformed: {e}");
            "Something went wrong checking sign-in status. Please try again.".to_string()
        })
}

async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<RefreshedToken, String> {
    let resp = client
        .post(format!("{}/v1/auth/refresh", api_base()))
        .json(&serde_json::json!({ "refreshToken": refresh_token }))
        .send()
        .await
        .map_err(|_| UNREACHABLE_MSG.to_string())?;
    resp.error_for_status()
        .map_err(|_| EXPIRED_MSG.to_string())?
        .json()
        .await
        .map_err(|_| "Something went wrong refreshing your session.".to_string())
}

async fn fetch_account(client: &reqwest::Client, access_token: &str) -> Result<Account, String> {
    let resp = client
        .get(format!("{}/v1/account/me", api_base()))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| UNREACHABLE_MSG.to_string())?;
    resp.error_for_status()
        .map_err(|_| EXPIRED_MSG.to_string())?
        .json()
        .await
        .map_err(|_| "Something went wrong loading your account.".to_string())
}

async fn revoke_session(client: &reqwest::Client, access_token: &str) {
    // Best-effort: the local sign-out already happened regardless of whether
    // the backend acknowledges the revoke.
    let _ = client
        .post(format!("{}/v1/auth/signout", api_base()))
        .bearer_auth(access_token)
        .send()
        .await;
}

// ---------------------------------------------------------------------------
// AccountManager
// ---------------------------------------------------------------------------

struct Inner {
    access_token: Option<String>,
    cache: CachedAccount,
    /// Bumped by `cancel_sign_in` (and at the start of every `sign_in`); an
    /// in-flight poll loop bails out once its captured value is stale,
    /// mirroring `EngineManager`'s `generation` guard against a superseded
    /// in-flight operation.
    sign_in_generation: u64,
}

pub struct AccountManager {
    cache_path: PathBuf,
    inner: tokio::sync::Mutex<Inner>,
    client: reqwest::Client,
    secret_store: Arc<dyn SecretStore>,
}

impl AccountManager {
    /// Loads the non-secret local cache synchronously — no network call, so
    /// this never blocks startup. The refresh token (if any) is only read
    /// from the keychain lazily, by `refresh()`.
    pub fn new(cache_path: PathBuf, secret_store: Arc<dyn SecretStore>) -> Self {
        let cache = load_cache(&cache_path);
        Self {
            cache_path,
            inner: tokio::sync::Mutex::new(Inner {
                access_token: None,
                cache,
                sign_in_generation: 0,
            }),
            client: reqwest::Client::new(),
            secret_store,
        }
    }

    /// The cached account snapshot, if any — instant, no network call.
    pub async fn cached(&self) -> Option<Account> {
        self.inner.lock().await.cache.account.clone()
    }

    /// The in-memory access token from the current run's sign-in/refresh, if
    /// any — used to authenticate a cloud chat request (`crate::cloud`).
    /// `None` until a sign-in or `refresh()` has succeeded this run, even if
    /// `cached()` shows an account: the token itself is never persisted to
    /// disk (see module docs), so a fresh launch starts with a cache but no
    /// live token until the boot-time `refresh()` completes.
    pub async fn access_token(&self) -> Option<String> {
        self.inner.lock().await.access_token.clone()
    }

    /// Whether the cached account has `capability`. A UX guard only — see
    /// module docs; the real gate is always the backend.
    pub async fn has(&self, capability: &str) -> bool {
        self.inner
            .lock()
            .await
            .cache
            .account
            .as_ref()
            .is_some_and(|a| a.capabilities.iter().any(|c| c == capability))
    }

    /// Silently refresh the cached account from a stored refresh token, if
    /// any. Meant to be called once, shortly after boot; failures (no stored
    /// token, expired session, offline) are logged and otherwise ignored —
    /// the app just keeps whatever was last cached.
    pub async fn refresh(&self) {
        let refresh_token = match load_refresh_token(self.secret_store.as_ref()) {
            crate::crypto::lookup::KeyLookup::Found(token) => token,
            crate::crypto::lookup::KeyLookup::Absent => return,
            crate::crypto::lookup::KeyLookup::Unavailable(reason) => {
                tracing::warn!("couldn't reach the OS keychain to refresh the session: {reason}");
                return;
            }
        };
        let refreshed = match refresh_access_token(&self.client, &refresh_token).await {
            Ok(r) => r,
            Err(e) => {
                tracing::info!("account refresh skipped: {e}");
                return;
            }
        };
        let account = match fetch_account(&self.client, &refreshed.access_token).await {
            Ok(a) => a,
            Err(e) => {
                tracing::info!("account fetch skipped: {e}");
                return;
            }
        };
        let mut inner = self.inner.lock().await;
        inner.access_token = Some(refreshed.access_token);
        inner.cache = CachedAccount {
            account: Some(account),
            checked_at_ms: now_ms(),
        };
        save_cache(&self.cache_path, &inner.cache);
    }

    /// Start a device-session sign-in and poll it to completion, emitting
    /// `on_event` along the way. Returns once the session completes, expires,
    /// is denied, is cancelled (via `cancel_sign_in`), or a request fails.
    pub async fn sign_in(
        &self,
        mut on_event: impl FnMut(AccountEvent) + Send,
    ) -> Result<(), String> {
        let generation = {
            let mut inner = self.inner.lock().await;
            inner.sign_in_generation += 1;
            inner.sign_in_generation
        };

        let session = start_device_session(&self.client).await?;
        on_event(AccountEvent::AwaitingBrowser {
            verification_url: session.verification_url.clone(),
        });

        let ttl = if session.expires_in > 0 {
            Duration::from_secs(session.expires_in)
        } else {
            DEFAULT_SESSION_TTL
        };
        let deadline = tokio::time::Instant::now() + ttl;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            if self.inner.lock().await.sign_in_generation != generation {
                return Ok(()); // superseded by cancel_sign_in or a new sign_in
            }
            if tokio::time::Instant::now() >= deadline {
                on_event(AccountEvent::Expired);
                return Ok(());
            }

            match poll_device_session(&self.client, &session.session_id).await {
                Ok(SessionPoll::Pending) => continue,
                Ok(SessionPoll::Complete {
                    access_token,
                    refresh_token,
                    account,
                }) => {
                    let persisted = store_refresh_token(self.secret_store.as_ref(), &refresh_token);
                    let mut inner = self.inner.lock().await;
                    inner.access_token = Some(access_token);
                    inner.cache = CachedAccount {
                        account: Some(account.clone()),
                        checked_at_ms: now_ms(),
                    };
                    save_cache(&self.cache_path, &inner.cache);
                    drop(inner);
                    on_event(AccountEvent::Complete { account, persisted });
                    return Ok(());
                }
                Ok(SessionPoll::Expired) => {
                    on_event(AccountEvent::Expired);
                    return Ok(());
                }
                Ok(SessionPoll::Denied) => {
                    let message = "Sign-in was cancelled.".to_string();
                    on_event(AccountEvent::Failed { message });
                    return Ok(());
                }
                Err(message) => {
                    on_event(AccountEvent::Failed {
                        message: message.clone(),
                    });
                    return Err(message);
                }
            }
        }
    }

    /// Stop an in-flight `sign_in` poll loop early. It returns `Ok(())` on
    /// its next tick without emitting a terminal event.
    pub async fn cancel_sign_in(&self) {
        self.inner.lock().await.sign_in_generation += 1;
    }

    pub async fn sign_out(&self) {
        let token = {
            let mut inner = self.inner.lock().await;
            let token = inner.access_token.take();
            inner.cache = CachedAccount::default();
            save_cache(&self.cache_path, &inner.cache);
            token
        };
        clear_refresh_token(self.secret_store.as_ref());
        if let Some(token) = token {
            revoke_session(&self.client, &token).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::secret_store::KeyringSecretStore;

    fn sample_account() -> Account {
        Account {
            email: "person@example.com".to_string(),
            tier: "pro".to_string(),
            capabilities: vec!["cloud-models".to_string()],
            quota: Some(Quota {
                used: 3,
                limit: 100,
            }),
        }
    }

    #[test]
    fn cache_roundtrip_and_corrupt_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");

        // Missing file -> empty cache, no panic.
        assert!(load_cache(&path).account.is_none());

        // Roundtrip.
        let cache = CachedAccount {
            account: Some(sample_account()),
            checked_at_ms: 42,
        };
        save_cache(&path, &cache);
        let loaded = load_cache(&path);
        assert_eq!(loaded.account, Some(sample_account()));
        assert_eq!(loaded.checked_at_ms, 42);

        // Corrupt file -> empty cache, no panic (matches settings::load's fallback).
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_cache(&path).account.is_none());
    }

    /// A cache written before `quota` existed (or a plan with no quota
    /// concept) must still load, with `quota: None` rather than failing to
    /// parse — same `#[serde(default)]` backward-compatibility contract as
    /// `Settings`.
    #[test]
    fn account_without_quota_round_trips_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        let mut account = sample_account();
        account.quota = None;
        save_cache(
            &path,
            &CachedAccount {
                account: Some(account),
                checked_at_ms: 0,
            },
        );

        let loaded = load_cache(&path);
        assert_eq!(loaded.account.unwrap().quota, None);
    }

    #[tokio::test]
    async fn new_loads_the_persisted_cache_and_has_reflects_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.json");
        save_cache(
            &path,
            &CachedAccount {
                account: Some(sample_account()),
                checked_at_ms: 0,
            },
        );

        let manager = AccountManager::new(path, Arc::new(KeyringSecretStore));
        assert_eq!(manager.cached().await, Some(sample_account()));
        assert!(manager.has("cloud-models").await);
        assert!(!manager.has("something-else").await);
    }

    #[tokio::test]
    async fn fresh_manager_has_no_cached_account() {
        let dir = tempfile::tempdir().unwrap();
        let manager = AccountManager::new(
            dir.path().join("account.json"),
            Arc::new(KeyringSecretStore),
        );
        assert_eq!(manager.cached().await, None);
        assert!(!manager.has("cloud-models").await);
    }

    #[tokio::test]
    async fn cancel_sign_in_bumps_generation_so_a_stale_poll_loop_would_bail() {
        let dir = tempfile::tempdir().unwrap();
        let manager = AccountManager::new(
            dir.path().join("account.json"),
            Arc::new(KeyringSecretStore),
        );
        let before = manager.inner.lock().await.sign_in_generation;
        manager.cancel_sign_in().await;
        let after = manager.inner.lock().await.sign_in_generation;
        assert_eq!(after, before + 1);
    }
}
