//! Prompt-injection detection: notice when text on its way to the model is
//! trying to give the model orders the user never gave, and say so — pause
//! for the user before the model reads it, note it in the reply, flag the
//! next consent card of the same agent turn
//! (`DangerFlag::PromptInjectionSuspected`), and write one entry in the app
//! log per detection.
//!
//! **Only turns that use tools are checked** — agent mode and the web-search
//! turn. That's where injected text can act without the user: call a tool,
//! or send the conversation somewhere with a `fetch_url`. Plain and cloud
//! chat and Writing Tools give the model no tools, so whatever injected text
//! does there still needs the user to act on the reply, and checking them
//! would cost every message the classifier's load and memory. Nothing they
//! carried is lost: a tool turn checks the conversation's whole model-bound
//! history, so a file attached in plain chat is reviewed the first time the
//! conversation reaches a tool.
//!
//! **Flag, log, pause for review, notice.** What a detection does depends on
//! who chose the text:
//! - Text the user didn't write — an attached document, an invoked skill's
//!   body, and every tool result the agent and web-search turns read — is
//!   held back until the user answers a review card ([`review`]): Continue
//!   sends it byte-for-byte unchanged, Stop ends the turn without the model
//!   reading any of it. Nothing is ever rewritten or partly withheld.
//! - What the user typed or pasted (the composer, an `ask_user` answer) is
//!   sent as written: pressing Send was the decision. The reply only gets a
//!   notice.
//! - A detection never gates a tool call by itself: the consent card's three
//!   buttons still decide (`agent::tool::DangerFlag`'s own contract).
//!
//! When there's no classifier to ask (still downloading, failed to load) text
//! goes through unchecked, as it always did. When the classifier fails on one
//! particular text, or has no verdict within [`review::GATE_SCAN_WAIT`], the
//! user is asked anyway: a crafted text must not be able to fail open.
//!
//! **A knowing exception to mono-model first** (ARCHITECTURE.md → Design
//! philosophy): this is a second, dedicated model on every machine's default
//! path, with no setting that turns it off — the reporter of issue #9 chose
//! that over teaching the loaded chat model to do it, for security. It's in
//! memory only around tool turns (see [`Detector::hold`]). The chat
//! model couldn't do it anyway: llama-server has one inference slot and an
//! agent turn holds it for the turn's whole length
//! (`EngineBackend::raw_classify` returns `None` mid-turn), and cloud chat has
//! no local engine at all.
//!
//! Pieces:
//! - [`Detector`]: app-lifetime. Owns the classifier — downloaded in the
//!   background ([`provision`]), loaded on the first scan of a turn that uses
//!   tools, held for that turn ([`ClassifierHold`]), released when idle or
//!   with the local chat model — plus the cache that keeps one text from
//!   being scored or logged twice, and the per-conversation memory of what
//!   the user chose to continue with.
//! - [`TurnGuard`]: one chat turn's view. [`TurnGuard::gate_user_side`] and
//!   [`TurnGuard::gate_tool_result`] (in `review.rs`) hold text back for the
//!   review card; [`TurnGuard::scan`] and [`TurnGuard::notice_in_background`]
//!   check typed text without holding anything up; [`TurnGuard::detected`]
//!   answers "was anything this turn flagged?" for the consent card.
//! - [`run_guarded`]: one turn's whole wrapper — the user-side gate, then the
//!   turn itself, never the turn after a Stop.
//! - [`OnnxClassifier`]: the model itself (`classifier.rs`); the pinned files
//!   and their download are in `model.rs`.
//!
//! What gets checked, and where: every model-bound user message's typed
//! text, skill body and attached document text at the start of every turn
//! that uses tools (`commands::chat_stream`, agent and web-search turns), and
//! every tool result in `agent::orchestrator::run_and_report` (the same
//! turns, subagents included). Not checked: plain chat, cloud chat and
//! Writing Tools; the model's own earlier replies; the system prompt (the
//! user's own setting); images; and messages the user already withheld from
//! the model (`engine::model_bound`).

mod classifier;
mod model;
pub mod review;

pub use classifier::{Classify, OnnxClassifier};
pub use model::{files_present, provision};
pub use review::{
    run_guarded, tool_label, Gate, Review, GATE_SCAN_WAIT, NOTICE_GRACE, STOPPED_NOTE,
};

use crate::agent::tool::EventSink;
use crate::types::{AttachmentKind, ChatEvent, ChatMessage, InjectionNotice};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// P(injection) at or above which text is flagged — the classifier's own
/// decision boundary. Not tuned on this app's sample sets on purpose (see
/// `tests/fixtures/prompt_injection/README.md`).
pub const THRESHOLD: f32 = 0.5;

/// `tracing` target of the one app-log line written per detection.
/// `crates/app/src/logging.rs` always lets it through, whatever log level the
/// user picked — a log level is not a way to turn detection off.
pub const LOG_TARGET: &str = "prompt_injection";

/// How long a loaded classifier (~0.8–1 GB with its tokenizer) stays in
/// memory after its last scan, or after the last tool turn holding it ended
/// (see [`Detector::unload_if_idle`]).
const IDLE_UNLOAD_AFTER: Duration = Duration::from_secs(5 * 60);
/// How long to wait before trying to load the files again after a load
/// failed, instead of failing the same way on every scan.
const LOAD_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
/// Waits between download attempts while the pinned files are missing; the
/// last one repeats.
const DOWNLOAD_RETRY: [Duration; 4] = [
    Duration::from_secs(30),
    Duration::from_secs(2 * 60),
    Duration::from_secs(10 * 60),
    Duration::from_secs(60 * 60),
];
/// Scores remembered per run, keyed by the scanned text's sha256. Cleared
/// wholesale when full — a re-scan is only a little CPU and, for a flagged
/// text, one more log line.
const CACHE_CAPACITY: usize = 4096;
/// Continued texts remembered per run (see [`Detector::remember_continued`]).
/// Cleared wholesale when full — the cost is asking once more.
const CONTINUED_CAPACITY: usize = 4096;

