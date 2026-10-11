//! Manage a `llama-server` subprocess and stream chat completions from it.
//!
//! ## Lifecycle contract
//!
//! - `start()`:
//!   1. If an engine is already running, stop it first (switching models).
//!   2. Pick a free port: bind a `std::net::TcpListener` to `127.0.0.1:0`,
//!      read the port, drop the listener.
//!   3. Spawn `server_binary` with `tokio::process::Command`,
//!      `.kill_on_drop(true)`, args:
//!      `-m <model_path> --port <port> --host 127.0.0.1 -c <cfg.context_length>
//!       -ngl <cfg.gpu_layers> --jinja --no-webui --api-key <token>`
//!      + `cfg.extra_args`, where `<token>` is a fresh random hex token per
//!        start so other local processes can't talk to the server.
//!        Redirect stdout+stderr to `<log_dir>/engine.log` (truncate per start —
//!        this file is our #1 debugging tool).
//!   4. Status: `Starting`. Poll `GET /health` every 300ms — `/health` is
//!      public even with `--api-key` (verified llama.cpp behavior), so the
//!      poll needs no key. Verified behavior
//!      (llama.cpp b9606): connection refused for a brief window before the
//!      listener binds, then 503 `{"error":{"message":"Loading model",...}}`
//!      while loading, then 200 `{"status":"ok"}`. Treat connection errors and
//!      503 alike as "still starting". Big models can take a while: time out
//!      after the caller's `startup_timeout` (see [`STARTUP_TIMEOUT`] for the
//!      normal default; a model running in "disk slow" mode needs a much
//!      longer budget — see [`disk_slow_startup_timeout`]). Between polls,
//!      also check the child: if it exited, read the tail of engine.log and
//!      fail with `Error::Engine` using a friendly message (the log tail goes
//!      to `tracing::error!`, not the user).
//!   5. Status: `Running { model_id, port }`. `start()` returns only then.
//!
//! Overlapping calls are safe: every change of child ownership bumps a
//! generation counter, and an in-flight `start()` bails out (without touching
//! the child or status) as soon as it sees it has been superseded by another
//! `start()`/`stop()`.
//! - `stop()`: kill the child (`child.kill().await`), wait, status `Stopped`.
//!   Idempotent — never errors if already stopped.
//! - If the child dies while Running (detected on the next chat call's
//!   connection error), set status `Failed` with a friendly message.
//!
//! ## Chat contract (`chat_stream`)
//!
//! - Requires status Running, else `Error::Engine` with friendly message.
//! - `POST http://127.0.0.1:<port>/v1/chat/completions` with header
//!   `Authorization: Bearer <token>` (the per-start key) and JSON body:
//!   `{ "messages": [...], "stream": true, ...SamplingParams }` — the caller's
//!   [`crate::types::SamplingParams`] (temperature, top_p, top_k, min_p,
//!   repeat_penalty, repeat_last_n, presence_penalty, frequency_penalty,
//!   max_tokens, seed) merged in via `SamplingParams::merge_into`.
//! - The response is an OpenAI-compatible SSE stream, drained by
//!   [`crate::sse::drain_sse`] — see that module for the wire format and the
//!   cancel/error/`ChatEvent` contract, which [`crate::cloud`]'s cloud-model
//!   chat shares exactly.
//!
//! The manager is shared behind `Arc` by the app layer; all methods take
//! `&self` (interior mutability via `tokio::sync::Mutex`).

use super::{ActivityInfo, EngineBackend, EngineEndpoint};
use crate::crypto::kdf::derive_subkey;
use crate::crypto::key::DataKey;
use crate::crypto::logframe;
use crate::types::{ChatEvent, ChatMessage, EngineConfig, EngineStatus, SamplingParams};
use crate::{Error, Result};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use zeroize::Zeroizing;

/// HKDF `info` label binding the engine log's derived key to this store and
/// no other (`derive_subkey`'s domain-separation contract) — matches the app
/// log's own `openpal-app-log-v1` sibling in `crates/app/src/logging.rs`.
const ENGINE_LOG_INFO: &[u8] = b"openpal-engine-log-v1";

const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(300);

const NOT_RUNNING_MSG: &str = "Your AI isn't ready yet. Pick a model, then try again.";
const CRASHED_MSG: &str =
    "Your AI stopped unexpectedly. Click the status light in the corner to wake it again.";

pub struct SubprocessBackend {
    inner: tokio::sync::Mutex<Inner>,
    client: reqwest::Client,
    log_dir: PathBuf,
    /// This store's own purpose-scoped log key, derived once from the DEK at
    /// unlock time (`set_dek`) — `SubprocessBackend` never holds the raw DEK
    /// itself, only its own subkey, same domain-separation posture as every
    /// other encrypted store. `None` in portable mode (no DEK) or before
    /// unlock.
    log_key: std::sync::Mutex<Option<DataKey>>,
}

