//! `list_project_conversations` / `read_project_conversation` — let an agent
//! turn look at sibling conversations the user has explicitly grouped into
//! the same project (see `crate::projects`), the one deliberate exception to
//! every other tool's per-conversation workspace jailing
//! (`agent::workspace::WorkspaceJail` never lets `read_file`/`write_file`
//! cross into another conversation's folder). Narrow like `spawn_subagent`'s
//! fixed kind menu rather than a general "read any conversation" tool: both
//! tools only ever resolve a conversation whose `project_id` matches the
//! *current* conversation's own, checked fresh on every call (not cached at
//! registration) since project membership can change mid-session.
//!
//! Only registered at all when the current conversation is actually in a
//! project — see `agent_bridge::build_deps`.

use super::super::tool::{cap_output, ConsentSummary, Tool, ToolContext, ToolError, ToolOutput};
use crate::types::Conversation;
use rusqlite::Connection;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// How many message-chunks a `read_project_conversation` call keeps —
/// mirrors `websearch::RERANK_TOP_K`'s role for a fetched page, just with a
/// larger budget since a sibling conversation is the whole point of the call
/// (not a fallback excerpt of a page fetched for another reason).
const MAX_CONSULT_CHUNKS: usize = 30;

/// Load `id` from the app-wide database and check it's actually a sibling in
/// `project_id` — the shared validation both tools in this file run *before*
/// any consent prompt, so a call for a conversation outside this project (or
/// that doesn't exist) is rejected outright rather than surfaced as
/// something to approve or deny. Locks `conn` only for the duration of this
/// one call, same discipline `AppState.app_db`'s own doc comment requires of
/// any tool holding a clone of it across a turn.
fn resolve_sibling(
    conn: &Mutex<Connection>,
    project_id: &str,
    id: &str,
) -> Result<Conversation, ToolError> {
    let conversation = crate::store::get(&conn.lock().unwrap(), id)
        .map_err(|_| ToolError::Rejected(format!("No conversation with id \"{id}\".")))?;
    if conversation.project_id.as_deref() != Some(project_id) {
        return Err(ToolError::Rejected(
            "That conversation isn't in this project.".to_string(),
        ));
    }
    Ok(conversation)
}

/// Every other conversation in the same project as `current_conversation_id`,
/// as a browsable list — the discovery step before `read_project_conversation`.
pub struct ListProjectConversationsTool {
    conn: Arc<Mutex<Connection>>,
    project_id: String,
    current_conversation_id: String,
}

impl ListProjectConversationsTool {
    pub fn new(
        conn: Arc<Mutex<Connection>>,
        project_id: String,
        current_conversation_id: String,
    ) -> Self {
        Self {
            conn,
            project_id,
            current_conversation_id,
        }
    }
}

