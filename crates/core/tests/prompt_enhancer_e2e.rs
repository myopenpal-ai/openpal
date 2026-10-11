//! End-to-end test proving the Message improver's request shape (the fixed
//! `prompt_enhancer::SYSTEM_PROMPT` + `prompt_enhancer::fixed_sampling`)
//! actually streams a real reply from the engine, and that cancelling
//! mid-stream ends in `Done`, never `Error`. Ignored by default, same
//! reasons and invocation as `engine_e2e.rs`/`writing_tools_e2e.rs`:
//!
//! ```sh
//! cargo test -p openpal-core --test prompt_enhancer_e2e -- --ignored
//! ```
//!
//! Structural only — SmolLM2 135M is a tiny model used only for automated
//! tests, not expected to produce a meaningfully improved rewrite. This
//! closes the same coverage gap `writing_tools_e2e.rs` does: the mocked-e2e
//! frontend tier can't exercise a real `Channel`/streaming round trip.

mod support;

use openpal_core::prompt_enhancer;
use openpal_core::types::{ChatEvent, ChatMessage};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn message(role: &str, content: String) -> ChatMessage {
    ChatMessage {
        id: format!("id-{content}"),
        role: role.to_string(),
        content,
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

#[tokio::test]
#[ignore]
async fn enhance_request_streams_tokens_and_terminates() {
    let (engine, _log_dir) = support::start_test_engine().await;

    let draft = "fix my resume";
    let messages = [
        message("system", prompt_enhancer::SYSTEM_PROMPT.to_string()),
        message("user", prompt_enhancer::user_message_text(draft)),
    ];

    let mut events = Vec::new();
    engine
        .chat_stream(
            &messages,
            &prompt_enhancer::fixed_sampling(draft, false),
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
        )
        .await
        .expect("chat stream");

    assert!(
        events.iter().any(|e| matches!(e, ChatEvent::Token { .. })),
        "expected at least one Token event, got: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, ChatEvent::Done { .. })),
        "expected a Done event, got: {events:?}"
    );

    let answer: String = events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !answer.is_empty(),
        "expected non-empty concatenated token text"
    );

    let reasoning_seen = events
        .iter()
        .any(|e| matches!(e, ChatEvent::Reasoning { .. }));
    // Must not panic, and must resolve to one of the two outcome shapes.
    match prompt_enhancer::clean_enhanced(&answer, draft, reasoning_seen) {
        prompt_enhancer::EnhanceOutcome::Enhanced { .. } => {}
        prompt_enhancer::EnhanceOutcome::Kept { .. } => {}
    }

    engine.stop().await.expect("engine stop");
}

#[tokio::test]
#[ignore]
async fn enhance_request_cancel_mid_stream_ends_with_done_not_error() {
    let (engine, _log_dir) = support::start_test_engine().await;

    let draft = "please write a longer message about my weekend plans for the upcoming holiday";
    let messages = [
        message("system", prompt_enhancer::SYSTEM_PROMPT.to_string()),
        message("user", prompt_enhancer::user_message_text(draft)),
    ];

    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_writer = Arc::clone(&cancel);
    let mut events = Vec::new();
    let mut seen_first_token = false;

    engine
        .chat_stream(
            &messages,
            &prompt_enhancer::fixed_sampling(draft, false),
            Arc::clone(&cancel),
            |e| {
                if !seen_first_token && matches!(e, ChatEvent::Token { .. }) {
                    seen_first_token = true;
                    cancel_writer.store(true, Ordering::SeqCst);
                }
                events.push(e);
            },
        )
        .await
        .expect("chat stream");

    assert!(
        !events.iter().any(|e| matches!(e, ChatEvent::Error { .. })),
        "cancelling must never surface as an Error event, got: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, ChatEvent::Done { .. })),
        "expected a Done event even when cancelled, got: {events:?}"
    );

    engine.stop().await.expect("engine stop");
}
