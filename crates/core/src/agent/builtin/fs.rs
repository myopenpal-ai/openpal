use super::{hash_grant_key, truncate_for_summary};
use crate::agent::tool::{
    cap_output, ConsentSummary, DangerFlag, ResolvedPath, Tool, ToolContext, ToolError, ToolOutput,
};
use crate::agent::workspace::{secure_open_for_read, secure_open_for_write, WriteMode};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read as _, Write as _};

pub struct FsReadTool;

fn danger_flags_for(resolved: &ResolvedPath) -> Vec<DangerFlag> {
    if resolved.inside_workspace {
        vec![]
    } else {
        vec![DangerFlag::OutsideWorkspace]
    }
}

fn path_detail(resolved: &ResolvedPath) -> String {
    let canonical = resolved.canonical.display().to_string();
    if resolved.raw == canonical {
        canonical
    } else {
        format!(
            "You asked for: {}\nThis actually resolves to: {}",
            resolved.raw, canonical
        )
    }
}

#[async_trait::async_trait]
impl Tool for FsReadTool {
    fn id(&self) -> &str {
        "read_file"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "read_file",
            "description": "Read a text file inside the agent workspace. Paths are resolved relative to the workspace root unless absolute.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to read." }
                },
                "required": ["path"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"path\".".to_string()))?;
        let resolved = ctx.workspace.resolve_for_read(path)?;
        Ok(ConsentSummary {
            summary: format!("Read {}", resolved.canonical.display()),
            detail: path_detail(&resolved),
            danger_flags: danger_flags_for(&resolved),
            resolved_paths: vec![resolved],
        })
    }

    fn grant_key(&self, args: &Value, ctx: &ToolContext) -> String {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let canonical = ctx
            .workspace
            .resolve_for_read(path)
            .map(|r| r.canonical.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string());
        hash_grant_key("read_file", &[&canonical])
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"path\".".to_string()))?;
        let resolved = ctx.workspace.resolve_for_read(path)?;
        let mut file = secure_open_for_read(&resolved.canonical)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf).map_err(|e| {
            ToolError::ExecutionFailed(format!("Could not read file (is it binary?): {e}"))
        })?;
        let content = cap_output(buf);
        Ok(ToolOutput {
            ok: true,
            preview: truncate_for_summary(&content),
            content,
        })
    }
}

pub struct FsWriteTool;

fn write_mode_from_str(s: &str) -> Result<WriteMode, ToolError> {
    match s {
        "overwrite" | "" => Ok(WriteMode::Overwrite),
        "append" => Ok(WriteMode::Append),
        "createOnly" => Ok(WriteMode::CreateOnly),
        other => Err(ToolError::Rejected(format!(
            "Unknown write mode \"{other}\". Use overwrite, append, or createOnly."
        ))),
    }
}

