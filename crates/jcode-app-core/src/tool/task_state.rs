use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

pub struct UpdateTaskStateTool;

impl UpdateTaskStateTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct UpdateTaskStateInput {
    content: String,
}

#[async_trait]
impl Tool for UpdateTaskStateTool {
    fn name(&self) -> &str {
        "update_task_state"
    }

    fn description(&self) -> &str {
        "Persist your working state (current plan, progress, key decisions, next steps) for this session. \
         The content is stored on disk, re-injected into your context every turn, and SURVIVES context \
         compaction — anything not saved here may be lost when the conversation is summarized. \
         For any multi-step task: write the plan when you start, update after completing a sub-task or \
         making an important decision, and prune finished items. Full replace on every call; keep it \
         under 8KB. Call with empty content to clear."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["content"],
            "properties": {
                "intent": super::intent_schema_property(),
                "content": {
                    "type": "string",
                    "description": "Full replacement task-state markdown (plan, progress, decisions, next steps). Empty string clears."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: UpdateTaskStateInput = serde_json::from_value(input)?;
        let content = params.content.trim();
        let cleared = content.is_empty();
        jcode_base::session::task_state::write_task_state(&ctx.session_id, content)?;
        let msg = if cleared {
            "Task state cleared.".to_string()
        } else {
            format!(
                "Task state updated ({} chars). It will be re-injected every turn and survives compaction.",
                content.chars().count()
            )
        };
        Ok(ToolOutput::new(msg).with_title("update_task_state"))
    }
}
