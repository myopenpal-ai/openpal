//! The engine seam: hosting the model that generates chat completions.
//!
//! `EngineManager` is a thin, backend-agnostic wrapper. Today the only
//! implementation is [`subprocess::SubprocessBackend`] — spawning the
//! official `llama-server` binary as a subprocess and talking
//! OpenAI-compatible HTTP/SSE to it on localhost; see that module's own doc
//! comment for the exact lifecycle contract. This split exists to make room
//! for a second, mobile-only implementation later (linking llama.cpp
//! in-process via FFI, since neither iOS nor Android allow spawning a
//! subprocess the way desktop does) without the rest of the app needing to
//! know which one is running — see `docs/design/MOBILE.md`.
//!
//! This module also hosts everything that's genuinely backend-agnostic:
//! request-body builders shared by both the chat and classify paths, and the
//! disk-slow startup-timeout math (pure functions, no process/transport
//! involved).

mod subprocess;

use subprocess::SubprocessBackend;

use crate::crypto::key::DataKey;
use crate::types::{
    AttachmentKind, ChatEvent, ChatMessage, EngineConfig, EngineStatus, RiskSignal, SamplingParams,
    TaskType,
};
use crate::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

/// Default startup budget for a model that fits in memory normally.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

/// Conservative worst-case disk read speed (bytes/sec) used to size a "disk
/// slow" model's startup timeout when no measured speed is available —
/// matches a slow HDD/USB drive, not the NVMe most machines actually have.
/// Callers should prefer a real measurement from
/// [`crate::disk_speed::probe_volume_speed`]; err generous here rather than
/// time out a model that's genuinely still loading.
const ASSUMED_SLOW_DISK_BYTES_PER_SEC: u64 = 100 * 1024 * 1024;
/// Floor for a disk-streamed model's startup budget, even for a small one —
/// on Linux, mmap's prefetch (`MAP_POPULATE`) blocks on a full sequential
/// read of the file regardless of size, and llama.cpp's own load/warmup work
/// needs room too.
const MIN_DISK_SLOW_STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
/// Ceiling for a disk-streamed model's startup budget. A probe reading can be
/// pathologically low (another process saturating the disk during the probe,
/// or a future bug) and, uncapped, a low-enough `measured_bytes_per_sec`
/// turns into a wait of days rather than minutes. If loading genuinely takes
/// this long, something is wrong — failing with a clear error is kinder than
/// leaving "Waking up…" on screen indefinitely.
const MAX_DISK_SLOW_STARTUP_TIMEOUT: Duration = Duration::from_secs(3600);

/// Startup budget for a model running in "disk slow" mode (see
/// `FitLevel::DiskSlow`): enough to read the whole file once at
/// `measured_bytes_per_sec` (falling back to [`ASSUMED_SLOW_DISK_BYTES_PER_SEC`]
/// when the caller has no disk-speed probe result — e.g. the probe itself
/// failed), doubled for slack (loading isn't purely sequential I/O), clamped
/// to [`MIN_DISK_SLOW_STARTUP_TIMEOUT`]..=[`MAX_DISK_SLOW_STARTUP_TIMEOUT`].
pub fn disk_slow_startup_timeout(
    model_size_bytes: u64,
    measured_bytes_per_sec: Option<u64>,
) -> Duration {
    // `.max(1)` keeps this total regardless of what a probe reports — a
    // measured 0 must widen the budget, not divide by zero.
    let bytes_per_sec = measured_bytes_per_sec
        .unwrap_or(ASSUMED_SLOW_DISK_BYTES_PER_SEC)
        .max(1);
    let read_once = Duration::from_secs(model_size_bytes / bytes_per_sec);
    (read_once * 2).clamp(MIN_DISK_SLOW_STARTUP_TIMEOUT, MAX_DISK_SLOW_STARTUP_TIMEOUT)
}

/// Everything an out-of-process client (e.g. the web-search sidecar) needs to
/// talk to the running engine's OpenAI-compatible API. Never sent to the
/// frontend — the api key must not leave the backend.
#[derive(Debug, Clone)]
pub struct EngineEndpoint {
    /// OpenAI base, e.g. `http://127.0.0.1:53412/v1` (no trailing slash).
    pub base_url: String,
    /// The per-start `--api-key` bearer token.
    pub api_key: String,
    /// The model the engine is currently serving.
    pub model_id: String,
    /// How long an out-of-process client should wait for one non-streaming
    /// completion before giving up. Mirrors the same disk-speed-scaled
    /// budget `start()` used for the health-check startup timeout (floored
    /// at 300s so a fast, in-memory model behaves exactly as before) — a
    /// flat client-side timeout would misreport a live-but-disk-slow engine
    /// as "not responding".
    pub request_timeout_secs: u64,
}

/// Idle/activity facts the auto-unload feature needs. `EngineManager` knows
/// nothing about `Settings` — the app layer combines this with the user's
/// policy to decide what to show/do.
#[derive(Debug, Clone, Copy)]
pub struct ActivityInfo {
    pub disk_slow: bool,
    pub idle_for: Duration,
    pub generating: bool,
    /// Generation counter at the moment this snapshot was taken — pass to
    /// `stop_if_current` so a decision based on a stale snapshot doesn't
    /// kill a model that was switched out from under it in the meantime.
    pub generation: u64,
}

/// Everything needed to restart a running engine *without* its MTP
/// self-drafting args after a first-message crash — the automatic-recovery
/// half of the MTP feature (`docs/design/DISK_OFFLOAD.md`). Built by
/// [`subprocess::SubprocessBackend::start`] when the start carries `--spec-*`
/// args; `cfg.extra_args` has already had every `--spec-*` flag stripped.
#[derive(Debug, Clone)]
pub struct MtpRestart {
    pub server_binary: PathBuf,
    pub model_id: String,
    pub model_path: PathBuf,
    pub cfg: EngineConfig,
    pub disk_slow: bool,
    pub startup_timeout: Duration,
}