#[async_trait::async_trait]
impl Tool for FsWriteTool {
    fn id(&self) -> &str {
        "write_file"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "write_file",
            "description": "Write a text file inside the agent workspace. Paths are resolved relative to the workspace root unless absolute.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File to write." },
                    "content": { "type": "string", "description": "Full text content to write." },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append", "createOnly"],
                        "description": "overwrite (default): create or truncate. append: create if missing, else append. createOnly: fail if the file already exists."
                    }
                },
                "required": ["path", "content"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"path\".".to_string()))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"content\".".to_string()))?;
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("overwrite");
        write_mode_from_str(mode)?;
        let resolved = ctx.workspace.resolve_for_write(path)?;
        Ok(ConsentSummary {
            summary: format!("Write {} ({mode})", resolved.canonical.display()),
            detail: format!(
                "{}\n\n---\n{}",
                path_detail(&resolved),
                truncate_for_summary(content)
            ),
            danger_flags: danger_flags_for(&resolved),
            resolved_paths: vec![resolved],
        })
    }

    fn grant_key(&self, args: &Value, ctx: &ToolContext) -> String {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("overwrite");
        let canonical = ctx
            .workspace
            .resolve_for_write(path)
            .map(|r| r.canonical.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string());
        // Deliberately includes a content hash: a path+mode-only grant key
        // would let a later, differently-worded write to the same path
        // silently re-execute under a standing "always allow rewriting this
        // file" grant — including a prompt-injected malicious overwrite.
        // Content-hashing makes "Always allow" behave like "Allow once"
        // whenever the content actually changes, closing that composition
        // with `run_shell`'s exact-text grants (e.g. "always allow rerunning
        // python3 run.py" + "always allow rewriting run.py").
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        let content_hash = format!("{:x}", hasher.finalize());
        hash_grant_key("write_file", &[&canonical, mode, &content_hash])
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"path\".".to_string()))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"content\".".to_string()))?;
        let mode = args
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("overwrite");
        let write_mode = write_mode_from_str(mode)?;
        let resolved = ctx.workspace.resolve_for_write(path)?;
        let mut file = secure_open_for_write(&resolved.canonical, write_mode)?;
        file.write_all(content.as_bytes())
            .map_err(|e| ToolError::ExecutionFailed(format!("Could not write file: {e}")))?;
        Ok(ToolOutput {
            ok: true,
            preview: format!(
                "Wrote {} bytes to {}",
                content.len(),
                resolved.canonical.display()
            ),
            content: format!(
                "Wrote {} bytes to {}.",
                content.len(),
                resolved.canonical.display()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::workspace::WorkspaceJail;
    use std::sync::{atomic::AtomicBool, Arc};

    fn ctx(workspace_root: &std::path::Path) -> ToolContext {
        ToolContext {
            turn_id: "t".into(),
            call_id: "c".into(),
            workspace: Arc::new(
                WorkspaceJail::new(
                    workspace_root,
                    std::path::Path::new("/nonexistent-home"),
                    std::path::Path::new("/nonexistent-data"),
                )
                .unwrap(),
            ),
            scratch_dir: workspace_root.join(".scratch"),
            cancel: Arc::new(AtomicBool::new(false)),
            on_event: Arc::new(|_| {}),
            citations: Arc::new(crate::agent::CitationRegistry::new()),
            fetch_allowlist: Arc::new(crate::agent::FetchAllowlist::new()),
            consent: Arc::new(crate::agent::ConsentBroker::new(
                workspace_root.join("grants.json"),
            )),
        }
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let write_args = serde_json::json!({ "path": "hello.txt", "content": "hi there" });
        FsWriteTool.execute(&write_args, &c).await.unwrap();

        let read_args = serde_json::json!({ "path": "hello.txt" });
        let out = FsReadTool.execute(&read_args, &c).await.unwrap();
        assert_eq!(out.content, "hi there");
    }

    #[tokio::test]
    async fn create_only_rejects_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        std::fs::write(tmp.path().join("f.txt"), "already here").unwrap();
        let args = serde_json::json!({ "path": "f.txt", "content": "new", "mode": "createOnly" });
        let err = FsWriteTool.execute(&args, &c).await.unwrap_err();
        assert!(matches!(err, ToolError::Rejected(_)));
    }

    #[test]
    fn write_grant_key_changes_when_content_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let a = serde_json::json!({ "path": "f.txt", "content": "version A" });
        let b = serde_json::json!({ "path": "f.txt", "content": "version B" });
        assert_ne!(FsWriteTool.grant_key(&a, &c), FsWriteTool.grant_key(&b, &c));
    }

    #[test]
    fn write_grant_key_is_stable_for_identical_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let a = serde_json::json!({ "path": "f.txt", "content": "same" });
        assert_eq!(FsWriteTool.grant_key(&a, &c), FsWriteTool.grant_key(&a, &c));
    }

    #[test]
    fn read_outside_workspace_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(tmp.path().join("outside.txt"), "x").unwrap();
        let c = ctx(&workspace);
        let args = serde_json::json!({ "path": "../outside.txt" });
        let summary = FsReadTool.consent_summary(&args, &c).unwrap();
        assert!(summary.danger_flags.contains(&DangerFlag::OutsideWorkspace));
    }
}
