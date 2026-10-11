//! Agent mode: let the model call tools that act on the user's real machine
//! (run shell commands, execute code, read/write files), gated by OS-native
//! sandboxing and an explicit per-call consent prompt.
//!
//! This module is Tauri-free, like the rest of `openpal-core`. It is an
//! independent consumer of [`crate::engine::EngineManager::endpoint`] — it
//! talks to the running `llama-server` directly over its OpenAI-compatible
//! HTTP API, the same way the web-search plugin's sidecar does, but without a
//! Python process: every tool this feature exposes needs OS-level process
//! sandboxing or filesystem jailing, which (per the app's own security
//! boundary — see ARCHITECTURE.md) can only happen in Rust, so there is
//! nothing a Python-hosted reasoning loop would buy here.
//!
//! Submodules:
//! - [`tool`] — the `Tool` trait every built-in (and, later, MCP-sourced) tool
//!   implements, plus the wire-facing consent/danger-flag shapes.
//! - [`registry`] — `ToolRegistry`, the OpenAI `tools` array builder.
//! - [`workspace`] — path jailing, the universal credential/persistence
//!   denylist, and the TOCTOU-safe file open used by `read_file`/`write_file`.
//! - [`sandbox`] — the `Sandbox` trait and the macOS Seatbelt implementation
//!   used by `run_shell`/`run_code`.
//! - [`builtin`] — the four v1 tools.
//! - [`consent`] — `ConsentBroker`: pending per-call approval + the persisted
//!   "Always allow" grant store.
//! - [`audit`] — the hash-chained tool-call lifecycle log.
//! - [`heuristics`] — non-blocking danger-flag detectors shown on the consent
//!   card (never auto-blocking — every call still goes through consent).
//! - [`llm_tools`] — reassembles a streamed `tool_calls` SSE round into
//!   complete calls.
//! - [`orchestrator`] — `run_agent_turn`, the loop tying all of the above
//!   together.

pub mod audit;
pub mod builtin;
pub mod citations;
pub mod consent;
pub mod heuristics;
pub mod llm_tools;
pub mod orchestrator;
pub mod registry;
pub mod sandbox;
pub mod tool;
pub mod workspace;

pub use citations::CitationRegistry;
pub use consent::{ConsentBroker, ConsentDecision, ToolGrant};
pub use orchestrator::{run_agent_turn, run_tool_loop, AgentDeps, LoopOutcome, MAX_AGENT_STEPS};
pub use registry::ToolRegistry;
pub use tool::{
    ConsentSummary, DangerFlag, EventSink, ResolvedPath, Tool, ToolContext, ToolError, ToolOrigin,
    ToolOutput,
};