/// Which route a piece of text took into the model. Only ever logged as its
/// [`label`](Source::label), and sent to the UI as its [`id`](Source::id) —
/// never the text itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// A page read with `fetch_url`.
    WebPage,
    /// `web_search` result titles and snippets.
    WebSearch,
    /// A file read with `read_file`.
    File,
    /// `run_shell` / `run_code` output.
    CommandOutput,
    /// Text extracted from a document attached to a chat message.
    Attachment,
    /// Another conversation's text, from `read_project_conversation` (or
    /// the titles `list_project_conversations` returns).
    ProjectConversation,
    /// An imported skill's body, folded into the user's message by a
    /// `/<skill>` invocation.
    Skill,
    /// What the user typed or pasted — the composer, or an `ask_user` answer.
    Composer,
    /// Any other tool's result.
    ToolOutput,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::WebPage => "web_page",
            Source::WebSearch => "web_search",
            Source::File => "file",
            Source::CommandOutput => "command_output",
            Source::Attachment => "attachment",
            Source::ProjectConversation => "project_conversation",
            Source::Skill => "skill",
            Source::Composer => "composer",
            Source::ToolOutput => "tool_output",
        }
    }

    /// The name the UI knows this route by (`FlaggedItem::source`,
    /// `InjectionNotice::source`) — camelCase like every other wire value;
    /// [`label`](Self::label) stays snake_case for the log.
    pub fn id(self) -> &'static str {
        match self {
            Source::WebPage => "webPage",
            Source::WebSearch => "webSearch",
            Source::File => "file",
            Source::CommandOutput => "commandOutput",
            Source::Attachment => "attachment",
            Source::ProjectConversation => "projectConversation",
            Source::Skill => "skill",
            Source::Composer => "composer",
            Source::ToolOutput => "toolOutput",
        }
    }

    /// The route a tool's result takes, or `None` for the few tools whose
    /// result is the app's (or the model's) own words: `write_file` and
    /// `create_artifact` return fixed confirmations, and `spawn_subagent`
    /// returns the helper model's answer — whose own tool results were
    /// already scanned inside its nested loop. A tool this list doesn't know
    /// is scanned, not skipped.
    pub fn for_tool(tool_id: &str) -> Option<Source> {
        match tool_id {
            "fetch_url" => Some(Source::WebPage),
            "web_search" => Some(Source::WebSearch),
            "read_file" => Some(Source::File),
            "run_shell" | "run_code" => Some(Source::CommandOutput),
            "read_project_conversation" | "list_project_conversations" => {
                Some(Source::ProjectConversation)
            }
            "ask_user" => Some(Source::Composer),
            "spawn_subagent" | "write_file" | "create_artifact" => None,
            _ => Some(Source::ToolOutput),
        }
    }
}

/// An attached document exactly as `engine::build_api_message` folds it into
/// the request (less the blank line after it): the file's name reaches the
/// model too, and whoever named the file chose it. Checked in this form
/// everywhere — at the start of a turn and when the file is picked — so the
/// verdict cached at pick time is the one the turn finds.
pub fn attachment_scan_text(name: &str, text: &str) -> String {
    format!("[Attached file: {name}]\n{text}")
}

/// What [`Detector::assess`] found out about one text.
#[derive(Debug, Clone, PartialEq)]
pub enum Assessment {
    /// The classifier's P(injection), and the byte range (in the prepared
    /// text) of its most suspicious window when it could say — for a
    /// flagged text, narrowed to what the review card can show whole
    /// ([`review::locate`]).
    Scored {
        score: f32,
        span: Option<Range<usize>>,
    },
    /// There's no classifier to ask: still downloading, failed to load (and
    /// waiting out its retry), or none configured. The text goes through
    /// unchecked — the documented contract while detection is unavailable.
    Unavailable,
    /// The classifier is there but failed on this text. Treated as "couldn't
    /// check", never as "safe": a text crafted to make it fail mustn't pass.
    Failed,
}

impl Assessment {
    pub fn flagged(&self) -> bool {
        matches!(self, Assessment::Scored { score, .. } if *score >= THRESHOLD)
    }
}

/// A cached verdict: P(injection) and the most suspicious span, as in
/// [`Assessment::Scored`].
type Scored = (f32, Option<Range<usize>>);

/// The app-lifetime detector: one per process (`AppState.injection`).
pub struct Detector {
    backend: Backend,
    /// Held across a whole scan, so scans run one at a time — bounded CPU
    /// next to the chat model, and a text queued twice is scored once.
    state: tokio::sync::Mutex<State>,
    /// Scores (and their most suspicious span) already computed this run,
    /// keyed by the scanned text's sha256. Behind its own short-lived lock,
    /// not `state`'s: a text already scored is answered at once instead of
    /// queueing behind a long scan in progress, so a consent card's wait
    /// never runs out on a verdict the detector already has.
    scores: Mutex<HashMap<[u8; 32], Scored>>,
    /// Flagged texts the user chose to continue with, per conversation (see
    /// [`Detector::remember_continued`]). In memory only: an approval saved
    /// with the conversation could be forged by importing or syncing one.
    continued: Mutex<HashSet<[u8; 32]>>,
    warned_unavailable: AtomicBool,
    /// Live [`ClassifierHold`]s: tool turns in progress. While any is held,
    /// the classifier isn't unloaded for sitting idle.
    holds: AtomicUsize,
    /// When the last hold was dropped, so a card left open for a while
    /// doesn't make the classifier look idle the moment its turn ends.
    released_at: Mutex<Option<Instant>>,
}

/// Loads the classifier from the pinned files in a directory.
type Loader = fn(&Path) -> Result<Arc<dyn Classify>, String>;

enum Backend {
    /// The pinned model files in `dir`, loaded on demand by `load`
    /// ([`OnnxClassifier::load`] outside tests).
    Files { dir: PathBuf, load: Loader },
    /// A ready classifier that is never unloaded (tests).
    Fixed(Arc<dyn Classify>),
    /// No classifier at all: every scan is a no-op.
    None,
}

#[derive(Default)]
struct State {
    loaded: Option<Loaded>,
    load_failed_at: Option<Instant>,
}

struct Loaded {
    classifier: Arc<dyn Classify>,
    last_used: Instant,
}

impl Detector {
    /// The production detector over the pinned model files in `dir` —
    /// downloaded there by [`Detector::download_until_present`].
    pub fn new(dir: PathBuf) -> Self {
        Self::with_backend(Backend::Files {
            dir,
            load: |dir| OnnxClassifier::load(dir).map(|c| Arc::new(c) as Arc<dyn Classify>),
        })
    }

    /// A detector around an already-loaded classifier.
    pub fn with_classifier(classifier: Arc<dyn Classify>) -> Self {
        Self::with_backend(Backend::Fixed(classifier))
    }

    /// A detector with nothing to ask: every scan is skipped (and says so
    /// once in the log).
    pub fn unavailable() -> Self {
        Self::with_backend(Backend::None)
    }

    fn with_backend(backend: Backend) -> Self {
        Self {
            backend,
            state: tokio::sync::Mutex::new(State::default()),
            scores: Mutex::new(HashMap::new()),
            continued: Mutex::new(HashSet::new()),
            warned_unavailable: AtomicBool::new(false),
            holds: AtomicUsize::new(0),
            released_at: Mutex::new(None),
        }
    }

    /// Keep the classifier in memory until the returned hold is dropped —
    /// for the length of one turn that uses tools (see
    /// [`TurnGuard::for_tool_turn`]). Loads nothing by itself: the turn's
    /// first scan does.
    pub fn hold(self: &Arc<Self>) -> ClassifierHold {
        self.holds.fetch_add(1, Ordering::SeqCst);
        ClassifierHold(self.clone())
    }

