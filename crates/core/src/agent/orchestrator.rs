//! `run_agent_turn` — the agent-mode loop. Talks to the running
//! `llama-server` directly over its OpenAI-compatible API (via
//! [`crate::engine::EngineManager::endpoint`]), the same way
//! `crate::engine::EngineManager::chat_stream` does for a plain turn, but
//! parses its own SSE stream so it can also see `delta.tool_calls` —
//! deliberately additive, [`crate::sse`] is untouched.

use super::audit::{AuditEvent, AuditLog};
use super::consent::{now_ms, ConsentBroker, ConsentDecision, ToolGrant};
use super::llm_tools::{AssembledToolCall, RejectedToolCall, ToolCallAssembler};
use super::registry::ToolRegistry;
use super::tool::{EventSink, Tool, ToolContext, ToolError};
use super::workspace::WorkspaceJail;
use crate::engine::{build_api_messages, EngineEndpoint};
use crate::types::{split_for_channel, ChatEvent, ChatMessage, ChatStats, SamplingParams};
use futures_util::StreamExt;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// After this many rounds of tool calls without a plain answer, the next
/// round is sent with no `tools` at all, forcing the model to answer
/// directly instead of looping indefinitely.
pub const MAX_AGENT_STEPS: u32 = 8;

#[derive(Clone)]
pub struct AgentDeps {
    pub registry: Arc<ToolRegistry>,
    pub consent: Arc<ConsentBroker>,
    pub audit: Arc<AuditLog>,
    pub workspace: Arc<WorkspaceJail>,
    /// Base dir per-call scratch dirs are created under, one subdirectory
    /// per tool-call id (e.g. `<data_dir>/agent-sandbox/<call_id>/`).
    pub scratch_root: PathBuf,
    pub http: reqwest::Client,
    /// Base URL of the managed SearXNG service backing a directly-registered
    /// `web_search`/`fetch_url` (plain-chat web search only —
    /// `agent_bridge::build_web_search_deps` resolves this eagerly via
    /// `PluginManager::ensure_capability_service`). Full agent mode leaves this
    /// `None` and lets `SpawnSubagentTool` resolve the same service
    /// lazily, only if its `"web-research"` kind actually gets invoked — see
    /// `agent_bridge::build_deps`'s doc comment for why.
    pub search_service_url: Option<String>,
    /// Stable per-URL `[N]` numbering for this turn's citations — see
    /// [`ToolContext::citations`].
    pub citations: Arc<super::citations::CitationRegistry>,
}

/// One nested or top-level tool-use loop's outcome: the final plain-text
/// answer (accumulated from `content` deltas across every round, not just
/// the last one, since a forced-final round still streams `Token`s) plus
/// how many tokens it cost.
pub struct LoopOutcome {
    pub total_tokens: u64,
    pub final_text: String,
}

/// Drive one full agent-mode turn: repeatedly call the model, dispatch any
/// tool calls it makes (through consent + sandboxed/jailed execution), feed
/// the results back, until it answers with no more tool calls or
/// [`MAX_AGENT_STEPS`] is reached.
pub async fn run_agent_turn(
    deps: &AgentDeps,
    endpoint: EngineEndpoint,
    messages: &[ChatMessage],
    sampling: &SamplingParams,
    cancel: Arc<AtomicBool>,
    on_event: impl Fn(ChatEvent) + Send + Sync + 'static,
) -> Result<(), String> {
    let on_event: EventSink = Arc::new(on_event);
    let turn_id = next_turn_id();
    let started = Instant::now();
    let mut wire_messages = build_api_messages(messages);
    let tool_defs = deps.registry.openai_tool_defs();

    let outcome = run_tool_loop(
        deps,
        &endpoint,
        &mut wire_messages,
        &tool_defs,
        sampling,
        &cancel,
        &turn_id,
        MAX_AGENT_STEPS,
        None,
        &on_event,
    )
    .await?;

    on_event(ChatEvent::Done {
        stats: stats_so_far(outcome.total_tokens, started.elapsed()),
    });
    Ok(())
}