/// The one deliberate fork point: how the engine is hosted. Every method
/// mirrors what a bare `EngineManager` used to expose directly — this trait
/// is a relocation of existing subprocess behavior, not a new design.
/// [`SubprocessBackend`] implements it with zero behavior change; a future
/// mobile-only backend (linking llama.cpp in-process via FFI, since neither
/// iOS nor Android allow spawning a subprocess) implements it against
/// `llama-cpp-2` instead — see `docs/design/MOBILE.md`.
// `async_trait` puts `#[must_use]` on methods returning a boxed future, which
// is already `must_use`; clippy 1.99 flags that as `double_must_use`.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait EngineBackend: Send + Sync {
    /// Start (or restart) the engine with the given model. Returns once the
    /// server is ready to chat. `disk_slow` is the caller's
    /// `selection::exceeds_ram` classification for this model — stored so
    /// the auto-unload feature can exempt disk-slow models by default.
    /// `startup_timeout` is how long to wait before giving up — pass
    /// [`STARTUP_TIMEOUT`] normally, or [`disk_slow_startup_timeout`] for a
    /// model running in "disk slow" mode.
    async fn start(
        &self,
        server_binary: &Path,
        model_id: &str,
        model_path: &Path,
        cfg: &EngineConfig,
        disk_slow: bool,
        startup_timeout: Duration,
    ) -> Result<()>;

    /// Kill the engine if running. Idempotent.
    async fn stop(&self) -> Result<()>;

    async fn status(&self) -> EngineStatus;

    /// Out-of-process HTTP connection details for the web-search sidecar and
    /// the agent-mode/web-search tool-calling loop (`agent::orchestrator`),
    /// which talk to the running model directly rather than through
    /// [`chat_stream`](Self::chat_stream)'s event callback. Defaults to
    /// `None` so a backend with no HTTP surface (e.g. a future in-process
    /// one) needs to override nothing to correctly report "not available
    /// this way" — see `docs/design/MOBILE.md`'s note on this gap.
    async fn endpoint(&self) -> Option<EngineEndpoint> {
        None
    }

    /// Idle/activity snapshot for the auto-unload feature, or `None` unless
    /// the engine is currently `Running`.
    async fn activity(&self) -> Option<ActivityInfo>;

    /// Reset the auto-unload idle clock to now. Called after a chat turn
    /// completes, and by the explicit "snooze" command — both are just
    /// "activity happened". A no-op unless the engine is `Running`.
    async fn touch_activity(&self);

    /// Mark a chat turn starting. Pairs with
    /// [`end_generation`](Self::end_generation).
    async fn begin_generation(&self);

    /// Mark a chat turn ending, and reset the idle clock.
    async fn end_generation(&self);

    /// Like [`stop`](Self::stop), but a no-op if the engine has moved on to
    /// a different generation since `expected_generation` was captured.
    async fn stop_if_current(&self, expected_generation: u64) -> Result<()>;

    /// Snapshot for restarting the current engine *without* its MTP
    /// self-drafting args — `Some` only while an MTP-armed start is running
    /// (the first-message crash fallback: [`EngineManager::chat_stream`]
    /// restarts via this and resends the request once). Only
    /// [`subprocess::SubprocessBackend`] produces one today; other backends
    /// return `None` (no auto-restart).
    async fn mtp_restart_snapshot(&self) -> Option<MtpRestart> {
        None
    }

    /// Whether the local engine process has exited (vs. a mere
    /// request-level failure) — the condition that makes a restart-and-resend
    /// meaningful. Only [`subprocess::SubprocessBackend`] has a process to
    /// observe; other backends default to `false` (no auto-restart).
    async fn child_died(&self) -> bool {
        false
    }

    /// Stream one assistant reply.
    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        sampling: &SamplingParams,
        cancel: Arc<AtomicBool>,
        on_event: &mut (dyn FnMut(ChatEvent) + Send),
    ) -> Result<()>;

    /// Low-level one-shot completion behind [`EngineManager::classify`]/
    /// [`EngineManager::classify_risk`]. Interpreting the label is
    /// `EngineManager`'s job (backend-agnostic); this only runs `body` and
    /// returns its raw text, or `None` for "nothing useful to report" (not
    /// running, mid-generation, request failure).
    async fn raw_classify(&self, body: serde_json::Value) -> Result<Option<String>>;

    /// Set (or clear, in portable mode with no DEK) this engine's log key.
    fn set_dek(&self, dek: Option<&DataKey>);

    /// Last `max_lines` lines of the currently-active engine log.
    fn tail_log(&self, max_lines: usize) -> String;

    /// Both engine log generations, decrypted and concatenated with headers,
    /// for "Export logs".
    fn export_logs(&self) -> String;

    /// Path of the engine log file (shown to users in "help me debug" flows).
    fn log_path(&self) -> PathBuf;
}

/// Owns exactly one running engine backend for the whole app — the single
/// place mono-model-first is enforced, on every platform: whichever backend
/// this wraps, there is only ever one `EngineManager` per app instance (see
/// `crates/app/src/state.rs`'s `AppState`).
pub struct EngineManager {
    backend: Box<dyn EngineBackend>,
    /// The manager's own client for out-of-band HTTP checks (`is_healthy`'s
    /// `/health` probe and the MTP recovery watchdog) — deliberately
    /// separate from the backend's request client, so a health probe is
    /// never served from a pooled connection to a wedged server.
    client: reqwest::Client,
}

