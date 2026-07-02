use super::{Registry, Tool, ToolContext, ToolOutput, expand_session_tools, session_expanded_tools};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct LoadToolsTool {
    registry: Registry,
}

impl LoadToolsTool {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
}

#[derive(Deserialize)]
struct LoadToolsInput {
    names: Vec<String>,
}

#[async_trait]
impl Tool for LoadToolsTool {
    fn name(&self) -> &str {
        "load_tools"
    }

    fn description(&self) -> &str {
        "Load full schemas for deferred tools by name. Deferred tools are listed in this tool's input schema description; call this before first use of any of them."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["names"],
            "properties": {
                "names": {
                    "type": "array",
                    "items": {
                        "type": "string"
                    },
                    "description": "Tool names to load."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: LoadToolsInput = serde_json::from_value(input)?;

        let available_names = self.registry.tool_names().await;
        let available_set: std::collections::HashSet<&str> =
            available_names.iter().map(|s| s.as_str()).collect();

        let mut unknown = Vec::new();
        let mut valid = Vec::new();

        for name in params.names {
            if available_set.contains(name.as_str()) {
                valid.push(name);
            } else {
                unknown.push(name);
            }
        }

        if !unknown.is_empty() {
            let index = self.registry.deferred_tool_index().await;
            let index_text = index
                .iter()
                .map(|(name, desc)| format!("{} — {}", name, desc))
                .collect::<Vec<_>>()
                .join("\n");

            let error_message = format!(
                "unknown tools: {} — available deferred tools:\n{}",
                unknown.join(", "),
                index_text
            );

            return Ok(ToolOutput::new(error_message));
        }

        // All names are valid - expand the session tools
        let string_names: Vec<String> = valid.clone();
        expand_session_tools(&ctx.session_id, &string_names);

        let loaded_text = format!(
            "loaded: {}. Schemas available from the next model call.",
            valid.join(", ")
        );
        Ok(ToolOutput::new(loaded_text).with_title("Tools loaded"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reuse the test infrastructure from tool/tests.rs
    use crate::provider::Provider;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockProvider;

    #[async_trait]
    impl Provider for MockProvider {
        async fn complete(
            &self,
            _messages: &[crate::message::Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            Err(anyhow::anyhow!(
                "Mock provider should not be used for streaming completions in load_tools tests"
            ))
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn fork(&self) -> Arc<dyn Provider> {
            Arc::new(MockProvider)
        }
    }

    #[test]
    fn schema_has_required_names() {
        let registry = Registry::empty();
        let tool = LoadToolsTool::new(registry);
        let schema = tool.parameters_schema();

        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("schema should have required array");
        assert!(
            required.iter().any(|v| v == "names"),
            "names should be required"
        );

        let props = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("schema should have properties");
        assert!(props.contains_key("names"));

        let names_prop = &props["names"];
        assert_eq!(names_prop.get("type").and_then(|v| v.as_str()), Some("array"));
    }

    #[tokio::test]
    async fn execute_with_unknown_name_returns_index_text() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider);
        let registry = Registry::new(provider).await;
        let tool = LoadToolsTool::new(registry);

        let input = json!({
            "names": ["nonexistent_tool"]
        });

        let ctx = ToolContext {
            session_id: "test-session".to_string(),
            message_id: "msg-123".to_string(),
            tool_call_id: "call-456".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        };

        let result = tool.execute(input, ctx).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        let text = &output.output;
        assert!(text.contains("unknown tools"));
        assert!(text.contains("nonexistent_tool"));
        assert!(text.contains("available deferred tools"));
    }

    #[tokio::test]
    async fn execute_with_valid_name_records_expansion() {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider);
        let registry = Registry::new(provider).await;
        let tool = LoadToolsTool::new(registry);

        let session_id = "test-session-valid";
        let tool_name = "bash".to_string(); // bash is always available

        let input = json!({
            "names": [tool_name.clone()]
        });

        let ctx = ToolContext {
            session_id: session_id.to_string(),
            message_id: "msg-123".to_string(),
            tool_call_id: "call-456".to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        };

        let result = tool.execute(input, ctx).await;
        assert!(result.is_ok());

        let output = result.unwrap();
        let text = &output.output;
        assert!(text.contains("loaded:"));
        assert!(text.contains(&tool_name));

        // Verify the tool was recorded in the session's expanded set
        let expanded = session_expanded_tools(session_id);
        assert!(expanded.contains("bash"));
    }
}
