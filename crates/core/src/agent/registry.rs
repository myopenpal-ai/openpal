//! `ToolRegistry` — holds the set of tools available this turn and builds the
//! OpenAI-compatible `tools` array for the chat request.

use super::tool::Tool;
use std::sync::Arc;

#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn resolve(&self, id: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.id() == id).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// The `tools` array for a `/v1/chat/completions` request body.
    pub fn openai_tool_defs(&self) -> Vec<serde_json::Value> {
        self.tools
            .iter()
            .map(|t| serde_json::json!({ "type": "function", "function": t.spec() }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tool::{ConsentSummary, ToolContext, ToolError, ToolOutput};

    struct FakeTool(&'static str);

    #[async_trait::async_trait]
    impl Tool for FakeTool {
        fn id(&self) -> &str {
            self.0
        }
        fn spec(&self) -> serde_json::Value {
            serde_json::json!({ "name": self.0, "description": "fake", "parameters": {} })
        }
        fn consent_summary(
            &self,
            _args: &serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ConsentSummary, ToolError> {
            Ok(ConsentSummary {
                summary: "fake".into(),
                detail: "fake".into(),
                resolved_paths: vec![],
                danger_flags: vec![],
            })
        }
        fn grant_key(&self, _args: &serde_json::Value, _ctx: &ToolContext) -> String {
            self.0.to_string()
        }
        async fn execute(
            &self,
            _args: &serde_json::Value,
            _ctx: &ToolContext,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                ok: true,
                content: String::new(),
                preview: String::new(),
            })
        }
    }

    #[test]
    fn resolves_registered_tools_by_id() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(FakeTool("run_shell")));
        reg.register(Arc::new(FakeTool("read_file")));
        assert_eq!(reg.len(), 2);
        assert!(reg.resolve("run_shell").is_some());
        assert!(reg.resolve("nope").is_none());
    }

    #[test]
    fn openai_tool_defs_wraps_each_spec_as_a_function() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(FakeTool("run_shell")));
        let defs = reg.openai_tool_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0]["type"], "function");
        assert_eq!(defs[0]["function"]["name"], "run_shell");
    }
}