/// The reusable tool-use loop primitive underneath [`run_agent_turn`]. Any
/// caller — the top-level turn, or a `spawn_subagent` tool's `execute()`
/// running its own nested loop with its own scoped registry and message
/// history — drives the same round-trip: call the model, dispatch tool
/// calls through consent + execution, feed results back, until it answers
/// with no more tool calls or `step_budget` is exhausted.
///
/// `turn_id` is only a label (used for audit-log/grant-store attribution);
/// callers running a nested loop should mint their own distinct id rather
/// than reusing the parent's, so nested tool calls are attributable.
///
/// `via` is `None` for the top-level turn and `Some(kind)` for a
/// `spawn_subagent` kind's own nested loop — stamped onto this loop's own
/// `ToolConsentRequested`/`ToolCallStarted` events (see
/// [`crate::types::ChatEvent::ToolConsentRequested::via`]) so the UI can
/// attribute a consent prompt to the helper that actually asked for it.
#[allow(clippy::too_many_arguments)]
pub async fn run_tool_loop(
    deps: &AgentDeps,
    endpoint: &EngineEndpoint,
    wire_messages: &mut Vec<Value>,
    tool_defs: &[Value],
    sampling: &SamplingParams,
    cancel: &Arc<AtomicBool>,
    turn_id: &str,
    step_budget: u32,
    via: Option<&str>,
    on_event: &EventSink,
) -> Result<LoopOutcome, String> {
    let mut total_tokens: u64 = 0;
    let mut final_text = String::new();

    for step in 0..step_budget {
        if cancel.load(Ordering::SeqCst) {
            deps.consent.cancel_all("The user stopped this turn.").await;
            return Ok(LoopOutcome {
                total_tokens,
                final_text,
            });
        }

        let force_final = step + 1 == step_budget;
        let round = run_one_round(
            deps,
            endpoint,
            wire_messages,
            if force_final { &[] } else { tool_defs },
            sampling,
            cancel,
            on_event,
            &mut final_text,
        )
        .await?;
        total_tokens += round.token_count;

        if round.tool_calls.is_empty() && round.rejected.is_empty() {
            return Ok(LoopOutcome {
                total_tokens,
                final_text,
            });
        }

        wire_messages.push(assistant_message_with_tool_calls(&round));

        for rejected in &round.rejected {
            let id = rejected_call_id(rejected);
            on_event(ChatEvent::ToolCallRejected {
                tool_call_id: id.clone(),
                tool_id: "unknown".to_string(),
                reason: rejected.reason.clone(),
            });
            let mut event = AuditEvent::new(turn_id, &id, "unknown", "rejected");
            event.reason = Some(rejected.reason.clone());
            deps.audit.append(&event);
            wire_messages.push(tool_result_message(&id, &rejected.reason));
        }

        for call in &round.tool_calls {
            let content = if cancel.load(Ordering::SeqCst) {
                let note = "The user stopped this turn.".to_string();
                on_event(ChatEvent::ToolCallDenied {
                    tool_call_id: call.id.clone(),
                    note: Some(note.clone()),
                });
                note
            } else {
                dispatch_call(deps, turn_id, call, cancel, via, on_event).await
            };
            wire_messages.push(tool_result_message(&call.id, &content));
        }
    }

    Ok(LoopOutcome {
        total_tokens,
        final_text,
    })
}

struct RoundResult {
    token_count: u64,
    tool_calls: Vec<AssembledToolCall>,
    rejected: Vec<RejectedToolCall>,
}

