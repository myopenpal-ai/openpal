//! Bridge one chat turn into `openpal_core::agent::run_agent_turn` when
//! agent mode is enabled — the agent-mode analogue of `plugin_chat.rs`, but
//! there's no subprocess to drive here: the orchestrator talks to
//! llama-server directly, so this module's only job is assembling
//! `AgentDeps` from `AppState`/`Settings` and forwarding the call.

use openpal_core::agent::builtin::{
    AskUserTool, CodeExecTool, CreateArtifactTool, FetchUrlTool, FsReadTool, FsWriteTool,
    ListProjectConversationsTool, ReadProjectConversationTool, ShellExecTool, SpawnSubagentTool,
    WebSearchTool,
};
use openpal_core::agent::sandbox::{self, Sandbox};
use openpal_core::agent::workspace::WorkspaceJail;
use openpal_core::agent::{
    audit::AuditLog, run_agent_turn, AgentDeps, CitationRegistry, ConsentBroker, ToolRegistry,
};
use openpal_core::engine::EngineEndpoint;
use openpal_core::plugin::PluginManager;
use openpal_core::types::{ChatEvent, ChatMessage, SamplingParams};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

/// Build the per-turn dependency bundle. Cheap enough to do on every turn —
/// the registry is a handful of small `Arc`-wrapped structs, and the
/// expensive/durable state (pending consent map, persisted grants, the audit
/// log) lives in `AppState` and is passed in, not rebuilt here.
///
/// `SpawnSubagentTool::bind_deps`. `search_service_url` is deliberately left
/// unresolved here (unlike [`build_web_search_deps`]): a full agent-mode turn
/// may never call the `web-research` helper at all, so eagerly starting
/// the web-search service (seconds when it isn't running yet — see
/// `PluginManager::ensure_capability_service`) on every agent turn would
/// tax every shell/code/file task with a cost only a research task should
/// ever pay. `SpawnSubagentTool` instead holds its own `plugins` handle and
/// starts the service lazily, only if/when its "web-research" kind is
/// actually invoked — see `SpawnSubagentTool::registry_for`.
///
/// `conn`/`conversation_id` are only needed to decide whether to register
/// `list_project_conversations`/`read_project_conversation` — looked up fresh
/// on every call (never cached) since project membership can change
/// mid-session. Absent entirely when the current conversation isn't in a
/// project, so the model never sees tools it has nothing to call them on.
/// Locked only for the one short `store::get` call below, never held across
/// an `.await` — same discipline `AppState.app_db`'s own doc comment
/// requires of any tool holding a clone of it across a turn (both
/// `ListProjectConversationsTool`/`ReadProjectConversationTool` do, below).
///
/// `conn`/`conversation_id` are also what `create_artifact` is scoped to —
/// see `openpal_core::artifacts`.
#[allow(clippy::too_many_arguments)]
pub async fn build_deps(
    workspace_dir: &Path,
    data_dir: &Path,
    conn: Arc<Mutex<Connection>>,
    conversation_id: &str,
    http: reqwest::Client,
    consent: Arc<ConsentBroker>,
    audit: Arc<AuditLog>,
    plugins: &Arc<PluginManager>,
    endpoint: EngineEndpoint,
    sampling: SamplingParams,
) -> Result<AgentDeps, String> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.to_path_buf());
    let workspace =
        Arc::new(WorkspaceJail::new(workspace_dir, &home, data_dir).map_err(|e| e.to_string())?);

    let sandbox: Arc<dyn Sandbox> = Arc::from(sandbox::platform_sandbox());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ShellExecTool::new(sandbox.clone())));
    registry.register(Arc::new(CodeExecTool::new(sandbox)));
    registry.register(Arc::new(FsReadTool));
    registry.register(Arc::new(FsWriteTool));
    registry.register(Arc::new(AskUserTool));
    registry.register(Arc::new(CreateArtifactTool::new(
        conn.clone(),
        conversation_id.to_string(),
    )));
    let subagent_tool = Arc::new(SpawnSubagentTool::new(endpoint, sampling, plugins.clone()));
    registry.register(subagent_tool.clone());

    let project_id = openpal_core::store::get(&conn.lock().unwrap(), conversation_id)
        .ok()
        .and_then(|c| c.project_id);
    if let Some(project_id) = project_id {
        registry.register(Arc::new(ListProjectConversationsTool::new(
            conn.clone(),
            project_id.clone(),
            conversation_id.to_string(),
        )));
        registry.register(Arc::new(ReadProjectConversationTool::new(conn, project_id)));
    }

    let deps = AgentDeps {
        registry: Arc::new(registry),
        consent,
        audit,
        workspace,
        scratch_root: data_dir.join("agent-sandbox"),
        http,
        // Not used by anything in this registry — see the doc comment above.
        search_service_url: None,
        citations: Arc::new(CitationRegistry::new()),
    };
    subagent_tool.bind_deps(deps.clone());
    Ok(deps)
}

