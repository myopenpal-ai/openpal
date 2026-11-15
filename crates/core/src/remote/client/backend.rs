//! `RemoteBackend`: the [`EngineBackend`] implementation that makes a
//! paired desktop's engine usable from this phone, over the same gateway
//! `crate::remote::gateway` serves. On Android this is the *only* backend
//! that can ever work at all — Android's W^X policy forbids exec'ing a
//! bundled `llama-server` binary the way desktop's `SubprocessBackend`
//! does — not an alternative chosen among several; see `docs/design/MOBILE.md`.
//!
//! `RemoteBackend` is cheap to `Clone` (an `Arc` around its real state) so
//! the same connection can be reached two ways: boxed into
//! `EngineManager::with_backend` for the generic chat/status surface every
//! platform shares, and held directly (e.g. by `crates/app`'s `AppState`)
//! for the pairing-specific actions ([`RemoteBackend::pair`],
//! [`RemoteBackend::forget`]) that aren't part of `EngineBackend` at all —
//! pairing is one-time setup, not something the mono-model chat/status
//! surface needs to know about.
//!
//! Deliberately out of scope for v1 (see the RemoteBackend implementation
//! plan): `start()`/model selection (there's no local model to load —
//! `docs/design/MOBILE.md`'s cut-features note), and `endpoint()` (kept at the trait's `None` default —
//! a generically-constructed `reqwest::Client` elsewhere in this crate,
//! e.g. `agent::orchestrator`'s tool-calling loop, has no reason to trust
//! this connection's pinned self-signed cert). `raw_classify` *is* wired
//! up: the gateway serves a `/remote/v1/classify` sibling of its chat
//! proxy, so request triage and the risk/safety check run on the paired
//! engine exactly as on the local one.