#[allow(clippy::too_many_arguments)]
async fn run_one_round(
    deps: &AgentDeps,
    endpoint: &EngineEndpoint,
    messages: &[Value],
    tools: &[Value],
    sampling: &SamplingParams,
    cancel: &Arc<AtomicBool>,
    on_event: &EventSink,
    final_text: &mut String,
) -> Result<RoundResult, String> {
    let mut body = serde_json::json!({
        "messages": messages,
        "stream": true,
    });
    if !tools.is_empty() {
        body["tools"] = serde_json::json!(tools);
    }
    if let Value::Object(map) = &mut body {
        sampling.merge_into(map);
    }

    let response = deps
        .http
        .post(format!("{}/chat/completions", endpoint.base_url))
        .bearer_auth(&endpoint.api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            let message = "Something went wrong talking to your AI. Please try again.".to_string();
            tracing::error!("agent turn: request failed: {e}");
            message
        })?;
    if !response.status().is_success() {
        let message = "Your AI couldn't process this message. Please try again.".to_string();
        tracing::error!("agent turn: request rejected: {}", response.status());
        on_event(ChatEvent::Error {
            message: message.clone(),
        });
        return Err(message);
    }

    let mut stream = response.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut assembler = ToolCallAssembler::new();
    let mut token_count = 0u64;

    'stream: while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let bytes = chunk.map_err(|e| {
            tracing::error!("agent turn: stream read failed: {e}");
            "The reply was cut off. Please try sending your message again.".to_string()
        })?;
        buf.extend_from_slice(&bytes);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=pos).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            let Some(payload) = line.strip_prefix("data: ") else {
                continue;
            };
            if payload.trim() == "[DONE]" {
                break 'stream;
            }
            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            let Some(choice) = value.get("choices").and_then(|c| c.get(0)) else {
                continue;
            };
            let delta = choice.get("delta");
            if let Some(content) = delta
                .and_then(|d| d.get("content"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                token_count += 1;
                final_text.push_str(content);
                // See `split_for_channel`'s doc comment: an unsplit delta
                // large enough to cross Tauri's async-channel threshold can
                // stall delivery of everything sent after it.
                for piece in split_for_channel(content) {
                    on_event(ChatEvent::Token {
                        text: piece.to_string(),
                    });
                }
            }
            if let Some(reasoning) = delta
                .and_then(|d| d.get("reasoning_content"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                for piece in split_for_channel(reasoning) {
                    on_event(ChatEvent::Reasoning {
                        text: piece.to_string(),
                    });
                }
            }
            if let Some(tool_calls) = delta
                .and_then(|d| d.get("tool_calls"))
                .and_then(Value::as_array)
            {
                assembler.push(tool_calls);
            }
        }
    }

    let (tool_calls, rejected) = assembler.finish();
    Ok(RoundResult {
        token_count,
        tool_calls,
        rejected,
    })
}

