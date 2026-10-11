//! Chat with a signed-in account's cloud model instead of the local engine.
//!
//! Talks to the same backend as [`crate::account`] (`account::api_base()`) —
//! one account/billing/cloud-inference service, not three. Reuses
//! [`crate::engine::build_api_messages`] for attachment handling and
//! [`crate::sse::drain_sse`] for the response stream, so a cloud reply
//! streams through the exact same `ChatEvent`s a local one does; the only
//! real differences from `engine::chat_stream` are the URL, an account
//! bearer token in place of the engine's per-start key, and that every
//! request names a `"model"` (the backend serves many, unlike the engine's
//! one-model-per-process).
//!
//! Entitlement is enforced by the backend, not here — a `401`/`403` response
//! is just another transport-error case this module turns into a friendly
//! message. The client-side check that avoids a doomed round trip in the
//! common case lives in `commands::require_entitlement`; see
//! `crate::account`'s module docs for why that check is a UX guard, not a
//! security boundary.

use crate::account;
use crate::engine::build_api_messages;
use crate::types::{ChatEvent, ChatMessage, SamplingParams};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// A named [`SamplingParams`] field a cloud model's underlying provider may
/// or may not accept. Variant names mirror `SamplingParams`'s own field
/// names (and, via the shared `camelCase` casing, its JSON keys exactly) so
/// there's one spelling to keep in sync, not two. Real providers diverge
/// sharply here — OpenAI's API has no `top_k`/`min_p`/`repeat_penalty`/
/// `repeat_last_n` at all, Anthropic additionally drops `presence_penalty`/
/// `frequency_penalty`/`seed` and requires `max_tokens` rather than
/// tolerating llama.cpp's "-1 = unlimited" sentinel — while an
/// Ollama-style/llama.cpp-backed cloud model can honor the full set. Rather
/// than the client guessing or the backend silently dropping fields
/// (OpenRouter's approach) or hard-rejecting them (raw OpenAI/Anthropic),
/// [`CloudModelInfo::supported_params`] lets the backend declare per-model
/// support so the Generation panel can show only what will actually take
/// effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SamplingParamKey {
    Temperature,
    TopP,
    TopK,
    MinP,
    RepeatPenalty,
    RepeatLastN,
    PresencePenalty,
    FrequencyPenalty,
    MaxTokens,
    Seed,
}

/// A model offered by the cloud backend, shown in Library's "Cloud models"
/// group alongside local ones. Deliberately not folded into `ModelSpec`/
/// `ModelStatus` (`crate::types`) — those describe a downloaded file's disk
/// state and hardware fit, neither of which applies to a cloud model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CloudModelInfo {
    pub id: String,
    pub name: String,
    pub family: String,
    pub publisher: String,
    pub description: String,
    pub strengths: Vec<String>,
    /// Plan tier that unlocks this model, e.g. "Pro" — shown as a badge when
    /// the signed-in account (or no account) doesn't have it yet.
    pub required_tier: String,
    /// Which [`SamplingParams`] fields this model's provider actually honors
    /// — the Generation panel only shows a control for a field listed here.
    /// `#[serde(default)]` so a catalog response that predates this field (or
    /// simply hasn't classified a model yet) degrades to "declare nothing
    /// supported" rather than failing to parse — the same conservative
    /// default `require_entitlement` uses elsewhere in this module: never
    /// assume support the backend hasn't confirmed.
    #[serde(default)]
    pub supported_params: Vec<SamplingParamKey>,
}

/// The cloud model catalog. Public — no bearer token sent — so a signed-out
/// user still sees what they'd unlock by signing in; `require_entitlement`
/// gates actually chatting with one, not seeing the list.
pub async fn list_models(
    client: &reqwest::Client,
) -> std::result::Result<Vec<CloudModelInfo>, String> {
    let resp = client
        .get(format!("{}/v1/cloud-models", account::api_base()))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("listing cloud models failed: {e}");
            "Couldn't reach the cloud model list. Check your connection and try again.".to_string()
        })?;
    resp.error_for_status()
        .map_err(|e| {
            tracing::warn!("listing cloud models failed: {e}");
            "Couldn't reach the cloud model list. Check your connection and try again.".to_string()
        })?
        .json()
        .await
        .map_err(|e| {
            tracing::warn!("cloud model list response was malformed: {e}");
            "Something went wrong loading cloud models. Please try again.".to_string()
        })
}