impl EngineManager {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            backend: Box::new(SubprocessBackend::new(log_dir)),
            client: reqwest::Client::new(),
        }
    }

    /// Zero-cost seam for tests (and later phases) that need a specific
    /// backend rather than the platform default — no production call site
    /// uses this today.
    pub fn with_backend(backend: Box<dyn EngineBackend>) -> Self {
        Self {
            backend,
            client: reqwest::Client::new(),
        }
    }

    /// Set (or clear, in portable mode with no DEK) this engine's log key —
    /// called once from `state::finish_unlock`, right alongside
    /// `crate::logging::upgrade_to_encrypted`'s app-log equivalent.
    pub fn set_dek(&self, dek: Option<&DataKey>) {
        self.backend.set_dek(dek)
    }

    /// Last `max_lines` lines of the currently-active engine log.
    pub fn tail_log(&self, max_lines: usize) -> String {
        self.backend.tail_log(max_lines)
    }

    /// Both engine log generations, decrypted and concatenated with headers,
    /// for "Export logs".
    pub fn export_logs(&self) -> String {
        self.backend.export_logs()
    }

    /// Start (or restart) the engine with the given model. See
    /// [`EngineBackend::start`] for the full contract.
    pub async fn start(
        &self,
        server_binary: &Path,
        model_id: &str,
        model_path: &Path,
        cfg: &EngineConfig,
        disk_slow: bool,
        startup_timeout: Duration,
    ) -> Result<()> {
        self.backend
            .start(
                server_binary,
                model_id,
                model_path,
                cfg,
                disk_slow,
                startup_timeout,
            )
            .await
    }

    /// Kill the engine if running. Idempotent.
    pub async fn stop(&self) -> Result<()> {
        self.backend.stop().await
    }

    pub async fn status(&self) -> EngineStatus {
        self.backend.status().await
    }

    /// Connection details for the running engine, or `None` if it isn't
    /// running (or the backend has no HTTP surface at all — see
    /// [`EngineBackend::endpoint`]).
    pub async fn endpoint(&self) -> Option<EngineEndpoint> {
        self.backend.endpoint().await
    }

    /// Idle/activity snapshot for the auto-unload feature, or `None` unless
    /// the engine is currently `Running`.
    pub async fn activity(&self) -> Option<ActivityInfo> {
        self.backend.activity().await
    }

    /// Reset the auto-unload idle clock to now. Called after a chat turn
    /// completes, and by the explicit "snooze" command — both are just
    /// "activity happened". A no-op unless the engine is `Running`.
    pub async fn touch_activity(&self) {
        self.backend.touch_activity().await
    }

    /// Mark a chat turn starting. Pairs with [`Self::end_generation`].
    pub async fn begin_generation(&self) {
        self.backend.begin_generation().await
    }

    /// Mark a chat turn ending, and reset the idle clock.
    pub async fn end_generation(&self) {
        self.backend.end_generation().await
    }

    /// Like [`Self::stop`], but a no-op if the engine has moved on to a
    /// different generation since `expected_generation` was captured.
    pub async fn stop_if_current(&self, expected_generation: u64) -> Result<()> {
        self.backend.stop_if_current(expected_generation).await
    }

    /// True when the currently-loaded engine is genuinely responsive — or,
    /// by default, when there is no Running engine to check (a Stopped /
    /// Starting / Failed engine is not this check's problem). A `Running`
    /// engine is unhealthy only if its subprocess has exited (the backend
    /// can observe that directly) or its `/health` endpoint doesn't answer
    /// within a short window (a wedged HTTP server — the state that made a
    /// mid-turn MTP failure permanent). This is the probe behind the MTP
    /// recovery watchdog and the agent-turn failure hook: quiet (one
    /// loopback GET, 3s cap), best-effort (never throws).
    pub async fn is_healthy(&self) -> bool {
        let Some(endpoint) = self.backend.endpoint().await else {
            return true; // not Running — nothing to health-check
        };
        if self.backend.child_died().await {
            return false;
        }
        let base = endpoint.base_url.trim_end_matches("/v1");
        self.client
            .get(format!("{base}/health"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// Whether the current engine was started with MTP self-drafting armed
    /// — i.e. it *could* be restarted without the `--spec-*` args. This is
    /// the gate that keeps recovery strictly MTP-shaped (see [`MtpRestart`]);
    /// an engine that was already running MTP-less has no stripped fallback
    /// to land on.
    pub async fn mtp_fallback_armed(&self) -> bool {
        self.backend.mtp_restart_snapshot().await.is_some()
    }

    /// Restart the current engine *without* its MTP self-drafting args,
    /// using the snapshot the backend armed when it was started with
    /// `--spec-*` args. The one shared primitive behind every MTP recovery
    /// path: the first-message retry in `chat_stream`, the agent/web-search
    /// turn failure hook in `crates/app` (`run_agent_turn_with_recovery`),
    /// and the idle-loop watchdog. Returns `false` (no restart) when there
    /// is no snapshot (nothing MTP-specific to strip), when a concurrent
    /// `stop()`/`start()` superseded the engine since `expected_generation`
    /// was captured (only the engine the caller observed gets restarted),
    /// or when the restart itself fails (the caller's error path stands).
    pub async fn restart_without_mtp(&self, expected_generation: Option<u64>) -> bool {
        let Some(restart) = self.backend.mtp_restart_snapshot().await else {
            return false;
        };
        // Only restart the engine the caller actually observed — a
        // concurrent stop()/start() (user switched models, manual reload)
        // owns the backend now and must not be resurrected as a stripped
        // copy of the engine it replaced. `None` keeps the historical
        // `chat_stream` contract: when no engine was observed at all, we act
        // only while there still isn't one (the crashed engine was ours).
        let still_ours = match (expected_generation, self.backend.activity().await) {
            (Some(expected), Some(info)) => info.generation == expected,
            (None, None) => true,
            _ => false,
        };
        if !still_ours {
            return false;
        }
        tracing::warn!(
            model = restart.model_id,
            "engine died or wedged with MTP self-drafting armed; restarting without MTP"
        );
        match self
            .start(
                &restart.server_binary,
                &restart.model_id,
                &restart.model_path,
                &restart.cfg,
                restart.disk_slow,
                restart.startup_timeout,
            )
            .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("MTP recovery restart failed: {e}");
                false
            }
        }
    }

    /// Stream one assistant reply. See [`EngineBackend::chat_stream`] for
    /// the full contract.
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        sampling: &SamplingParams,
        cancel: Arc<AtomicBool>,
        mut on_event: impl FnMut(ChatEvent) + Send,
    ) -> Result<()> {
        let on_event = &mut on_event as &mut (dyn FnMut(ChatEvent) + Send);
        // Generation snapshot for the "is the engine still ours?" check below
        // — a concurrent start()/stop() that supersedes the engine mid-retry
        // gets to keep it (`start()` already bails safely on a generation
        // mismatch); we bow out to the normal error path instead of racing it.
        let generation = self.backend.activity().await.map(|a| a.generation);
        let emitted_answer = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let first = self
            .chat_stream_once(
                messages,
                sampling,
                cancel.clone(),
                on_event,
                &emitted_answer,
            )
            .await;

        // MTP crash fallback: an engine started with MTP self-drafting can
        // hard-abort on the *first* generation (a GGUF whose MTP heads this
        // engine can't run — see docs/design/DISK_OFFLOAD.md), before a
        // single token is emitted. Restart it without the MTP args and resend
        // the request exactly once; only a second failure falls through to
        // the normal error/failed-state path. A mid-answer crash never
        // triggers this — `emitted_answer` proves the model was working, so a
        // resend would duplicate tokens already shown.
        if first.is_err()
            && !emitted_answer.load(std::sync::atomic::Ordering::Relaxed)
            && self.backend.child_died().await
            && self.restart_without_mtp(generation).await
        {
            return self
                .chat_stream_once(messages, sampling, cancel, on_event, &emitted_answer)
                .await;
        }
        first
    }

    /// One `chat_stream` round trip against the backend, forwarding `on_event`
    /// while flagging the moment any answer token ([`ChatEvent::Token`]) is
    /// emitted — the "mid-answer" guard for the MTP crash fallback.
    async fn chat_stream_once(
        &self,
        messages: &[ChatMessage],
        sampling: &SamplingParams,
        cancel: Arc<AtomicBool>,
        on_event: &mut (dyn FnMut(ChatEvent) + Send),
        emitted_answer: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        use std::sync::atomic::Ordering;
        let emitted = emitted_answer.clone();
        let mut forward: Box<dyn FnMut(ChatEvent) + Send> = Box::new(move |ev| {
            if matches!(ev, ChatEvent::Token { .. }) {
                emitted.store(true, Ordering::Relaxed);
            }
            on_event(ev);
        });
        self.backend
            .chat_stream(messages, sampling, cancel, forward.as_mut())
            .await
    }

    /// One-shot, non-streaming classification of `text` into one of
    /// [`TaskType`]'s labels, run against the already-running engine — no
    /// second model, no extra process (the request-triage feature's
    /// classifier). Returns `Ok(None)` whenever
    /// [`EngineBackend::raw_classify`] has nothing to report, or the
    /// model's constrained output doesn't match a known label. Calling code
    /// (the composer's debounce) treats `None` as "no suggestion this time"
    /// and tries again on the next keystroke pause. A successful match is
    /// logged (`tracing::info!`) — this classification never appears in the
    /// chat UI, only in the app's log file.
    pub async fn classify(&self, text: &str) -> Result<Option<TaskType>> {
        let label = self.backend.raw_classify(build_classify_body(text)).await?;
        let tag = label.and_then(|l| TaskType::ALL.into_iter().find(|t| t.wire() == l));
        if let Some(tag) = tag {
            tracing::info!("triage: classified request as {}", tag.wire());
        }
        Ok(tag)
    }

    /// One-shot, non-streaming safety classification of `text` — typically
    /// the user's message from a just-settled chat turn — into a
    /// [`RiskSignal`], run against the already-running local engine via the
    /// same [`EngineBackend::raw_classify`] plumbing as
    /// [`classify`](Self::classify): no second model, no extra process.
    /// Returns `Ok(None)` whenever there's nothing to report (engine not
    /// running, mid-generation, request failure) or the model's constrained
    /// output doesn't match a known label; callers treat `None` the same as
    /// `Some(RiskSignal::Safe)` — "don't show the banner". Unlike
    /// [`classify`](Self::classify)'s request-triage match, a `Risk` result
    /// is *never* logged: `crates/app/src/logging.rs`'s app-wide log file is
    /// explicitly designed to be viewed in Settings and shared for support,
    /// so even a content-free "flagged a message" line would leave a
    /// timestamped record of someone's crisis on disk for anyone with
    /// access to that file to find.
    pub async fn classify_risk(&self, text: &str) -> Result<Option<RiskSignal>> {
        let label = self
            .backend
            .raw_classify(build_risk_classify_body(text))
            .await?;
        Ok(label.and_then(|l| RiskSignal::ALL.into_iter().find(|s| s.wire() == l)))
    }

    /// Path of the engine log file (shown to users in "help me debug" flows).
    pub fn log_path(&self) -> PathBuf {
        self.backend.log_path()
    }
}

