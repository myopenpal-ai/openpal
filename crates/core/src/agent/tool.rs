//! The `Tool` trait every built-in tool implements, and the wire-facing types
//! a consent prompt is built from.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Where tool-call lifecycle events (consent prompts, start/result, etc.) go.
/// A plain `Arc<dyn Fn>` rather than an `FnMut` closure so it can be cloned
/// into a [`ToolContext`] and, in turn, forwarded into a nested tool-use loop
/// (see `builtin::subagent`) without needing a borrow — any caller that needs
/// to mutate state in response (e.g. a test collecting events into a `Vec`)
/// does so through its own interior mutability (`Mutex`/`Channel`), the same
/// way the production Tauri `Channel::send` sink already does.
pub type EventSink = Arc<dyn Fn(crate::types::ChatEvent) + Send + Sync>;

/// Where a tool came from. `BuiltIn` is the default so every existing tool
/// costs nothing today; a future MCP client adapter overrides this to `Mcp`
/// so its consent card can be marked "externally executed, unsandboxed" —
/// unlike a built-in, an MCP tool's `execute()` forwards to a remote server
/// with no Seatbelt equivalent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolOrigin {
    BuiltIn,
    Mcp { server_id: String },
}

/// A file/dir argument resolved to its canonical, symlink-followed target.
/// Shown on the consent card distinctly from the model's raw argument when
/// they differ, so a user isn't approving a path they never actually saw.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPath {
    pub raw: String,
    pub canonical: PathBuf,
    pub inside_workspace: bool,
}

/// Non-blocking warning badges shown on the consent card. These never gate
/// execution by themselves — every call still goes through the same
/// allow-once/always-allow/deny prompt — they only give a human reviewing the
/// call under time pressure a signal beyond parsing shell syntax by eye.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DangerFlag {
    OutsideWorkspace,
    ForkBombShape,
    RawDeviceIo,
    NetworkFetchPipeShell,
    SudoOrPrivEsc,
    DerivedFromRecentOutput { step: u32 },
}

/// What the consent prompt shows the user, built by the tool itself before
/// any prompt is emitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentSummary {
    /// Short, one-line description shown on the card's title row.
    pub summary: String,
    /// Full detail (the command text, the code, or the resolved path) shown
    /// in the card's body — rendered as plain escaped text only, never
    /// through the markdown/HTML renderer used for ordinary chat messages,
    /// since this content can be attacker-influenceable.
    pub detail: String,
    #[serde(default)]
    pub resolved_paths: Vec<ResolvedPath>,
    #[serde(default)]
    pub danger_flags: Vec<DangerFlag>,
}

/// Everything a tool needs about the turn it's executing within.
pub struct ToolContext {
    pub turn_id: String,
    pub call_id: String,
    /// Path jailing + denylist for this workspace — every tool that touches
    /// the filesystem (directly, or indirectly via a sandboxed process's
    /// `cwd`) resolves through this, never a raw path.
    pub workspace: Arc<super::workspace::WorkspaceJail>,
    /// Per-call scratch dir (holds the `.cmd`/`.code` file `run_shell`/
    /// `run_code` write their text to, so it never appears in argv).
    pub scratch_dir: PathBuf,
    pub cancel: Arc<AtomicBool>,
    /// Where this call's own lifecycle events go — plumbed through so a tool
    /// that runs a nested tool-use loop (`spawn_subagent`) can forward its
    /// nested calls' consent prompts/results to the same sink the top-level
    /// turn uses, instead of them going nowhere and hanging forever waiting
    /// for a consent decision the user was never shown.
    pub on_event: EventSink,
    /// Stable per-URL `[N]` numbering for this turn — shared across every
    /// tool call in the turn so a URL searched, then fetched, keeps one id.
    pub citations: Arc<super::citations::CitationRegistry>,
    /// The URLs `fetch_url` may open this turn and its fetch budget — see
    /// [`super::fetch_allowlist`]. `web_search` and `ask_user` add to it.
    pub fetch_allowlist: Arc<super::fetch_allowlist::FetchAllowlist>,
    /// The same broker `dispatch_call` uses for the consent gate, handed to
    /// every tool so one that needs its own interactive round-trip with the
    /// user beyond that gate can register one — see `builtin::AskUserTool`,
    /// the only current user of this.
    pub consent: Arc<super::consent::ConsentBroker>,
}