struct Inner {
    status: EngineStatus,
    child: Option<tokio::process::Child>,
    /// Bumped on every change of child ownership (start/stop/fail). An
    /// in-flight `start()` compares it to the value it captured when it
    /// installed its child, and bails out once superseded.
    generation: u64,
    /// Per-start `--api-key` token; `chat_stream` sends it as a Bearer header.
    api_key: String,
    /// Set at `start()` from the caller's `disk_slow` flag (mirrors
    /// `commands::start_engine`'s own `selection::exceeds_ram` check).
    disk_slow: bool,
    /// Instant the engine was last "active": became `Running`, a chat turn
    /// finished, or was explicitly snoozed. The auto-unload idle clock.
    last_activity: Instant,
    /// True while a chat turn (`plain_chat`/`web_search_chat_turn`) is in
    /// flight — the auto-unload idle timer must never fire mid-generation.
    generating: bool,
    /// Set from `start()`'s `startup_timeout`, floored at 300s — see
    /// [`EngineEndpoint::request_timeout_secs`].
    request_timeout: Duration,
}

/// Floor for the per-request timeout an out-of-process client (the web-search
/// sidecar) is told to use, matching the sidecar's own prior flat default —
/// a model that starts up quickly shouldn't get a *shorter* per-request
/// budget than before just because `STARTUP_TIMEOUT` (180s) is less than
/// that default.
const MIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

impl SubprocessBackend {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(Inner {
                status: EngineStatus::Stopped,
                child: None,
                generation: 0,
                api_key: String::new(),
                disk_slow: false,
                last_activity: Instant::now(),
                generating: false,
                request_timeout: MIN_REQUEST_TIMEOUT,
            }),
            client: reqwest::Client::new(),
            log_dir,
            log_key: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl EngineBackend for SubprocessBackend {
    /// Set (or clear, in portable mode with no DEK) this engine's log key —
    /// called once from `state::finish_unlock`, right alongside
    /// `crate::logging::upgrade_to_encrypted`'s app-log equivalent. Derives
    /// and stores only the purpose-scoped subkey; the raw DEK never enters
    /// `SubprocessBackend` at all.
    fn set_dek(&self, dek: Option<&DataKey>) {
        let key = dek.map(|d| derive_subkey(d, ENGINE_LOG_INFO));
        *self.log_key.lock().unwrap() = key;
    }

    /// Last `max_lines` lines of the currently-active engine log. The live
    /// file is written by redirecting the child process's own stdout/stderr
    /// file descriptors directly to it (see `spawn_server`) — there is no
    /// Rust-side write to intercept per line, so this almost always hits the
    /// plaintext branch of [`Self::read_and_decrypt`] for the *active* file;
    /// encryption happens at rotation instead (see
    /// `encrypt_log_file_in_place`'s doc comment for why that
    /// bounded-plaintext-window trade-off is deliberate, not a gap).
    fn tail_log(&self, max_lines: usize) -> String {
        let text = self.read_and_decrypt(&self.log_path()).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(max_lines);
        lines[start..].join("\n")
    }

    /// Both engine log generations — the active `engine.log` (plaintext, in
    /// practice) and the prior session's `engine.prev.log` (sealed at rest
    /// if a log key is set) — decrypted and concatenated with headers, for
    /// "Export logs", which wants everything rather than just a tail.
    fn export_logs(&self) -> String {
        let mut out = String::new();
        for (label, path) in [
            ("engine.log", self.log_path()),
            ("engine.prev.log", self.log_dir.join("engine.prev.log")),
        ] {
            if let Some(text) = self.read_and_decrypt(&path) {
                out.push_str(&format!("==== {label} ====\n{text}\n\n"));
            }
        }
        out
    }

    /// Start (or restart) the engine with the given model. Returns once the
    /// server is ready to chat. See module docs for the full contract.
    /// `disk_slow` is the caller's `selection::exceeds_ram` classification for
    /// this model — stored so the auto-unload feature can exempt disk-slow
    /// models by default. `startup_timeout` is how long to wait for `/health`
    /// before giving up — pass [`STARTUP_TIMEOUT`] normally, or
    /// [`disk_slow_startup_timeout`] for a model running in "disk slow" mode.
    async fn start(
        &self,
        server_binary: &Path,
        model_id: &str,
        model_path: &Path,
        cfg: &EngineConfig,
        disk_slow: bool,
        startup_timeout: Duration,
    ) -> Result<()> {
        self.stop().await?;

        let port = match free_port() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("could not pick a free port: {e}");
                return Err(self.fail("Your AI couldn't start. Please try again.").await);
            }
        };
        let api_key = generate_api_key(port);

