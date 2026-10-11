//! Reassemble a streamed `delta.tool_calls` round into complete calls.
//!
//! llama-server (like OpenAI) streams a tool call's `function.arguments` as
//! a JSON *string* built incrementally across chunks, keyed by `index` — not
//! necessarily arriving in one piece, or even in index order. This is
//! additive: it doesn't touch [`crate::sse`], which only knows about plain
//! content/reasoning tokens — the agent orchestrator parses its own SSE
//! stream when tools are in play.

use std::collections::BTreeMap;

/// Per-call cap. A malicious or buggy stream that never stops appending to
/// one call's `arguments` must not grow memory unboundedly.
pub const MAX_ARGS_BYTES_PER_CALL: usize = 4 * 1024 * 1024;
/// Combined cap across every call in flight this round — guards against
/// several large calls arriving at once, each individually under the
/// per-call cap.
pub const MAX_COMBINED_ARGS_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
struct PartialCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    overflowed: bool,
}

#[derive(Debug, Clone)]
pub struct AssembledToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A call that reached the end of the round in a state that must never be
/// handed to a `Tool` — overflowed, missing a name, or malformed JSON
/// arguments. The orchestrator synthesizes a rejection tool-result for these
/// directly, without a consent prompt.
#[derive(Debug, Clone)]
pub struct RejectedToolCall {
    pub index: u64,
    pub id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct ToolCallAssembler {
    calls: BTreeMap<u64, PartialCall>,
    combined_bytes: usize,
}

impl ToolCallAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one SSE chunk's `delta.tool_calls` array (or a slice of it).
    pub fn push(&mut self, deltas: &[serde_json::Value]) {
        for d in deltas {
            let index = d.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let entry = self.calls.entry(index).or_default();
            if entry.overflowed {
                continue;
            }
            if let Some(id) = d.get("id").and_then(|v| v.as_str()) {
                entry.id = Some(id.to_string());
            }
            if let Some(func) = d.get("function") {
                if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                    entry.name = Some(name.to_string());
                }
                if let Some(fragment) = func.get("arguments").and_then(|v| v.as_str()) {
                    if entry.arguments.len() + fragment.len() > MAX_ARGS_BYTES_PER_CALL
                        || self.combined_bytes + fragment.len() > MAX_COMBINED_ARGS_BYTES
                    {
                        entry.overflowed = true;
                        entry.arguments.clear();
                        continue;
                    }
                    entry.arguments.push_str(fragment);
                    self.combined_bytes += fragment.len();
                }
            }
        }
    }

    /// Finish the round. Complete, well-formed calls and rejected ones are
    /// reported separately, both in ascending `index` order.
    pub fn finish(self) -> (Vec<AssembledToolCall>, Vec<RejectedToolCall>) {
        let mut calls = Vec::new();
        let mut rejected = Vec::new();
        for (index, partial) in self.calls {
            if partial.overflowed {
                rejected.push(RejectedToolCall {
                    index,
                    id: partial.id,
                    reason: "Arguments exceeded the size limit and were rejected before execution."
                        .to_string(),
                });
                continue;
            }
            let Some(name) = partial.name else {
                rejected.push(RejectedToolCall {
                    index,
                    id: partial.id,
                    reason: "Tool call was missing a function name.".to_string(),
                });
                continue;
            };
            let arguments: serde_json::Value = if partial.arguments.trim().is_empty() {
                serde_json::json!({})
            } else {
                match serde_json::from_str(&partial.arguments) {
                    Ok(v) => v,
                    Err(_) => {
                        rejected.push(RejectedToolCall {
                            index,
                            id: partial.id,
                            reason: "Tool call arguments were not valid JSON.".to_string(),
                        });
                        continue;
                    }
                }
            };
            let id = partial.id.unwrap_or_else(|| format!("call_{index}"));
            calls.push(AssembledToolCall {
                id,
                name,
                arguments,
            });
        }
        (calls, rejected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(
        index: u64,
        id: Option<&str>,
        name: Option<&str>,
        args_fragment: &str,
    ) -> serde_json::Value {
        let mut function = serde_json::json!({ "arguments": args_fragment });
        if let Some(name) = name {
            function["name"] = serde_json::json!(name);
        }
        let mut call = serde_json::json!({ "index": index, "function": function });
        if let Some(id) = id {
            call["id"] = serde_json::json!(id);
        }
        call
    }

    #[test]
    fn assembles_a_single_call_split_across_chunks() {
        let mut a = ToolCallAssembler::new();
        a.push(&[delta(0, Some("call_1"), Some("run_shell"), "{\"comm")]);
        a.push(&[delta(0, None, None, "and\":\"ls\"}")]);
        let (calls, rejected) = a.finish();
        assert!(rejected.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "run_shell");
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    #[test]
    fn assembles_multiple_interleaved_calls_by_index() {
        let mut a = ToolCallAssembler::new();
        a.push(&[
            delta(0, Some("c0"), Some("run_shell"), "{\"command\":"),
            delta(1, Some("c1"), Some("read_file"), "{\"path\":"),
        ]);
        a.push(&[
            delta(1, None, None, "\"a.txt\"}"),
            delta(0, None, None, "\"ls\"}"),
        ]);
        let (mut calls, rejected) = a.finish();
        calls.sort_by(|a, b| a.id.cmp(&b.id));
        assert!(rejected.is_empty());
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "run_shell");
        assert_eq!(calls[1].name, "read_file");
    }

    #[test]
    fn a_call_missing_a_name_is_rejected_not_executed() {
        let mut a = ToolCallAssembler::new();
        a.push(&[delta(0, Some("c0"), None, "{}")]);
        let (calls, rejected) = a.finish();
        assert!(calls.is_empty());
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn malformed_json_arguments_are_rejected_not_executed() {
        let mut a = ToolCallAssembler::new();
        a.push(&[delta(0, Some("c0"), Some("run_shell"), "{not json")]);
        let (calls, rejected) = a.finish();
        assert!(calls.is_empty());
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn oversized_arguments_overflow_to_a_rejection_instead_of_growing_forever() {
        let mut a = ToolCallAssembler::new();
        let chunk = "a".repeat(1024 * 1024);
        for _ in 0..5 {
            a.push(&[delta(0, Some("c0"), Some("write_file"), &chunk)]);
        }
        let (calls, rejected) = a.finish();
        assert!(
            calls.is_empty(),
            "an overflowed call must never reach execution"
        );
        assert_eq!(rejected.len(), 1);
    }

    #[test]
    fn empty_arguments_default_to_an_empty_object() {
        let mut a = ToolCallAssembler::new();
        a.push(&[delta(0, Some("c0"), Some("read_file"), "")]);
        let (calls, rejected) = a.finish();
        assert!(rejected.is_empty());
        assert_eq!(calls[0].arguments, serde_json::json!({}));
    }
}