#[async_trait::async_trait]
impl Tool for ListProjectConversationsTool {
    fn id(&self) -> &str {
        "list_project_conversations"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "list_project_conversations",
            "description": "List the other conversations in this same project (a group of conversations the user organized together). Use read_project_conversation on one of the returned ids to actually read it.",
            "parameters": {
                "type": "object",
                "properties": {},
            }
        })
    }

    fn consent_summary(
        &self,
        _args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        Ok(ConsentSummary {
            summary: "List this project's other conversations".to_string(),
            detail: "Titles and message counts only, not their content.".to_string(),
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, _args: &Value, _ctx: &ToolContext) -> String {
        format!("list_project_conversations:{}", self.project_id)
    }

    async fn execute(&self, _args: &Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let siblings: Vec<_> = crate::store::list(&self.conn.lock().unwrap())
            .into_iter()
            .filter(|m| {
                m.project_id.as_deref() == Some(self.project_id.as_str())
                    && m.id != self.current_conversation_id
            })
            .collect();

        if siblings.is_empty() {
            return Ok(ToolOutput {
                ok: true,
                preview: "No other conversations in this project".to_string(),
                content: "No other conversations in this project.".to_string(),
            });
        }

        let lines: Vec<String> = siblings
            .iter()
            .map(|m| format!("- {}: \"{}\" ({} messages)", m.id, m.title, m.message_count))
            .collect();
        Ok(ToolOutput {
            ok: true,
            preview: format!("{} other conversation(s) in this project", siblings.len()),
            content: lines.join("\n"),
        })
    }
}

/// Read one sibling conversation from the same project, optionally ranked
/// against `query` (via `rerank::top_chunks`, the same BM25 scoring
/// `fetch_url` uses for a long page) when it's too long to hand over whole.
pub struct ReadProjectConversationTool {
    conn: Arc<Mutex<Connection>>,
    project_id: String,
}

impl ReadProjectConversationTool {
    pub fn new(conn: Arc<Mutex<Connection>>, project_id: String) -> Self {
        Self { conn, project_id }
    }
}

/// Each non-system message as its own "role: content" chunk — `top_chunks`'s
/// unit of ranking, so a match lands on the specific exchange that mentioned
/// it rather than the conversation as an undifferentiated blob.
fn transcript_chunks(conversation: &Conversation) -> Vec<String> {
    conversation
        .messages
        .iter()
        .filter(|m| m.role != "system")
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect()
}

#[async_trait::async_trait]
impl Tool for ReadProjectConversationTool {
    fn id(&self) -> &str {
        "read_project_conversation"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "read_project_conversation",
            "description": "Read a sibling conversation from this same project (see list_project_conversations for ids). For a long conversation, pass \"query\" (what you're looking for) to get back only the most relevant excerpts instead of the whole thing.",
            "parameters": {
                "type": "object",
                "properties": {
                    "conversation_id": {
                        "type": "string",
                        "description": "A conversation id from list_project_conversations."
                    },
                    "query": {
                        "type": "string",
                        "description": "What you're looking for, to rank excerpts by relevance. Omit to read from the start."
                    }
                },
                "required": ["conversation_id"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let id = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"conversation_id\".".to_string()))?;
        let conversation = resolve_sibling(&self.conn, &self.project_id, id)?;
        Ok(ConsentSummary {
            summary: format!("Read \"{}\" from this project", conversation.title),
            detail: format!(
                "{} messages. This conversation's content will be shown to the model.",
                conversation.messages.len()
            ),
            resolved_paths: vec![],
            danger_flags: vec![],
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        let id = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        format!("read_project_conversation:{id}")
    }

    async fn execute(&self, args: &Value, _ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = args
            .get("conversation_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"conversation_id\".".to_string()))?;
        let conversation = resolve_sibling(&self.conn, &self.project_id, id)?;
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");

        let chunks = transcript_chunks(&conversation);
        if chunks.is_empty() {
            return Ok(ToolOutput {
                ok: true,
                preview: format!("\"{}\" has no messages yet", conversation.title),
                content: format!("\"{}\" has no messages yet.", conversation.title),
            });
        }
        let picked = super::rerank::top_chunks(query, chunks, MAX_CONSULT_CHUNKS);
        let content = cap_output(picked.join("\n\n"));
        Ok(ToolOutput {
            ok: true,
            preview: format!(
                "Read \"{}\" ({} messages)",
                conversation.title,
                conversation.messages.len()
            ),
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::workspace::WorkspaceJail;
    use crate::types::{ChatMessage, SamplingParams, TaskType};
    use std::sync::{atomic::AtomicBool, Arc};

    fn ctx(dir: &std::path::Path) -> ToolContext {
        ToolContext {
            turn_id: "t".into(),
            call_id: "c".into(),
            workspace: Arc::new(
                WorkspaceJail::new(
                    dir,
                    std::path::Path::new("/nonexistent-home"),
                    std::path::Path::new("/nonexistent-data"),
                )
                .unwrap(),
            ),
            scratch_dir: dir.join(".scratch"),
            cancel: Arc::new(AtomicBool::new(false)),
            on_event: Arc::new(|_| {}),
            citations: Arc::new(crate::agent::CitationRegistry::new()),
            fetch_allowlist: Arc::new(crate::agent::FetchAllowlist::new()),
            consent: Arc::new(crate::agent::ConsentBroker::new(dir.join("grants.json"))),
        }
    }

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

    fn conversation(
        id: &str,
        title: &str,
        project_id: Option<&str>,
        messages: Vec<&str>,
    ) -> Conversation {
        Conversation {
            id: id.to_string(),
            title: title.to_string(),
            created_at_ms: 1,
            updated_at_ms: 1,
            messages: messages
                .into_iter()
                .map(|content| ChatMessage {
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
                })
                .collect(),
            sampling: SamplingParams::default(),
            context_length: 8192,
            task_type: TaskType::General,
            project_id: project_id.map(|s| s.to_string()),
            pinned: false,
            branches: std::collections::HashMap::new(),
            forked_from: None,
        }
    }

    #[tokio::test]
    async fn list_excludes_the_current_conversation_and_other_projects() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        crate::store::save(
            &conn.lock().unwrap(),
            &conversation("self", "Self", Some("p1"), vec!["hi"]),
        )
        .unwrap();
        crate::store::save(
            &conn.lock().unwrap(),
            &conversation("sibling", "Sibling", Some("p1"), vec!["hi"]),
        )
        .unwrap();
        crate::store::save(
            &conn.lock().unwrap(),
            &conversation("other-project", "Other", Some("p2"), vec!["hi"]),
        )
        .unwrap();

        let tool = ListProjectConversationsTool::new(conn, "p1".to_string(), "self".to_string());
        let out = tool
            .execute(&serde_json::json!({}), &ctx(dir.path()))
            .await
            .unwrap();
        assert!(out.content.contains("Sibling"));
        assert!(!out.content.contains("Other"));
        assert!(!out.content.contains("\"self\""));
    }

    #[tokio::test]
    async fn read_rejects_a_conversation_outside_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        crate::store::save(
            &conn.lock().unwrap(),
            &conversation("outsider", "Outsider", Some("p2"), vec!["secret"]),
        )
        .unwrap();

        let tool = ReadProjectConversationTool::new(conn, "p1".to_string());
        let args = serde_json::json!({ "conversation_id": "outsider" });
        let err = tool.execute(&args, &ctx(dir.path())).await.unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }

    #[tokio::test]
    async fn read_rejects_an_unassigned_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        crate::store::save(
            &conn.lock().unwrap(),
            &conversation("unassigned", "Loose", None, vec!["hi"]),
        )
        .unwrap();

        let tool = ReadProjectConversationTool::new(conn, "p1".to_string());
        let args = serde_json::json!({ "conversation_id": "unassigned" });
        let err = tool.execute(&args, &ctx(dir.path())).await.unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }

    #[tokio::test]
    async fn read_returns_sibling_content() {
        let dir = tempfile::tempdir().unwrap();
        let conn = test_conn();
        crate::store::save(
            &conn.lock().unwrap(),
            &conversation(
                "sibling",
                "Sibling",
                Some("p1"),
                vec!["the launch date is March 3rd"],
            ),
        )
        .unwrap();

        let tool = ReadProjectConversationTool::new(conn, "p1".to_string());
        let args = serde_json::json!({ "conversation_id": "sibling", "query": "launch date" });
        let out = tool.execute(&args, &ctx(dir.path())).await.unwrap();
        assert!(out.content.contains("March 3rd"));
    }

    #[tokio::test]
    async fn read_rejects_a_nonexistent_conversation_before_consent() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ReadProjectConversationTool::new(test_conn(), "p1".to_string());
        let args = serde_json::json!({ "conversation_id": "nope" });
        let err = tool.consent_summary(&args, &ctx(dir.path())).unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }
}
