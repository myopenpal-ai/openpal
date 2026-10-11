use super::{hash_grant_key, truncate_for_summary, write_private_script};
use crate::agent::heuristics::danger_flags_for_code;
use crate::agent::sandbox::{Sandbox, SandboxProfile, SandboxSpec};
use crate::agent::tool::{cap_output, ConsentSummary, Tool, ToolContext, ToolError, ToolOutput};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub struct CodeExecTool {
    sandbox: Arc<dyn Sandbox>,
    pub timeout: Duration,
}

impl CodeExecTool {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self {
            sandbox,
            timeout: Duration::from_secs(30),
        }
    }

    fn args<'a>(&self, args: &'a Value) -> Result<(&'a str, &'a str), ToolError> {
        let language = args
            .get("language")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::Rejected("Missing \"language\".".to_string()))?;
        let code = args
            .get("code")
            .and_then(Value::as_str)
            .filter(|c| !c.trim().is_empty())
            .ok_or_else(|| ToolError::Rejected("Missing or empty \"code\".".to_string()))?;
        Ok((language, code))
    }
}

/// Resolve `language` to an interpreter present on this machine and the file
/// extension its scratch file should use. `run_code` is intentionally
/// limited to a small, named set — it is not a general "run this program"
/// tool (that's `run_shell`).
fn interpreter_for(language: &str) -> Result<(PathBuf, &'static str), ToolError> {
    let (candidates, ext): (&[&str], &'static str) = match language {
        "python3" | "python" => (
            &[
                "/usr/bin/python3",
                "/opt/homebrew/bin/python3",
                "/usr/local/bin/python3",
            ],
            "py",
        ),
        "node" | "javascript" | "js" => (
            &[
                "/opt/homebrew/bin/node",
                "/usr/local/bin/node",
                "/usr/bin/node",
            ],
            "js",
        ),
        "bash" | "shell" => (&["/bin/bash"], "sh"),
        other => {
            return Err(ToolError::Rejected(format!(
                "Unsupported language \"{other}\". Use python3, node, or bash."
            )))
        }
    };
    for c in candidates {
        let p = Path::new(c);
        if p.exists() {
            return Ok((p.to_path_buf(), ext));
        }
    }
    Err(ToolError::Rejected(format!(
        "No {language} interpreter was found on this machine."
    )))
}

#[async_trait::async_trait]
impl Tool for CodeExecTool {
    fn id(&self) -> &str {
        "run_code"
    }

    fn spec(&self) -> Value {
        serde_json::json!({
            "name": "run_code",
            "description": "Run a short script with a fixed interpreter (python3, node, or bash) inside the sandboxed agent workspace. Runs under the same filesystem and network confinement as run_shell — its benefit is structure and avoiding shell-quoting hazards, not a smaller sandbox. No network access.",
            "parameters": {
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "enum": ["python3", "node", "bash"],
                        "description": "Interpreter to run the code with."
                    },
                    "code": {
                        "type": "string",
                        "description": "The full source code to run."
                    }
                },
                "required": ["language", "code"]
            }
        })
    }

    fn consent_summary(
        &self,
        args: &Value,
        _ctx: &ToolContext,
    ) -> Result<ConsentSummary, ToolError> {
        let (language, code) = self.args(args)?;
        interpreter_for(language)?;
        Ok(ConsentSummary {
            summary: format!("Run {language} code: {}", truncate_for_summary(code)),
            detail: code.to_string(),
            resolved_paths: vec![],
            danger_flags: danger_flags_for_code(code),
        })
    }

    fn grant_key(&self, args: &Value, _ctx: &ToolContext) -> String {
        let (language, code) = self.args(args).unwrap_or(("", ""));
        hash_grant_key("run_code", &[language, code])
    }

    async fn execute(&self, args: &Value, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let (language, code) = self.args(args)?;
        let (interpreter, ext) = interpreter_for(language)?;

        std::fs::create_dir_all(&ctx.scratch_dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("scratch dir: {e}")))?;
        let script_path = ctx.scratch_dir.join(format!(".code.{ext}"));
        write_private_script(&script_path, code)?;

        let mut spec = SandboxSpec::new(ctx.workspace.root.clone(), ctx.scratch_dir.clone());
        spec.timeout = self.timeout;

        let output = self
            .sandbox
            .run(
                &SandboxProfile::Code {
                    interpreter: interpreter.clone(),
                },
                &interpreter,
                &[script_path.to_string_lossy().into_owned()],
                &spec,
            )
            .await
            .map_err(ToolError::ExecutionFailed)?;

        let ok = output.exit_code == Some(0) && !output.timed_out && !output.disk_quota_exceeded;
        let mut content = String::new();
        if output.timed_out {
            content.push_str("[code timed out and was stopped]\n");
        }
        if output.disk_quota_exceeded {
            content.push_str("[code wrote more disk space than allowed and was stopped]\n");
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