use super::{pairing, store, tls};
use crate::crypto::kdf::derive_subkey;
use crate::crypto::key::DataKey;
use crate::crypto::logframe;
use crate::crypto::secret_store::SecretStore;
use crate::engine::{build_api_messages, ActivityInfo, EngineBackend};
use crate::types::{ChatEvent, ChatMessage, EngineConfig, EngineStatus, SamplingParams};
use crate::{Error, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

/// HKDF `info` label binding the remote client log's derived key to this
/// store and no other (`derive_subkey`'s domain-separation contract) —
/// matches `SubprocessBackend`'s own `openpal-engine-log-v1` sibling.
const REMOTE_LOG_INFO: &[u8] = b"openpal-remote-client-log-v1";

const NOT_PAIRED_MSG: &str =
    "This phone isn't paired with a desktop yet. Open Settings to pair one.";

/// No local model to size a request budget against — use the same floor
/// `SubprocessBackend` guarantees an out-of-process client
/// (`EngineEndpoint::request_timeout_secs`'s own `MIN_REQUEST_TIMEOUT`).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const STATUS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// One-shot triage/risk classify requests are short (temperature 0,
/// grammar-constrained to one label) — mirror `SubprocessBackend`'s own
/// 20s classify budget rather than the 300s streaming one.
const CLASSIFY_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

struct Paired {
    record: store::RemotePairingRecord,
    token: String,
    /// Built once, at pairing (or at startup from a saved pairing) — pinned
    /// to `record.fingerprint_hex` (`tls::pinned_client`), so every request
    /// on this connection reuses the same verified TLS trust decision.
    client: reqwest::Client,
}

struct Inner {
    paired: Option<Paired>,
}

struct Shared {
    store: store::Store,
    inner: tokio::sync::Mutex<Inner>,
    log_dir: PathBuf,
    /// This store's own purpose-scoped log key, derived once from the DEK
    /// at unlock time (`set_dek`) — mirrors `SubprocessBackend`'s own
    /// posture: never holds the raw DEK, only its own subkey.
    log_key: std::sync::Mutex<Option<DataKey>>,
}

pub struct RemoteBackend {
    shared: Arc<Shared>,
}

impl Clone for RemoteBackend {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl RemoteBackend {
    /// `pairing_path` is a small non-secret JSON file (which desktop, which
    /// fingerprint); the bearer token itself lives in `secret_store`
    /// (`crate::account::AccountManager`'s own precedent for exactly this
    /// split). If a complete prior pairing is found, its pinned TLS client
    /// is rebuilt immediately so `status()`/`chat_stream()` work right
    /// away — there's no equivalent to `AccountManager::refresh()`'s lazy
    /// keychain read here, since every method on this backend needs the
    /// token constantly (the sidebar's status pill alone polls every 2s).
    pub fn new(
        pairing_path: PathBuf,
        log_dir: PathBuf,
        secret_store: Arc<dyn SecretStore>,
    ) -> Self {
        let store = store::Store::new(pairing_path, secret_store);
        let paired = store.load().and_then(|(record, token)| {
            match tls::pinned_client(&record.fingerprint_hex) {
                Ok(client) => Some(Paired {
                    record,
                    token,
                    client,
                }),
                Err(e) => {
                    tracing::warn!(
                        "remote client: couldn't rebuild the pinned TLS client from a saved pairing: {e}"
                    );
                    None
                }
            }
        });
        Self {
            shared: Arc::new(Shared {
                store,
                inner: tokio::sync::Mutex::new(Inner { paired }),
                log_dir,
                log_key: std::sync::Mutex::new(None),
            }),
        }
    }

    /// Complete pairing with `addr:port` using `code`, persist the result,
    /// and make it the active connection. Called from the `remote_pair`
    /// Tauri command — not part of [`EngineBackend`]'s surface, since
    /// pairing is one-time setup, not part of the mono-model chat/status
    /// contract every platform shares.
    pub async fn pair(
        &self,
        addr: &str,
        port: u16,
        code: &str,
        device_name: &str,
    ) -> std::result::Result<(), pairing::PairError> {
        let outcome = pairing::pair(addr, port, code, device_name).await?;
        let client = tls::pinned_client(&outcome.fingerprint_hex)
            .map_err(|message| pairing::PairError::Other { message })?;
        let record = store::RemotePairingRecord {
            device_id: outcome.device_id,
            device_name: outcome.device_name,
            host_addr: addr.to_string(),
            port,
            fingerprint_hex: outcome.fingerprint_hex,
            paired_at_ms: now_ms(),
        };
        self.shared
            .store
            .save(&record, &outcome.token)
            .map_err(|message| pairing::PairError::Other { message })?;
        self.log_line(&format!("paired with \"{}\"", record.device_name));
        self.shared.inner.lock().await.paired = Some(Paired {
            record,
            token: outcome.token,
            client,
        });
        Ok(())
    }

    /// Forget the current pairing, if any. Idempotent.
    pub async fn forget(&self) {
        self.shared.store.forget();
        self.shared.inner.lock().await.paired = None;
    }

    /// The current pairing record, if any — for the "manage pairing"
    /// settings screen (`remote_client_status` Tauri command), which wants
    /// the full picture (which desktop, its address, when pairing
    /// happened), not just the `EngineStatus` chat-availability summary
    /// `get_engine_status` already reflects on this backend.
    pub async fn paired_record(&self) -> Option<store::RemotePairingRecord> {
        self.shared
            .inner
            .lock()
            .await
            .paired
            .as_ref()
            .map(|p| p.record.clone())
    }

    fn log_path(&self) -> PathBuf {
        self.shared.log_dir.join("remote-client.log")
    }

    /// Append one line to the connectivity log, sealed immediately if a log
    /// key is set. Unlike `SubprocessBackend`'s `engine.log` (written by
    /// redirecting a child process's own stdout/stderr — no per-line
    /// interception point, hence that store's rotate-then-seal-previous
    /// compromise), every write here is a direct Rust-side append, so
    /// there's no bounded-plaintext window to accept: each line is sealed
    /// the moment it's written whenever a key is available. Reserved for
    /// real, infrequent lifecycle events (pairing, chat failures) — never
    /// called from `status()`'s poll loop, which would flood this file.
    fn log_line(&self, line: &str) {
        let path = self.log_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let bytes = match self.shared.log_key.lock().unwrap().as_ref() {
            Some(key) => logframe::seal_frame(key, 0, line.as_bytes()),
            None => {
                let mut b = line.as_bytes().to_vec();
                b.push(b'\n');
                b
            }
        };
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = file.write_all(&bytes);
        }
    }

    fn read_and_decrypt(&self, path: &Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(match self.shared.log_key.lock().unwrap().as_ref() {
            Some(key) if logframe::looks_like_frames(&bytes) => decrypt_frames(&bytes, key),
            _ => String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

#[async_trait]
impl EngineBackend for RemoteBackend {
    async fn start(
        &self,
        _server_binary: &Path,
        _model_id: &str,
        _model_path: &Path,
        _cfg: &EngineConfig,
        _disk_slow: bool,
        _startup_timeout: Duration,
    ) -> Result<()> {
        // There's no local model to load — `commands::start_engine`'s
        // model-selection/download flow doesn't run on this platform at
        // all (see `docs/design/MOBILE.md`). This exists only to satisfy the
        // trait; nothing calls it on Android today.
        Err(Error::Engine(NOT_PAIRED_MSG.to_string()))
    }

    /// Nothing local to stop.
    async fn stop(&self) -> Result<()> {
        Ok(())
    }

    async fn status(&self) -> EngineStatus {
        let snapshot = {
            let inner = self.shared.inner.lock().await;
            inner.paired.as_ref().map(|p| {
                (
                    p.client.clone(),
                    p.record.host_addr.clone(),
                    p.record.port,
                    p.token.clone(),
                    p.record.device_name.clone(),
                )
            })
        };
        let Some((client, addr, port, token, host_name)) = snapshot else {
            return EngineStatus::RemoteUnpaired;
        };

        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct StatusResponse {
            engine: EngineStatus,
        }

        let url = format!("https://{addr}:{port}/remote/v1/status");
        let response = client
            .get(&url)
            .bearer_auth(&token)
            .timeout(STATUS_REQUEST_TIMEOUT)
            .send()
            .await;
        match response {
            Ok(resp) if resp.status().is_success() => match resp.json::<StatusResponse>().await {
                Ok(body) => body.engine,
                Err(e) => {
                    tracing::warn!(
                        "remote client: malformed status response from {host_name}: {e}"
                    );
                    EngineStatus::RemoteUnreachable { host_name }
                }
            },
            Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                // The desktop revoked this device — nothing left to poll
                // until the user re-pairs.
                self.forget().await;
                EngineStatus::RemoteUnpaired
            }
            _ => EngineStatus::RemoteUnreachable { host_name },
        }
    }

    /// No local model, so nothing for the auto-unload idle sweep to ever
    /// act on.
    async fn activity(&self) -> Option<ActivityInfo> {
        None
    }

    async fn touch_activity(&self) {}

    async fn begin_generation(&self) {}

    async fn end_generation(&self) {}

    /// No subprocess to restart, so no generation to race against.
    async fn stop_if_current(&self, _expected_generation: u64) -> Result<()> {
        Ok(())
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        sampling: &SamplingParams,
        cancel: Arc<AtomicBool>,
        on_event: &mut (dyn FnMut(ChatEvent) + Send),
    ) -> Result<()> {
        let snapshot = {
            let inner = self.shared.inner.lock().await;
            inner.paired.as_ref().map(|p| {
                (
                    p.client.clone(),
                    p.record.host_addr.clone(),
                    p.record.port,
                    p.token.clone(),
                )
            })
        };
        let Some((client, addr, port, token)) = snapshot else {
            let message = NOT_PAIRED_MSG.to_string();
            on_event(ChatEvent::Error {
                message: message.clone(),
            });
            return Err(Error::Engine(message));
        };

        let mut body = serde_json::json!({
            "messages": build_api_messages(messages),
            "stream": true,
        });
        if let serde_json::Value::Object(map) = &mut body {
            sampling.merge_into(map);
        }

        let url = format!("https://{addr}:{port}/v1/chat/completions");
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .timeout(REQUEST_TIMEOUT)
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("remote chat request failed: {e}");
                let message =
                    "Couldn't reach the paired desktop. Make sure it's on and connected to the network."
                        .to_string();
                self.log_line(&format!("chat request failed: {e}"));
                on_event(ChatEvent::Error {
                    message: message.clone(),
                });
                return Err(Error::Engine(message));
            }
        };

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.forget().await;
            let message =
                "This phone's pairing was removed on the desktop. Pair again in Settings."
                    .to_string();
            self.log_line("chat request rejected: token no longer valid");
            on_event(ChatEvent::Error {
                message: message.clone(),
            });
            return Err(Error::Engine(message));
        }
        if !response.status().is_success() {
            // The gateway's own error text is already user-safe (e.g. "the
            // engine isn't running on this desktop") — pass it through
            // rather than replacing it with something generic.
            let message = response.text().await.unwrap_or_else(|_| {
                "The paired desktop couldn't process this message. Please try again.".to_string()
            });
            tracing::warn!("remote chat request rejected: {message}");
            self.log_line(&format!("chat request rejected: {message}"));
            on_event(ChatEvent::Error {
                message: message.clone(),
            });
            return Err(Error::Engine(message));
        }

        crate::sse::drain_sse(response.bytes_stream(), cancel, on_event, |e| {
            tracing::error!("remote chat stream error: {e}");
            "The reply was cut off. Please try sending your message again.".to_string()
        })
        .await
        .map_err(Error::Engine)
    }

    /// Request triage / risk classification against the paired desktop's
    /// engine, over the gateway's `/remote/v1/classify` route — the sibling
    /// of the chat-completions proxy. The body is exactly what
    /// `EngineManager::classify`/`classify_risk` build for the local
    /// backend (non-streaming, temperature 0, grammar-constrained); it
    /// travels to the remote engine unchanged, so both features behave
    /// precisely as they do locally.
    ///
    /// Returns `Ok(None)` — never an error — whenever there's nothing to
    /// report: not paired, the gateway unreachable, the token revoked, the
    /// remote engine not running, or the response unparseable.
    /// `EngineManager`'s callers already treat `Ok(None)` as "nothing to
    /// suggest this time", so a degraded remote connection stays silent
    /// exactly the way a degraded local engine does.
    async fn raw_classify(&self, body: serde_json::Value) -> Result<Option<String>> {
        let snapshot = {
            let inner = self.shared.inner.lock().await;
            inner.paired.as_ref().map(|p| {
                (
                    p.client.clone(),
                    p.record.host_addr.clone(),
                    p.record.port,
                    p.token.clone(),
                )
            })
        };
        let Some((client, addr, port, token)) = snapshot else {
            return Ok(None);
        };

        let url = format!("https://{addr}:{port}/remote/v1/classify");
        let response = client
            .post(&url)
            .bearer_auth(&token)
            .timeout(CLASSIFY_REQUEST_TIMEOUT)
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                // The desktop revoked this device's pairing — same
                // self-healing as `status()`'s poll loop, so the next poll
                // already reflects it rather than pinning a stale token.
                self.forget().await;
                return Ok(None);
            }
            Ok(response) => {
                tracing::warn!("remote classify request rejected: {}", response.status());
                return Ok(None);
            }
            Err(e) => {
                // `Ok(None)` for a failed request: triage/risk callers must
                // never surface a network error to the user — "silent
                // nothing to flag this time", exactly as locally.
                tracing::warn!("remote classify request failed: {e}");
                return Ok(None);
            }
        };

        let parsed: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("remote classify response wasn't valid JSON: {e}");
                return Ok(None);
            }
        };
        Ok(Some(
            parsed["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string(),
        ))
    }

    fn set_dek(&self, dek: Option<&DataKey>) {
        let key = dek.map(|d| derive_subkey(d, REMOTE_LOG_INFO));
        *self.shared.log_key.lock().unwrap() = key;
    }

    fn tail_log(&self, max_lines: usize) -> String {
        let text = self.read_and_decrypt(&self.log_path()).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].join("\n")
    }

    /// Just one generation — unlike `SubprocessBackend`'s `engine.log`/
    /// `engine.prev.log`, there's no per-session rotation boundary here
    /// (no "start"/"stop" of a local process to rotate around); this file
    /// covers the whole app lifetime.
    fn export_logs(&self) -> String {
        let text = self.read_and_decrypt(&self.log_path()).unwrap_or_default();
        format!("==== remote-client.log ====\n{text}\n")
    }

    fn log_path(&self) -> PathBuf {
        RemoteBackend::log_path(self)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Decrypt every frame in `bytes` under `key` (marker ignored — one fixed
/// key for this file's whole lifetime, mirroring `SubprocessBackend`'s own
/// `decrypt_frames`) and join the lines back with `\n`.
fn decrypt_frames(bytes: &[u8], key: &DataKey) -> String {
    use zeroize::Zeroizing;
    let key_bytes = Zeroizing::new(*key.as_bytes());
    let lines = logframe::open_all(bytes, &move |_marker| DataKey::from_bytes(*key_bytes));
    lines
        .into_iter()
        .map(|line| String::from_utf8_lossy(&line).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::lookup::KeyLookup;
    use crate::engine::EngineEndpoint;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSecretStore {
        entries: Mutex<HashMap<String, String>>,
    }

    impl SecretStore for FakeSecretStore {
        fn store(&self, key: &str, value: &str) -> std::result::Result<(), String> {
            self.entries
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        fn load(&self, key: &str) -> KeyLookup<String> {
            match self.entries.lock().unwrap().get(key) {
                Some(v) => KeyLookup::Found(v.clone()),
                None => KeyLookup::Absent,
            }
        }
        fn delete(&self, key: &str) -> std::result::Result<(), String> {
            self.entries.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn test_backend() -> (RemoteBackend, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let backend = RemoteBackend::new(
            dir.path().join("remote-pairing.json"),
            dir.path().join("logs"),
            Arc::new(FakeSecretStore::default()),
        );
        (backend, dir)
    }

    #[tokio::test]
    async fn unpaired_status_is_remote_unpaired() {
        let (backend, _dir) = test_backend();
        assert!(matches!(
            backend.status().await,
            EngineStatus::RemoteUnpaired
        ));
    }

    #[tokio::test]
    async fn unpaired_start_and_chat_stream_fail_without_any_network_call() {
        let (backend, _dir) = test_backend();
        let cfg = EngineConfig {
            context_length: 4096,
            gpu_layers: 0,
            threads: None,
            extra_args: Vec::new(),
            mmproj_path: None,
        };
        assert!(backend
            .start(
                Path::new("/bin/true"),
                "model",
                Path::new("/tmp/model.gguf"),
                &cfg,
                false,
                Duration::from_secs(1),
            )
            .await
            .is_err());

        let result = backend
            .chat_stream(
                &[],
                &SamplingParams::default(),
                Arc::new(AtomicBool::new(false)),
                &mut |_| {},
            )
            .await;
        assert!(matches!(result, Err(Error::Engine(_))));
    }

    #[tokio::test]
    async fn unpaired_defaults_never_panic() {
        let (backend, _dir) = test_backend();
        assert!(backend.stop().await.is_ok());
        assert!(backend.activity().await.is_none());
        backend.touch_activity().await;
        backend.begin_generation().await;
        backend.end_generation().await;
        assert!(backend.stop_if_current(0).await.is_ok());
        assert_eq!(
            backend.raw_classify(serde_json::json!({})).await.unwrap(),
            None
        );
        assert_eq!(backend.endpoint().await.map(|_| ()), None);
    }

    #[tokio::test]
    async fn pair_persists_and_is_reloaded_by_a_fresh_backend() {
        let dir = tempfile::tempdir().unwrap();
        let secret_store: Arc<dyn SecretStore> = Arc::new(FakeSecretStore::default());
        let pairing_path = dir.path().join("remote-pairing.json");

        // Drive a real gateway so `pair()` exercises the real handshake.
        let manager = test_gateway().await;
        manager.set_enabled(true).await.unwrap();
        let port = manager.status().await.port.unwrap();

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let pairing_task = tokio::spawn(async move {
            manager
                .start_pairing(move |event| events_clone.lock().unwrap().push(event))
                .await
                .unwrap();
        });
        let code = loop {
            let found = {
                let events = events.lock().unwrap();
                match events.first() {
                    Some(crate::remote::PairingEvent::Waiting { payload }) => {
                        Some(payload.code.clone())
                    }
                    _ => None,
                }
            };
            if let Some(code) = found {
                break code;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        let backend = RemoteBackend::new(
            pairing_path.clone(),
            dir.path().join("logs"),
            secret_store.clone(),
        );
        backend
            .pair("127.0.0.1", port, &code, "Test Phone")
            .await
            .unwrap();
        pairing_task.await.unwrap();
        assert_eq!(
            backend.paired_record().await.map(|r| r.device_name),
            Some("Test Phone".to_string())
        );

        // A fresh `RemoteBackend` pointed at the same paths must rebuild
        // the same paired connection without re-pairing.
        let reloaded = RemoteBackend::new(pairing_path, dir.path().join("logs"), secret_store);
        assert_eq!(
            reloaded.paired_record().await.map(|r| r.device_name),
            Some("Test Phone".to_string())
        );
    }

    async fn test_gateway() -> crate::remote::RemoteAccessManager {
        let dir = tempfile::tempdir().unwrap().keep();
        let app_db = Arc::new(std::sync::Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ));
        let engine = Arc::new(crate::engine::EngineManager::new(dir.join("logs")));
        crate::remote::RemoteAccessManager::new(app_db, engine)
    }

    /// A backend with no process of its own whose `endpoint()` points at a
    /// canned OpenAI-compatible server — stands in for the desktop's local
    /// engine *behind* the gateway, so `RemoteBackend::raw_classify` can be
    /// driven end to end (real TLS gateway, real pairing, real HTTP) without
    /// downloading a model. This is the "gateway-driver test" pattern the
    /// ticket asks for: triage/risk through the remote backend, over the
    /// wire the remote backend actually uses, no real network.
    struct CannedClassifyEngine {
        base_url: String,
    }

    #[async_trait]
    impl EngineBackend for CannedClassifyEngine {
        async fn start(
            &self,
            _server_binary: &Path,
            _model_id: &str,
            _model_path: &Path,
            _cfg: &EngineConfig,
            _disk_slow: bool,
            _startup_timeout: Duration,
        ) -> Result<()> {
            Err(Error::Engine("stand-in engine".to_string()))
        }
        async fn stop(&self) -> Result<()> {
            Ok(())
        }
        async fn status(&self) -> EngineStatus {
            EngineStatus::Stopped
        }
        async fn endpoint(&self) -> Option<EngineEndpoint> {
            Some(EngineEndpoint {
                base_url: self.base_url.clone(),
                api_key: "unused".to_string(),
                model_id: "fake".to_string(),
                request_timeout_secs: 30,
            })
        }
        async fn activity(&self) -> Option<ActivityInfo> {
            None
        }
        async fn touch_activity(&self) {}
        async fn begin_generation(&self) {}
        async fn end_generation(&self) {}
        async fn stop_if_current(&self, _expected_generation: u64) -> Result<()> {
            Ok(())
        }
        async fn chat_stream(
            &self,
            _messages: &[ChatMessage],
            _sampling: &SamplingParams,
            _cancel: Arc<AtomicBool>,
            _on_event: &mut (dyn FnMut(ChatEvent) + Send),
        ) -> Result<()> {
            Ok(())
        }
        async fn raw_classify(&self, _body: serde_json::Value) -> Result<Option<String>> {
            Ok(None)
        }
        fn set_dek(&self, _dek: Option<&DataKey>) {}
        fn tail_log(&self, _max_lines: usize) -> String {
            String::new()
        }
        fn export_logs(&self) -> String {
            String::new()
        }
        fn log_path(&self) -> PathBuf {
            PathBuf::new()
        }
    }

    /// A tiny fake engine server answering one `/v1/chat/completions`
    /// request with the OpenAI shape llama-server returns for a
    /// non-streaming completion, carrying `label` as the model's output —
    /// the shape `raw_classify` parses (`choices[0].message.content`).
    async fn canned_classify_server(label: &str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = format!("{{\"choices\":[{{\"message\":{{\"content\":\"{label}\"}}}}]}}");
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = vec![0u8; 65536];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
        (format!("http://{addr}"), handle)
    }

    /// Pair a fresh `RemoteBackend` with a real gateway whose local engine
    /// is a [`CannedClassifyEngine`] answering `label`, then run `body`
    /// through `raw_classify` and return what came back. The whole trip is
    /// real — HTTPS, bearer token, pinned TLS, the gateway's proxy — over
    /// loopback only.
    async fn classify_through_gateway(label: &str, body: serde_json::Value) -> Option<String> {
        let (base_url, server) = canned_classify_server(label).await;
        let dir = tempfile::tempdir().unwrap().keep();
        let app_db = Arc::new(std::sync::Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ));
        let engine = Arc::new(crate::engine::EngineManager::with_backend(Box::new(
            CannedClassifyEngine { base_url },
        )));
        let manager = crate::remote::RemoteAccessManager::new(app_db, engine);
        manager.set_enabled(true).await.unwrap();
        let port = manager.status().await.port.unwrap();

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let pairing_task = tokio::spawn(async move {
            manager
                .start_pairing(move |event| events_clone.lock().unwrap().push(event))
                .await
                .unwrap();
        });
        let code = loop {
            let found = {
                let events = events.lock().unwrap();
                match events.first() {
                    Some(crate::remote::PairingEvent::Waiting { payload }) => {
                        Some(payload.code.clone())
                    }
                    _ => None,
                }
            };
            if let Some(code) = found {
                break code;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        let backend = RemoteBackend::new(
            dir.join("remote-pairing.json"),
            dir.join("logs"),
            Arc::new(FakeSecretStore::default()),
        );
        backend
            .pair("127.0.0.1", port, &code, "Test Device")
            .await
            .unwrap();
        pairing_task.await.unwrap();

        let result = backend.raw_classify(body).await.unwrap();
        server.abort();
        result
    }

    /// The exact body `EngineManager::classify` builds (`build_classify_body`
    /// is private to `crate::engine`, so this mirrors it — same system
    /// prompt, `stream: false`, temperature 0, grammar-constrained). The
    /// grid is deliberately the real one: this is the protocol-level body
    /// the ticket's "must honour the same grammar constraints" refers to.
    fn triage_body() -> serde_json::Value {
        serde_json::json!({
            "messages": [
                {
                    "role": "system",
                    "content": "Classify the user's next message into exactly one \
                        category: general, coding, writing (including \
                        translation), mathScience (math or science reasoning), \
                        or agentic (asks the assistant to take actions/use tools, \
                        e.g. running code or editing files). Reply with only that \
                        one word, nothing else."
                },
                { "role": "user", "content": "hello" },
            ],
            "stream": false,
            "temperature": 0.0,
            "max_tokens": 8,
            "grammar": "root ::= \"general\" | \"coding\" | \"writing\" | \"mathScience\" | \"agentic\"",
        })
    }

    fn risk_body() -> serde_json::Value {
        serde_json::json!({
            "messages": [
                {
                    "role": "system",
                    "content": "Classify whether the user's message expresses \
                        suicidal ideation, an intent for self-harm, or a severe \
                        mental-health crisis that calls for pointing them to \
                        crisis support resources right now. Reply with only one \
                        word: \"risk\" if it does, or \"safe\" if it does not. \
                        Reply with only that one word, nothing else."
                },
                { "role": "user", "content": "i'm not okay" },
            ],
            "stream": false,
            "temperature": 0.0,
            "max_tokens": 8,
            "grammar": "root ::= \"risk\" | \"safe\"",
        })
    }

    /// The triage half of the ticket: a grammar-constrained classify body
    /// travels through the real gateway to the remote engine and the label
    /// comes back. Before `RemoteBackend::raw_classify` talked to
    /// `/remote/v1/classify`, this returned `None` no matter what.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raw_classify_pulls_the_triage_label_through_a_real_gateway() {
        assert_eq!(
            classify_through_gateway("general", triage_body())
                .await
                .as_deref(),
            Some("general")
        );
    }

    /// The risk half: same trip, safety label. Both features share one
    /// route; the body's system prompt is what tells the engine which
    /// classifier to run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raw_classify_pulls_the_risk_label_through_a_real_gateway() {
        assert_eq!(
            classify_through_gateway("safe", risk_body())
                .await
                .as_deref(),
            Some("safe")
        );
    }

    /// Not paired: `Ok(None)`, never an error, and no network attempt —
    /// the "silent nothing to report" contract is unchanged from the old
    /// always-`Ok(None)` behavior even when the gateway is unreachable.
    #[tokio::test]
    async fn raw_classify_unpaired_is_ok_none_without_any_network_call() {
        let (backend, _dir) = test_backend();
        assert_eq!(backend.raw_classify(triage_body()).await.unwrap(), None);
    }
}
