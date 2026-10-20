//! End-to-end test proving the Writing Tools request shape (a fixed system
//! prompt from `writing_tools` + `writing_tools::fixed_sampling`) actually
//! streams a real reply from the engine. Ignored by default, same reasons
//! and invocation as `engine_e2e.rs`:
//!
//! ```sh
//! cargo test -p openpal-core --test writing_tools_e2e -- --ignored
//! ```
//!
//! This does **not** assert on translation quality — SmolLM2 135M is a tiny
//! model used only for automated tests, not expected to translate well —
//! only that the request is accepted and streams to completion. That closes
//! the coverage gap the mocked-e2e frontend tier explicitly can't reach:
//! `ui/tests/tauriMock.ts` doesn't implement the `Channel`/
//! `transformCallback` protocol `translate_stream`/`correct_stream` (and
//! ordinary `chat_stream`) use for streaming.

mod support;

use openpal_core::types::{ChatEvent, ChatMessage};
use openpal_core::writing_tools;
use std::sync::atomic::AtomicBool;
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
        injection_notices: None,
        withheld_from_model: None,
    }
}

#[tokio::test]
#[ignore]
async fn translate_request_streams_tokens_and_terminates() {
    let (engine, _log_dir) = support::start_test_engine().await;

    let system = writing_tools::translate_system_prompt(Some("en"), "fr");
    let messages = [
        message("system", system),
        message("user", "hello".to_string()),
    ];

    let mut events = Vec::new();
    engine
        .chat_stream(
            &messages,
            &writing_tools::fixed_sampling(),
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

    engine.stop().await.expect("engine stop");
}

#[tokio::test]
#[ignore]
async fn correct_request_streams_tokens_and_terminates() {
    let (engine, _log_dir) = support::start_test_engine().await;

    let messages = [
        message("system", writing_tools::CORRECT_SYSTEM_PROMPT.to_string()),
        message("user", "this dont work good".to_string()),
    ];

    let mut events = Vec::new();
    engine
        .chat_stream(
            &messages,
            &writing_tools::fixed_sampling(),
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

    engine.stop().await.expect("engine stop");
}