/// Build the OpenAI `messages` array for the chat request, turning a message's
/// attachments into content the model can use:
///
/// - Document attachments: their extracted text is folded in as a labeled
///   context block before the user's own words.
/// - Image attachments (vision models): become `image_url` content parts with
///   their `data:` URI — the OpenAI multimodal shape llama-server accepts
///   (verified: base64 `data:` URIs are supported).
///
/// A message with no images stays a plain `{role, content}` string object. Our
/// extra fields (model, thinking, citations, steps, display_content) are
/// dropped — llama-server doesn't use them.
///
/// Messages the user withheld from the model are left out (see
/// [`model_bound`]).
///
/// Public so the plugin chat path (web search) can fold attachments the same
/// way before handing messages to its sidecar — otherwise attachments would be
/// silently lost whenever a tool plugin drives the turn.
pub fn build_api_messages(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    model_bound(messages).map(build_api_message).collect()
}

/// The messages that may reach the model, in order: everything except a user
/// message marked [`ChatMessage::withheld_from_model`] (the user stopped a
/// review of text it carried) and the assistant replies that follow it, up to
/// the next user message. System messages always pass. Dropping the replies
/// too keeps user/assistant turns alternating, and a reply written from the
/// withheld text could repeat it. Every request builder goes through this
/// ([`build_api_messages`]), and so do the prompt-injection scan
/// (`injection::TurnGuard::user_side_parts`): withheld text is never sent,
/// so it's never asked about again either — and `read_project_conversation`,
/// so another conversation's model can't read it either.
pub fn model_bound(messages: &[ChatMessage]) -> impl Iterator<Item = &ChatMessage> {
    let mut skipping = false;
    messages.iter().filter(move |m| match m.role.as_str() {
        "user" => {
            skipping = m.withheld_from_model == Some(true);
            !skipping
        }
        "assistant" => !skipping,
        _ => true,
    })
}

