//! `ask_user` — lets the model pause a turn to ask the person a short
//! clarifying question instead of guessing, when a request is genuinely
//! ambiguous or missing something it needs to proceed.
//!
//! Unlike every other built-in tool, this one's "execution" *is* an
//! interactive round-trip with the user — there's nothing to sandbox or
//! jail, so it skips the consent gate entirely (see `requires_consent`) and
//! instead blocks on its own pending answer, registered on the same
//! `ConsentBroker` `dispatch_call` uses for consent (see that struct's own
//! doc comment for why it lives there rather than in a second broker type).

use super::truncate_for_summary;
use crate::agent::orchestrator::wait_for_cancel;
use crate::agent::tool::{ConsentSummary, Tool, ToolContext, ToolError, ToolOutput};
use crate::types::ChatEvent;
use serde_json::Value;

/// The model may suggest at most this many quick-reply options — enough to
/// cover a real short list (e.g. "this file or that one") without turning
/// into a multi-select form; anything larger belongs in the free-text answer
/// every question also accepts.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;

pub struct AskUserTool;

#[async_trait::async_trait]
impl Tool for AskUserTool {
    fn id(&self) -> &str {
        "ask_user"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "ask_user",
            "description": "Ask the user one short clarifying question when you genuinely can't proceed without more information — not for details you could reasonably assume or infer. Use sparingly. Optionally suggest 2-4 short reply options shown as quick picks; the user can always type something else instead.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask, as one short sentence."
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 2,
                        "maxItems": 4,
                        "description": "Optional short suggested replies (a few words each), shown as quick-pick buttons. Omit if there's no natural small set of choices."
                    }
                },
                "required": ["question"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let (question, _options) = parse_args(args)?;
        Ok(ConsentSummary {
            summary: format!("Asked: {}", truncate_for_summary(&question)),
            detail: question,
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        // Never actually consulted — see `requires_consent` below — but every
        // tool must return something for `dispatch_call`'s audit log entry.
        args.to_string()
    }

    // Asking a question has no blast radius at all: nothing is read, written,
    // or sent anywhere. Same posture as `WebSearchTool`.
    fn requires_consent(&self, _args: &Value, _ctx: &ToolContext) -> bool {
        false
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (question, options) = parse_args(args)?;

        // Register before emitting the event, so an answer can never arrive
        // before there's something to resolve it against — same ordering
        // `ConsentBroker::register`'s own doc comment requires.
        let rx = ctx.consent.register_clarification(&ctx.call_id).await;
        (ctx.on_event)(ChatEvent::ClarifyingQuestionAsked {
            request_id: ctx.call_id.clone(),
            tool_call_id: ctx.call_id.clone(),
            question: question.clone(),
            options: options.clone(),
        });

        let answer = tokio::select! {
            biased;
            _ = wait_for_cancel(&ctx.cancel) => "The user stopped this turn.".to_string(),
            answer = rx => match answer {
                Ok(answer) => {
                    // A typed answer is the user's own text, so `fetch_url`
                    // may open a URL in it. Picking one of the model's own
                    // options adds nothing the model didn't write itself.
                    if !options.iter().any(|option| option == answer.trim()) {
                        ctx.fetch_allowlist.allow_text(&answer);
                    }
                    answer
                }
                Err(_) => "The prompt was closed without an answer.".to_string(),
            },
        };

        Ok(ToolOutput {
            ok: true,
            preview: format!("Answered: {}", truncate_for_summary(&answer)),
            content: answer,
        })
    }
}

/// Validate `args` and pull out `(question, options)`. Shared by
/// `consent_summary` and `execute`, mirroring every other built-in tool's
/// re-parse-in-both-places convention.
fn parse_args(args: &Value) -> Result<(String, Vec<String>), ToolError> {
    let question = args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::Rejected("A question needs a \"question\".".to_string()))?
        .to_string();

    let options = match args.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let options: Vec<String> = items
                .iter()
                .map(|v| v.as_str().map(str::trim).map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .filter(|opts| opts.iter().all(|o| !o.is_empty()))
                .ok_or_else(|| {
                    ToolError::Rejected("\"options\" must be non-empty strings.".to_string())
                })?;
            if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
                return Err(ToolError::Rejected(format!(
                    "\"options\" must have between {MIN_OPTIONS} and {MAX_OPTIONS} entries."
                )));
            }
            options
        }
        Some(_) => {
            return Err(ToolError::Rejected(
                "\"options\" must be an array.".to_string(),
            ))
        }
    };

    Ok((question, options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::consent::ConsentBroker;
    use crate::agent::workspace::WorkspaceJail;
    use std::sync::{atomic::AtomicBool, Arc};

    fn ctx(tmp: &std::path::Path) -> ToolContext {
        ToolContext {
            turn_id: "t".into(),
            call_id: "c1".into(),
            workspace: Arc::new(
                WorkspaceJail::new(
                    tmp,
                    std::path::Path::new("/nonexistent-home"),
                    std::path::Path::new("/nonexistent-data"),
                )
                .unwrap(),
            ),
            scratch_dir: tmp.join(".scratch"),
            cancel: Arc::new(AtomicBool::new(false)),
            on_event: Arc::new(|_| {}),
            citations: Arc::new(crate::agent::CitationRegistry::new()),
            fetch_allowlist: Arc::new(crate::agent::FetchAllowlist::new()),
            consent: Arc::new(ConsentBroker::new(tmp.join("grants.json"))),
        }
    }

    #[test]
    fn rejects_a_missing_question() {
        assert!(parse_args(&serde_json::json!({})).is_err());
    }

    #[test]
    fn rejects_a_blank_question() {
        assert!(parse_args(&serde_json::json!({"question": "   "})).is_err());
    }

    #[test]
    fn rejects_too_few_options() {
        assert!(parse_args(&serde_json::json!({"question": "Which?", "options": ["a"]})).is_err());
    }

    #[test]
    fn rejects_too_many_options() {
        let args = serde_json::json!({"question": "Which?", "options": ["a", "b", "c", "d", "e"]});
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn rejects_a_blank_option() {
        let args = serde_json::json!({"question": "Which?", "options": ["a", "  "]});
        assert!(parse_args(&args).is_err());
    }

    #[test]
    fn accepts_a_bare_question_with_no_options() {
        let (question, options) =
            parse_args(&serde_json::json!({"question": "Which city?"})).unwrap();
        assert_eq!(question, "Which city?");
        assert!(options.is_empty());
    }

    #[test]
    fn accepts_a_question_with_valid_options() {
        let args = serde_json::json!({"question": "Which?", "options": ["Paris", "London"]});
        let (question, options) = parse_args(&args).unwrap();
        assert_eq!(question, "Which?");
        assert_eq!(options, vec!["Paris".to_string(), "London".to_string()]);
    }

    #[test]
    fn does_not_require_consent() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(tmp.path());
        assert!(!AskUserTool.requires_consent(&serde_json::json!({"question": "Which?"}), &ctx));
    }

    #[tokio::test]
    async fn execute_returns_the_answer_delivered_via_the_broker() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(tmp.path());
        let consent = ctx.consent.clone();
        let call = tokio::spawn(async move {
            AskUserTool
                .execute(&serde_json::json!({"question": "Which city?"}), &ctx)
                .await
        });
        // Give `execute` a moment to register before resolving.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            consent
                .resolve_clarification("c1", "Paris".to_string())
                .await
        );
        let output = call.await.unwrap().unwrap();
        assert!(output.ok);
        assert_eq!(output.content, "Paris");
    }

    /// Ask `options`, answer with `answer`, and return the call's context
    /// so its fetch allowlist can be inspected.
    async fn answered(options: &[&str], answer: &str) -> ToolContext {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Arc::new(ctx(tmp.path()));
        let consent = ctx.consent.clone();
        let args = serde_json::json!({"question": "Which page?", "options": options});
        let call = {
            let ctx = ctx.clone();
            tokio::spawn(async move { AskUserTool.execute(&args, &ctx).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            consent
                .resolve_clarification("c1", answer.to_string())
                .await
        );
        call.await.unwrap().unwrap();
        Arc::into_inner(ctx).unwrap()
    }

    #[tokio::test]
    async fn a_typed_answer_makes_its_url_fetchable() {
        let ctx = answered(
            &["https://example.com/a", "https://example.com/b"],
            "use https://example.com/mine",
        )
        .await;
        assert!(ctx.fetch_allowlist.is_allowed("https://example.com/mine"));
    }

    #[tokio::test]
    async fn picking_one_of_the_models_options_does_not() {
        let ctx = answered(
            &["https://example.com/a", "https://example.com/b"],
            "https://example.com/a",
        )
        .await;
        assert!(!ctx.fetch_allowlist.is_allowed("https://example.com/a"));
    }

    #[tokio::test]
    async fn execute_stops_promptly_when_the_turn_is_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ctx(tmp.path());
        let cancel = Arc::new(AtomicBool::new(false));
        ctx.cancel = cancel.clone();
        let call = tokio::spawn(async move {
            AskUserTool
                .execute(&serde_json::json!({"question": "Which city?"}), &ctx)
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let output = call.await.unwrap().unwrap();
        assert_eq!(output.content, "The user stopped this turn.");
    }
}
