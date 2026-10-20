//! `spawn_subagent` — lets a top-level agent turn hand a self-contained
//! sub-task to a fixed-purpose helper that runs its own short tool-use loop
//! (its own message history, its own scoped tool set) and reports back only
//! its final answer, the way a Claude Code subagent does.
//!
//! Deliberately narrow for v1: a fixed menu of subagent *kinds*, each with a
//! hardcoded tool subset and framing prompt, not a general "give me any
//! tools" mechanism. A subagent can't itself spawn a subagent — `execute()`
//! builds a registry that never includes [`SpawnSubagentTool`] — so nesting
//! is capped at depth 1 without needing a separate depth counter.

use super::super::orchestrator::{run_tool_loop, AgentDeps};
use super::super::registry::ToolRegistry;
use super::super::tool::{
    cap_output, ConsentSummary, EventSink, Tool, ToolContext, ToolError, ToolOutput,
};
use super::{FetchUrlTool, WebSearchTool};
use crate::engine::EngineEndpoint;
use crate::plugin::PluginManager;
use crate::types::{ChatEvent, SamplingParams};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

/// Rounds a subagent gets before it's forced to answer — smaller than the
/// top-level turn's [`super::super::orchestrator::MAX_AGENT_STEPS`] since a
/// sub-task is meant to be narrow.
const SUBAGENT_MAX_STEPS: u32 = 4;

fn kinds() -> &'static [(&'static str, &'static str)] {
    &[
        (
            "web-research",
            "You are a focused research assistant. Use web_search to find sources \
             and fetch_url to read the most promising ones — always pass fetch_url's \
             \"query\" argument (what you're trying to find) so a long page returns \
             its most relevant excerpts instead of just its start. Then answer the \
             question you were given directly and concisely, citing source URLs \
             inline. Don't ask the user anything — you can't; just do the \
             research and report back.",
        ),
        (
            "math",
            "You are a careful math/calculation assistant. Use run_code to \
             compute or verify anything nontrivial rather than doing arithmetic \
             in your head. Report just the final result and a brief line of \
             reasoning. Don't ask the user anything — you can't; just solve it \
             and report back.",
        ),
    ]
}