fn build_api_message(m: &ChatMessage) -> serde_json::Value {
    let mut image_urls: Vec<&str> = Vec::new();
    let mut text = String::new();
    if let Some(attachments) = &m.attachments {
        for att in attachments {
            match att.kind {
                AttachmentKind::Document => {
                    if let Some(doc) = &att.text {
                        text.push_str("[Attached file: ");
                        text.push_str(&att.name);
                        text.push_str("]\n");
                        text.push_str(doc);
                        text.push_str("\n\n");
                    }
                }
                AttachmentKind::Image => {
                    if let Some(uri) = att.data_uri.as_deref() {
                        image_urls.push(uri);
                    }
                }
            }
        }
    }
    text.push_str(&m.content);

    if image_urls.is_empty() {
        return serde_json::json!({ "role": m.role, "content": text });
    }
    // Multimodal: an array of content parts. Skip an empty leading text part
    // (e.g. an image sent with no caption) so the array is just the image(s).
    let mut parts: Vec<serde_json::Value> = Vec::with_capacity(image_urls.len() + 1);
    if !text.is_empty() {
        parts.push(serde_json::json!({ "type": "text", "text": text }));
    }
    for uri in image_urls {
        parts.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": uri },
        }));
    }
    serde_json::json!({ "role": m.role, "content": parts })
}

/// Build the request body for [`EngineManager::classify`]. Pure (no I/O), so
/// it's unit-testable without a live server. `grammar` is a GBNF rule
/// restricting the entire generation to exactly one of [`TaskType`]'s wire
/// labels — this is the one place a grammar is used; it's never threaded
/// through the public [`SamplingParams`]/frontend sampling UI.
fn build_classify_body(text: &str) -> serde_json::Value {
    let alternatives = TaskType::ALL
        .iter()
        .map(|t| format!("\"{}\"", t.wire()))
        .collect::<Vec<_>>()
        .join(" | ");
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
            { "role": "user", "content": text },
        ],
        "stream": false,
        "temperature": 0.0,
        "max_tokens": 8,
        "grammar": format!("root ::= {alternatives}"),
    })
}