        let child = match self.spawn_server(server_binary, model_path, port, cfg, &api_key) {
            Ok(child) => child,
            Err(e) => {
                tracing::error!("could not launch {server_binary:?}: {e}");
                return Err(self
                    .fail("Your AI couldn't be launched. Reinstalling OpenPal usually fixes this.")
                    .await);
            }
        };
        let my_gen = {
            let mut inner = self.inner.lock().await;
            inner.generation += 1;
            inner.child = Some(child);
            inner.api_key = api_key;
            inner.request_timeout = startup_timeout.max(MIN_REQUEST_TIMEOUT);
            inner.status = EngineStatus::Starting {
                model_id: model_id.to_string(),
                progress_percent: None,
            };
            inner.generation
        };

        // Real per-byte progress is only observable for a GPU-offloaded load
        // (`-ngl` > 0): llama.cpp mmaps weights and a CPU-only load pages them
        // in lazily on first use, so RSS stays flat while "loading" and there
        // is nothing honest to report. A GPU-offloaded load, though, must
        // synchronously copy each offloaded layer's bytes off the mmap into
        // VRAM before `/health` goes ready, so the child's RSS climbing
        // toward the model file's size is a real, OS-observed proxy for how
        // far that copy has gotten — see `EngineStatus::Starting`'s doc.
        let load_progress_denominator = (cfg.gpu_layers > 0)
            .then(|| std::fs::metadata(model_path).ok())
            .flatten()
            .map(|m| m.len())
            .filter(|&len| len > 0);
        let mut progress_sys = sysinfo::System::new();
        // Ratchets forward only — RSS can wobble slightly from one 300ms
        // sample to the next, and a progress indicator visibly ticking
        // backward reads as broken even though the underlying number is real.
        let mut best_progress_percent: u8 = 0;

        // Poll /health until ready, watching for an early exit. The lock is
        // held only for the brief try_wait() each round, never across awaits
        // on the network.
        let health_url = format!("http://127.0.0.1:{port}/health");
        let deadline = Instant::now() + startup_timeout;
        loop {
            {
                let mut inner = self.inner.lock().await;
                if inner.generation != my_gen {
                    // Another start()/stop() took over: the child (if any) is
                    // theirs now, so leave everything alone and bow out.
                    return Err(Error::Engine(NOT_RUNNING_MSG.to_string()));
                }
                let Some(child) = inner.child.as_mut() else {
                    // Someone called stop() while we were starting.
                    return Err(Error::Engine(NOT_RUNNING_MSG.to_string()));
                };
                if let Ok(Some(exit)) = child.try_wait() {
                    inner.child = None;
                    inner.generation += 1;
                    let tail = log_tail(&self.log_path());
                    let message = unsupported_architecture_hint(&tail).unwrap_or_else(|| {
                        "Your AI stopped while loading. Try again, or pick a different model."
                            .to_string()
                    });
                    inner.status = EngineStatus::Failed {
                        message: message.clone(),
                    };
                    drop(inner);
                    tracing::error!(
                        "engine exited during startup ({exit}){}; log tail:\n{}",
                        oom_hint(&exit),
                        tail
                    );
                    return Err(Error::Engine(message));
                }

                if let Some(total_len) = load_progress_denominator {
                    if let Some(pid) = child.id() {
                        let sys_pid = sysinfo::Pid::from_u32(pid);
                        progress_sys
                            .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);
                        if let Some(proc) = progress_sys.process(sys_pid) {
                            let sampled = (proc.memory() * 100 / total_len).min(99) as u8;
                            best_progress_percent = best_progress_percent.max(sampled);
                            inner.status = EngineStatus::Starting {
                                model_id: model_id.to_string(),
                                progress_percent: Some(best_progress_percent),
                            };
                        }
                    }
                }
            }

            let ready = matches!(
                self.client
                    .get(&health_url)
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await,
                Ok(resp) if resp.status().is_success()
            );
            if ready {
                break;
            }

            if Instant::now() >= deadline {
                let message = "The AI model is taking too long to load. Try a smaller model, or \
                               close other apps and try again.";
                {
                    let mut inner = self.inner.lock().await;
                    if inner.generation != my_gen {
                        // Superseded: the child belongs to a newer start().
                        return Err(Error::Engine(NOT_RUNNING_MSG.to_string()));
                    }
                    if let Some(mut child) = inner.child.take() {
                        let _ = child.kill().await;
                    }
                    inner.generation += 1;
                    inner.status = EngineStatus::Failed {
                        message: message.to_string(),
                    };
                }
                tracing::error!(
                    "engine startup timed out after {startup_timeout:?}; log tail:\n{}",
                    log_tail(&self.log_path())
                );
                return Err(Error::Engine(message.to_string()));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }

