use super::{Registry, Tool, ToolContext, ToolOutput};
use crate::agent::cheap_route::{CheapRouteOutcome, ProviderCheapBackend, run_cheap_route};
use crate::agent::debate_status::SidePanelDebateReporter;
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

/// Compute the effective gold-mode flag: both the per-session toggle AND the
/// global config gate must be `true`. Extracted as a pure function so it can be
/// unit-tested without a live provider or config singleton.
pub(crate) fn resolve_gold(session_flag: Option<bool>, global: bool) -> bool {
    session_flag.unwrap_or(false) && global
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

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: CheapRouteInput = serde_json::from_value(input)?;
        let task = params.task.trim();
        if task.is_empty() {
            return Err(anyhow!("cheap_route requires a non-empty 'task'"));
        }

        let session = crate::session::Session::load(&ctx.session_id).ok();
        let gold_mode = resolve_gold(
            session.as_ref().and_then(|s| s.gold_mode_enabled),
            crate::config::config().agents.cheap_route_gold_mode,
        );
        let gold_k = crate::config::config().agents.cheap_route_gold_k;

        let reporter = Arc::new(SidePanelDebateReporter::new(ctx.session_id.clone()));
        let backend = ProviderCheapBackend::new(self.provider.clone(), self.registry.clone())
            .with_gold(gold_mode, gold_k)
            .with_reporter(reporter);
        let outcome = run_cheap_route(&backend, task).await?;
        let output = format_cheap_outcome(&outcome);

        Ok(ToolOutput::new(output)
            .with_title(format!("cheap_route · {}", models_used_label(&outcome)))
            .with_metadata(json!({
                "recommendedModel": outcome.recommended_model,
                "subtaskCount": outcome.subtasks.len(),
            })))
    }
}

/// How to open the live cheap-route view.
///
/// The panel already streams the plan, the model per subtask, and a rolling
/// output tail, but nothing pointed at it, so a run looked opaque while it was
/// the part users most wanted to watch. Read the actual binding rather than
/// hardcoding "Alt+M": `keybindings.side_panel_toggle` is user-configurable, and
/// a hint naming the wrong key is worse than no hint.
fn side_panel_hint() -> String {
    let configured = crate::config::config()
        .keybindings
        .side_panel_toggle
        .trim()
        .to_string();
    if configured.is_empty() {
        "the side-panel toggle".to_string()
    } else {
        configured
    }
}

/// The distinct models a run actually used, for the tool-call title.
///
/// The title previously showed only `recommended_model`, which hides the case
/// this whole feature exists to make visible: subtasks routing to DIFFERENT
/// models, including a fallback after a cheaper route failed.
fn models_used_label(outcome: &CheapRouteOutcome) -> String {
    let mut models: Vec<&str> = outcome
        .results
        .iter()
        .map(|r| r.model_used.as_str())
        .collect();
    models.sort_unstable();
    models.dedup();
    match models.len() {
        0 => outcome.recommended_model.clone(),
        1 => models[0].to_string(),
        // Keep the chip short; the full per-subtask breakdown is in the body.
        _ => format!("{} +{}", models[0], models.len() - 1),
    }
}

/// Render a cheap-routing outcome as human-readable text for the tool result.
fn format_cheap_outcome(outcome: &CheapRouteOutcome) -> String {
    // Report the models that ACTUALLY ran (may differ from the recommendation
    // when cheaper routes errored and we fell back), not just the recommendation.
    let mut models_used: Vec<&str> = outcome
        .results
        .iter()
        .map(|r| r.model_used.as_str())
        .collect();
    models_used.sort_unstable();
    models_used.dedup();
    let ran_on = if models_used.is_empty() {
        outcome.recommended_model.clone()
    } else {
        models_used.join(", ")
    };
    let mut out = format!(
        "Ran {} subtask(s) on {} (recommended: {}).\n\
         Live progress for a run is in the side panel (toggle it with {}).\n\n",
        outcome.results.len(),
        ran_on,
        outcome.recommended_model,
        side_panel_hint(),
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

    // --- resolve_gold pure-logic tests (no live provider needed) ---

    #[test]
    fn resolve_gold_both_true_yields_true() {
        assert!(resolve_gold(Some(true), true));
    }

    #[test]
    fn resolve_gold_session_true_but_global_off_yields_false() {
        assert!(!resolve_gold(Some(true), false));
    }

    #[test]
    fn resolve_gold_session_none_global_true_yields_false() {
        assert!(!resolve_gold(None, true));
    }

    #[test]
    fn resolve_gold_session_false_global_true_yields_false() {
        assert!(!resolve_gold(Some(false), true));
    }

    // --- format_cheap_outcome tests ---

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

#[cfg(test)]
mod visibility_tests {
    use super::*;
    use crate::agent::cheap_route::SubtaskResult;

    fn result(model: &str) -> SubtaskResult {
        SubtaskResult {
            description: "task".to_string(),
            output: "out".to_string(),
            review: "OK".to_string(),
            model_used: model.to_string(),
        }
    }

    fn outcome(models: &[&str]) -> CheapRouteOutcome {
        CheapRouteOutcome {
            recommended_model: "recommended-model".to_string(),
            subtasks: Vec::new(),
            results: models.iter().map(|m| result(m)).collect(),
        }
    }

    /// The tool-call chip previously showed only `recommended_model`, which
    /// hides the exact thing cheap routing needs to make visible: a subtask
    /// running on a DIFFERENT model than recommended, e.g. after a cheaper
    /// route failed and it fell back.
    #[test]
    fn title_reports_models_that_actually_ran() {
        assert_eq!(
            models_used_label(&outcome(&["deepseek-v4-flash"])),
            "deepseek-v4-flash",
            "a single model should be named outright, not shown as the recommendation"
        );

        // Two different models: the chip must signal the split rather than
        // silently showing one of them.
        let label = models_used_label(&outcome(&["deepseek-v4-flash", "glm-5.2"]));
        assert!(
            label.contains("+1"),
            "a multi-model run must be visible in the title, got {label:?}"
        );

        // Duplicates collapse: three subtasks on one model is still one model.
        assert_eq!(
            models_used_label(&outcome(&["glm-5.2", "glm-5.2", "glm-5.2"])),
            "glm-5.2"
        );

        // No results (e.g. an early failure) falls back to the recommendation
        // rather than rendering an empty chip.
        assert_eq!(models_used_label(&outcome(&[])), "recommended-model");
    }

    /// The live view existed but nothing pointed at it, so a run looked opaque.
    /// The hint must also name the REAL binding, since it is user-configurable.
    #[test]
    fn output_points_at_the_live_view_using_the_configured_key() {
        let rendered = format_cheap_outcome(&outcome(&["deepseek-v4-flash"]));
        assert!(
            rendered.contains("side panel"),
            "output must tell the user where live progress is: {rendered}"
        );
        let hint = side_panel_hint();
        assert!(!hint.is_empty(), "hint must never render empty");
        assert!(
            rendered.contains(&hint),
            "output must name the configured toggle ({hint}), not a hardcoded key"
        );
    }
}