/// Build the request body for [`EngineManager::classify_risk`]. Pure (no
/// I/O), unit-testable without a live server. Same shape as
/// [`build_classify_body`] (non-streaming, temperature 0, grammar-constrained
/// to exactly one label) with a safety-specific system prompt instead of a
/// topic one.
fn build_risk_classify_body(text: &str) -> serde_json::Value {
    let alternatives = RiskSignal::ALL
        .iter()
        .map(|s| format!("\"{}\"", s.wire()))
        .collect::<Vec<_>>()
        .join(" | ");
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
            { "role": "user", "content": text },
        ],
        "stream": false,
        "temperature": 0.0,
        "max_tokens": 8,
        "grammar": format!("root ::= {alternatives}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_msg(content: &str, attachments: Option<Vec<crate::types::Attachment>>) -> ChatMessage {
        ChatMessage {
            id: crate::types::new_message_id(),
            role: "user".to_string(),
            content: content.to_string(),
            created_at_ms: None,
            model: None,
            citations: None,
            thinking: None,
            steps: None,
            attachments,
            artifacts: None,
            stats: None,
            display_content: None,
            injection_notices: None,
            withheld_from_model: None,
        }
    }

    #[test]
    fn plain_message_serializes_content_as_a_string() {
        let body = build_api_message(&user_msg("hello", None));
        assert_eq!(body["role"], "user");
        assert_eq!(body["content"], "hello");
    }

    #[test]
    fn model_bound_skips_a_withheld_turn_and_its_replies() {
        let mut system = user_msg("You are OpenPal.", None);
        system.role = "system".to_string();
        let first = user_msg("first question", None);
        let reply = |content: &str| {
            let mut m = user_msg(content, None);
            m.role = "assistant".to_string();
            m
        };
        let mut withheld = user_msg("summarize the attached file", None);
        withheld.withheld_from_model = Some(true);
        let mut kept = user_msg("next question", None);
        kept.withheld_from_model = Some(false);
        let messages = vec![
            system,
            first,
            reply("first answer"),
            withheld,
            reply(""),
            reply("an answer written from the withheld file"),
            kept,
            reply("next answer"),
        ];

        let contents: Vec<&str> = model_bound(&messages).map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec![
                "You are OpenPal.",
                "first question",
                "first answer",
                "next question",
                "next answer",
            ]
        );
        // Every request is built from the same filtered list.
        let wire = build_api_messages(&messages);
        let roles: Vec<&str> = wire.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(
            roles,
            vec!["system", "user", "assistant", "user", "assistant"]
        );
        assert!(!serde_json::to_string(&wire).unwrap().contains("withheld"));
    }

    #[test]
    fn classify_body_is_non_streaming_and_constrains_output_to_every_tag() {
        let body = build_classify_body("how do I reverse a linked list in Rust?");
        assert_eq!(body["stream"], false);
        assert_eq!(
            body["messages"][1]["content"],
            "how do I reverse a linked list in Rust?"
        );
        let grammar = body["grammar"].as_str().unwrap();
        for tag in TaskType::ALL {
            assert!(
                grammar.contains(&format!("\"{}\"", tag.wire())),
                "grammar must offer {} as a choice: {grammar}",
                tag.wire()
            );
        }
    }

    #[test]
    fn risk_classify_body_is_non_streaming_and_constrains_output_to_every_signal() {
        let body = build_risk_classify_body("I don't want to be here anymore");
        assert_eq!(body["stream"], false);
        assert_eq!(
            body["messages"][1]["content"],
            "I don't want to be here anymore"
        );
        let grammar = body["grammar"].as_str().unwrap();
        for signal in RiskSignal::ALL {
            assert!(
                grammar.contains(&format!("\"{}\"", signal.wire())),
                "grammar must offer {} as a choice: {grammar}",
                signal.wire()
            );
        }
    }

    #[test]
    fn document_attachment_is_folded_into_the_text() {
        use crate::types::{Attachment, AttachmentKind};
        let att = Attachment {
            kind: AttachmentKind::Document,
            name: "notes.txt".to_string(),
            data_uri: None,
            text: Some("the report says X".to_string()),
        };
        let body = build_api_message(&user_msg("summarize this", Some(vec![att])));
        let content = body["content"].as_str().expect("plain string content");
        assert!(content.contains("[Attached file: notes.txt]"));
        assert!(content.contains("the report says X"));
        assert!(content.ends_with("summarize this"));
    }

    #[test]
    fn image_attachment_becomes_a_content_part_array() {
        use crate::types::{Attachment, AttachmentKind};
        let att = Attachment {
            kind: AttachmentKind::Image,
            name: "pic.png".to_string(),
            data_uri: Some("data:image/png;base64,AAAA".to_string()),
            text: None,
        };
        let body = build_api_message(&user_msg("what is this?", Some(vec![att])));
        let parts = body["content"].as_array().expect("array content");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn image_with_no_caption_omits_the_empty_text_part() {
        use crate::types::{Attachment, AttachmentKind};
        let att = Attachment {
            kind: AttachmentKind::Image,
            name: "pic.png".to_string(),
            data_uri: Some("data:image/png;base64,AAAA".to_string()),
            text: None,
        };
        let body = build_api_message(&user_msg("", Some(vec![att])));
        let parts = body["content"].as_array().expect("array content");
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "image_url");
    }

    #[test]
    fn disk_slow_startup_timeout_floors_small_models() {
        // A tiny model would compute well under the floor from read time
        // alone — the floor exists precisely so Linux's mmap prefetch and
        // llama.cpp's own load work still get room regardless of size.
        assert_eq!(
            disk_slow_startup_timeout(1_000_000_000, None),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn disk_slow_startup_timeout_scales_with_size_beyond_the_floor() {
        // 800 * (100 MiB/s, the assumed default) worth of bytes reads in
        // 800s; doubled for slack = 1600s — comfortably past the floor, so
        // the size-based budget wins.
        let size = 800 * 100 * 1024 * 1024;
        let timeout = disk_slow_startup_timeout(size, None);
        assert_eq!(timeout, Duration::from_secs(1600));
        assert!(
            timeout > STARTUP_TIMEOUT,
            "a disk-slow budget must exceed the normal one"
        );
    }

    #[test]
    fn disk_slow_startup_timeout_uses_measured_speed_when_available() {
        // A fast measured drive (1 GB/s) on a 4 GB model reads in 4s, doubled
        // is 8s — well under the floor, so the floor still wins.
        let fast = disk_slow_startup_timeout(4_000_000_000, Some(1_000_000_000));
        assert_eq!(fast, MIN_DISK_SLOW_STARTUP_TIMEOUT);

        // The same model measured much slower than the assumed default must
        // widen the budget past what the fallback constant would give.
        let size = 50_000_000_000;
        let slow_measured = disk_slow_startup_timeout(size, Some(50_000_000));
        let slow_assumed = disk_slow_startup_timeout(size, None);
        assert!(
            slow_measured > slow_assumed,
            "a slower-than-assumed measured speed must widen the budget beyond the fallback"
        );
    }

    #[test]
    fn disk_slow_startup_timeout_measured_zero_does_not_panic() {
        // A pathological probe result must never divide by zero — it should
        // just fall back to "as slow as we can express", which the ceiling
        // (not the floor — 0 widens the budget, it doesn't shrink it) covers.
        let timeout = disk_slow_startup_timeout(10_000_000_000, Some(0));
        assert!(timeout >= MIN_DISK_SLOW_STARTUP_TIMEOUT);
        assert!(timeout <= MAX_DISK_SLOW_STARTUP_TIMEOUT);
    }

    #[test]
    fn disk_slow_startup_timeout_caps_a_pathologically_low_measurement() {
        // A huge model measured at a near-zero speed (e.g. disk contention
        // during the probe) must not turn into a multi-day wait — it should
        // hit the ceiling and fail visibly instead of hanging silently.
        let timeout = disk_slow_startup_timeout(50_000_000_000, Some(20_000));
        assert_eq!(timeout, MAX_DISK_SLOW_STARTUP_TIMEOUT);
    }

    // ------------------------------------------------------------------
    // MTP first-message crash fallback
    // ------------------------------------------------------------------

    /// Test double that dies (optionally after emitting content) on a chosen
    /// chat call, rebuilds a stripped MTP snapshot on every MTP-armed start,
    /// and records every start's args + every chat call.
    struct FakeMtpState {
        starts: Vec<Vec<String>>,
        chat_calls: u32,
        /// 1-based call number that fails; 0 = never fails.
        fail_on_call: u32,
        /// Emit a Token before failing (models a mid-answer stream break).
        emit_before_fail: bool,
        generation: u64,
        running: bool,
        mtp_fallback: Option<MtpRestart>,
        /// Set when a chat call fails, or directly by a test — what
        /// [`EngineBackend::child_died`] reports (the "subprocess exited"
        /// signal that makes recovery meaningful).
        child_exited: bool,
        /// Optional `/health`-able address; when `Some`, the fake advertises
        /// a Running engine the manager's `is_healthy` can probe.
        endpoint: Option<EngineEndpoint>,
    }

    struct FakeMtpBackend {
        state: Arc<tokio::sync::Mutex<FakeMtpState>>,
    }

    #[async_trait]
    impl EngineBackend for FakeMtpBackend {
        async fn start(
            &self,
            server_binary: &Path,
            model_id: &str,
            model_path: &Path,
            cfg: &EngineConfig,
            disk_slow: bool,
            startup_timeout: Duration,
        ) -> Result<()> {
            let mut s = self.state.lock().await;
            s.generation += 1;
            s.running = true;
            s.starts.push(cfg.extra_args.clone());
            let has_mtp = cfg
                .extra_args
                .iter()
                .any(|a| a == "--spec-type" || a.starts_with("--spec-type="));
            // Mirror SubprocessBackend::start's snapshot shape: the stored
            // config already has every --spec-* arg stripped.
            s.mtp_fallback = has_mtp.then(|| MtpRestart {
                server_binary: server_binary.to_path_buf(),
                model_id: model_id.to_string(),
                model_path: model_path.to_path_buf(),
                cfg: EngineConfig {
                    extra_args: cfg
                        .extra_args
                        .iter()
                        .filter(|a| !a.starts_with("--spec-"))
                        .cloned()
                        .collect(),
                    ..cfg.clone()
                },
                disk_slow,
                startup_timeout,
            });
            Ok(())
        }

        async fn stop(&self) -> Result<()> {
            let mut s = self.state.lock().await;
            s.generation += 1;
            s.running = false;
            s.mtp_fallback = None;
            Ok(())
        }

        async fn status(&self) -> EngineStatus {
            let s = self.state.lock().await;
            if s.running {
                EngineStatus::Running {
                    model_id: "fake".to_string(),
                    port: 1,
                    disk_slow: false,
                    auto_unload_at: None,
                }
            } else {
                EngineStatus::Stopped
            }
        }

        async fn activity(&self) -> Option<ActivityInfo> {
            let s = self.state.lock().await;
            s.running.then(|| ActivityInfo {
                disk_slow: false,
                idle_for: Duration::ZERO,
                generating: false,
                generation: s.generation,
            })
        }

        async fn touch_activity(&self) {}
        async fn begin_generation(&self) {}
        async fn end_generation(&self) {}

        async fn stop_if_current(&self, expected_generation: u64) -> Result<()> {
            let mut s = self.state.lock().await;
            if s.generation == expected_generation {
                s.generation += 1;
                s.running = false;
                s.mtp_fallback = None;
            }
            Ok(())
        }

        async fn chat_stream(
            &self,
            _messages: &[ChatMessage],
            _sampling: &SamplingParams,
            _cancel: Arc<AtomicBool>,
            on_event: &mut (dyn FnMut(ChatEvent) + Send),
        ) -> Result<()> {
            let mut s = self.state.lock().await;
            s.chat_calls += 1;
            let fails = s.chat_calls == s.fail_on_call;
            if fails && s.emit_before_fail {
                on_event(ChatEvent::Token {
                    text: "partial answer".to_string(),
                });
            }
            if fails {
                s.child_exited = true;
                return Err(crate::Error::Engine("fake died".to_string()));
            }
            on_event(ChatEvent::Token {
                text: "full answer".to_string(),
            });
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

        async fn mtp_restart_snapshot(&self) -> Option<MtpRestart> {
            self.state.lock().await.mtp_fallback.clone()
        }

        async fn child_died(&self) -> bool {
            self.state.lock().await.child_exited
        }

        async fn endpoint(&self) -> Option<EngineEndpoint> {
            self.state.lock().await.endpoint.clone()
        }
    }

    fn fake_manager(
        fail_on_call: u32,
        emit_before_fail: bool,
    ) -> (EngineManager, Arc<tokio::sync::Mutex<FakeMtpState>>) {
        fake_manager_with(fail_on_call, emit_before_fail, false, None)
    }

    fn fake_manager_with(
        fail_on_call: u32,
        emit_before_fail: bool,
        child_exited: bool,
        endpoint: Option<EngineEndpoint>,
    ) -> (EngineManager, Arc<tokio::sync::Mutex<FakeMtpState>>) {
        let state = Arc::new(tokio::sync::Mutex::new(FakeMtpState {
            starts: vec![],
            chat_calls: 0,
            fail_on_call,
            emit_before_fail,
            generation: 0,
            running: false,
            mtp_fallback: None,
            child_exited,
            endpoint,
        }));
        let manager = EngineManager::with_backend(Box::new(FakeMtpBackend {
            state: state.clone(),
        }));
        (manager, state)
    }

    fn mtp_cfg() -> EngineConfig {
        EngineConfig {
            context_length: 4096,
            gpu_layers: 0,
            threads: None,
            extra_args: vec![
                "--spec-type".to_string(),
                "draft-mtp".to_string(),
                "--spec-draft-n-max".to_string(),
                "4".to_string(),
            ],
            mmproj_path: None,
        }
    }

    /// The plan's core retry test: a fake backend that dies on the first
    /// chat request must be restarted WITHOUT its MTP args, then answer —
    /// with no Failed status and no protocol change for the caller.
    #[tokio::test]
    async fn chat_stream_restarts_without_mtp_and_resends_after_a_first_message_crash() {
        let (manager, state) = fake_manager(/* fail first chat */ 1, false);
        manager
            .start(
                Path::new("/fake/llama-server"),
                "qwen3.6-27b",
                Path::new("/fake/qwen.gguf"),
                &mtp_cfg(),
                false,
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        let mut events: Vec<ChatEvent> = vec![];
        let result = manager
            .chat_stream(
                &[],
                &SamplingParams::default(),
                Arc::new(AtomicBool::new(false)),
                |ev| events.push(ev),
            )
            .await;

        assert!(result.is_ok(), "the MTP fallback must deliver an answer");
        let s = state.lock().await;
        assert_eq!(s.chat_calls, 2, "exactly one resend after the crash");
        assert_eq!(s.starts.len(), 2, "the engine must restart exactly once");
        assert!(
            !s.starts[1].iter().any(|a| a.starts_with("--spec-")),
            "the restart must drop every --spec-* arg: {:?}",
            s.starts[1]
        );
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, ChatEvent::Token { text } if text == "full answer")),
            "the retried request must deliver the answer"
        );
        drop(s);
        assert!(
            matches!(manager.status().await, EngineStatus::Running { .. }),
            "a recovered MTP crash must not leave the engine Failed"
        );
    }

    /// Without an MTP snapshot there is nothing to fall back to — a plain
    /// crash keeps today's single-attempt error path.
    #[tokio::test]
    async fn chat_stream_does_not_restart_without_an_mtp_snapshot() {
        let (manager, state) = fake_manager(1, false);
        let mut cfg = mtp_cfg();
        cfg.extra_args.clear(); // no MTP args -> no snapshot
        manager
            .start(
                Path::new("/fake/llama-server"),
                "plain-model",
                Path::new("/fake/plain.gguf"),
                &cfg,
                false,
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        let result = manager
            .chat_stream(
                &[],
                &SamplingParams::default(),
                Arc::new(AtomicBool::new(false)),
                |_| {},
            )
            .await;
        assert!(result.is_err(), "a plain crash keeps the error path");
        let s = state.lock().await;
        assert_eq!(s.chat_calls, 1, "no retry without an MTP snapshot");
        assert_eq!(s.starts.len(), 1, "no restart without an MTP snapshot");
    }

    /// A crash that happened mid-answer (a token already emitted) must NOT
    /// trigger the resend — it would duplicate tokens the user already saw.
    #[tokio::test]
    async fn chat_stream_does_not_resend_after_a_mid_answer_crash() {
        let (manager, state) = fake_manager(1, true);
        manager
            .start(
                Path::new("/fake/llama-server"),
                "qwen3.6-27b",
                Path::new("/fake/qwen.gguf"),
                &mtp_cfg(),
                false,
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        let result = manager
            .chat_stream(
                &[],
                &SamplingParams::default(),
                Arc::new(AtomicBool::new(false)),
                |_| {},
            )
            .await;
        assert!(result.is_err(), "a mid-answer crash surfaces as an error");
        let s = state.lock().await;
        assert_eq!(s.chat_calls, 1, "a mid-answer crash must not resend");
        assert_eq!(s.starts.len(), 1, "a mid-answer crash must not restart");
    }

    #[tokio::test]
    async fn restart_without_mtp_restarts_with_stripped_cfg_and_bumps_generation() {
        let (manager, state) = fake_manager(0, false);
        manager
            .start(
                Path::new("/fake/llama-server"),
                "qwen3.6-27b",
                Path::new("/fake/qwen.gguf"),
                &mtp_cfg(),
                false,
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        let generation = state.lock().await.generation;
        assert!(
            manager.restart_without_mtp(Some(generation)).await,
            "an MTP-armed engine restart must be reported as done"
        );
        let s = state.lock().await;
        assert_eq!(s.starts.len(), 2, "the recovery restart calls start again");
        assert!(
            s.starts
                .last()
                .unwrap()
                .iter()
                .all(|a| !a.starts_with("--spec-")),
            "the restarted engine must have every --spec-* arg stripped"
        );
        assert!(
            s.mtp_fallback.is_none(),
            "a stripped restart has nothing left to recover from"
        );
        assert!(s.running, "the restarted engine is running");
        assert_eq!(
            s.generation,
            generation + 1,
            "the recovery restart bumps the generation"
        );
    }

    #[tokio::test]
    async fn restart_without_mtp_is_a_noop_without_a_snapshot() {
        let (manager, state) = fake_manager(0, false);
        let mut cfg = mtp_cfg();
        cfg.extra_args.clear(); // plain engine -> no snapshot armed
        manager
            .start(
                Path::new("/fake/llama-server"),
                "plain-model",
                Path::new("/fake/plain.gguf"),
                &cfg,
                false,
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        let generation = state.lock().await.generation;
        assert!(!manager.restart_without_mtp(Some(generation)).await);
        let s = state.lock().await;
        assert_eq!(s.starts.len(), 1, "no snapshot means no recovery restart");
    }

    #[tokio::test]
    async fn restart_without_mtp_refuses_a_superseded_engine() {
        let (manager, state) = fake_manager(0, false);
        manager
            .start(
                Path::new("/fake/llama-server"),
                "qwen3.6-27b",
                Path::new("/fake/qwen.gguf"),
                &mtp_cfg(),
                false,
                Duration::from_secs(30),
            )
            .await
            .unwrap();

        // The caller captured a stale generation — a concurrent stop/start
        // already moved on — so recovery must refuse rather than resurrect a
        // stripped copy of the engine it replaced.
        let stale = state.lock().await.generation - 1;
        assert!(!manager.restart_without_mtp(Some(stale)).await);
        let s = state.lock().await;
        assert_eq!(s.starts.len(), 1, "a superseded engine is not restarted");
    }

    #[tokio::test]
    async fn is_healthy_is_true_when_no_engine_is_running() {
        let (manager, _) = fake_manager(0, false);
        assert!(
            manager.is_healthy().await,
            "an engine that isn't Running is vacuously healthy"
        );
    }

    #[tokio::test]
    async fn is_healthy_is_false_when_the_child_has_exited() {
        // The endpoint is never reached — child_died short-circuits first.
        let endpoint = EngineEndpoint {
            base_url: "http://127.0.0.1:1/v1".to_string(),
            api_key: "k".to_string(),
            model_id: "qwen3.6-27b".to_string(),
            request_timeout_secs: 300,
        };
        let (manager, _) = fake_manager_with(0, false, true, Some(endpoint));
        assert!(!manager.is_healthy().await);
    }

    #[tokio::test]
    async fn is_healthy_pings_the_health_endpoint() {
        let (manager, _) = fake_manager_with(0, false, false, Some(tiny_ok_endpoint()));
        assert!(
            manager.is_healthy().await,
            "a /health 200 from the live endpoint means healthy"
        );
    }

    #[tokio::test]
    async fn is_healthy_is_false_when_the_endpoint_is_unreachable() {
        // Bind then drop so the port is guaranteed closed: the manager's
        // probe must time out fast and report unhealthy, not hang the test.
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            format!("http://127.0.0.1:{port}/v1")
        };
        let endpoint = EngineEndpoint {
            base_url: closed,
            api_key: "k".to_string(),
            model_id: "qwen3.6-27b".to_string(),
            request_timeout_secs: 300,
        };
        let (manager, _) = fake_manager_with(0, false, false, Some(endpoint));
        assert!(!manager.is_healthy().await);
    }

    /// An `EngineEndpoint` whose `/health` answers 200 once, for
    /// `is_healthy`'s probe. The listener's thread lives for the lifetime of
    /// the single accept, then dies with it — fine for one-shot tests.
    fn tiny_ok_endpoint() -> EngineEndpoint {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            }
        });
        EngineEndpoint {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            api_key: "k".to_string(),
            model_id: "qwen3.6-27b".to_string(),
            request_timeout_secs: 300,
        }
    }
}