/// Stream one assistant reply from the cloud backend. Mirrors
/// `EngineManager::chat_stream`'s contract (same `ChatEvent`s, same
/// cancel-drops-the-connection behavior — see `crate::sse::drain_sse`) but
/// authenticates with an account bearer token and names the model explicitly.
pub async fn chat_stream(
    client: &reqwest::Client,
    access_token: &str,
    model_id: &str,
    messages: &[ChatMessage],
    sampling: &SamplingParams,
    cancel: Arc<AtomicBool>,
    mut on_event: impl FnMut(ChatEvent) + Send,
) -> Result<()> {
    let mut body = serde_json::json!({
        "model": model_id,
        "messages": build_api_messages(messages),
        "stream": true,
    });
    if let serde_json::Value::Object(map) = &mut body {
        sampling.merge_into(map);
    }
    let response = client
        .post(format!("{}/v1/chat/completions", account::api_base()))
        .bearer_auth(access_token)
        .json(&body)
        .send()
        .await;
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("cloud chat request failed: {e}");
            let message = "Couldn't reach your cloud model. Check your connection and try again.";
            on_event(ChatEvent::Error {
                message: message.to_string(),
            });
            return Err(Error::Cloud(message.to_string()));
        }
    };
    if !response.status().is_success() {
        let status = response.status();
        tracing::error!("cloud chat request rejected: {status}");
        let message = if status == reqwest::StatusCode::UNAUTHORIZED {
            "Your sign-in expired. Please sign in again."
        } else if status == reqwest::StatusCode::FORBIDDEN {
            "Your plan doesn't include this model."
        } else {
            "Your cloud model couldn't process this message. Please try again."
        };
        on_event(ChatEvent::Error {
            message: message.to_string(),
        });
        return Err(Error::Cloud(message.to_string()));
    }

    crate::sse::drain_sse(response.bytes_stream(), cancel, on_event, |e| {
        tracing::error!("cloud chat stream error: {e}");
        "The reply was cut off. Please try sending your message again.".to_string()
    })
    .await
    .map_err(Error::Cloud)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

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

    fn cloud_model(supported_params: Vec<SamplingParamKey>) -> CloudModelInfo {
        CloudModelInfo {
            id: "gpt-test".to_string(),
            name: "Test GPT".to_string(),
            family: "gpt".to_string(),
            publisher: "OpenAI".to_string(),
            description: "".to_string(),
            strengths: vec![],
            required_tier: "Pro".to_string(),
            supported_params,
        }
    }

    /// `SamplingParamKey` must serialize `camelCase`, matching both this
    /// struct's own field casing and `SamplingParams`'s field names exactly
    /// — the frontend filters the Generation panel by comparing these
    /// strings against `SamplingParams`'s own (also `camelCase`) keys.
    #[test]
    fn supported_params_serializes_camelcase_matching_sampling_params_keys() {
        let model = cloud_model(vec![
            SamplingParamKey::TopP,
            SamplingParamKey::RepeatLastN,
            SamplingParamKey::MaxTokens,
        ]);
        let json = serde_json::to_value(&model).unwrap();
        assert_eq!(
            json["supportedParams"],
            serde_json::json!(["topP", "repeatLastN", "maxTokens"]),
        );
    }

    /// A catalog response written before `supportedParams` existed (or a
    /// model the backend hasn't classified yet) must still parse, defaulting
    /// to "nothing declared supported" — never a guess that could send a
    /// field the provider rejects.
    #[test]
    fn cloud_model_info_deserializes_missing_supported_params_as_empty() {
        let json = r#"{
            "id": "m1", "name": "Model", "family": "gpt", "publisher": "OpenAI",
            "description": "", "strengths": [], "requiredTier": "Pro"
        }"#;
        let model: CloudModelInfo =
            serde_json::from_str(json).expect("old catalog entries must still parse");
        assert_eq!(model.supported_params, Vec::new());
    }

    /// Both cloud calls must fail with a friendly message (not a panic, not a
    /// raw reqwest error) when the backend is unreachable — `chat_stream`
    /// additionally emits a matching `ChatEvent::Error` before returning.
    /// Points `OPENPAL_ACCOUNT_API` at a closed local port for an immediate,
    /// deterministic connection-refused rather than a real network call.
    #[tokio::test]
    async fn unreachable_backend_yields_friendly_errors() {
        // SAFETY-in-spirit note: cargo test runs crate tests in one process;
        // no other test in this crate reads this env var, so this is safe
        // despite tests running in parallel by default.
        std::env::set_var("OPENPAL_ACCOUNT_API", "http://127.0.0.1:1");

        let client = reqwest::Client::new();

        let list_err = list_models(&client)
            .await
            .expect_err("closed port must fail");
        assert!(!list_err.is_empty());

        let mut events = Vec::new();
        let result = chat_stream(
            &client,
            "token",
            "some-model",
            &[user_msg("hi")],
            &SamplingParams::default(),
            Arc::new(AtomicBool::new(false)),
            |e| events.push(e),
        )
        .await;

        assert!(matches!(result, Err(Error::Cloud(_))));
        assert!(matches!(&events[0], ChatEvent::Error { .. }));

        std::env::remove_var("OPENPAL_ACCOUNT_API");
    }
}