    /// Score `text` as it reaches the model from `source`: `Some(P(injection))`,
    /// or `None` when there's no score to give (see [`Detector::assess`],
    /// which tells those cases apart).
    pub async fn score(&self, source: Source, text: &str) -> Option<f32> {
        match self.assess(source, text).await {
            Assessment::Scored { score, .. } => Some(score),
            Assessment::Unavailable | Assessment::Failed => None,
        }
    }

    /// Check `text` as it reaches the model from `source`. The first time a
    /// given text is flagged this run, one detection line goes to the app
    /// log — its route, score and length, never the text: the log is meant
    /// to be shared for support.
    pub async fn assess(&self, source: Source, text: &str) -> Assessment {
        let prepared = prepare(source, text);
        if prepared.trim().is_empty() {
            return Assessment::Scored {
                score: 0.0,
                span: None,
            };
        }
        let key: [u8; 32] = Sha256::digest(prepared.as_bytes()).into();
        if let Some((score, span)) = self.cached(&key) {
            return Assessment::Scored { score, span };
        }

        let mut state = self.state.lock().await;
        // Queued behind another scan of this same text, which has scored it.
        if let Some((score, span)) = self.cached(&key) {
            return Assessment::Scored { score, span };
        }
        let Some(classifier) = self.classifier(&mut state).await else {
            return Assessment::Unavailable;
        };
        let owned = prepared.into_owned();
        let chars = owned.chars().count();
        let (score, span) = match tokio::task::spawn_blocking(move || {
            let (score, span) = classifier.p_injection_located(&owned)?;
            // Flagged: find the part the review card should show while the
            // classifier is at hand, so the cached verdict carries it.
            let span = if score >= THRESHOLD {
                review::locate(classifier.as_ref(), &owned, span)
            } else {
                span
            };
            Ok::<_, String>((score, span))
        })
        .await
        {
            Ok(Ok(scored)) => scored,
            Ok(Err(e)) => {
                tracing::warn!("prompt-injection scan failed ({}): {e}", source.label());
                return Assessment::Failed;
            }
            Err(e) => {
                tracing::warn!("prompt-injection scan failed ({}): {e}", source.label());
                return Assessment::Failed;
            }
        };
        self.remember(key, (score, span.clone()));
        if let Some(loaded) = state.loaded.as_mut() {
            loaded.last_used = Instant::now();
        }
        drop(state);

        if score >= THRESHOLD {
            tracing::warn!(
                target: LOG_TARGET,
                source = source.label(),
                score = %format!("{score:.2}"),
                chars,
                "possible prompt injection in text sent to the model"
            );
        }
        Assessment::Scored { score, span }
    }

    fn cached(&self, key: &[u8; 32]) -> Option<Scored> {
        self.scores.lock().unwrap().get(key).cloned()
    }

    fn remember(&self, key: [u8; 32], scored: Scored) {
        let mut scores = self.scores.lock().unwrap();
        if scores.len() >= CACHE_CAPACITY {
            scores.clear();
        }
        scores.insert(key, scored);
    }

    /// Whether the user already chose Continue for this exact text, from this
    /// route, in this conversation, during this run. Never true without a
    /// conversation id (Writing Tools, tests): an approval with no
    /// conversation to scope it to would let the text through everywhere.
    pub(crate) fn was_continued(&self, conversation_id: &str, source: Source, text: &str) -> bool {
        !conversation_id.is_empty()
            && self.continued.lock().unwrap().contains(&continued_key(
                conversation_id,
                source,
                text,
            ))
    }

    /// Remember that the user chose Continue for `text`, so the rest of this
    /// conversation doesn't ask about it again — the whole history is
    /// re-sent every turn, an attachment included. Stop is never remembered:
    /// a stopped message is withheld from the model instead.
    pub(crate) fn remember_continued(&self, conversation_id: &str, source: Source, text: &str) {
        if conversation_id.is_empty() {
            return;
        }
        let mut continued = self.continued.lock().unwrap();
        if continued.len() >= CONTINUED_CAPACITY {
            continued.clear();
        }
        continued.insert(continued_key(conversation_id, source, text));
    }

    async fn classifier(&self, state: &mut State) -> Option<Arc<dyn Classify>> {
        let (dir, load) = match &self.backend {
            Backend::Fixed(classifier) => return Some(classifier.clone()),
            Backend::None => {
                self.warn_unavailable("no classifier is configured");
                return None;
            }
            Backend::Files { dir, load } => (dir, *load),
        };
        if let Some(loaded) = &state.loaded {
            return Some(loaded.classifier.clone());
        }
        if state
            .load_failed_at
            .is_some_and(|at| at.elapsed() < LOAD_RETRY_AFTER)
        {
            return None;
        }
        if !files_present(dir) {
            self.warn_unavailable("the classifier hasn't finished downloading");
            return None;
        }

        let dir = dir.clone();
        let started = Instant::now();
        match tokio::task::spawn_blocking(move || load(&dir)).await {
            Ok(Ok(classifier)) => {
                tracing::info!(
                    "prompt-injection classifier loaded in {} ms",
                    started.elapsed().as_millis()
                );
                state.loaded = Some(Loaded {
                    classifier: classifier.clone(),
                    last_used: Instant::now(),
                });
                state.load_failed_at = None;
                Some(classifier)
            }
            Ok(Err(e)) => Self::load_failed(state, &e),
            Err(e) => Self::load_failed(state, &e.to_string()),
        }
    }

    /// Logged on every failed attempt (at most once per
    /// [`LOAD_RETRY_AFTER`]) rather than through the once-per-run
    /// [`Detector::warn_unavailable`]: an earlier "still downloading" line
    /// must not hide why the files, once there, still can't be used.
    fn load_failed(state: &mut State, why: &str) -> Option<Arc<dyn Classify>> {
        state.load_failed_at = Some(Instant::now());
        tracing::warn!(
            "the prompt-injection classifier failed to load, so text sent to the model isn't \
             being checked (next try in {} min): {why}",
            LOAD_RETRY_AFTER.as_secs() / 60
        );
        None
    }

    /// One app-log line per run the first time detection can't run — not
    /// one per scan.
    fn warn_unavailable(&self, why: &str) {
        if !self.warned_unavailable.swap(true, Ordering::SeqCst) {
            tracing::warn!(
                "prompt-injection detection is unavailable for now ({why}); text sent to the \
                 model isn't being checked until it is"
            );
        }
    }

    /// Drop the loaded classifier now — called when the local chat model is
    /// unloaded, so the two leave memory together. Waits for a scan in
    /// progress; the next scan loads it again.
    pub async fn unload(&self) {
        if self.state.lock().await.loaded.take().is_some() {
            tracing::info!("prompt-injection classifier unloaded with the engine");
        }
    }

