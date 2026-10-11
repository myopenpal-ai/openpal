//! `create_artifact` — lets the model produce a standalone file (report,
//! script, HTML page, data file, …) that the user can view/download from the
//! chat as its own item, separate from the model's reply text. Storage is
//! `crate::artifacts` (a row per artifact, in the app-wide database); this
//! module is only the `Tool` wiring — consent card, grant key, and the
//! `ChatEvent::ArtifactCreated` emission the frontend uses to show the file
//! as a downloadable chip.

use super::{hash_grant_key, truncate_for_summary};
use crate::agent::tool::{ConsentSummary, Tool, ToolContext, ToolError, ToolOutput};
use crate::types::ChatEvent;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

pub struct CreateArtifactTool {
    conn: Arc<Mutex<Connection>>,
    conversation_id: String,
}

impl CreateArtifactTool {
    pub fn new(conn: Arc<Mutex<Connection>>, conversation_id: String) -> Self {
        Self {
            conn,
            conversation_id,
        }
    }
}

fn extract_args(args: &Value) -> Result<(&str, &str), ToolError> {
    let filename = args
        .get("filename")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Rejected("Missing \"filename\".".to_string()))?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::Rejected("Missing \"content\".".to_string()))?;
    Ok((filename, content))
}

#[async_trait::async_trait]
impl Tool for CreateArtifactTool {
    fn id(&self) -> &str {
        "create_artifact"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "create_artifact",
            "description": "Create a standalone file (a document, report, script, HTML page, data file, etc.) that the user can view and download from the chat as its own item, separate from your reply text. Use this whenever the user asks you to write, draft, generate, or produce a file — don't paste the full content inline in your answer when this tool fits; just create the artifact and briefly refer to it by name in your reply.",
            "parameters": {
                "type": "object",
                "properties": {
                    "filename": {
                        "type": "string",
                        "description": "A short descriptive file name with an extension, e.g. \"weekly-report.md\" or \"landing-page.html\"."
                    },
                    "content": {
                        "type": "string",
                        "description": "The full file content."
                    }
                },
                "required": ["filename", "content"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let (filename, content) = extract_args(args)?;
        Ok(ConsentSummary {
            summary: format!("Create \"{filename}\" ({} bytes)", content.len()),
            detail: format!("{filename}\n\n---\n{}", truncate_for_summary(content)),
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        let filename = args.get("filename").and_then(Value::as_str).unwrap_or("");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        // Content-hashed for the same reason `write_file`'s grant key is: a
        // differently-worded artifact to the same filename under a standing
        // "always allow" grant should not silently re-execute with different
        // content.
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let content_hash = format!("{:x}", hasher.finalize());
        hash_grant_key(
            "create_artifact",
            &[&self.conversation_id, filename, &content_hash],
        )
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (filename, content) = extract_args(args)?;
        // NOT wired through `sync::capture::save_artifact_synced` (Phase 3
        // multi-device sync) — deliberately, not an oversight. This tool is
        // constructed in `crates/app/src/agent_bridge.rs::build_deps` from
        // individually-extracted fields (`conn`/`conversation_id`), not a
        // reference to `AppState`, so it has no access to `sync_enabled`,
        // `state.sync_clock`, or the sync key/mailbox id `capture`'s
        // functions need. Threading those through would mean widening this
        // tool's constructor (and `build_deps`'s already-`#[allow(clippy::
        // too_many_arguments)]` signature) for the sake of one call site,
        // which felt like the wrong trade against just documenting the gap
        // here. Net effect: an artifact the model creates via this tool
        // isn't currently pushed to other paired devices (it still saves
        // locally exactly as before) until this is revisited — a real but
        // narrow gap, since `download_artifact`/`get_artifact` still work
        // from local storage on the device that created it either way.
        let artifact = crate::artifacts::save(
            &self.conn.lock().unwrap(),
            &self.conversation_id,
            &ctx.call_id,
            filename,
            content,
        )
        .map_err(|e| ToolError::ExecutionFailed(format!("Couldn't create the artifact: {e}")))?;

        (ctx.on_event)(ChatEvent::ArtifactCreated {
            id: artifact.id.clone(),
            filename: artifact.filename.clone(),
            size_bytes: artifact.size_bytes,
        });

        Ok(ToolOutput {
            ok: true,
            preview: format!(
                "Created \"{}\" ({} bytes)",
                artifact.filename, artifact.size_bytes
            ),
            content: format!(
                "Created \"{}\" ({} bytes). It now appears in the chat as a downloadable file \
                 for the user — don't repeat its full contents in your reply, just refer to it \
                 by name.",
                artifact.filename, artifact.size_bytes
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::workspace::WorkspaceJail;
    use std::sync::{atomic::AtomicBool, Arc, Mutex};

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Arc<Mutex<Connection>> {
        let dir = tempfile::tempdir().unwrap().keep();
        Arc::new(Mutex::new(
            crate::app_db::open(&dir.join("app.sqlite"), None)
                .unwrap()
                .0,
        ))
    }

    fn ctx(artifacts_dir: &std::path::Path, call_id: &str) -> ToolContext {
        ToolContext {
            turn_id: "t".into(),
            call_id: call_id.into(),
            // Not used by `CreateArtifactTool` at all — a dummy jail matching
            // `fs.rs`'s test fixture pattern.
            workspace: Arc::new(
                WorkspaceJail::new(
                    artifacts_dir,
                    std::path::Path::new("/nonexistent-home"),
                    std::path::Path::new("/nonexistent-data"),
                )
                .unwrap(),
            ),
            scratch_dir: artifacts_dir.join(".scratch"),
            cancel: Arc::new(AtomicBool::new(false)),
            on_event: Arc::new(|_| {}),
            citations: Arc::new(crate::agent::CitationRegistry::new()),
            consent: Arc::new(crate::agent::ConsentBroker::new(
                artifacts_dir.join("grants.json"),
            )),
        }
    }

    #[tokio::test]
    async fn execute_writes_a_retrievable_artifact_and_emits_event() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = test_conn();
        let tool = CreateArtifactTool::new(conn.clone(), "conv-1".to_string());

        let events: Arc<Mutex<Vec<ChatEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let mut c = ctx(tmp.path(), "call-1");
        c.on_event = Arc::new(move |event| events_clone.lock().unwrap().push(event));

        let args = serde_json::json!({ "filename": "report.md", "content": "# Hello" });
        let out = tool.execute(&args, &c).await.unwrap();
        assert!(out.ok);
        assert!(out.preview.contains("report.md"));

        let saved = crate::artifacts::get(&conn.lock().unwrap(), "conv-1", "call-1").unwrap();
        assert_eq!(saved.filename, "report.md");
        assert_eq!(saved.content, "# Hello");

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ChatEvent::ArtifactCreated { filename, .. } => assert_eq!(filename, "report.md"),
            other => panic!("expected ArtifactCreated, got {other:?}"),
        }
    }

    #[test]
    fn grant_key_changes_when_content_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = CreateArtifactTool::new(test_conn(), "conv-1".to_string());
        let c = ctx(tmp.path(), "call-1");
        let a = serde_json::json!({ "filename": "f.txt", "content": "version A" });
        let b = serde_json::json!({ "filename": "f.txt", "content": "version B" });
        assert_ne!(tool.grant_key(&a, &c), tool.grant_key(&b, &c));
    }

    #[test]
    fn grant_key_is_stable_for_identical_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = CreateArtifactTool::new(test_conn(), "conv-1".to_string());
        let c = ctx(tmp.path(), "call-1");
        let a = serde_json::json!({ "filename": "f.txt", "content": "same" });
        assert_eq!(tool.grant_key(&a, &c), tool.grant_key(&a, &c));
    }

    #[test]
    fn missing_filename_or_content_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = CreateArtifactTool::new(test_conn(), "conv-1".to_string());
        let c = ctx(tmp.path(), "call-1");

        let missing_filename = serde_json::json!({ "content": "x" });
        assert!(matches!(
            tool.consent_summary(&missing_filename, &c),
            Err(ToolError::Rejected(_))
        ));

        let missing_content = serde_json::json!({ "filename": "f.txt" });
        assert!(matches!(
            tool.consent_summary(&missing_content, &c),
            Err(ToolError::Rejected(_))
        ));
    }
}
