//! Parse and drain an OpenAI-compatible chat-completions SSE stream.
//!
//! Shared by [`crate::engine`] (talks to the local `llama-server` subprocess)
//! and [`crate::cloud`] (talks to a signed-in account's cloud backend) — both
//! speak the exact same wire format, so the parsing lives here once rather
//! than risking the two implementations drifting apart on a subtle protocol.
//! Deliberately has no `reqwest` dependency: callers hand [`drain_sse`] their
//! own byte stream and their own transport-error-to-message mapping, so this
//! module only ever deals in bytes, lines, and [`ChatEvent`]s.

use crate::types::{split_for_channel, ChatEvent, ChatStats};
use futures_util::Stream;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Drain an OpenAI-compatible chat-completions SSE stream, emitting
/// [`ChatEvent`]s as chunks arrive, until `data: [DONE]`, the stream ends, a
/// transport error occurs, or `cancel` is set. Verified wire format
/// (llama.cpp b9606, also what an OpenAI-compatible cloud backend speaks):
/// each chunk is `{"choices":[{"delta":{"content":"T",...},"finish_reason":
/// null,...}],...}`; the first chunk carries `delta.role` with `content:
/// null`; the final chunk has `finish_reason: "stop"|"length"`, an empty
/// delta, and a top-level `timings` object (`predicted_n`/
/// `predicted_per_second`), used for [`ChatStats`] when present, falling back
/// to counted chunks + wall time. Non-`data:` lines (SSE keep-alive pings)
/// are ignored.
///
/// `cancel` is checked once per received chunk (not proactively — an idle
/// stream isn't polled): when set, the connection is dropped (ending
/// generation server-side) and `Done` is emitted with the stats so far: a
/// user stopping generation is not an error. Any transport error mid-stream
/// is turned into a message by `on_transport_error` and returned as both a
/// `ChatEvent::Error` and an `Err`.
pub async fn drain_sse<S, B, E>(
    mut stream: S,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(ChatEvent) + Send,
    on_transport_error: impl Fn(&E) -> String,
) -> Result<(), String>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let started = Instant::now();
    let mut lines = SseLineBuffer::new();
    let mut token_count: u64 = 0;
    let mut timing_tokens: Option<u64> = None;
    let mut timing_tps: Option<f64> = None;

    let stats = |tokens: u64, tps: Option<f64>, elapsed: Duration| ChatStats {
        total_tokens: tokens,
        tokens_per_sec: tps.unwrap_or_else(|| {
            let wall_secs = elapsed.as_secs_f64();
            if wall_secs > 0.0 {
                tokens as f64 / wall_secs
            } else {
                0.0
            }
        }),
        duration_ms: elapsed.as_millis() as u64,
    };

    'stream: while let Some(next) = stream.next().await {
        if cancel.load(Ordering::SeqCst) {
            // Dropping the stream closes the connection, which makes the
            // server stop generating. Stopping is not an error.
            drop(stream);
            on_event(ChatEvent::Done {
                stats: stats(token_count, None, started.elapsed()),
            });
            return Ok(());
        }
        let bytes = match next {
            Ok(bytes) => bytes,
            Err(e) => {
                let message = on_transport_error(&e);
                on_event(ChatEvent::Error {
                    message: message.clone(),
                });
                return Err(message);
            }
        };
        for line in lines.push(bytes.as_ref()) {
            let Some(payload) = line.strip_prefix("data: ") else {
                continue; // SSE pings/comments
            };
            if payload.trim() == "[DONE]" {
                break 'stream;
            }
            let Some(chunk) = parse_chunk(payload) else {
                // Don't log `payload` itself: on a parse failure it's often a
                // truncated/non-conforming chunk that still carries a literal
                // fragment of the model's streamed answer.
                tracing::warn!("unparseable SSE chunk ({} bytes)", payload.len());
                continue;
            };
            // Reasoning models split their private "thinking" into a
            // separate `reasoning_content` delta (llama.cpp's default
            // reasoning-format); surface it as a distinct event so the UI
            // can show it apart from the answer. Empty unless thinking is on.
            if let Some(text) = chunk.reasoning {
                // See `split_for_channel`'s doc comment: some reasoning-format
                // parsers flush a whole `<think>` block as one delta, large
                // enough to stall the Tauri channel if sent unsplit.
                for piece in split_for_channel(&text) {
                    on_event(ChatEvent::Reasoning {
                        text: piece.to_string(),
                    });
                }
            }
            if let Some(text) = chunk.content {
                token_count += 1;
                for piece in split_for_channel(&text) {
                    on_event(ChatEvent::Token {
                        text: piece.to_string(),
                    });
                }
            }
            if chunk.finished {
                timing_tokens = chunk.timing_tokens;
                timing_tps = chunk.timing_tps;
            }
        }
    }

    on_event(ChatEvent::Done {
        stats: stats(
            timing_tokens.unwrap_or(token_count),
            timing_tps,
            started.elapsed(),
        ),
    });
    Ok(())
}