/// A tool's execution result. `content` is what's fed back to the model as
/// the `tool`-role message; `preview` is a short, audit-log-safe summary
/// (already capped) distinct from the full content, which can be large.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub ok: bool,
    pub content: String,
    pub preview: String,
}

/// Maximum bytes of tool output fed back to the model / shown in the
/// transcript. Applies uniformly across tools so one runaway command can't
/// blow up the conversation's context or the UI.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 200 * 1024;

/// Truncate `s` to at most `MAX_TOOL_OUTPUT_BYTES`, on a UTF-8 boundary, with
/// a trailing marker so the model knows the output was cut.
pub fn cap_output(mut s: String) -> String {
    if s.len() <= MAX_TOOL_OUTPUT_BYTES {
        return s;
    }
    let mut cut = MAX_TOOL_OUTPUT_BYTES;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n\n[output truncated at 200 KB]");
    s
}

/// A rejection is decided *before* any consent prompt is shown (malformed
/// arguments, a path outside the jail, a denylisted target) — it never
/// interrupts the user, it just goes straight back to the model as the
/// reason. `ExecutionFailed` is a normal runtime failure after the user did
/// approve the call (sandbox spawn error, timeout, non-zero exit is not an
/// error — that's still `Ok(ToolOutput{ok:false,..})`).
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Rejected(String),
    #[error("{0}")]
    ExecutionFailed(String),
    #[error("this tool isn't available on this platform yet")]
    Unsupported,
}

/// Something the model can call. Every built-in tool implements this; a
/// future MCP client adapter would too, flowing through the exact same
/// consent/audit pipeline via [`Tool::origin`].
// See `EngineBackend` for why `double_must_use` is allowed on `async_trait` traits.
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Stable id, e.g. `"run_shell"`. Sent to the model as the OpenAI
    /// function name and used as the grant-store/audit-log key prefix.
    fn id(&self) -> &str;

    /// Defaults to `BuiltIn` so every existing tool costs nothing; override
    /// for an MCP-sourced tool.
    fn origin(&self) -> ToolOrigin {
        ToolOrigin::BuiltIn
    }

    /// The OpenAI function-calling schema (`{name, description, parameters}`)
    /// — wrapped in `{"type":"function","function": ...}` by the registry.
    fn spec(&self) -> serde_json::Value;

    /// Validate `args` and build the consent card. Must run all jailing/
    /// denylist checks and return `Err(ToolError::Rejected(..))` *before* any
    /// prompt is shown — a malformed or out-of-jail call never interrupts
    /// the user.
    fn consent_summary(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError>;

    /// A stable key identifying "this exact call" for the "Always allow"
    /// grant store. Must be narrow enough that granting it doesn't implicitly
    /// grant a materially different action (see `write_file`'s content-hash
    /// inclusion) — never just the tool id or program name.
    fn grant_key(&self, args: &serde_json::Value, ctx: &ToolContext) -> String;

    /// Whether this exact call needs a consent prompt (or a matching grant)
    /// before it runs. Defaults to `true` — every tool is gated unless it
    /// opts out. Only a tool with no meaningful blast radius (read-only,
    /// sandboxed, nothing destructive even in the worst case) should return
    /// `false`; it still goes through [`Tool::consent_summary`]'s rejection
    /// checks first; this only skips the *interactive* prompt.
    fn requires_consent(&self, _args: &serde_json::Value, _ctx: &ToolContext) -> bool {
        true
    }

    /// Run the call. Only reached after consent (fresh or grant-hit, or
    /// [`Tool::requires_consent`] returning `false`).
    async fn execute(
        &self,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_output_leaves_short_strings_untouched() {
        assert_eq!(cap_output("hello".to_string()), "hello");
    }

    #[test]
    fn cap_output_truncates_and_marks_long_strings() {
        let long = "a".repeat(MAX_TOOL_OUTPUT_BYTES + 100);
        let capped = cap_output(long);
        assert!(capped.len() < MAX_TOOL_OUTPUT_BYTES + 100);
        assert!(capped.ends_with("[output truncated at 200 KB]"));
    }

    #[test]
    fn cap_output_never_splits_a_utf8_char() {
        // Put a multi-byte character right at the cut boundary.
        let mut s = "a".repeat(MAX_TOOL_OUTPUT_BYTES - 1);
        s.push('€'); // 3 bytes in UTF-8
        s.push_str(&"b".repeat(50));
        let capped = cap_output(s);
        assert!(capped.is_char_boundary(capped.len() - "\n\n[output truncated at 200 KB]".len()));
    }
}