/// The framing appended to the plain-chat system prompt when web search is
/// on — mirrors the Python sidecar's `agent.py` framing (search, fetch with a
/// query so long pages come back as relevant excerpts, cite inline), minus
/// the sidecar-specific tool-dispatch-mode plumbing that existed only to work
/// around small/incapable models' function-calling support.
pub const WEB_SEARCH_FRAMING: &str =
    "\n\nYou can search the web and read pages when you need current \
     information or sources you don't already know. Use web_search to find results, then fetch_url \
     — passing \"query\" so a long page returns its most relevant excerpts — to read the most \
     promising ones. Cite sources inline using the [N] ids you're given. Don't search for things \
     you already know confidently.";

/// Framing appended alongside the turn's other framing (or standing alone, in
/// full agent mode) to nudge the model toward `ask_user` — appended in both
/// `agent_chat_turn` and `web_search_chat_turn`, the two surfaces
/// `AskUserTool` is registered in (`build_deps`/`build_web_search_deps`
/// above). Deliberately narrow: it nudges only the one zero-blast-radius
/// tool, not "use your tools eagerly" in general — the consent-gated tools
/// (`run_shell`/`write_file`/etc.) are gated *because* their use should stay
/// deliberate, and a generic "reach for your tools" framing would cut
/// against that. Without this, a tool's own OpenAI function-calling
/// `description` is the model's only signal that `ask_user` exists at all —
/// many local models, especially smaller/general-purpose ones without
/// tool-calling-specific fine-tuning, don't reliably act on a schema
/// description alone and default to answering (or asking) in plain text
/// instead of emitting a real tool call.
pub const ASK_USER_FRAMING: &str =
    "\n\nIf a request is ambiguous or you're missing something you genuinely need to proceed (a \
     path, a choice between options, a missing detail), use ask_user to ask one short question \
     instead of guessing — but don't overuse it: only when getting it wrong would cost the user \
     real rework, not for details you could reasonably infer.";

/// Build the per-turn dependency bundle for plain-chat web search: a registry
/// scoped to `web_search`/`fetch_url` plus `ask_user` (no shell/fs/code/
/// subagent access, unlike full agent mode) — the replacement for the Python
/// sidecar (`plugins/web-search/sidecar.py`), reusing the same
/// `run_agent_turn` loop agent mode uses instead of a subprocess. `ask_user`
/// rides along here too since it has no blast radius of its own to scope —
/// a vague search request ("check the weather") benefits from it exactly as
/// much as a full agent-mode task does.
///
/// Web search is the experimental `web-search` add-on: turning it on in
/// Plugins installs its SearXNG, and the composer's globe then turns it on
/// per conversation. This call starts the SearXNG service if it isn't
/// running yet; `on_event` receives `ChatEvent::SearchSetupProgress` while
/// it starts, so the chat UI has something to show besides an unexplained
/// pause. Returns `Ok(None)` when the service can't be had (the add-on went
/// off or is still setting up, or the service won't start), so the caller
/// can fall back to a plain answer — but not silently: the reason is sent
/// through `on_event` first, as the same `SearchSetupProgress` status line.
pub async fn build_web_search_deps(
    workspace_dir: &Path,
    data_dir: &Path,
    http: reqwest::Client,
    consent: Arc<ConsentBroker>,
    audit: Arc<AuditLog>,
    plugins: &PluginManager,
    on_event: &(impl Fn(ChatEvent) + Send + Sync),
) -> Result<Option<AgentDeps>, String> {
    let port = match plugins
        .ensure_capability_service("search", |event| {
            if let Some(chat_event) = event.as_search_setup_chat_event() {
                on_event(chat_event);
            }
        })
        .await
    {
        Ok(port) => port,
        Err(e) => {
            on_event(ChatEvent::SearchSetupProgress { message: e });
            return Ok(None);
        }
    };
    let search_service_url = format!("http://127.0.0.1:{port}");

    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.to_path_buf());
    let workspace =
        Arc::new(WorkspaceJail::new(workspace_dir, &home, data_dir).map_err(|e| e.to_string())?);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WebSearchTool::new(
        search_service_url.clone(),
        http.clone(),
    )));
    registry.register(Arc::new(FetchUrlTool::new(http.clone())));
    registry.register(Arc::new(AskUserTool));

    Ok(Some(AgentDeps {
        registry: Arc::new(registry),
        consent,
        audit,
        workspace,
        scratch_root: data_dir.join("agent-sandbox"),
        http,
        search_service_url: Some(search_service_url),
        citations: Arc::new(CitationRegistry::new()),
    }))
}

/// Drive one agent-mode chat turn. `messages` is the same history
/// `chat_stream`'s other paths receive (a leading system message already
/// prepended by the caller, matching `plain_chat`'s convention).
pub async fn run_turn(
    deps: &AgentDeps,
    endpoint: EngineEndpoint,
    messages: &[ChatMessage],
    sampling: &SamplingParams,
    cancel: Arc<AtomicBool>,
    on_event: impl Fn(ChatEvent) + Send + Sync + 'static,
) -> Result<(), String> {
    run_agent_turn(deps, endpoint, messages, sampling, cancel, on_event).await
}
