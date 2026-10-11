use super::{hash_grant_key, truncate_for_summary, write_private_script};
use crate::agent::heuristics::danger_flags_for_shell;
use crate::agent::sandbox::{Sandbox, SandboxProfile, SandboxSpec};
use crate::agent::tool::{cap_output, ConsentSummary, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

pub struct ShellExecTool {
    sandbox: Arc<dyn Sandbox>,
    pub timeout: Duration,
}

impl ShellExecTool {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self {
            sandbox,
            timeout: Duration::from_secs(30),
        }
    }

    fn args<'a>(&self, args: &'a Value) -> Result<(&'a str, Option<&'a str>), ToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| ToolError::Rejected("Missing or empty \"command\".".to_string()))?;
        let cwd = args.get("cwd").and_then(Value::as_str);
        Ok((command, cwd))
    }
}

#[async_trait::async_trait]
impl Tool for ShellExecTool {
    fn id(&self) -> &str {
        "run_shell"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "run_shell",
            "description": "Run a shell command inside the sandboxed agent workspace (bash syntax). Confined to the workspace filesystem, no network access. Shares run_code's capability ceiling — it's not a smaller sandbox, just a different interface.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to run."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Directory to run in, relative to the agent workspace root. Defaults to the workspace root."
                    }
                },
                "required": ["command"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let (command, cwd) = self.args(args)?;
        ctx.workspace.require_cwd_in_workspace(cwd)?;
        Ok(ConsentSummary {
            summary: format!("Run: {}", truncate_for_summary(command)),
            detail: command.to_string(),
            resolved_paths: vec![],
            danger_flags: danger_flags_for_shell(command),
        })
    }

    fn grant_key(&self, args: &Value, ctx: &ToolContext) -> String {
        let (command, cwd) = self.args(args).unwrap_or(("", None));
        hash_grant_key(
            "run_shell",
            &[
                &ctx.workspace.root.to_string_lossy(),
                cwd.unwrap_or(""),
                command,
            ],
        )
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (command, cwd_arg) = self.args(args)?;
        let cwd = ctx.workspace.require_cwd_in_workspace(cwd_arg)?;

        std::fs::create_dir_all(&ctx.scratch_dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("scratch dir: {e}")))?;
        let script_path = ctx.scratch_dir.join(".cmd");
        write_private_script(&script_path, command)?;

        let mut spec = SandboxSpec::new(ctx.workspace.root.clone(), ctx.scratch_dir.clone());
        spec.cwd = cwd;
        spec.timeout = self.timeout;

        let output = self
            .sandbox
            .run(
                &SandboxProfile::Shell,
                std::path::Path::new("/bin/bash"),
                &[
                    "--noprofile".to_string(),
                    "--norc".to_string(),
                    script_path.to_string_lossy().into_owned(),
                ],
                &spec,
            )
            .await
            .map_err(ToolError::ExecutionFailed)?;

        let ok = output.exit_code == Some(0) && !output.timed_out && !output.disk_quota_exceeded;
        let mut content = String::new();
        if output.timed_out {
            content.push_str("[command timed out and was stopped]\n");
        }
        if output.disk_quota_exceeded {
            content.push_str("[command wrote more disk space than allowed and was stopped]\n");
        }
        content.push_str(&format!(
            "exit code: {}\n",
            output
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "killed".to_string())
        ));
        if !output.stdout.is_empty() {
            content.push_str("stdout:\n");
            content.push_str(&output.stdout);
        }
        if !output.stderr.is_empty() {
            content.push_str("\nstderr:\n");
            content.push_str(&output.stderr);
        }
        let content = cap_output(content);
        Ok(ToolOutput {
            ok,
            preview: truncate_for_summary(&content),
            content,
        })
    }
}
