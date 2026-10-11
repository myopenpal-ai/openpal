//! End-to-end engine test against the real llama-server binary and a tiny
//! real model. Ignored by default; run explicitly with:
//!
//! ```sh
//! cargo test -p openpal-core --test engine_e2e -- --ignored
//! ```

mod support;

use openpal_core::types::{ChatEvent, ChatMessage, EngineStatus, SamplingParams};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[tokio::test]
#[ignore]
async fn engine_end_to_end() {
    let (engine, _log_dir) = support::start_test_engine().await;
    assert!(
        matches!(engine.status().await, EngineStatus::Running { .. }),
        "engine should be running after start"
    );

    let messages = [ChatMessage {
        id: "msg-1".to_string(),
        role: "user".to_string(),
        content: "Reply with exactly: hello".to_string(),
        created_at_ms: None,
        model: None,
        citations: None,
        thinking: None,
        steps: None,
        attachments: None,
        artifacts: None,
        stats: None,
        display_content: None,
    }];
    let mut events = Vec::new();
    let sampling = SamplingParams {
        temperature: 0.0,
        ..Default::default()
    };
    engine
        .chat_stream(
            &messages,
            &sampling,
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
    assert!(matches!(engine.status().await, EngineStatus::Stopped));
}