        let mut inner = self.inner.lock().await;
        if inner.generation != my_gen {
            // stop() or another start() won the race after our health check.
            return Err(Error::Engine(NOT_RUNNING_MSG.to_string()));
        }
        inner.disk_slow = disk_slow;
        // The idle clock starts when the model becomes ready to chat, not
        // from whenever the caller clicked "Load".
        inner.last_activity = Instant::now();
        inner.generating = false;
        inner.status = EngineStatus::Running {
            model_id: model_id.to_string(),
            port,
            disk_slow,
            // Core has no knowledge of `Settings`; `commands::get_engine_status`
            // fills this in fresh on every poll.
            auto_unload_at: None,
        };
        Ok(())
    }

    /// Kill the engine if running. Idempotent.
    async fn stop(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(mut child) = inner.child.take() {
            if let Err(e) = child.kill().await {
                tracing::warn!("engine kill failed (already gone?): {e}");
            }
        }
        inner.generation += 1;
        inner.status = EngineStatus::Stopped;
        Ok(())
    }

    async fn status(&self) -> EngineStatus {
        self.inner.lock().await.status.clone()
    }

    /// Connection details for the running engine, or `None` if it isn't
    /// running. Lets the web-search sidecar reach the same OpenAI endpoint
    /// `chat_stream` uses, with the same per-start bearer token.
    async fn endpoint(&self) -> Option<EngineEndpoint> {
        let inner = self.inner.lock().await;
        match &inner.status {
            EngineStatus::Running { model_id, port, .. } => Some(EngineEndpoint {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                api_key: inner.api_key.clone(),
                model_id: model_id.clone(),
                request_timeout_secs: inner.request_timeout.as_secs(),
            }),
            _ => None,
        }
    }

    /// Idle/activity snapshot for the auto-unload feature, or `None` unless
    /// the engine is currently `Running`.
    async fn activity(&self) -> Option<ActivityInfo> {
        let inner = self.inner.lock().await;
        matches!(inner.status, EngineStatus::Running { .. }).then(|| ActivityInfo {
            disk_slow: inner.disk_slow,
            idle_for: inner.last_activity.elapsed(),
            generating: inner.generating,
            generation: inner.generation,
        })
    }

    /// Reset the auto-unload idle clock to now. Called after a chat turn
    /// completes, and by the explicit "snooze" command — both are just
    /// "activity happened". A no-op unless the engine is `Running`.
    async fn touch_activity(&self) {
        let mut inner = self.inner.lock().await;
        if matches!(inner.status, EngineStatus::Running { .. }) {
            inner.last_activity = Instant::now();
        }
    }

    /// Mark a chat turn starting. Pairs with [`Self::end_generation`].
    async fn begin_generation(&self) {
        self.inner.lock().await.generating = true;
    }

    /// Mark a chat turn ending, and reset the idle clock — the auto-unload
    /// countdown restarts fresh from when the reply finished, not when it
    /// began generating.
    async fn end_generation(&self) {
        let mut inner = self.inner.lock().await;
        inner.generating = false;
        if matches!(inner.status, EngineStatus::Running { .. }) {
            inner.last_activity = Instant::now();
        }
    }

    /// Like [`Self::stop`], but a no-op if the engine has moved on to a
    /// different generation since `expected_generation` was captured — guards
    /// the auto-unload background loop against killing a model that was
    /// manually switched in the narrow window between "decided to unload"
    /// and "actually unloading".
    async fn stop_if_current(&self, expected_generation: u64) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.generation != expected_generation {
            return Ok(());
        }
        if let Some(mut child) = inner.child.take() {
            if let Err(e) = child.kill().await {
                tracing::warn!("engine kill failed (already gone?): {e}");
            }
        }
        inner.generation += 1;
        inner.status = EngineStatus::Stopped;
        Ok(())
    }

    /// Stream one assistant reply. See module docs for the full contract.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        sampling: &SamplingParams,
        cancel: Arc<AtomicBool>,
        on_event: &mut (dyn FnMut(ChatEvent) + Send),
    ) -> Result<()> {
        // Read the port and key, then stream without holding the lock.
        let (port, api_key) = {
            let inner = self.inner.lock().await;
            match &inner.status {
                EngineStatus::Running { port, .. } => (*port, inner.api_key.clone()),
                _ => return Err(Error::Engine(NOT_RUNNING_MSG.to_string())),
            }
        };

        let mut body = serde_json::json!({
            "messages": super::build_api_messages(messages),
            "stream": true,
        });
        if let serde_json::Value::Object(map) = &mut body {
            sampling.merge_into(map);
        }
        let response = self
            .client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                let hint = self.child_oom_hint().await;
                tracing::error!("chat request failed: {e}{hint}");
                let message = if self.child_has_exited().await {
                    self.fail(CRASHED_MSG).await;
                    CRASHED_MSG
                } else {
                    "Something went wrong talking to your AI. Please try again."
                };
                on_event(ChatEvent::Error {
                    message: message.to_string(),
                });
                return Err(Error::Engine(message.to_string()));
            }
        };
        if !response.status().is_success() {
            tracing::error!("chat request rejected: {}", response.status());
            let message = "Your AI couldn't process this message. Please try again.";
            on_event(ChatEvent::Error {
                message: message.to_string(),
            });
            return Err(Error::Engine(message.to_string()));
        }

        crate::sse::drain_sse(response.bytes_stream(), cancel, on_event, |e| {
            tracing::error!("chat stream error: {e}");
            "The reply was cut off. Please try sending your message again.".to_string()
        })
        .await
        .map_err(Error::Engine)
    }

    /// Shared plumbing behind [`classify`](Self::classify) and
    /// [`classify_risk`](Self::classify_risk): post `body` to the
    /// already-running engine's `/v1/chat/completions` and return the
    /// trimmed label string it replied with. Returns `Ok(None)`, never an
    /// error, whenever there's nothing useful to do: the engine isn't
    /// `Running`, a real chat turn is already generating (llama-server has a
    /// single inference slot; never contend with an actual reply), or the
    /// request/response fails.
    async fn raw_classify(&self, body: serde_json::Value) -> Result<Option<String>> {
        let (port, api_key) = {
            let inner = self.inner.lock().await;
            if inner.generating {
                return Ok(None);
            }
            match &inner.status {
                EngineStatus::Running { port, .. } => (*port, inner.api_key.clone()),
                _ => return Ok(None),
            }
        };

        let response = self
            .client
            .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .bearer_auth(&api_key)
            .timeout(Duration::from_secs(20))
            .json(&body)
            .send()
            .await;
        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!("classify request rejected: {}", r.status());
                return Ok(None);
            }
            Err(e) => {
                tracing::warn!("classify request failed: {e}");
                return Ok(None);
            }
        };
        // A classify call is real engine activity too — it must keep the
        // auto-unload idle clock from firing while the user is mid-thought,
        // exactly like a chat turn does.
        self.touch_activity().await;

        let parsed: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("classify response wasn't valid JSON: {e}");
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

    /// Path of the engine log file (shown to users in "help me debug" flows).
    fn log_path(&self) -> PathBuf {
        self.log_dir.join("engine.log")
    }
}