/// Accumulates raw bytes and yields complete `\n`-terminated lines, handling
/// chunk boundaries that fall mid-line. Trailing `\r` is stripped.
struct SseLineBuffer {
    buf: Vec<u8>,
}

impl SseLineBuffer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // the '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }
}

/// What we need from one parsed SSE `data:` payload.
struct SseChunk {
    content: Option<String>,
    /// `delta.reasoning_content` — the model's private "thinking", present only
    /// for reasoning models with thinking enabled. `None`/empty otherwise.
    reasoning: Option<String>,
    finished: bool,
    timing_tokens: Option<u64>,
    timing_tps: Option<f64>,
}

fn parse_chunk(payload: &str) -> Option<SseChunk> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let choice = value.get("choices")?.get(0)?;
    let timings = value.get("timings");
    let delta = choice.get("delta");
    // Treat an empty-string reasoning delta as absent so we never emit blanks.
    let reasoning = delta
        .and_then(|d| d.get("reasoning_content"))
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(SseChunk {
        content: delta
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
            .map(str::to_string),
        reasoning,
        finished: choice.get("finish_reason").is_some_and(|f| !f.is_null()),
        timing_tokens: timings
            .and_then(|t| t.get("predicted_n"))
            .and_then(|n| n.as_u64()),
        timing_tps: timings
            .and_then(|t| t.get("predicted_per_second"))
            .and_then(|n| n.as_f64()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    // Verified llama.cpp b9606 wire format.
    const FIRST_CHUNK: &str = r#"data: {"choices":[{"finish_reason":null,"index":0,"delta":{"role":"assistant","content":null}}],"created":1749700000,"id":"chatcmpl-1","model":"m","object":"chat.completion.chunk"}"#;
    const TOKEN_CHUNK: &str = r#"data: {"choices":[{"finish_reason":null,"index":0,"delta":{"content":"T"}}],"created":1749700000,"id":"chatcmpl-1","model":"m","object":"chat.completion.chunk"}"#;
    // Reasoning models stream their "thinking" in a separate delta field.
    const REASONING_CHUNK: &str = r#"data: {"choices":[{"finish_reason":null,"index":0,"delta":{"reasoning_content":"Let me think"}}],"created":1749700000,"id":"chatcmpl-1","model":"m","object":"chat.completion.chunk"}"#;
    const FINAL_CHUNK: &str = r#"data: {"choices":[{"finish_reason":"stop","index":0,"delta":{}}],"created":1749700000,"id":"chatcmpl-1","model":"m","object":"chat.completion.chunk","timings":{"predicted_n":12,"predicted_per_second":98.5,"prompt_n":9}}"#;

    #[test]
    fn sse_buffer_yields_complete_lines() {
        let mut buf = SseLineBuffer::new();
        let input = format!("{TOKEN_CHUNK}\n\n{FINAL_CHUNK}\n\ndata: [DONE]\n\n");
        let lines = buf.push(input.as_bytes());
        assert_eq!(lines.len(), 6); // three data lines + three blank separators
        assert_eq!(lines[0], TOKEN_CHUNK);
        assert_eq!(lines[2], FINAL_CHUNK);
        assert_eq!(lines[4], "data: [DONE]");
    }

    #[test]
    fn sse_buffer_handles_a_line_split_across_pushes() {
        let mut buf = SseLineBuffer::new();
        let (a, b) = TOKEN_CHUNK.split_at(37);
        assert!(buf.push(a.as_bytes()).is_empty());
        let lines = buf.push(format!("{b}\n").as_bytes());
        assert_eq!(lines, vec![TOKEN_CHUNK.to_string()]);
    }

    #[test]
    fn sse_buffer_strips_carriage_returns() {
        let mut buf = SseLineBuffer::new();
        let lines = buf.push(b"data: x\r\n: ping\r\n");
        assert_eq!(lines, vec!["data: x".to_string(), ": ping".to_string()]);
    }

    #[test]
    fn parse_chunk_reads_the_verified_wire_format() {
        let first = parse_chunk(FIRST_CHUNK.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(first.content, None); // role-only chunk: content is null
        assert!(!first.finished);

        let token = parse_chunk(TOKEN_CHUNK.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(token.content.as_deref(), Some("T"));
        assert_eq!(token.reasoning, None); // an answer chunk carries no thinking
        assert!(!token.finished);

        let reasoning = parse_chunk(REASONING_CHUNK.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(reasoning.reasoning.as_deref(), Some("Let me think"));
        assert_eq!(reasoning.content, None); // a thinking chunk carries no answer
        assert!(!reasoning.finished);

        let last = parse_chunk(FINAL_CHUNK.strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(last.content, None);
        assert!(last.finished);
        assert_eq!(last.timing_tokens, Some(12));
        assert_eq!(last.timing_tps, Some(98.5));

        assert!(parse_chunk("not json").is_none());
    }

    /// End-to-end over a fake stream (no reqwest/network involved at all) —
    /// exercises the same path `engine::chat_stream`/`cloud::chat_stream` do,
    /// including the token/reasoning/timings wiring, without a real server.
    #[tokio::test]
    async fn drain_sse_emits_events_and_final_stats_from_timings() {
        let body =
            format!("{TOKEN_CHUNK}\n\n{REASONING_CHUNK}\n\n{FINAL_CHUNK}\n\ndata: [DONE]\n\n");
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![Ok(body.into_bytes())];
        let stream = stream::iter(chunks);

        let mut events = Vec::new();
        drain_sse(
            stream,
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
            |e| e.to_string(),
        )
        .await
        .unwrap();

        assert!(matches!(&events[0], ChatEvent::Token { text } if text == "T"));
        assert!(matches!(&events[1], ChatEvent::Reasoning { text } if text == "Let me think"));
        match &events[2] {
            ChatEvent::Done { stats } => {
                assert_eq!(stats.total_tokens, 12);
                assert_eq!(stats.tokens_per_sec, 98.5);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn drain_sse_stops_on_cancel_without_reading_further() {
        let chunks: Vec<Result<Vec<u8>, std::io::Error>> = vec![
            Ok(format!("{TOKEN_CHUNK}\n\n").into_bytes()),
            Ok(format!("{TOKEN_CHUNK}\n\n").into_bytes()),
        ];
        let stream = stream::iter(chunks);
        let cancel = Arc::new(AtomicBool::new(true));

        let mut events = Vec::new();
        drain_sse(
            stream,
            cancel,
            |e| events.push(e),
            |e: &std::io::Error| e.to_string(),
        )
        .await
        .unwrap();

        // Cancel is checked before the first chunk is even processed.
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], ChatEvent::Done { .. }));
    }

    #[tokio::test]
    async fn drain_sse_surfaces_a_transport_error() {
        let chunks: Vec<Result<Vec<u8>, &str>> = vec![Err("boom")];
        let stream = stream::iter(chunks);

        let mut events = Vec::new();
        let result = drain_sse(
            stream,
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
            |e| format!("mapped: {e}"),
        )
        .await;

        assert_eq!(result, Err("mapped: boom".to_string()));
        assert!(matches!(&events[0], ChatEvent::Error { message } if message == "mapped: boom"));
    }
}