/// Resolve, get consent for, and run one tool call. Always returns the
/// string that becomes the `tool`-role message content fed back to the
/// model — a result, a denial note, or a rejection reason — never panics or
/// leaves the call unanswered.
async fn dispatch_call(
    deps: &AgentDeps,
    turn_id: &str,
    call: &AssembledToolCall,
    cancel: &Arc<AtomicBool>,
    via: Option<&str>,
    on_event: &EventSink,
) -> String {
    let Some(tool) = deps.registry.resolve(&call.name) else {
        let reason = format!("There is no tool named \"{}\".", call.name);
        on_event(ChatEvent::ToolCallRejected {
            tool_call_id: call.id.clone(),
            tool_id: call.name.clone(),
            reason: reason.clone(),
        });
        deps.audit
            .append(&AuditEvent::new(turn_id, &call.id, &call.name, "rejected"));
        return reason;
    };

    let scratch_dir = deps.scratch_root.join(&call.id);
    let ctx = ToolContext {
        turn_id: turn_id.to_string(),
        call_id: call.id.clone(),
        workspace: deps.workspace.clone(),
        scratch_dir,
        cancel: cancel.clone(),
        on_event: on_event.clone(),
        citations: deps.citations.clone(),
        consent: deps.consent.clone(),
    };

    let summary = match tool.consent_summary(&call.arguments, &ctx) {
        Ok(s) => s,
        Err(err) => {
            let reason = err.to_string();
            on_event(ChatEvent::ToolCallRejected {
                tool_call_id: call.id.clone(),
                tool_id: tool.id().to_string(),
                reason: reason.clone(),
            });
            deps.audit
                .append(&AuditEvent::new(turn_id, &call.id, tool.id(), "rejected"));
            return reason;
        }
    };

    let grant_key = tool.grant_key(&call.arguments, &ctx);
    let auto_approved = !tool.requires_consent(&call.arguments, &ctx)
        || deps.consent.find_grant(&grant_key).await.is_some();

    if auto_approved {
        if cancel.load(Ordering::SeqCst) {
            return "The user stopped this turn.".to_string();
        }
        on_event(ChatEvent::ToolCallStarted {
            tool_call_id: call.id.clone(),
            tool_id: tool.id().to_string(),
            summary: summary.summary.clone(),
            auto_approved: true,
            via: via.map(str::to_string),
        });
        let mut event = AuditEvent::new(turn_id, &call.id, tool.id(), "started");
        event.grant_key = Some(grant_key.clone());
        deps.audit.append(&event);
        return run_and_report(&tool, &call.arguments, &ctx, turn_id, on_event, &deps.audit).await;
    }

    let request_id = call.id.clone();
    let rx = deps.consent.register(&request_id).await;
    on_event(ChatEvent::ToolConsentRequested {
        request_id: request_id.clone(),
        tool_call_id: call.id.clone(),
        tool_id: tool.id().to_string(),
        summary: summary.summary.clone(),
        detail: summary.detail.clone(),
        resolved_paths: summary.resolved_paths.clone(),
        danger_flags: summary.danger_flags.clone(),
        via: via.map(str::to_string),
    });
    deps.audit.append(&AuditEvent::new(
        turn_id,
        &call.id,
        tool.id(),
        "consentRequested",
    ));

    let (kind, note) = tokio::select! {
        biased;
        _ = wait_for_cancel(cancel) => {
            deps.consent.cancel_all("The user stopped this turn.").await;
            (ConsentDecision::Deny, Some("The user stopped this turn.".to_string()))
        }
        result = rx => result.unwrap_or_else(|_| {
            (ConsentDecision::Deny, Some("The prompt was closed without an answer.".to_string()))
        }),
    };

    match kind {
        ConsentDecision::Deny => {
            on_event(ChatEvent::ToolCallDenied {
                tool_call_id: call.id.clone(),
                note: note.clone(),
            });
            let mut event = AuditEvent::new(turn_id, &call.id, tool.id(), "decided");
            event.decision = Some("denied".to_string());
            deps.audit.append(&event);
            note.unwrap_or_else(|| "The user denied this action.".to_string())
        }
        ConsentDecision::AllowOnce | ConsentDecision::AlwaysAllow => {
            if kind == ConsentDecision::AlwaysAllow {
                let outside_workspace = summary.resolved_paths.iter().any(|p| !p.inside_workspace);
                let expires_at_ms = outside_workspace.then(|| now_ms() + 30 * 24 * 60 * 60 * 1000);
                deps.consent
                    .save_grant(ToolGrant {
                        grant_key: grant_key.clone(),
                        tool_id: tool.id().to_string(),
                        summary: summary.summary.clone(),
                        created_at_ms: now_ms(),
                        expires_at_ms,
                    })
                    .await;
            }
            let mut decided = AuditEvent::new(turn_id, &call.id, tool.id(), "decided");
            decided.decision = Some(if kind == ConsentDecision::AlwaysAllow {
                "alwaysAllow".to_string()
            } else {
                "allowOnce".to_string()
            });
            deps.audit.append(&decided);

            on_event(ChatEvent::ToolCallStarted {
                tool_call_id: call.id.clone(),
                tool_id: tool.id().to_string(),
                summary: summary.summary.clone(),
                auto_approved: false,
                via: via.map(str::to_string),
            });
            deps.audit
                .append(&AuditEvent::new(turn_id, &call.id, tool.id(), "started"));
            run_and_report(&tool, &call.arguments, &ctx, turn_id, on_event, &deps.audit).await
        }
    }
}

async fn run_and_report(
    tool: &Arc<dyn Tool>,
    args: &Value,
    ctx: &ToolContext,
    turn_id: &str,
    on_event: &EventSink,
    audit: &AuditLog,
) -> String {
    match tool.execute(args, ctx).await {
        Ok(output) => {
            on_event(ChatEvent::ToolCallResult {
                tool_call_id: ctx.call_id.clone(),
                ok: output.ok,
                output_preview: output.preview.clone(),
            });
            let mut event = AuditEvent::new(turn_id, &ctx.call_id, tool.id(), "finished");
            event.ok = Some(output.ok);
            audit.append(&event);
            output.content
        }
        Err(err) => {
            let message = match &err {
                ToolError::Unsupported => {
                    "This tool isn't available on this platform yet.".to_string()
                }
                _ => err.to_string(),
            };
            on_event(ChatEvent::ToolCallResult {
                tool_call_id: ctx.call_id.clone(),
                ok: false,
                output_preview: message.clone(),
            });
            let mut event = AuditEvent::new(turn_id, &ctx.call_id, tool.id(), "finished");
            event.ok = Some(false);
            event.reason = Some(message.clone());
            audit.append(&event);
            message
        }
    }
}

