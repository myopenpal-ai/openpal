//! The v1 tools: `run_shell`, `run_code`, `read_file`, `write_file`,
//! `create_artifact`, `ask_user`, plus `list_project_conversations`/
//! `read_project_conversation` (registered only when the current conversation
//! is in a project — see `project`'s own doc comment).

pub mod artifact;
pub mod ask_user;
pub mod code;
pub mod fs;
pub mod project;
mod rerank;
pub mod shell;
pub mod subagent;
pub mod websearch;

pub use artifact::CreateArtifactTool;
pub use ask_user::AskUserTool;
pub use code::CodeExecTool;
pub use fs::{FsReadTool, FsWriteTool};
pub use project::{ListProjectConversationsTool, ReadProjectConversationTool};
pub use shell::ShellExecTool;
pub use subagent::SpawnSubagentTool;
pub use websearch::{FetchUrlTool, WebSearchTool};

use sha2::{Digest, Sha256};
use std::io::Write as _;

/// Build a stable "Always allow" grant key by hashing `tool_id` and `parts`
/// together, `\0`-separated. A grant key must be narrow enough that granting
/// it doesn't implicitly grant a materially different action — see
/// `FsWriteTool`'s inclusion of a content hash.
pub(super) fn hash_grant_key(tool_id: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tool_id.as_bytes());
    for p in parts {
        hasher.update(b"\0");
        hasher.update(p.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Write `content` to `path` as a private (`0600`) file, creating or
/// truncating it. Used so command/code text never appears on argv — a
/// secret a user inlines into a command is visible only to processes that
/// can read this file, not to every process on the account via `ps`.
/// Windows has no mode bits: the file inherits the scratch dir's ACL instead.
pub(super) fn write_private_script(
    path: &std::path::Path,
    content: &str,
) -> Result<(), super::tool::ToolError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|e| super::tool::ToolError::ExecutionFailed(format!("scratch file: {e}")))?;
    file.write_all(content.as_bytes())
        .map_err(|e| super::tool::ToolError::ExecutionFailed(format!("scratch file: {e}")))?;
    Ok(())
}

/// One-line summary for a consent card title — the model can send an
/// arbitrarily long command/code string, so the title row stays scannable
/// while `detail` always carries the full, untruncated text.
pub(super) fn truncate_for_summary(s: &str) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.chars().count() > 80 {
        format!("{}…", first_line.chars().take(80).collect::<String>())
    } else if first_line.len() < s.len() {
        format!("{first_line} …")
    } else {
        first_line.to_string()
    }
}