impl SubprocessBackend {
    /// Read `path` back, decrypting it if it's sealed and a log key is set,
    /// or `None` if the file doesn't exist. Shared by [`EngineBackend::tail_log`]
    /// (the active, almost-always-plaintext `engine.log`) and
    /// [`EngineBackend::export_logs`] (which also wants the sealed
    /// `engine.prev.log`).
    fn read_and_decrypt(&self, path: &Path) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(match self.log_key.lock().unwrap().as_ref() {
            Some(key) if logframe::looks_like_frames(&bytes) => decrypt_frames(&bytes, key),
            _ => String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    fn spawn_server(
        &self,
        server_binary: &Path,
        model_path: &Path,
        port: u16,
        cfg: &EngineConfig,
        api_key: &str,
    ) -> std::io::Result<tokio::process::Child> {
        std::fs::create_dir_all(&self.log_dir)?;
        // Keep the previous session's log as `engine.prev.log` before we
        // truncate, so the trace of a crash isn't lost the moment the user
        // restarts the engine. Sealed at rest immediately after the rename —
        // see `encrypt_log_file_in_place`'s doc comment for why this, not a
        // live-encrypting writer, is where the engine log gets encrypted.
        let log_path = self.log_path();
        if log_path.exists() {
            let prev_path = self.log_dir.join("engine.prev.log");
            let _ = std::fs::rename(&log_path, &prev_path);
            if let Some(key) = self.log_key.lock().unwrap().as_ref() {
                encrypt_log_file_in_place(&prev_path, key);
            }
        }
        let log = std::fs::File::create(&log_path)?; // truncate per start
        let log_err = log.try_clone()?;

        let mut cmd = tokio::process::Command::new(server_binary);
        cmd.arg("-m")
            .arg(model_path)
            .arg("--port")
            .arg(port.to_string())
            .arg("--host")
            .arg("127.0.0.1")
            .arg("-c")
            .arg(cfg.context_length.to_string())
            .arg("-ngl")
            .arg(cfg.gpu_layers.to_string())
            .arg("--jinja")
            .arg("--no-webui")
            .arg("--api-key")
            .arg(api_key);
        // Vision models load their multimodal projector here; without it the
        // same model still starts, just text-only. See `EngineConfig::mmproj_path`.
        if let Some(mmproj) = &cfg.mmproj_path {
            cmd.arg("--mmproj").arg(mmproj);
        }
        if let Some(threads) = cfg.threads {
            cmd.arg("-t").arg(threads.to_string());
        }
        cmd.args(&cfg.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .kill_on_drop(true);
        cmd.spawn()
    }

    /// Kill any child and set a Failed status with `message`; returns the
    /// matching error so callers can `return Err(self.fail(...).await)`.
    async fn fail(&self, message: &str) -> Error {
        let mut inner = self.inner.lock().await;
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill().await;
        }
        inner.generation += 1;
        inner.status = EngineStatus::Failed {
            message: message.to_string(),
        };
        Error::Engine(message.to_string())
    }

    async fn child_has_exited(&self) -> bool {
        let mut inner = self.inner.lock().await;
        match inner.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(Some(_))),
            None => true,
        }
    }