/// Poll `cancel` until it flips true. `pub(crate)` so a tool whose `execute()`
/// can block for a while on something other than I/O with its own timeout
/// (e.g. `builtin::AskUserTool` awaiting an answer) can race the same
/// cancellation signal this loop's own consent-wait does.
pub(crate) async fn wait_for_cancel(cancel: &Arc<AtomicBool>) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn rejected_call_id(rejected: &RejectedToolCall) -> String {
    rejected
        .id
        .clone()
        .unwrap_or_else(|| format!("call_{}", rejected.index))
}

fn assistant_message_with_tool_calls(round: &RoundResult) -> Value {
    let mut tool_calls: Vec<Value> = round
        .tool_calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": serde_json::to_string(&call.arguments).unwrap_or_default(),
                }
            })
        })
        .collect();
    for rejected in &round.rejected {
        tool_calls.push(serde_json::json!({
            "id": rejected_call_id(rejected),
            "type": "function",
            "function": { "name": "unknown", "arguments": "{}" }
        }));
    }
    serde_json::json!({ "role": "assistant", "content": Value::Null, "tool_calls": tool_calls })
}

fn tool_result_message(tool_call_id: &str, content: &str) -> Value {
    serde_json::json!({
        "role": "tool",
        "tool_call_id": tool_call_id,
        "content": content,
    })
}

fn stats_so_far(tokens: u64, elapsed: Duration) -> ChatStats {
    let secs = elapsed.as_secs_f64();
    ChatStats {
        total_tokens: tokens,
        tokens_per_sec: if secs > 0.0 {
            tokens as f64 / secs
        } else {
            0.0
        },
        duration_ms: elapsed.as_millis() as u64,
    }
}

