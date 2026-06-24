use super::{Registry, Tool, ToolContext, ToolOutput};
use crate::agent::cheap_route::{CheapRouteOutcome, ProviderCheapBackend, run_cheap_route};
use crate::provider::Provider;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

/// Tool that offloads a task to the cheapest capable model via the cheap-routing
/// orchestrator. Mirrors `SubagentTool`: holds the parent provider + registry,
/// and on each call builds a `ProviderCheapBackend` and runs `run_cheap_route`.
pub struct CheapRouteTool {
    provider: Arc<dyn Provider>,
    registry: Registry,
}

impl CheapRouteTool {
    pub fn new(provider: Arc<dyn Provider>, registry: Registry) -> Self {
        Self { provider, registry }
    }
}

#[derive(Deserialize)]
struct CheapRouteInput {
    task: String,
}

#[async_trait]
impl Tool for CheapRouteTool {
    fn name(&self) -> &str {
        "cheap_route"
    }

    fn description(&self) -> &str {
        "Offload a task to the cheapest capable model: decompose into subtasks, \
         recommend one cheap model across available providers, run each subtask on it, \
         and review the results. Use for routine multi-step work to save budget."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task"],
            "properties": {
                "intent": super::intent_schema_property(),
                "task": {
                    "type": "string",
                    "description": "The task to offload to cheap models."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: CheapRouteInput = serde_json::from_value(input)?;
        let task = params.task.trim();
        if task.is_empty() {
            return Err(anyhow!("cheap_route requires a non-empty 'task'"));
        }

        let backend = ProviderCheapBackend::new(self.provider.clone(), self.registry.clone());
        let outcome = run_cheap_route(&backend, task).await?;
        let output = format_cheap_outcome(&outcome);

        Ok(ToolOutput::new(output)
            .with_title(format!("cheap_route · {}", outcome.recommended_model))
            .with_metadata(json!({
                "recommendedModel": outcome.recommended_model,
                "subtaskCount": outcome.subtasks.len(),
            })))
    }
}

/// Render a cheap-routing outcome as human-readable text for the tool result.
fn format_cheap_outcome(outcome: &CheapRouteOutcome) -> String {
    let mut out = format!(
        "Ran {} subtask(s) on '{}'.\n\n",
        outcome.results.len(),
        outcome.recommended_model
    );
    for (index, result) in outcome.results.iter().enumerate() {
        out.push_str(&format!(
            "### {}. {} _(ran on {})_\n\n{}\n\nReview: {}\n\n",
            index + 1,
            result.description,
            result.model_used,
            result.output.trim(),
            result.review.trim()
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::cheap_route::SubtaskResult;

    #[test]
    fn format_cheap_outcome_lists_subtasks_and_reviews() {
        let outcome = CheapRouteOutcome {
            recommended_model: "cheapo".to_string(),
            subtasks: Vec::new(),
            results: vec![SubtaskResult {
                description: "edit auth".to_string(),
                output: "did it".to_string(),
                review: "OK".to_string(),
                model_used: "deepseek-v4-flash".to_string(),
            }],
        };

        let rendered = format_cheap_outcome(&outcome);
        assert!(rendered.contains("cheapo"));
        assert!(rendered.contains("edit auth"));
        assert!(rendered.contains("did it"));
        assert!(rendered.contains("Review: OK"));
        assert!(rendered.contains("deepseek-v4-flash"));
    }
}