    /// Best-effort OOM hint (see [`oom_hint`]) for the child behind a failed
    /// chat request. `None` (already reaped by an earlier check) or a
    /// non-signal exit yields no hint.
    async fn child_oom_hint(&self) -> String {
        let mut inner = self.inner.lock().await;
        match inner
            .child
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
        {
            Some(exit) => oom_hint(&exit),
            None => String::new(),
        }
    }
}

/// Bind to an ephemeral port to find a free one, then release it.
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Random hex token passed to llama-server as `--api-key`, so other local
/// processes can't talk to it. Reads /dev/urandom; if that ever fails, falls
/// back to a weaker port+time-derived token rather than blocking startup.
fn generate_api_key(port: u16) -> String {
    let mut bytes = [0u8; 16];
    let urandom = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes));
    match urandom {
        Ok(()) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        Err(e) => {
            tracing::warn!("could not read /dev/urandom for the engine api key: {e}");
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::time::Instant::now().hash(&mut hasher);
            format!("{port:04x}{:016x}", hasher.finish())
        }
    }
}

/// Seal `path`'s current plaintext content in place, one AEAD frame per
/// line — used to encrypt `engine.prev.log` immediately after rotation.
/// Unlike the app log (`crates/app/src/logging.rs`), the live `engine.log`
/// is written by redirecting the child process's own stdout/stderr file
/// descriptors directly to it (`spawn_server`'s `Stdio::from(log)`), so
/// there is no Rust-side write to intercept per line the way a
/// `tracing_subscriber` writer can be swapped. The live file is therefore
/// plaintext for the bounded duration of one engine session, then sealed
/// the moment the *next* `start()` rotates it out of the way — the same
/// bounded-plaintext-window trade-off `crate::logging`'s pre-unlock
/// `boot.log` already makes, applied here for a different reason (a
/// write path with no interception point, rather than a phase before any
/// key exists). Best-effort: a failure here must never fail engine startup.
fn encrypt_log_file_in_place(path: &Path, key: &DataKey) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    if bytes.is_empty() || logframe::looks_like_frames(&bytes) {
        return; // nothing to seal, or already sealed
    }
    let mut sealed = Vec::new();
    for line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        // The marker is unused for this store (see `crate::crypto::logframe`'s
        // own doc comment: "the engine log ignores it") — one fixed key for
        // the file's lifetime, no per-day rotation, since it's sealed once
        // at rotation time rather than continuously like the app log.
        sealed.extend(logframe::seal_frame(key, 0, line));
    }
    if std::fs::write(path, &sealed).is_err() {
        tracing::warn!("couldn't encrypt {path:?} at rest after rotation");
    }
}