fn next_turn_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("turn-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::builtin::FsReadTool;
    use crate::agent::tool::{ConsentSummary, ToolOutput};
    use std::sync::atomic::AtomicBool;
    use tokio::net::TcpListener;

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn id(&self) -> &str {
            "echo"
        }
        fn spec(&self) -> Value {
            serde_json::json!({ "name": "echo", "description": "echo", "parameters": {"type":"object","properties":{"text":{"type":"string"}}}})
        }
        fn consent_summary(
            &self,
            args: &Value,
            _ctx: &ToolContext,
        ) -> Result<ConsentSummary, ToolError> {
            Ok(ConsentSummary {
                summary: "echo".into(),
                detail: args.to_string(),
                resolved_paths: vec![],
                danger_flags: vec![],
            })
        }
        fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
            args.to_string()
        }
        async fn execute(&self, args: &Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(ToolOutput {
                ok: true,
                preview: text.clone(),
                content: text,
            })
        }
    }

    /// Same shape as [`EchoTool`] but opts out of consent — stands in for
    /// `WebSearchTool`/`FetchUrlTool`, which do the same, without needing a
    /// live HTTP dependency in this test.
    struct NoConsentEchoTool;

    #[async_trait::async_trait]
    impl Tool for NoConsentEchoTool {
        fn id(&self) -> &str {
            "no_consent_echo"
        }
        fn spec(&self) -> Value {
            serde_json::json!({ "name": "no_consent_echo", "description": "echo", "parameters": {"type":"object","properties":{"text":{"type":"string"}}}})
        }
        fn consent_summary(
            &self,
            args: &Value,
            _ctx: &ToolContext,
        ) -> Result<ConsentSummary, ToolError> {
            Ok(ConsentSummary {
                summary: "no_consent_echo".into(),
                detail: args.to_string(),
                resolved_paths: vec![],
                danger_flags: vec![],
            })
        }
        fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
            args.to_string()
        }
        fn requires_consent(&self, _args: &Value, _ctx: &ToolContext) -> bool {
            false
        }
        async fn execute(&self, args: &Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(ToolOutput {
                ok: true,
                preview: text.clone(),
                content: text,
            })
        }
    }

    fn deps(tmp: &std::path::Path, tools: Vec<Arc<dyn Tool>>) -> AgentDeps {
        let mut registry = ToolRegistry::new();
        for t in tools {
            registry.register(t);
        }
        let workspace_dir = tmp.join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        AgentDeps {
            registry: Arc::new(registry),
            consent: Arc::new(ConsentBroker::new(tmp.join("grants.json"))),
            audit: Arc::new(AuditLog::open(Arc::new(std::sync::Mutex::new(
                crate::app_db::open(&tmp.join("app.sqlite"), None)
                    .unwrap()
                    .0,
            )))),
            workspace: Arc::new(
                WorkspaceJail::new(
                    &workspace_dir,
                    std::path::Path::new("/nonexistent-home"),
                    std::path::Path::new("/nonexistent-data"),
                )
                .unwrap(),
            ),
            scratch_root: tmp.join("scratch"),
            http: reqwest::Client::new(),
            search_service_url: None,
            citations: Arc::new(crate::agent::CitationRegistry::new()),
        }
    }

    fn user_msg(content: &str) -> ChatMessage {
        ChatMessage {
            id: crate::types::new_message_id(),
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
        }
    }

    /// A tiny fake OpenAI-compatible server: first request answers with one
    /// tool call (asking to auto-approve via the consent responder below),
    /// second request (after the tool result is fed back) answers plainly.
    async fn fake_server_tool_then_answer() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for round in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf).await;
                let body = if round == 0 {
                    "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"text\\\":\\\"hi\\\"}\"}}]},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n"
                } else {
                    "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (format!("http://{addr}"), handle)
    }

    /// A fake OpenAI-compatible server that answers each successive request
    /// with the next body in `bodies`, in order — generalizes
    /// `fake_server_tool_then_answer` to the multi-round exchanges a nested
    /// (subagent) loop produces on top of the parent turn's own rounds.
    async fn fake_server_with_bodies(
        bodies: Vec<&'static str>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            for body in bodies {
                let (mut stream, _) = listener.accept().await.unwrap();
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (format!("http://{addr}"), handle)
    }

    fn fake_endpoint(base: String) -> EngineEndpoint {
        EngineEndpoint {
            base_url: base,
            api_key: "test".to_string(),
            model_id: "test-model".to_string(),
            request_timeout_secs: 30,
        }
    }

    #[tokio::test]
    async fn a_grant_hit_runs_without_a_consent_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let d = deps(tmp.path(), vec![Arc::new(EchoTool)]);
        // Pre-seed a grant matching EchoTool's grant_key for this exact call.
        d.consent
            .save_grant(ToolGrant {
                grant_key: serde_json::json!({"text":"hi"}).to_string(),
                tool_id: "echo".into(),
                summary: "echo".into(),
                created_at_ms: now_ms(),
                expires_at_ms: None,
            })
            .await;

        let (base, _server) = fake_server_tool_then_answer().await;
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_sink = events.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        run_agent_turn(
            &d,
            fake_endpoint(base),
            &[user_msg("say hi")],
            &SamplingParams::default(),
            cancel,
            move |e| events_sink.lock().unwrap().push(e),
        )
        .await
        .unwrap();

        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            ChatEvent::ToolCallStarted {
                auto_approved: true,
                ..
            }
        )));
        assert!(!events
            .iter()
            .any(|e| matches!(e, ChatEvent::ToolConsentRequested { .. })));
        assert!(events.iter().any(|e| matches!(e, ChatEvent::Done { .. })));
    }

    #[tokio::test]
    async fn a_tool_that_opts_out_of_consent_runs_with_no_grant_and_no_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let d = deps(tmp.path(), vec![Arc::new(NoConsentEchoTool)]);
        // No pre-seeded grant — `requires_consent() == false` alone must be
        // enough to skip both the grant lookup and the interactive prompt.

        let bodies = vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"no_consent_echo\",\"arguments\":\"{\\\"text\\\":\\\"hi\\\"}\"}}]},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
        ];
        let (base, _server) = fake_server_with_bodies(bodies).await;
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_sink = events.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        run_agent_turn(
            &d,
            fake_endpoint(base),
            &[user_msg("say hi")],
            &SamplingParams::default(),
            cancel,
            move |e| events_sink.lock().unwrap().push(e),
        )
        .await
        .unwrap();

        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            ChatEvent::ToolCallStarted {
                auto_approved: true,
                ..
            }
        )));
        assert!(!events
            .iter()
            .any(|e| matches!(e, ChatEvent::ToolConsentRequested { .. })));
        assert!(events.iter().any(|e| matches!(e, ChatEvent::Done { .. })));
    }

    #[tokio::test]
    async fn cancelling_before_the_first_round_ends_the_turn_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let d = deps(tmp.path(), vec![Arc::new(EchoTool)]);
        let (base, _server) = fake_server_tool_then_answer().await;
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_sink = events.clone();
        let cancel = Arc::new(AtomicBool::new(true));
        run_agent_turn(
            &d,
            fake_endpoint(base),
            &[user_msg("say hi")],
            &SamplingParams::default(),
            cancel,
            move |e| events_sink.lock().unwrap().push(e),
        )
        .await
        .unwrap();
        assert!(matches!(
            events.lock().unwrap().last(),
            Some(ChatEvent::Done { .. })
        ));
    }

    #[test]
    fn read_file_tool_is_constructible_in_a_registry() {
        // Sanity check that the built-in tool type used elsewhere in the
        // crate implements `Tool` and can be boxed/registered like any
        // other — guards against a signature drift between builtin/fs.rs
        // and the orchestrator's expectations.
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(FsReadTool));
        assert!(registry.resolve("read_file").is_some());
    }

    /// Same shape as `EchoTool` but with a configurable id, so a test can
    /// stand in for `run_code` (which `SpawnSubagentTool`'s "math" kind
    /// resolves by id) without needing a real sandboxed executor.
    struct NamedEchoTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for NamedEchoTool {
        fn id(&self) -> &str {
            self.0
        }
        fn spec(&self) -> Value {
            serde_json::json!({ "name": self.0, "description": self.0, "parameters": {"type":"object","properties":{}}})
        }
        fn consent_summary(
            &self,
            args: &Value,
            _ctx: &ToolContext,
        ) -> Result<ConsentSummary, ToolError> {
            Ok(ConsentSummary {
                summary: self.0.to_string(),
                detail: args.to_string(),
                resolved_paths: vec![],
                danger_flags: vec![],
            })
        }
        fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
            args.to_string()
        }
        async fn execute(
            &self,
            _args: &Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                ok: true,
                preview: "ran".to_string(),
                content: "2".to_string(),
            })
        }
    }

    /// Regression test for the deadlock this whole `EventSink` plumbing was
    /// added to fix: a `spawn_subagent` call's *own* nested tool calls
    /// (here, the "math" kind's `run_code`) must still surface their
    /// consent/lifecycle events through the top-level sink, and the
    /// subagent's streamed text must never leak into the parent's visible
    /// reply — only its final answer, returned as the tool's output, should.
    #[tokio::test]
    async fn spawn_subagent_runs_a_nested_loop_and_forwards_its_events() {
        use crate::agent::builtin::SpawnSubagentTool;

        let tmp = tempfile::tempdir().unwrap();
        let workspace_dir = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let workspace = Arc::new(
            WorkspaceJail::new(
                &workspace_dir,
                std::path::Path::new("/nonexistent-home"),
                std::path::Path::new("/nonexistent-data"),
            )
            .unwrap(),
        );
        let consent = Arc::new(ConsentBroker::new(tmp.path().join("grants.json")));
        let audit = Arc::new(AuditLog::open(Arc::new(std::sync::Mutex::new(
            crate::app_db::open(&tmp.path().join("app.sqlite"), None)
                .unwrap()
                .0,
        ))));

        let (base, _server) = fake_server_with_bodies(vec![
            // Top-level round 0: the model asks for the "math" helper.
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"spawn_subagent\",\"arguments\":\"{\\\"kind\\\":\\\"math\\\",\\\"task\\\":\\\"what is 1+1\\\"}\"}}]},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            // Nested round 0: the helper calls run_code.
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_2\",\"function\":{\"name\":\"run_code\",\"arguments\":\"{\\\"code\\\":\\\"1+1\\\"}\"}}]},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            // Nested round 1: the helper answers.
            "data: {\"choices\":[{\"delta\":{\"content\":\"2\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            // Top-level round 1: the parent relays the helper's answer.
            "data: {\"choices\":[{\"delta\":{\"content\":\"The helper says 2\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
        ])
        .await;

        let endpoint = fake_endpoint(base);
        let sampling = SamplingParams::default();
        let db = std::sync::Arc::new(std::sync::Mutex::new(
            crate::app_db::open(&tmp.path().join("app.sqlite"), None)
                .unwrap()
                .0,
        ));
        let plugins = Arc::new(crate::plugin::PluginManager::new(
            vec![],
            tmp.path().join("plugin-data"),
            db,
        ));
        let subagent_tool = Arc::new(SpawnSubagentTool::new(endpoint.clone(), sampling, plugins));

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(NamedEchoTool("run_code")));
        registry.register(subagent_tool.clone());

        let d = AgentDeps {
            registry: Arc::new(registry),
            consent: consent.clone(),
            audit,
            workspace,
            scratch_root: tmp.path().join("scratch"),
            http: reqwest::Client::new(),
            search_service_url: None,
            citations: Arc::new(crate::agent::CitationRegistry::new()),
        };
        subagent_tool.bind_deps(d.clone());

        // Pre-seed grants for both calls so neither needs an interactive
        // consent prompt nobody in this test would ever answer.
        consent
            .save_grant(ToolGrant {
                grant_key: "spawn_subagent:math:what is 1+1".to_string(),
                tool_id: "spawn_subagent".into(),
                summary: String::new(),
                created_at_ms: now_ms(),
                expires_at_ms: None,
            })
            .await;
        consent
            .save_grant(ToolGrant {
                grant_key: serde_json::json!({"code": "1+1"}).to_string(),
                tool_id: "run_code".into(),
                summary: String::new(),
                created_at_ms: now_ms(),
                expires_at_ms: None,
            })
            .await;

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_sink = events.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        run_agent_turn(
            &d,
            endpoint,
            &[user_msg("what is 1+1, ask the math helper")],
            &SamplingParams::default(),
            cancel,
            move |e| events_sink.lock().unwrap().push(e),
        )
        .await
        .unwrap();

        let events = events.lock().unwrap();
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatEvent::SubagentStarted { kind, .. } if kind == "math")));
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatEvent::SubagentFinished { ok: true, .. })));
        // The helper's own run_code call must have reached the top-level
        // sink (the deadlock this test guards against: with nowhere for its
        // consent/lifecycle events to go, the nested loop would hang
        // forever awaiting a decision the user was never shown).
        assert!(events.iter().any(
            |e| matches!(e, ChatEvent::ToolCallStarted { tool_id, .. } if tool_id == "run_code")
        ));
        // The nested loop's own streamed answer ("2") must never leak into
        // the parent's visible reply — only the top-level round's own
        // tokens ("The helper says 2") should appear.
        assert!(!events
            .iter()
            .any(|e| matches!(e, ChatEvent::Token { text } if text == "2")));
        assert!(events
            .iter()
            .any(|e| matches!(e, ChatEvent::Token { text } if text.contains("The helper says"))));
    }

    /// End-to-end wiring check for `ask_user`: it never shows a consent
    /// prompt (no grant needed), its `ChatEvent::ClarifyingQuestionAsked`
    /// reaches the top-level sink with the right ids, and the answer
    /// delivered through `ConsentBroker::resolve_clarification` (standing in
    /// for the frontend's `respond_clarifying_question` command) is what the
    /// model sees fed back as the tool result.
    #[tokio::test]
    async fn ask_user_round_trips_an_answer_with_no_consent_prompt() {
        use crate::agent::builtin::AskUserTool;

        let tmp = tempfile::tempdir().unwrap();
        let d = deps(tmp.path(), vec![Arc::new(AskUserTool)]);
        let consent = d.consent.clone();

        let bodies = vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"ask_user\",\"arguments\":\"{\\\"question\\\":\\\"Which city?\\\",\\\"options\\\":[\\\"Paris\\\",\\\"London\\\"]}\"}}]},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Great, using Paris.\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
        ];
        let (base, _server) = fake_server_with_bodies(bodies).await;

        // Resolve the question shortly after it's asked, standing in for the
        // frontend's `respond_clarifying_question` call.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            consent
                .resolve_clarification("call_1", "Paris".to_string())
                .await;
        });

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_sink = events.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        run_agent_turn(
            &d,
            fake_endpoint(base),
            &[user_msg("which city should I use?")],
            &SamplingParams::default(),
            cancel,
            move |e| events_sink.lock().unwrap().push(e),
        )
        .await
        .unwrap();

        let events = events.lock().unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, ChatEvent::ToolConsentRequested { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            ChatEvent::ClarifyingQuestionAsked { question, options, .. }
                if question == "Which city?" && options == &["Paris".to_string(), "London".to_string()]
        )));
        assert!(events.iter().any(
            |e| matches!(e, ChatEvent::ToolCallResult { ok: true, output_preview, .. } if output_preview == "Answered: Paris")
        ));
        assert!(events.iter().any(
            |e| matches!(e, ChatEvent::Token { text } if text.contains("Great, using Paris"))
        ));
    }
}