fn kind_names() -> String {
    kinds()
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A tool that runs a nested, scoped tool-use loop and returns only the final
/// text. Needs the parent turn's [`AgentDeps`] (reused verbatim for the
/// consent/audit/workspace/http plumbing; each kind builds its own scoped
/// registry on top of it) and the engine endpoint + sampling params to talk
/// to the model.
///
/// `AgentDeps` isn't available yet when the registry (which this tool lives
/// in) is being built, so it's bound after the fact via [`Self::bind_deps`]
/// rather than passed to `new` — the caller keeps the `Arc<SpawnSubagentTool>`
/// handle around just long enough to call that once, right after building
/// `AgentDeps`, before handing the registry off to the orchestrator.
pub struct SpawnSubagentTool {
    endpoint: EngineEndpoint,
    sampling: SamplingParams,
    /// Handle to the app's plugin services — used only to lazily start the
    /// web-search backend the "web-research" kind needs, exactly if/when
    /// that kind actually gets invoked (see `registry_for`).
    /// Kept separate from `AgentDeps` (rather than reading
    /// `AgentDeps::search_service_url`) so a full agent-mode turn that never
    /// calls "web-research" never pays that provisioning cost — see
    /// `agent_bridge::build_deps`'s doc comment.
    plugins: Arc<PluginManager>,
    deps: OnceLock<AgentDeps>,
}

impl SpawnSubagentTool {
    pub fn new(
        endpoint: EngineEndpoint,
        sampling: SamplingParams,
        plugins: Arc<PluginManager>,
    ) -> Self {
        Self {
            endpoint,
            sampling,
            plugins,
            deps: OnceLock::new(),
        }
    }

    /// Bind the parent turn's `AgentDeps`. Idempotent-ish: only the first
    /// call takes effect, which is fine since a `SpawnSubagentTool` is built
    /// fresh per turn (see `agent_bridge::build_deps`), never reused.
    pub fn bind_deps(&self, deps: AgentDeps) {
        let _ = self.deps.set(deps);
    }

    async fn registry_for(
        &self,
        kind: &str,
        deps: &AgentDeps,
        on_event: &EventSink,
    ) -> Result<Option<ToolRegistry>, ToolError> {
        let mut registry = ToolRegistry::new();
        match kind {
            "web-research" => {
                let base = self.ensure_search_service(on_event).await?;
                registry.register(Arc::new(WebSearchTool::new(base)));
                registry.register(Arc::new(FetchUrlTool::new()));
            }
            "math" => {
                // The parent's own run_code tool, sandboxed the same way.
                let Some(code_tool) = deps.registry.resolve("run_code") else {
                    return Ok(None);
                };
                registry.register(code_tool);
            }
            _ => return Ok(None),
        }
        Ok(Some(registry))
    }

    /// Start the web-search add-on's service if it isn't running and return
    /// its base URL. Fails, telling the model why, while the add-on is off
    /// (it's experimental, and off by default). Forwards start-up progress
    /// through `on_event` as `ChatEvent::SearchSetupProgress` so a slow start
    /// shows *something* instead of the tool call just sitting there.
    async fn ensure_search_service(&self, on_event: &EventSink) -> Result<String, ToolError> {
        if !self.plugins.capability_enabled("search").await {
            return Err(ToolError::ExecutionFailed(
                "Web search is turned off, so this helper can't search the web.".to_string(),
            ));
        }
        let sink = on_event.clone();
        let port = self
            .plugins
            .ensure_capability_service("search", move |event| {
                if let Some(chat_event) = event.as_search_setup_chat_event() {
                    sink(chat_event);
                }
            })
            .await
            .map_err(ToolError::ExecutionFailed)?;
        Ok(format!("http://127.0.0.1:{port}"))
    }
}

#[async_trait::async_trait]
impl Tool for SpawnSubagentTool {
    fn id(&self) -> &str {
        "spawn_subagent"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "spawn_subagent",
            "description": format!(
                "Hand a self-contained sub-task to a focused helper and get back just its final answer. Available kinds: {}.",
                kind_names()
            ),
            "parameters": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": kinds().iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                        "description": "Which kind of helper to run."
                    },
                    "task": {
                        "type": "string",
                        "description": "The sub-task, written as a complete, self-contained instruction — the helper sees nothing else from this conversation."
                    }
                },
                "required": ["kind", "task"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let kind = args
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("A subagent needs a \"kind\".".to_string()))?;
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("A subagent needs a \"task\".".to_string()))?;
        if !kinds().iter().any(|(id, _)| *id == kind) {
            return Err(ToolError::Rejected(format!(
                "\"{kind}\" isn't a subagent kind. Available: {}.",
                kind_names()
            )));
        }
        Ok(ConsentSummary {
            summary: format!("Run the \"{kind}\" helper"),
            detail: task.to_string(),
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        format!(
            "spawn_subagent:{}:{}",
            args.get("kind").and_then(Value::as_str).unwrap_or(""),
            args.get("task").and_then(Value::as_str).unwrap_or("")
        )
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let kind = args
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("A subagent needs a \"kind\".".to_string()))?;
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("A subagent needs a \"task\".".to_string()))?;
        let framing = kinds()
            .iter()
            .find(|(id, _)| *id == kind)
            .map(|(_, prompt)| *prompt)
            .ok_or_else(|| {
                ToolError::Rejected(format!(
                    "\"{kind}\" isn't a subagent kind. Available: {}.",
                    kind_names()
                ))
            })?;

        // `ctx` carries the parent turn's workspace/cancel — the nested loop
        // reuses `super::super::orchestrator`'s dispatch path unchanged, it
        // just runs against a registry scoped to this kind and a message
        // history seeded fresh (the parent conversation is deliberately not
        // forwarded — `task` must be self-contained).
        let deps = self.deps.get().ok_or(ToolError::Unsupported)?;
        let registry = self
            .registry_for(kind, deps, &ctx.on_event)
            .await?
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "The \"{kind}\" helper isn't available right now (its tools aren't set up)."
                ))
            })?;

        let mut nested_deps = deps.clone();
        nested_deps.registry = Arc::new(registry);

        let mut wire_messages = vec![
            serde_json::json!({ "role": "system", "content": framing }),
            serde_json::json!({ "role": "user", "content": task }),
        ];
        let tool_defs = nested_deps.registry.openai_tool_defs();
        let sub_turn_id = format!("{}-sub-{}", ctx.turn_id, next_sub_id());

        // Forward everything except the raw text stream: the parent's
        // visible reply is only ever the helper's final answer (returned
        // below as this tool's output), but consent prompts and tool-call
        // lifecycle events for the helper's *own* tool calls (e.g. a
        // `run_code` call inside the `math` helper) must still reach the
        // real UI sink — otherwise a nested consent request would have
        // nobody to answer it and the whole turn would hang waiting.
        let parent_sink = ctx.on_event.clone();
        let sink: EventSink = Arc::new(move |event: ChatEvent| {
            if !matches!(event, ChatEvent::Token { .. } | ChatEvent::Reasoning { .. }) {
                parent_sink(event);
            }
        });

        (ctx.on_event)(ChatEvent::SubagentStarted {
            tool_call_id: ctx.call_id.clone(),
            kind: kind.to_string(),
            task: task.to_string(),
        });

        let loop_result = run_tool_loop(
            &nested_deps,
            &self.endpoint,
            &mut wire_messages,
            &tool_defs,
            &self.sampling,
            &ctx.cancel,
            &sub_turn_id,
            SUBAGENT_MAX_STEPS,
            Some(kind),
            &sink,
        )
        .await
        .map_err(ToolError::ExecutionFailed);
        // A failed request never reaches a `LoopOutcome`, so there's no
        // partial count to report — 0 in that case, not a missing field,
        // since `total_tokens` is meant for "roughly how much did this
        // helper cost", not billing-grade accounting.
        let total_tokens = loop_result.as_ref().map(|o| o.total_tokens).unwrap_or(0);
        let result = loop_result.and_then(|outcome| {
            if outcome.final_text.trim().is_empty() {
                Err(ToolError::ExecutionFailed(
                    "The helper didn't produce an answer.".to_string(),
                ))
            } else {
                Ok(outcome.final_text)
            }
        });

        (ctx.on_event)(ChatEvent::SubagentFinished {
            tool_call_id: ctx.call_id.clone(),
            ok: result.is_ok(),
            total_tokens,
        });

        let final_text = result?;
        Ok(ToolOutput {
            ok: true,
            preview: format!("{kind} helper finished"),
            content: cap_output(final_text),
        })
    }
}

fn next_sub_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