/// Decrypt every frame in `bytes` under `key` (marker ignored — one fixed
/// key per file, see [`encrypt_log_file_in_place`]) and join the lines back
/// with `\n`, mirroring the app log's own `decrypt_or_plaintext`. The raw
/// key bytes are `Zeroizing`-guarded for the whole closure's lifetime —
/// `[u8; 32]` is `Copy`, so wrapping only the *result* of `key.as_bytes()`
/// would leave the plain copy captured by the closure unscrubbed on return.
fn decrypt_frames(bytes: &[u8], key: &DataKey) -> String {
    let key_bytes = Zeroizing::new(*key.as_bytes());
    let lines = logframe::open_all(bytes, &move |_marker| DataKey::from_bytes(*key_bytes));
    lines
        .into_iter()
        .map(|line| String::from_utf8_lossy(&line).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Last ~40 lines of the engine log, for tracing (never shown to users).
fn log_tail(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let mut tail: Vec<&str> = text.lines().rev().take(40).collect();
    tail.reverse();
    tail.join("\n")
}

#[cfg(unix)]
fn exit_signal(exit: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    exit.signal()
}

#[cfg(not(unix))]
fn exit_signal(_exit: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// A bare SIGKILL (9) exit — with `log_tail` empty, since a killed process
/// never gets to write a crash reason of its own — is the fingerprint of the
/// kernel OOM killer, not a llama.cpp-reported failure. That's exactly the
/// case a disk-offloaded MoE model (mmap-streamed, resident set can spike
/// past what the memory-fit check assumed) can hit. We can't know for
/// certain it was the OOM killer, so this only ever adds a suffix to an
/// existing `tracing::error!`, never changes the user-facing message.
fn oom_hint(exit: &std::process::ExitStatus) -> String {
    if exit_signal(exit) != Some(9) {
        return String::new();
    }
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    format!(
        "; signal 9 (SIGKILL) with no captured log output is consistent with the OS \
         killing the process for using too much memory (possible OOM) — available RAM \
         at detection time: {:.1} GB / {:.1} GB total",
        sys.available_memory() as f64 / 1e9,
        sys.total_memory() as f64 / 1e9,
    )
}

/// llama.cpp fails fast with `unknown model architecture: '<name>'` when a
/// GGUF's `general.architecture` isn't compiled into this build of the
/// engine — most often a model family newer than the llama.cpp release
/// OpenPal currently bundles (see `crates/app/resources/llama/engine-version.json`).
/// That's a different problem from "stopped while loading" (corrupt file,
/// OOM, bad flags): naming the architecture and pointing at an app update is
/// actionable, the generic message isn't.
fn unsupported_architecture_hint(log: &str) -> Option<String> {
    let marker = "unknown model architecture: '";
    let start = log.find(marker)? + marker.len();
    let end = start + log[start..].find('\'')?;
    let arch = &log[start..end];
    Some(format!(
        "This model uses the '{arch}' architecture, which this version of OpenPal's AI engine \
         doesn't support yet. Check for an app update, or pick a different model."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_keys_are_hex_and_unique_per_start() {
        let a = generate_api_key(8080);
        let b = generate_api_key(8080);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "every start must get a fresh key");
    }

    #[test]
    fn free_port_returns_a_bindable_port() {
        let port = free_port().unwrap();
        assert!(port > 0);
        // The port is free again once the probe listener is dropped.
        std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    }

    #[tokio::test]
    async fn chat_stream_requires_a_running_engine() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SubprocessBackend::new(dir.path().to_path_buf());
        let result = engine
            .chat_stream(
                &[],
                &crate::types::SamplingParams::default(),
                Arc::new(AtomicBool::new(false)),
                &mut |_| {},
            )
            .await;
        assert!(matches!(result, Err(Error::Engine(_))));
    }

    #[tokio::test]
    async fn stop_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SubprocessBackend::new(dir.path().to_path_buf());
        engine.stop().await.unwrap();
        engine.stop().await.unwrap();
        assert!(matches!(engine.status().await, EngineStatus::Stopped));
    }

    #[tokio::test]
    async fn activity_is_none_when_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SubprocessBackend::new(dir.path().to_path_buf());
        assert!(engine.activity().await.is_none());
    }

    #[tokio::test]
    async fn activity_stays_none_after_touch_and_generation_markers_when_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SubprocessBackend::new(dir.path().to_path_buf());
        // None of these must panic on a never-started (or stopped) engine.
        engine.touch_activity().await;
        engine.begin_generation().await;
        engine.end_generation().await;
        assert!(matches!(engine.status().await, EngineStatus::Stopped));
        assert!(engine.activity().await.is_none());
    }

    #[tokio::test]
    async fn stop_if_current_noops_on_a_stale_generation_but_stops_on_the_current_one() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SubprocessBackend::new(dir.path().to_path_buf());
        let stale_gen = engine.inner.lock().await.generation;
        engine.stop().await.unwrap(); // bumps generation past `stale_gen`
        let current_gen = engine.inner.lock().await.generation;
        assert_ne!(stale_gen, current_gen);

        // A snapshot taken before the bump must not touch anything.
        engine.stop_if_current(stale_gen).await.unwrap();
        assert_eq!(
            engine.inner.lock().await.generation,
            current_gen,
            "a stale generation must be a no-op"
        );

        // The current generation still stops (and bumps again) as normal.
        engine.stop_if_current(current_gen).await.unwrap();
        assert_eq!(engine.inner.lock().await.generation, current_gen + 1);
        assert!(matches!(engine.status().await, EngineStatus::Stopped));
    }

    // ------------------------- Engine log encryption -------------------------

    #[test]
    fn tail_log_with_no_key_reads_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SubprocessBackend::new(dir.path().to_path_buf());
        std::fs::write(manager.log_path(), "line one\nline two\nline three\n").unwrap();
        assert_eq!(manager.tail_log(2), "line two\nline three");
        assert_eq!(manager.tail_log(99), "line one\nline two\nline three");
    }

    #[test]
    fn tail_log_with_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SubprocessBackend::new(dir.path().to_path_buf());
        assert_eq!(manager.tail_log(10), "");
    }

    #[test]
    fn encrypt_log_file_in_place_roundtrips_and_a_wrong_key_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.prev.log");
        std::fs::write(&path, "first\nsecond\nthird\n").unwrap();

        let key = DataKey::random();
        encrypt_log_file_in_place(&path, &key);

        let sealed = std::fs::read(&path).unwrap();
        assert!(
            logframe::looks_like_frames(&sealed),
            "the file must now be in the encrypted frame format"
        );
        assert_eq!(decrypt_frames(&sealed, &key), "first\nsecond\nthird");

        let wrong = DataKey::random();
        assert_eq!(
            decrypt_frames(&sealed, &wrong),
            "",
            "a wrong key must not recover any line"
        );
    }

    #[test]
    fn encrypt_log_file_in_place_is_idempotent_on_an_already_sealed_file() {
        // A rotation racing with (or repeating after) a prior encryption pass
        // must not double-seal the file into unreadable garbage.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.prev.log");
        std::fs::write(&path, "one line\n").unwrap();
        let key = DataKey::random();
        encrypt_log_file_in_place(&path, &key);
        let sealed_once = std::fs::read(&path).unwrap();

        encrypt_log_file_in_place(&path, &key);
        let sealed_twice = std::fs::read(&path).unwrap();

        assert_eq!(sealed_once, sealed_twice, "a second pass must be a no-op");
        assert_eq!(decrypt_frames(&sealed_twice, &key), "one line");
    }

    #[test]
    fn encrypt_log_file_in_place_on_empty_or_missing_file_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.log");
        encrypt_log_file_in_place(&missing, &DataKey::random());
        assert!(!missing.exists());

        let empty = dir.path().join("empty.log");
        std::fs::write(&empty, "").unwrap();
        encrypt_log_file_in_place(&empty, &DataKey::random());
        assert_eq!(std::fs::read(&empty).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn tail_log_decrypts_a_sealed_active_file() {
        // Not the normal case (the active file is plaintext while the child
        // writes it — see `tail_log`'s doc comment) but `read_and_decrypt`
        // must still handle it if it's ever hit, e.g. right after a restart
        // races a caller reading the file mid-rotation.
        let dir = tempfile::tempdir().unwrap();
        let manager = SubprocessBackend::new(dir.path().to_path_buf());
        let dek = DataKey::random();
        manager.set_dek(Some(&dek));
        // Same derivation `set_dek` performs internally — the seal below
        // must use the *purpose* key, not the raw `dek`.
        let purpose_key = derive_subkey(&dek, ENGINE_LOG_INFO);
        std::fs::write(
            manager.log_path(),
            logframe::seal_frame(&purpose_key, 0, b"sealed line"),
        )
        .unwrap();

        assert_eq!(manager.tail_log(10), "sealed line");
    }

    #[test]
    fn export_logs_includes_both_generations_and_decrypts_the_sealed_one() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SubprocessBackend::new(dir.path().to_path_buf());
        std::fs::write(manager.log_path(), "current session\n").unwrap();

        let dek = DataKey::random();
        manager.set_dek(Some(&dek));
        let purpose_key = derive_subkey(&dek, ENGINE_LOG_INFO);
        let prev_path = dir.path().join("engine.prev.log");
        std::fs::write(&prev_path, "previous session\n").unwrap();
        encrypt_log_file_in_place(&prev_path, &purpose_key);

        let export = manager.export_logs();
        assert!(export.contains("==== engine.log ===="));
        assert!(export.contains("current session"));
        assert!(export.contains("==== engine.prev.log ===="));
        assert!(export.contains("previous session"));
    }

    #[test]
    fn export_logs_skips_a_generation_that_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SubprocessBackend::new(dir.path().to_path_buf());
        std::fs::write(manager.log_path(), "only session\n").unwrap();

        let export = manager.export_logs();
        assert!(export.contains("only session"));
        assert!(!export.contains("engine.prev.log"));
    }

    #[test]
    fn set_dek_none_clears_a_previously_set_key() {
        let dir = tempfile::tempdir().unwrap();
        let manager = SubprocessBackend::new(dir.path().to_path_buf());
        manager.set_dek(Some(&DataKey::random()));
        assert!(manager.log_key.lock().unwrap().is_some());
        manager.set_dek(None);
        assert!(manager.log_key.lock().unwrap().is_none());
    }

    #[test]
    fn unsupported_architecture_hint_names_the_architecture_from_the_log() {
        let log = "0.00.214.455 I srv    load_model: loading model 'foo.gguf'\n\
                    0.00.312.564 E llama_model_load: error loading model: unknown model architecture: 'bailingmoe3'\n\
                    0.00.312.729 E llama_model_load_from_file_impl: failed to load model";
        let message = unsupported_architecture_hint(log).unwrap();
        assert!(
            message.contains("'bailingmoe3'"),
            "message should name the unsupported architecture: {message}"
        );
    }

    #[test]
    fn unsupported_architecture_hint_is_none_for_unrelated_failures() {
        assert_eq!(unsupported_architecture_hint(""), None);
        assert_eq!(
            unsupported_architecture_hint("Killed\n"),
            None,
            "an OOM kill has no architecture line to key off of"
        );
    }
}