    /// Drop the loaded classifier if no tool turn holds it and nothing has
    /// used it for [`IDLE_UNLOAD_AFTER`] — counted from its last scan or
    /// from the end of the last turn that held it, whichever is later. The
    /// next scan loads it again. The app's idle loop calls this on every
    /// tick, whatever the chat model is doing. Never waits: a classifier
    /// mid-scan isn't idle.
    pub fn unload_if_idle(&self) {
        self.unload_if_idle_after(IDLE_UNLOAD_AFTER);
    }

    fn unload_if_idle_after(&self, idle: Duration) {
        let released_at = {
            let released_at = self.released_at.lock().unwrap();
            if self.holds.load(Ordering::SeqCst) > 0 {
                return;
            }
            *released_at
        };
        let Ok(mut state) = self.state.try_lock() else {
            return;
        };
        if state.loaded.as_ref().is_some_and(|loaded| {
            released_at
                .map_or(loaded.last_used, |at| at.max(loaded.last_used))
                .elapsed()
                >= idle
        }) {
            state.loaded = None;
            tracing::info!("prompt-injection classifier unloaded after sitting idle");
        }
    }

    /// The detector's background download, spawned once after unlock: fetch
    /// the pinned files if they're missing, retrying with backoff — an
    /// offline machine or a full disk just leaves detection unavailable until
    /// it works.
    pub async fn download_until_present(self: Arc<Self>, http: reqwest::Client) {
        if let Backend::Files { dir, .. } = &self.backend {
            let was_missing = !files_present(dir);
            let mut attempt = 0;
            while let Err(e) = provision(&http, dir).await {
                let wait = DOWNLOAD_RETRY[attempt.min(DOWNLOAD_RETRY.len() - 1)];
                tracing::warn!(
                    "couldn't download the prompt-injection classifier ({e}); retrying in {} s",
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            if was_missing {
                tracing::info!("prompt-injection classifier downloaded");
            }
        }
    }
}

/// Keeps the classifier in memory while it lives ([`Detector::hold`]). The
/// idle clock restarts when the last one is dropped.
pub struct ClassifierHold(Arc<Detector>);

impl Drop for ClassifierHold {
    fn drop(&mut self) {
        let detector = &self.0;
        // Under the lock, so an idle check can't see the count at zero with
        // the old release time.
        let mut released_at = detector.released_at.lock().unwrap();
        if detector.holds.fetch_sub(1, Ordering::SeqCst) == 1 {
            *released_at = Some(Instant::now());
        }
    }
}

fn continued_key(conversation_id: &str, source: Source, text: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(conversation_id.as_bytes());
    hash.update([0]);
    hash.update(source.label().as_bytes());
    hash.update([0]);
    hash.update(prepare(source, text).as_bytes());
    hash.finalize().into()
}

/// One chat turn's checks. Created at the start of `commands::chat_stream`
/// for a turn that uses tools ([`TurnGuard::for_tool_turn`]); the agent paths
/// carry it on `AgentDeps.injection`, shared with subagents' nested loops, so
/// a detection anywhere in the turn reaches the next consent card.
pub struct TurnGuard {
    detector: Arc<Detector>,
    /// Scopes the user's Continue decisions (see
    /// [`Detector::remember_continued`]). Empty for a guard with no
    /// conversation, which then remembers nothing.
    conversation_id: String,
    flagged: Arc<AtomicBool>,
    in_flight: Arc<tokio::sync::watch::Sender<usize>>,
    /// Cleared by [`TurnGuard::finish`] once the turn is over: a notice that
    /// lands later would attach to whatever reply the UI shows next.
    live: Arc<AtomicBool>,
    /// How long a review gate waits for a verdict before asking with an
    /// "unchecked" item ([`GATE_SCAN_WAIT`]).
    scan_wait: Duration,
    /// Keeps the classifier loaded until the turn is over
    /// ([`TurnGuard::for_tool_turn`]).
    _hold: Option<ClassifierHold>,
}

impl TurnGuard {
    /// A guard with no conversation behind it (tests).
    pub fn new(detector: Arc<Detector>) -> Self {
        Self::for_chat(detector, "")
    }

    /// One turn of the chat `conversation_id`, with no hold on the
    /// classifier.
    pub fn for_chat(detector: Arc<Detector>, conversation_id: &str) -> Self {
        Self {
            detector,
            conversation_id: conversation_id.to_string(),
            flagged: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(tokio::sync::watch::channel(0).0),
            live: Arc::new(AtomicBool::new(true)),
            scan_wait: GATE_SCAN_WAIT,
            _hold: None,
        }
    }

    /// One turn of the chat `conversation_id` that uses tools — the only
    /// turns that are checked. It holds the classifier for as long as the
    /// guard lives, so a review card left open, or a long tool run, doesn't
    /// see it unloaded for sitting idle halfway through the turn.
    pub fn for_tool_turn(detector: Arc<Detector>, conversation_id: &str) -> Self {
        let hold = detector.hold();
        Self {
            _hold: Some(hold),
            ..Self::for_chat(detector, conversation_id)
        }
    }

    #[cfg(test)]
    pub(crate) fn with_scan_wait(mut self, wait: Duration) -> Self {
        self.scan_wait = wait;
        self
    }

    /// Start scanning `text` in the background and return immediately. The
    /// text the caller goes on to send the model is untouched — this works
    /// on a copy. A detection only flags the next consent card and is
    /// logged; text that has to wait for the user goes through a gate in
    /// `review.rs` instead.
    pub fn scan(&self, source: Source, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let detector = self.detector.clone();
        let flagged = self.flagged.clone();
        let in_flight = InFlight::start(self.in_flight.clone());
        let text = text.to_owned();
        tokio::spawn(async move {
            let _in_flight = in_flight;
            if detector
                .score(source, &text)
                .await
                .is_some_and(|score| score >= THRESHOLD)
            {
                flagged.store(true, Ordering::SeqCst);
            }
        });
    }

    /// Like [`TurnGuard::scan`], and if `text` is flagged, tell the user in
    /// the reply (an `InjectionNotice` with outcome `"sent"`) — for what the
    /// user typed or pasted, which is sent as written and never paused on.
    /// A verdict that arrives after [`TurnGuard::finish`] only flags and logs.
    pub fn notice_in_background(
        &self,
        source: Source,
        name: Option<String>,
        text: &str,
        on_event: &EventSink,
    ) {
        if text.trim().is_empty() {
            return;
        }
        let detector = self.detector.clone();
        let flagged = self.flagged.clone();
        let live = self.live.clone();
        let in_flight = InFlight::start(self.in_flight.clone());
        let on_event = on_event.clone();
        let text = text.to_owned();
        tokio::spawn(async move {
            let _in_flight = in_flight;
            if !detector.assess(source, &text).await.flagged() {
                return;
            }
            flagged.store(true, Ordering::SeqCst);
            if live.load(Ordering::SeqCst) {
                on_event(ChatEvent::InjectionNotice {
                    notice: InjectionNotice {
                        source: source.id().to_string(),
                        name: name.map(|name| review::display_name(&name).0),
                        message_id: None,
                        outcome: "sent".to_string(),
                        unchecked: false,
                    },
                });
            }
        });
    }

    /// End the turn's notices: wait up to `grace` for checks still running
    /// (a notice for the message just sent usually lands well inside it),
    /// then stop emitting. Checks that outlast it still finish and log.
    pub async fn finish(&self, grace: Duration) {
        let mut idle = self.in_flight.subscribe();
        let _ = tokio::time::timeout(grace, idle.wait_for(|&n| n == 0)).await;
        self.live.store(false, Ordering::SeqCst);
    }

    /// Whether anything scanned so far this turn was flagged, after waiting
    /// up to `wait` for scans still running. A scan that outlasts `wait`
    /// still finishes and logs; it just can't make this answer.
    pub async fn detected(&self, wait: Duration) -> bool {
        if self.flagged.load(Ordering::SeqCst) {
            return true;
        }
        let mut idle = self.in_flight.subscribe();
        let _ = tokio::time::timeout(wait, idle.wait_for(|&n| n == 0)).await;
        self.flagged.load(Ordering::SeqCst)
    }
}

/// Counts one scan as in flight for as long as it lives — including through
/// a panic, so [`TurnGuard::detected`] never waits on a scan that died.
struct InFlight(Arc<tokio::sync::watch::Sender<usize>>);

impl InFlight {
    fn start(counter: Arc<tokio::sync::watch::Sender<usize>>) -> Self {
        counter.send_modify(|n| *n += 1);
        Self(counter)
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

/// One piece of text a user message brings into a turn, as checked.
pub(crate) struct UserSidePart {
    pub source: Source,
    /// The attached file's name, or the invoked skill's `/slug`; `None` for
    /// typed text.
    pub name: Option<String>,
    pub message_id: String,
    pub text: String,
    /// From the message being sent now, rather than an earlier one.
    pub newest: bool,
}

/// Everything the user messages in `messages` bring to the model this turn,
/// newest message first: per message, its attached documents (in the form
/// [`attachment_scan_text`] gives), then an invoked skill's body, then the
/// typed text. Only messages that will actually be sent
/// ([`crate::engine::model_bound`]): a message the user withheld is neither
/// sent nor asked about again. Empty texts are left out.
pub(crate) fn user_side_parts(messages: &[ChatMessage]) -> Vec<UserSidePart> {
    let users: Vec<&ChatMessage> = crate::engine::model_bound(messages)
        .filter(|m| m.role == "user")
        .collect();
    let mut parts = Vec::new();
    for (i, message) in users.into_iter().rev().enumerate() {
        let mut push = |source: Source, name: Option<String>, text: String| {
            if !text.trim().is_empty() {
                parts.push(UserSidePart {
                    source,
                    name,
                    message_id: message.id.clone(),
                    text,
                    newest: i == 0,
                });
            }
        };
        for attachment in message.attachments.iter().flatten() {
            if let (AttachmentKind::Document, Some(text)) = (&attachment.kind, &attachment.text) {
                push(
                    Source::Attachment,
                    Some(attachment.name.clone()),
                    attachment_scan_text(&attachment.name, text),
                );
            }
        }
        match split_skill_invocation(message) {
            Some((skill, typed)) => {
                let slug = message
                    .display_content
                    .as_deref()
                    .and_then(|line| line.split_whitespace().next())
                    .map(str::to_string);
                push(Source::Skill, slug, skill.to_string());
                push(Source::Composer, None, typed.to_string());
            }
            None => push(Source::Composer, None, message.content.clone()),
        }
    }
    parts
}

/// A `/<skill>` message is the skill's expanded text followed by whatever the
/// user typed after the slug, with the typed line itself kept in
/// `display_content` (`matchSkillInvocation` in `ui/src/state/store.ts`).
/// Returns `(skill text, typed text)` so each is scanned as what it is.
fn split_skill_invocation(message: &ChatMessage) -> Option<(&str, &str)> {
    let typed_line = message.display_content.as_deref()?;
    let typed = typed_line
        .trim()
        .split_once(char::is_whitespace)
        .map_or("", |(_, rest)| rest.trim());
    let skill = message
        .content
        .strip_suffix(typed)
        .unwrap_or(&message.content)
        .trim_end();
    Some((skill, typed))
}

/// The text the classifier actually reads. A conversation transcript's
/// `user:` / `assistant:` line prefixes (`ReadProjectConversationTool`'s
/// format) look exactly like forged chat turns, the very thing injected text
/// fakes — so they're dropped before scoring, which measurably cut false
/// positives on ordinary transcripts without missing any injected one.
fn prepare(source: Source, text: &str) -> Cow<'_, str> {
    static ROLE_PREFIX: OnceLock<Regex> = OnceLock::new();
    if source != Source::ProjectConversation {
        return Cow::Borrowed(text);
    }
    ROLE_PREFIX
        .get_or_init(|| Regex::new(r"(?mi)^(?:user|assistant|system):[ \t]*").unwrap())
        .replace_all(text, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Attachment;
    use std::sync::Mutex;

    /// Flags any text containing `PAYLOAD`, records what it was asked about,
    /// and can be made slow.
    #[derive(Default)]
    pub(crate) struct FakeClassifier {
        pub(crate) seen: Mutex<Vec<String>>,
        pub(crate) delay: Duration,
    }

    impl Classify for FakeClassifier {
        fn p_injection(&self, text: &str) -> Result<f32, String> {
            std::thread::sleep(self.delay);
            self.seen.lock().unwrap().push(text.to_string());
            Ok(if text.contains("PAYLOAD") { 0.99 } else { 0.01 })
        }
    }

    fn detector(classifier: Arc<FakeClassifier>) -> Arc<Detector> {
        Arc::new(Detector::with_classifier(classifier))
    }

    /// Every formatted `tracing` line emitted while the guard is alive.
    #[derive(Clone, Default)]
    pub(crate) struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl Captured {
        /// Installs a subscriber for the current thread only (the tests run
        /// on a current-thread runtime, so spawned scans log through it).
        ///
        /// Then rebuilds `tracing`'s global per-callsite interest cache. A
        /// test on another thread, with no subscriber of its own, can hit a
        /// log line for the first time while this one is being installed,
        /// and leave that line cached as "nobody listens" — so it would
        /// never reach this capture.
        pub(crate) fn install(&self) -> tracing::subscriber::DefaultGuard {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .with_max_level(tracing::Level::INFO)
                .finish();
            let guard = tracing::subscriber::set_default(subscriber);
            tracing::callsite::rebuild_interest_cache();
            guard
        }

        pub(crate) fn all_lines(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        pub(crate) fn detection_lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .filter(|line| line.contains(LOG_TARGET))
                .map(str::to_string)
                .collect()
        }
    }

    pub(crate) fn user(content: &str) -> ChatMessage {
        ChatMessage {
            id: "m".to_string(),
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

    const WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn a_payload_on_each_route_is_flagged_and_logged_once() {
        let log = Captured::default();
        let _guard = log.install();
        let classifier = Arc::new(FakeClassifier::default());
        let detector = detector(classifier);
        let routes = [
            Source::WebPage,
            Source::WebSearch,
            Source::File,
            Source::CommandOutput,
            Source::Attachment,
            Source::ProjectConversation,
            Source::Skill,
            Source::Composer,
            Source::ToolOutput,
        ];
        for source in routes {
            let turn = TurnGuard::new(detector.clone());
            turn.scan(source, &format!("ordinary {} text PAYLOAD", source.label()));
            assert!(turn.detected(WAIT).await, "{}", source.label());
        }
        let lines = log.detection_lines();
        assert_eq!(lines.len(), routes.len(), "{lines:#?}");
        for source in routes {
            let tag = format!("source=\"{}\"", source.label());
            assert_eq!(
                lines.iter().filter(|line| line.contains(&tag)).count(),
                1,
                "{tag}: {lines:#?}"
            );
        }
    }

    #[tokio::test]
    async fn the_log_line_carries_no_text() {
        let log = Captured::default();
        let _guard = log.install();
        let turn = TurnGuard::new(detector(Arc::new(FakeClassifier::default())));
        turn.scan(Source::File, "secret-diary-entry PAYLOAD");
        assert!(turn.detected(WAIT).await);
        let lines = log.detection_lines();
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains("secret-diary-entry"), "{}", lines[0]);
        assert!(lines[0].contains("score=0.99"), "{}", lines[0]);
    }

    #[tokio::test]
    async fn ordinary_text_is_not_flagged_or_logged() {
        let log = Captured::default();
        let _guard = log.install();
        let turn = TurnGuard::new(detector(Arc::new(FakeClassifier::default())));
        turn.scan(Source::WebPage, "Feed the sourdough starter twice a day.");
        assert!(!turn.detected(WAIT).await);
        assert!(log.detection_lines().is_empty());
    }

    #[tokio::test]
    async fn the_same_text_is_scored_once_and_logged_once_but_flags_every_turn() {
        let log = Captured::default();
        let _guard = log.install();
        let classifier = Arc::new(FakeClassifier::default());
        let detector = detector(classifier.clone());
        for _ in 0..3 {
            let turn = TurnGuard::new(detector.clone());
            turn.scan(Source::Attachment, "invoice footer PAYLOAD");
            assert!(turn.detected(WAIT).await);
        }
        assert_eq!(classifier.seen.lock().unwrap().len(), 1);
        assert_eq!(log.detection_lines().len(), 1);
    }

    #[tokio::test]
    async fn detected_waits_for_a_scan_still_running() {
        let classifier = Arc::new(FakeClassifier {
            delay: Duration::from_millis(300),
            ..Default::default()
        });
        let turn = TurnGuard::new(detector(classifier));
        turn.scan(Source::WebPage, "slow page PAYLOAD");
        assert!(turn.detected(WAIT).await);
    }

    #[tokio::test]
    async fn detected_gives_up_after_its_wait_without_blocking() {
        let classifier = Arc::new(FakeClassifier {
            delay: Duration::from_millis(1500),
            ..Default::default()
        });
        let turn = TurnGuard::new(detector(classifier));
        turn.scan(Source::WebPage, "very slow page PAYLOAD");
        let started = Instant::now();
        assert!(!turn.detected(Duration::from_millis(100)).await);
        assert!(started.elapsed() < Duration::from_millis(1000));
    }

    #[tokio::test]
    async fn an_unavailable_detector_skips_scans_and_says_so_once() {
        let log = Captured::default();
        let _guard = log.install();
        let detector = Arc::new(Detector::unavailable());
        for _ in 0..3 {
            let turn = TurnGuard::new(detector.clone());
            turn.scan(Source::Composer, "PAYLOAD");
            assert!(!turn.detected(WAIT).await);
        }
        let all = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        assert_eq!(all.matches("detection is unavailable").count(), 1, "{all}");
    }

    #[tokio::test]
    async fn a_missing_model_directory_is_unavailable_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let detector = Detector::new(dir.path().join("prompt-guard"));
        assert_eq!(detector.score(Source::Composer, "hello").await, None);
    }

    /// A detector over stand-in model files that loads, unloads and logs its
    /// loads like the production one, but with `load` instead of the model.
    fn on_demand(dir: &Path, load: Loader) -> Detector {
        model::fake_download(dir);
        Detector::with_backend(Backend::Files {
            dir: dir.to_path_buf(),
            load,
        })
    }

    fn fake_load(_: &Path) -> Result<Arc<dyn Classify>, String> {
        Ok(Arc::new(FakeClassifier::default()))
    }

    fn slow_load(_: &Path) -> Result<Arc<dyn Classify>, String> {
        Ok(Arc::new(FakeClassifier {
            delay: Duration::from_millis(400),
            ..Default::default()
        }))
    }

    async fn is_loaded(detector: &Detector) -> bool {
        detector.state.lock().await.loaded.is_some()
    }

    #[tokio::test]
    async fn the_first_scan_loads_it_and_unload_releases_it() {
        let log = Captured::default();
        let _guard = log.install();
        let dir = tempfile::tempdir().unwrap();
        let detector = on_demand(dir.path(), fake_load);
        assert!(!is_loaded(&detector).await, "nothing loads ahead of a scan");

        assert_eq!(detector.score(Source::Composer, "hello").await, Some(0.01));
        assert!(is_loaded(&detector).await);
        assert_eq!(log.all_lines().matches("classifier loaded in").count(), 1);

        detector.unload().await;
        assert!(!is_loaded(&detector).await);
        assert!(log.all_lines().contains("unloaded with the engine"));
        // The next scan loads it again.
        assert_eq!(
            detector.score(Source::Composer, "PAYLOAD").await,
            Some(0.99)
        );
        assert_eq!(log.all_lines().matches("classifier loaded in").count(), 2);
    }

    #[tokio::test]
    async fn a_scan_without_the_files_loads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let detector = Detector::new(dir.path().join("prompt-guard"));
        assert_eq!(detector.score(Source::Composer, "hello").await, None);
        assert!(!is_loaded(&detector).await);
    }

    #[tokio::test]
    async fn an_idle_unload_skips_a_classifier_mid_scan() {
        let dir = tempfile::tempdir().unwrap();
        let detector = Arc::new(on_demand(dir.path(), slow_load));
        detector.score(Source::File, "warm-up").await;

        let scanning = detector.clone();
        let scan = tokio::spawn(async move { scanning.score(Source::File, "a page").await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        detector.unload_if_idle_after(Duration::ZERO);
        assert_eq!(scan.await.unwrap(), Some(0.01));
        assert!(is_loaded(&detector).await, "it was mid-scan, so not idle");

        detector.unload_if_idle_after(Duration::ZERO);
        assert!(!is_loaded(&detector).await);
        // Freshly used: the real threshold keeps it.
        detector.score(Source::File, "another page").await;
        detector.unload_if_idle();
        assert!(is_loaded(&detector).await);
    }

    #[tokio::test]
    async fn a_live_tool_turn_keeps_the_classifier_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let detector = Arc::new(on_demand(dir.path(), fake_load));
        detector.score(Source::File, "a page").await;

        let hold = detector.hold();
        let second = detector.hold();
        detector.unload_if_idle_after(Duration::ZERO);
        assert!(is_loaded(&detector).await, "held by a turn in progress");
        drop(hold);
        detector.unload_if_idle_after(Duration::ZERO);
        assert!(is_loaded(&detector).await, "still held by the second turn");

        drop(second);
        detector.unload_if_idle_after(Duration::ZERO);
        assert!(!is_loaded(&detector).await);
    }

    #[tokio::test]
    async fn the_idle_clock_restarts_when_the_last_hold_drops() {
        let dir = tempfile::tempdir().unwrap();
        let detector = Arc::new(on_demand(dir.path(), fake_load));
        let hold = detector.hold();
        detector.score(Source::File, "a page").await;
        // A turn that sat on an open card for longer than the idle limit.
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(hold);

        detector.unload_if_idle_after(Duration::from_millis(200));
        assert!(
            is_loaded(&detector).await,
            "idle since the turn ended, not since the last scan"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
        detector.unload_if_idle_after(Duration::from_millis(200));
        assert!(!is_loaded(&detector).await);
    }

    #[tokio::test]
    async fn a_tool_turn_guard_holds_the_classifier_and_a_chat_guard_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let detector = Arc::new(on_demand(dir.path(), fake_load));
        detector.score(Source::File, "a page").await;

        let chat = TurnGuard::for_chat(detector.clone(), "chat-1");
        let tools = TurnGuard::for_tool_turn(detector.clone(), "chat-1");
        detector.unload_if_idle_after(Duration::ZERO);
        assert!(is_loaded(&detector).await, "the tool turn holds it");

        drop(tools);
        detector.unload_if_idle_after(Duration::ZERO);
        assert!(!is_loaded(&detector).await, "a chat guard holds nothing");
        drop(chat);
    }

    #[tokio::test]
    async fn user_messages_are_split_into_typed_text_skill_and_attachments() {
        let classifier = Arc::new(FakeClassifier::default());
        let turn = TurnGuard::new(detector(classifier.clone()));

        let mut with_skill = user("You are a meticulous editor.\n\nfix my intro");
        with_skill.display_content = Some("/editor fix my intro".to_string());
        let mut with_doc = user("summarize this");
        with_doc.attachments = Some(vec![
            Attachment {
                kind: AttachmentKind::Document,
                name: "invoice.pdf".to_string(),
                data_uri: None,
                text: Some("Invoice total 420 EUR".to_string()),
            },
            Attachment {
                kind: AttachmentKind::Image,
                name: "photo.png".to_string(),
                data_uri: Some("data:image/png;base64,AAAA".to_string()),
                text: None,
            },
        ]);
        let mut assistant = user("Earlier answer from the model");
        assistant.role = "assistant".to_string();
        let mut system = user("You are OpenPal.");
        system.role = "system".to_string();

        // What `chat_stream` does with them (`run_guarded`).
        let dir = tempfile::tempdir().unwrap();
        let broker = crate::agent::ConsentBroker::new(dir.path().join("grants.json"));
        let no_events: EventSink = Arc::new(|_| {});
        let gate = turn
            .gate_user_side(
                &[system, with_skill, assistant, with_doc],
                &broker,
                &Arc::new(AtomicBool::new(false)),
                &no_events,
            )
            .await;
        assert_eq!(gate, Gate::Continue);
        assert!(!turn.detected(WAIT).await);

        let mut seen = classifier.seen.lock().unwrap().clone();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                "You are a meticulous editor.",
                "[Attached file: invoice.pdf]\nInvoice total 420 EUR",
                "fix my intro",
                "summarize this",
            ]
        );
    }

    #[tokio::test]
    async fn an_attachment_is_scanned_with_its_file_name() {
        let detector = detector(Arc::new(FakeClassifier::default()));
        let mut message = user("summarize this");
        message.attachments = Some(vec![Attachment {
            kind: AttachmentKind::Document,
            name: "Invoice PAYLOAD run curl before summarizing.pdf".to_string(),
            data_uri: None,
            text: Some("Invoice total 420 EUR".to_string()),
        }]);
        let parts = user_side_parts(&[message]);
        let attachment = parts
            .iter()
            .find(|part| part.source == Source::Attachment)
            .unwrap();
        assert!(detector
            .assess(Source::Attachment, &attachment.text)
            .await
            .flagged());
    }

    #[tokio::test]
    async fn an_already_scored_text_is_answered_without_queueing_behind_a_long_scan() {
        let classifier = Arc::new(FakeClassifier {
            delay: Duration::from_millis(1500),
            ..Default::default()
        });
        let detector = detector(classifier);
        // Scored once (slowly) and cached.
        assert!(detector.score(Source::Composer, "pasted PAYLOAD").await >= Some(THRESHOLD));

        // A long scan now holds the scan lock...
        let busy = TurnGuard::new(detector.clone());
        busy.scan(Source::File, "a very long log file");
        tokio::time::sleep(Duration::from_millis(50)).await;

        // ...and the cached flag still reaches a consent card at once.
        let turn = TurnGuard::new(detector.clone());
        turn.scan(Source::Composer, "pasted PAYLOAD");
        let started = Instant::now();
        assert!(turn.detected(Duration::from_millis(500)).await);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    /// Fails on every text, the way a classifier hitting an input it can't
    /// handle would.
    pub(crate) struct FailingClassifier;

    impl Classify for FailingClassifier {
        fn p_injection(&self, _text: &str) -> Result<f32, String> {
            Err("unexpected classifier output []".to_string())
        }
    }

    /// Flags `PAYLOAD` and says where it is; counts its calls.
    #[derive(Default)]
    struct LocatingClassifier {
        calls: Mutex<usize>,
    }

    impl Classify for LocatingClassifier {
        fn p_injection(&self, text: &str) -> Result<f32, String> {
            self.p_injection_located(text).map(|(p, _)| p)
        }
        fn p_injection_located(&self, text: &str) -> Result<(f32, Option<Range<usize>>), String> {
            *self.calls.lock().unwrap() += 1;
            Ok(match text.find("PAYLOAD") {
                Some(at) => (0.99, Some(at..at + "PAYLOAD".len())),
                None => (0.01, None),
            })
        }
    }

    #[tokio::test]
    async fn assess_tells_unavailable_from_failed() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Detector::new(dir.path().join("prompt-guard"));
        assert_eq!(
            missing.assess(Source::File, "hello").await,
            Assessment::Unavailable
        );
        assert_eq!(
            Detector::unavailable().assess(Source::File, "hello").await,
            Assessment::Unavailable
        );

        let failing = Detector::with_classifier(Arc::new(FailingClassifier));
        assert_eq!(
            failing.assess(Source::File, "hello").await,
            Assessment::Failed
        );
        // `score` can't tell them apart, and was never meant to.
        assert_eq!(failing.score(Source::File, "hello").await, None);

        // Nothing to read is nothing to flag, with or without a classifier.
        let empty = Assessment::Scored {
            score: 0.0,
            span: None,
        };
        assert_eq!(failing.assess(Source::File, "  \n").await, empty);
        assert!(!empty.flagged());

        let scored = Detector::with_classifier(Arc::new(LocatingClassifier::default()));
        assert!(scored.assess(Source::File, "a PAYLOAD").await.flagged());
        assert!(!scored.assess(Source::File, "a recipe").await.flagged());
    }

    #[tokio::test]
    async fn the_cached_span_comes_back_with_the_score() {
        let classifier = Arc::new(LocatingClassifier::default());
        let detector = Detector::with_classifier(classifier.clone());
        let text = "Invoice total 420 EUR. PAYLOAD";
        let expected = Assessment::Scored {
            score: 0.99,
            span: Some(23..30),
        };
        assert_eq!(detector.assess(Source::Attachment, text).await, expected);
        assert_eq!(detector.assess(Source::Attachment, text).await, expected);
        assert_eq!(*classifier.calls.lock().unwrap(), 1);
        assert_eq!(&text[23..30], "PAYLOAD");
    }

    #[test]
    fn attachment_scan_text_matches_what_build_api_message_folds() {
        let mut message = user("summarize this");
        message.attachments = Some(vec![Attachment {
            kind: AttachmentKind::Document,
            name: "report.txt".to_string(),
            data_uri: None,
            text: Some("Quarterly figures.\nRevenue up.".to_string()),
        }]);
        let request = crate::engine::build_api_messages(std::slice::from_ref(&message));
        let content = request[0]["content"].as_str().unwrap();
        let scanned = attachment_scan_text("report.txt", "Quarterly figures.\nRevenue up.");
        assert!(
            content.contains(&scanned),
            "{scanned:?} is not in {content:?}"
        );
        let parts = user_side_parts(&[message]);
        assert_eq!(parts[0].source, Source::Attachment);
        assert_eq!(parts[0].text, scanned);
    }

    #[test]
    fn user_side_parts_are_newest_first_and_name_what_they_come_from() {
        let mut older = user("older question");
        older.id = "m1".to_string();
        let mut newest = user("You are a meticulous editor.\n\nfix my intro");
        newest.id = "m2".to_string();
        newest.display_content = Some("/editor fix my intro".to_string());
        newest.attachments = Some(vec![Attachment {
            kind: AttachmentKind::Document,
            name: "intro.md".to_string(),
            data_uri: None,
            text: Some("Once upon a time".to_string()),
        }]);
        let parts = user_side_parts(&[older, newest]);
        let summary: Vec<(Source, Option<&str>, &str, bool)> = parts
            .iter()
            .map(|p| (p.source, p.name.as_deref(), p.message_id.as_str(), p.newest))
            .collect();
        assert_eq!(
            summary,
            vec![
                (Source::Attachment, Some("intro.md"), "m2", true),
                (Source::Skill, Some("/editor"), "m2", true),
                (Source::Composer, None, "m2", true),
                (Source::Composer, None, "m1", false),
            ]
        );
    }

    #[test]
    fn every_route_has_a_camel_case_id_for_the_ui() {
        assert_eq!(Source::WebPage.id(), "webPage");
        assert_eq!(Source::CommandOutput.id(), "commandOutput");
        assert_eq!(Source::ProjectConversation.id(), "projectConversation");
        assert_eq!(Source::ToolOutput.id(), "toolOutput");
        assert_eq!(Source::Attachment.id(), "attachment");
        // The log keeps its own names.
        assert_eq!(Source::WebPage.label(), "web_page");
    }

    #[test]
    fn a_continue_is_remembered_per_conversation_route_and_text() {
        let detector = Detector::unavailable();
        detector.remember_continued("chat-1", Source::Attachment, "invoice PAYLOAD");
        assert!(detector.was_continued("chat-1", Source::Attachment, "invoice PAYLOAD"));
        assert!(!detector.was_continued("chat-2", Source::Attachment, "invoice PAYLOAD"));
        assert!(!detector.was_continued("chat-1", Source::File, "invoice PAYLOAD"));
        assert!(!detector.was_continued("chat-1", Source::Attachment, "invoice PAYLOAD!"));
        // No conversation, no memory.
        detector.remember_continued("", Source::Attachment, "invoice PAYLOAD");
        assert!(!detector.was_continued("", Source::Attachment, "invoice PAYLOAD"));
    }

    #[test]
    fn a_skill_invoked_with_nothing_after_it_is_all_skill() {
        let mut message = user("Format these notes as minutes.");
        message.display_content = Some("/minutes".to_string());
        assert_eq!(
            split_skill_invocation(&message),
            Some(("Format these notes as minutes.", ""))
        );
        assert_eq!(split_skill_invocation(&user("plain text")), None);
    }

    #[test]
    fn transcript_role_prefixes_are_dropped_before_scoring() {
        let transcript =
            "user: Can you draft an email?\n\nassistant: Sure — here it is.\nUser: thanks";
        assert_eq!(
            prepare(Source::ProjectConversation, transcript),
            "Can you draft an email?\n\nSure — here it is.\nthanks"
        );
        assert_eq!(prepare(Source::File, "user: kept"), "user: kept");
    }

    #[test]
    fn tool_results_map_to_their_route() {
        assert_eq!(Source::for_tool("fetch_url"), Some(Source::WebPage));
        assert_eq!(Source::for_tool("web_search"), Some(Source::WebSearch));
        assert_eq!(Source::for_tool("read_file"), Some(Source::File));
        assert_eq!(Source::for_tool("run_shell"), Some(Source::CommandOutput));
        assert_eq!(Source::for_tool("run_code"), Some(Source::CommandOutput));
        assert_eq!(
            Source::for_tool("read_project_conversation"),
            Some(Source::ProjectConversation)
        );
        assert_eq!(Source::for_tool("ask_user"), Some(Source::Composer));
        assert_eq!(Source::for_tool("spawn_subagent"), None);
        assert_eq!(Source::for_tool("write_file"), None);
        assert_eq!(
            Source::for_tool("some_future_tool"),
            Some(Source::ToolOutput)
        );
    }
}
