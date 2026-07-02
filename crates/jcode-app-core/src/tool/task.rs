use super::{Registry, Tool, ToolContext, ToolOutput};
use crate::agent::Agent;
use crate::bus::{Bus, BusEvent, SubagentStatus, ToolSummary, ToolSummaryState};
use crate::logging;
use crate::protocol::HistoryMessage;
use crate::provider::Provider;
use crate::session::Session;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub struct SubagentTool {
    provider: Arc<dyn Provider>,
    registry: Registry,
}

impl SubagentTool {
    pub fn new(provider: Arc<dyn Provider>, registry: Registry) -> Self {
        Self { provider, registry }
    }

    fn preferred_parent_subagent_model(parent_session_id: &str) -> Option<String> {
        Session::load(parent_session_id)
            .ok()
            .and_then(|session| session.subagent_model)
    }

    fn resolve_model(
        requested_model: Option<&str>,
        existing_session_model: Option<&str>,
        parent_subagent_model: Option<&str>,
        provider_model: &str,
    ) -> String {
        requested_model
            .or(existing_session_model)
            .or(parent_subagent_model)
            .or(crate::config::config().agents.swarm_model.as_deref())
            .unwrap_or(provider_model)
            .to_string()
    }
}

#[derive(Deserialize)]
struct SubagentInput {
    description: String,
    prompt: String,
    subagent_type: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    output_mode: SubagentOutputMode,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    /// Per-task inactivity (stall) budget in seconds, decided by the orchestrator
    /// based on how long this task may legitimately run silently. The subagent is
    /// aborted only if it makes NO progress (no API call, stream, or tool event)
    /// for this long — NOT a total-runtime cap, so genuinely long work keeps
    /// running. Omit to use `JCODE_SUBAGENT_STALL_SECS` (default 300s). 0 disables.
    #[serde(default)]
    stall_timeout_secs: Option<u64>,
    #[serde(default)]
    output_schema: Option<Value>,
    #[serde(rename = "command", default)]
    _command: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SubagentOutputMode {
    /// Return only the subagent's final answer plus metadata. This preserves the
    /// historical low-token default for ordinary delegation.
    #[default]
    Answer,
    /// Return the final answer plus a human-readable transcript similar to what
    /// a user would inspect: roles, text, tool calls, and tool results.
    Compact,
    /// Return the final answer plus the persisted raw child session messages as
    /// pretty JSON for debugging/auditing.
    FullTranscript,
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        "Run a subagent."
    }

    fn parameters_schema(&self) -> Value {
        subagent_parameters_schema()
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: SubagentInput = serde_json::from_value(input)?;

        let mut session = if let Some(session_id) = &params.session_id {
            Session::load(session_id).unwrap_or_else(|err| {
                logging::warn(&format!(
                    "[tool:subagent] failed to load existing session {}; creating a new subagent session instead: {}",
                    session_id, err
                ));
                Session::create(Some(ctx.session_id.clone()), Some(subagent_title(&params)))
            })
        } else {
            Session::create(Some(ctx.session_id.clone()), Some(subagent_title(&params)))
        };
        let parent_subagent_model = Self::preferred_parent_subagent_model(&ctx.session_id);
        let provider_model = self.provider.model();
        let mut resolved_model = Self::resolve_model(
            params.model.as_deref(),
            session.model.as_deref(),
            parent_subagent_model.as_deref(),
            &provider_model,
        );
        // Single chokepoint: resolve the worker's (model, route) and gate it
        // against cheap_route_ban in ONE place. A spawned worker forks the
        // coordinator's provider, whose default backend is the coordinator's own
        // (expensive) model; this is the only function that decides what backend
        // the fork actually uses, and it refuses rather than fall through to a
        // banned model. See cheap_route::resolve_worker_route.
        let (model, route_api) = crate::agent::cheap_route::resolve_worker_route(
            self.provider.as_ref(),
            &resolved_model,
            session.route_api_method.is_some(),
        )?;
        resolved_model = model;
        if let Some(api) = route_api {
            session.route_api_method = Some(api);
        }
        session.model = Some(resolved_model.clone());

        if let Some(ref working_dir) = ctx.working_dir {
            session.working_dir = Some(working_dir.display().to_string());
        }

        session.save()?;

        let mut allowed: HashSet<String> = self.registry.tool_names().await.into_iter().collect();
        for blocked in [
            "subagent",
            "task",
            "todo",
            "todowrite",
            "todoread",
            "cheap_route",
            "tournament",
        ] {
            allowed.remove(blocked);
        }
        crate::config::config()
            .tools
            .apply_to_allowed_set(&mut allowed);
        let allowed = prune_allowed_tools(allowed, params.allowed_tools.as_deref());

        let summary_map: Arc<Mutex<HashMap<String, ToolSummary>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let summary_map_handle = summary_map.clone();
        let session_id = session.id.clone();

        // Heartbeat for the stall watchdog: bumped whenever the child publishes ANY
        // progress event for its session (a tool start/finish OR an API/stream
        // status). The watchdog reads this to tell "genuinely working but slow"
        // apart from "hung". Shared with the run loop below.
        let last_activity: Arc<Mutex<tokio::time::Instant>> =
            Arc::new(Mutex::new(tokio::time::Instant::now()));
        let listener_last_activity = last_activity.clone();
        let listener_session_id = session_id.clone();
        let parent_session_id = ctx.session_id.clone();
        let description = params.description.clone();

        let mut receiver = Bus::global().subscribe();
        let listener = tokio::spawn(async move {
            let bump = |la: &Arc<Mutex<tokio::time::Instant>>| {
                *la.lock().unwrap_or_else(|p| p.into_inner()) = tokio::time::Instant::now();
            };
            loop {
                match receiver.recv().await {
                    Ok(BusEvent::ToolUpdated(event)) => {
                        if event.session_id != listener_session_id {
                            continue;
                        }
                        bump(&listener_last_activity);
                        let mut summary = summary_map_handle
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        summary.insert(
                            event.tool_call_id.clone(),
                            ToolSummary {
                                id: event.tool_call_id.clone(),
                                tool: event.tool_name.clone(),
                                state: ToolSummaryState {
                                    status: event.status.as_str().to_string(),
                                    title: if event.status.as_str() == "completed" {
                                        event.title.clone()
                                    } else {
                                        None
                                    },
                                },
                            },
                        );
                    }
                    Ok(BusEvent::SubagentStatus(status)) => {
                        if status.session_id == listener_session_id {
                            bump(&listener_last_activity);
                            Bus::global().publish(BusEvent::SubagentStatus(forward_subagent_status(
                                &parent_session_id,
                                &description,
                                &status,
                            )));
                        }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });

        logging::info(&format!(
            "Subagent starting: {} (type: {})",
            params.description, params.subagent_type
        ));

        // Run subagent on an isolated provider fork so model/session changes do not
        // mutate the coordinator's provider instance.
        let mut agent = Agent::new_with_session(
            self.provider.fork(),
            self.registry.clone(),
            session,
            Some(allowed),
        );
        // A spawned subagent may auto-switch to the next-cheapest healthy model if
        // its model rate-limits/quota-fails (e.g. GLM 429 under concurrent load),
        // instead of the subagent failing outright.
        agent.set_allow_auto_reroute(true);

        let augmented_prompt = if let Some(ref schema) = params.output_schema {
            format!(
                "{}\n\n## Output contract\nYour FINAL message must be exactly one JSON object (no prose, no code fences) conforming to this JSON Schema:\n{}",
                params.prompt,
                serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string())
            )
        } else {
            params.prompt.clone()
        };

        let start = std::time::Instant::now();
        // Inactivity watchdog: drive the subagent to completion, but if it makes no
        // progress (no heartbeat) for the resolved stall budget, abort so the
        // coordinator's tool call returns an error instead of blocking forever (the
        // hung-subagent freeze). Dropping the run future cancels the in-flight work;
        // we also fire graceful_shutdown for any cooperative cleanup.
        let run_outcome = match resolve_subagent_stall_timeout(params.stall_timeout_secs) {
            Some(stall) => {
                let poll = std::time::Duration::from_secs(5).min(stall);
                match run_until_complete_or_stalled(
                    agent.run_once_capture(&augmented_prompt),
                    last_activity.clone(),
                    stall,
                    poll,
                )
                .await
                {
                    StallResult::Completed(res) => res,
                    StallResult::Stalled { idle } => {
                        agent.request_graceful_shutdown();
                        logging::warn(&format!(
                            "[tool:subagent] stall watchdog fired: no progress for {}s (limit {}s) session_id={} description={}",
                            idle.as_secs(),
                            stall.as_secs(),
                            agent.session_id(),
                            params.description,
                        ));
                        Err(anyhow::anyhow!(
                            "subagent '{}' stalled: no progress for {}s (inactivity limit {}s); aborted so the coordinator is not blocked. Retry, raise stall_timeout_secs, or split the task.",
                            params.description,
                            idle.as_secs(),
                            stall.as_secs(),
                        ))
                    }
                }
            }
            None => agent.run_once_capture(&augmented_prompt).await,
        };
        let final_text = run_outcome.map_err(|err| {
            logging::warn(&format!(
                "[tool:subagent] subagent failed description={} type={} session_id={} model={} error={}",
                params.description,
                params.subagent_type,
                agent.session_id(),
                resolved_model,
                err
            ));
            err
        })?;
        let sub_session_id = agent.session_id().to_string();

        let (processed_text, structured_success) = if let Some(_) = params.output_schema {
            match enforce_structured_output(&final_text) {
                Ok(canonical) => (canonical, true),
                Err(err) => {
                    let prefixed = format!("[structured output requested but the final message was not valid JSON: {}]\n\n{}", err, final_text);
                    (prefixed, false)
                }
            }
        } else {
            (final_text, true)
        };

        let history = if params.output_mode == SubagentOutputMode::Compact {
            Some(agent.get_history())
        } else {
            None
        };
        let full_transcript = if params.output_mode == SubagentOutputMode::FullTranscript {
            let session = Session::load(&sub_session_id)?;
            Some(serde_json::to_string_pretty(&session.messages)?)
        } else {
            None
        };

        logging::info(&format!(
            "Subagent completed: {} in {:.1}s",
            params.description,
            start.elapsed().as_secs_f64()
        ));

        listener.abort();

        let mut summary: Vec<ToolSummary> = summary_map
            .lock()
            .map_err(|_| anyhow::anyhow!("tool summary lock poisoned"))?
            .values()
            .cloned()
            .collect();
        summary.sort_by(|a, b| a.id.cmp(&b.id));

        let output = format_subagent_output(
            &processed_text,
            &sub_session_id,
            params.output_mode,
            history.as_deref(),
            full_transcript.as_deref(),
        );

        let mut metadata = json!({
            "summary": summary,
            "sessionId": sub_session_id,
            "model": resolved_model,
            "outputMode": params.output_mode.as_str(),
        });
        if params.output_schema.is_some() {
            metadata["structured"] = json!(structured_success);
        }

        Ok(ToolOutput::new(output)
            .with_title(subagent_display_title(&params, &resolved_model))
            .with_metadata(metadata))
    }
}

fn subagent_parameters_schema() -> Value {
    json!({
        "type": "object",
        "required": ["description", "prompt", "subagent_type"],
        "properties": {
            "intent": super::intent_schema_property(),
            "description": {
                "type": "string",
                "description": "Task description."
            },
            "prompt": {
                "type": "string",
                "description": "Task prompt."
            },
            "subagent_type": {
                "type": "string",
                "description": "Subagent type."
            },
            "model": {
                "type": "string",
                "description": "Model override."
            },
            "session_id": {
                "type": "string",
                "description": "Existing session ID."
            },
            "output_mode": {
                "type": "string",
                "enum": ["answer", "compact", "full_transcript"],
                "description": "Return mode. 'answer' returns the final answer only, 'compact' adds a user-visible transcript, and 'full_transcript' adds raw persisted messages. Defaults to 'answer'."
            },
            "output_schema": {
                "type": "object",
                "description": "Optional JSON Schema. When set, the subagent must answer with a single JSON object; the result is parse-checked (structural JSON check, not full schema validation) and returned as canonical JSON."
            },
            "allowed_tools": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Optional subset of tool names this subagent may use. When set, the subagent's tools are intersected with this list (can only remove tools, never grant new ones)."
            },
            "stall_timeout_secs": {
                "type": "integer",
                "minimum": 0,
                "description": "Inactivity budget in seconds for this subagent: abort it only if it makes NO progress (no API call, stream, or tool event) for this long. This is a STALL guard, not a total-runtime cap — long-running work that keeps progressing is never killed. Set higher for tasks with long silent steps (e.g. multi-minute scans/builds), lower for quick tasks. Omit for the default (300s); 0 disables the guard."
            },
            "command": {
                "type": "string",
                "description": "Source command."
            }
        }
    })
}

fn subagent_title(params: &SubagentInput) -> String {
    format!(
        "{} (@{} subagent)",
        params.description, params.subagent_type
    )
}

/// Narrow an allowed-tool set to only the tools the caller requested. When
/// `requested` is `None`, the set is returned unchanged. Requested names that
/// are not already allowed are ignored (set intersection), so this can only ever
/// remove tools, never grant new ones.
fn prune_allowed_tools(
    mut allowed: std::collections::HashSet<String>,
    requested: Option<&[String]>,
) -> std::collections::HashSet<String> {
    if let Some(requested) = requested {
        let keep: std::collections::HashSet<&str> =
            requested.iter().map(String::as_str).collect();
        allowed.retain(|tool| keep.contains(tool.as_str()));
    }
    allowed
}

fn subagent_display_title(params: &SubagentInput, model: &str) -> String {
    format!(
        "{} ({} · {})",
        params.description, params.subagent_type, model
    )
}

impl SubagentOutputMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Compact => "compact",
            Self::FullTranscript => "full_transcript",
        }
    }
}

/// Best-effort structural check for schema-requested subagent output.
/// Strips one ```/```json fence pair if present, then requires valid JSON.
pub(super) fn enforce_structured_output(final_text: &str) -> Result<String, String> {
    let trimmed = final_text.trim();

    // Strip markdown fence if present
    let content = if trimmed.starts_with("```json") {
        let after_fence = trimmed.strip_prefix("```json").unwrap_or("").trim_start();
        after_fence.strip_suffix("```").unwrap_or(after_fence).trim()
    } else if trimmed.starts_with("```") {
        let after_fence = trimmed.strip_prefix("```").unwrap_or("").trim_start();
        after_fence.strip_suffix("```").unwrap_or(after_fence).trim()
    } else {
        trimmed
    };

    match serde_json::from_str::<Value>(content) {
        Ok(value) => {
            serde_json::to_string_pretty(&value)
                .map_err(|e| format!("Failed to serialize parsed JSON: {}", e))
        }
        Err(e) => Err(format!("Invalid JSON: {}", e)),
    }
}

fn format_subagent_output(
    final_text: &str,
    sub_session_id: &str,
    output_mode: SubagentOutputMode,
    history: Option<&[HistoryMessage]>,
    full_transcript: Option<&str>,
) -> String {
    let mut output = final_text.to_string();
    if !output.ends_with('\n') {
        output.push('\n');
    }

    match output_mode {
        SubagentOutputMode::Answer => {}
        SubagentOutputMode::Compact => {
            output.push_str("\n## Subagent transcript (compact)\n\n");
            output.push_str(&format_compact_subagent_history(history.unwrap_or(&[])));
        }
        SubagentOutputMode::FullTranscript => {
            output.push_str("\n## Subagent transcript (full)\n\n```json\n");
            output.push_str(full_transcript.unwrap_or("[]"));
            output.push_str("\n```\n");
        }
    }

    output.push('\n');
    output.push_str("<subagent_metadata>\n");
    output.push_str(&format!("session_id: {}\n", sub_session_id));
    output.push_str(&format!("output_mode: {}\n", output_mode.as_str()));
    output.push_str("</subagent_metadata>");
    output
}

fn format_compact_subagent_history(messages: &[HistoryMessage]) -> String {
    if messages.is_empty() {
        return "(empty transcript)\n".to_string();
    }

    let mut output = String::new();
    for (index, message) in messages.iter().enumerate() {
        output.push_str(&format!("### {}. {}\n\n", index + 1, message.role));
        if !message.content.trim().is_empty() {
            output.push_str(message.content.trim());
            output.push_str("\n\n");
        }
        if let Some(tool_calls) = &message.tool_calls
            && !tool_calls.is_empty()
        {
            output.push_str("Tool calls:\n");
            for call in tool_calls {
                output.push_str(&format!("- `{}`\n", call));
            }
            output.push('\n');
        }
        if let Some(tool_data) = &message.tool_data {
            output.push_str("Tool result:\n");
            output.push_str("```json\n");
            match serde_json::to_string_pretty(tool_data) {
                Ok(json) => output.push_str(&json),
                Err(_) => output.push_str("<unserializable tool data>"),
            }
            output.push_str("\n```\n\n");
        }
    }
    output
}

/// Default inactivity budget for a spawned subagent when neither the orchestrator
/// nor `JCODE_SUBAGENT_STALL_SECS` specify one.
const DEFAULT_SUBAGENT_STALL_SECS: u64 = 300;

/// Resolve the inactivity (stall) budget for a spawned subagent.
///
/// Precedence: explicit per-task `stall_timeout_secs` (set by the orchestrator) →
/// env `JCODE_SUBAGENT_STALL_SECS` → [`DEFAULT_SUBAGENT_STALL_SECS`]. A value of 0
/// (from either source) disables the watchdog entirely (unbounded — the old
/// behavior), returning `None`.
fn resolve_subagent_stall_timeout(override_secs: Option<u64>) -> Option<std::time::Duration> {
    let secs = override_secs.unwrap_or_else(|| {
        std::env::var("JCODE_SUBAGENT_STALL_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_SUBAGENT_STALL_SECS)
    });
    if secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(secs))
    }
}

/// Outcome of [`run_until_complete_or_stalled`].
enum StallResult<T> {
    /// The future finished on its own; carries its output.
    Completed(T),
    /// No heartbeat for `>= stall`; the future was dropped (cancelled).
    Stalled { idle: std::time::Duration },
}

/// Drive `fut` to completion, but give up (return [`StallResult::Stalled`]) if
/// `last_activity` is not bumped for `stall`. `poll` is how often the watchdog
/// wakes to check. Returning `Stalled` drops `fut`, cancelling its in-flight
/// `.await` chain. Generic over the future so it is unit-testable without a real
/// `Agent`.
async fn run_until_complete_or_stalled<F, T>(
    fut: F,
    last_activity: Arc<Mutex<tokio::time::Instant>>,
    stall: std::time::Duration,
    poll: std::time::Duration,
) -> StallResult<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(fut);
    loop {
        tokio::select! {
            biased;
            out = &mut fut => return StallResult::Completed(out),
            _ = tokio::time::sleep(poll) => {
                let idle = last_activity
                    .lock()
                    .map(|t| t.elapsed())
                    .unwrap_or(std::time::Duration::ZERO);
                if idle >= stall {
                    return StallResult::Stalled { idle };
                }
            }
        }
    }
}

/// Rebroadcast a child subagent's status under the parent session so the
/// parent TUI status line shows live subagent activity instead of a spinner.
fn forward_subagent_status(
    parent_session_id: &str,
    description: &str,
    status: &SubagentStatus,
) -> SubagentStatus {
    const LABEL_MAX_CHARS: usize = 40;
    let trimmed = description.trim();
    let label: String = if trimmed.chars().count() > LABEL_MAX_CHARS {
        trimmed.chars().take(LABEL_MAX_CHARS - 1).chain(std::iter::once('…')).collect()
    } else {
        trimmed.to_string()
    };
    SubagentStatus {
        session_id: parent_session_id.to_string(),
        status: format!("{label}: {}", status.status),
        model: status.model.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StallResult, SubagentInput, SubagentOutputMode, format_compact_subagent_history,
        format_subagent_output, prune_allowed_tools, resolve_subagent_stall_timeout,
        run_until_complete_or_stalled, subagent_display_title,
    };
    use crate::protocol::HistoryMessage;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn forward_subagent_status_relabels_under_parent_session() {
        let child = crate::bus::SubagentStatus {
            session_id: "child".to_string(),
            status: "running grep".to_string(),
            model: Some("haiku".to_string()),
        };
        let forwarded = super::forward_subagent_status("parent", "Fix unwraps", &child);
        assert_eq!(forwarded.session_id, "parent");
        assert_eq!(forwarded.status, "Fix unwraps: running grep");
        assert_eq!(forwarded.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn forward_subagent_status_truncates_long_descriptions() {
        let child = crate::bus::SubagentStatus {
            session_id: "child".to_string(),
            status: "streaming".to_string(),
            model: None,
        };
        let desc = "x".repeat(60);
        let forwarded = super::forward_subagent_status("parent", &desc, &child);
        assert_eq!(forwarded.session_id, "parent");
        assert!(forwarded.status.starts_with(&"x".repeat(39)));
        assert!(forwarded.status.ends_with("…: streaming"));
        assert!(forwarded.model.is_none());
    }

    #[test]
    fn subagent_display_title_includes_type_and_model() {
        let params = SubagentInput {
            description: "Verify subagent model".to_string(),
            prompt: "prompt".to_string(),
            subagent_type: "general".to_string(),
            model: None,
            session_id: None,
            output_mode: SubagentOutputMode::Answer,
            allowed_tools: None,
            stall_timeout_secs: None,
            output_schema: None,
            _command: None,
        };

        assert_eq!(
            subagent_display_title(&params, "gpt-5.4"),
            "Verify subagent model (general · gpt-5.4)"
        );
    }

    #[test]
    fn resolve_model_prefers_explicit_then_existing_then_parent_then_provider() {
        assert_eq!(
            super::SubagentTool::resolve_model(
                Some("explicit"),
                Some("existing"),
                Some("parent"),
                "provider"
            ),
            "explicit"
        );
        assert_eq!(
            super::SubagentTool::resolve_model(None, Some("existing"), Some("parent"), "provider"),
            "existing"
        );
        assert_eq!(
            super::SubagentTool::resolve_model(None, None, Some("parent"), "provider"),
            "parent"
        );
        let configured_or_provider = crate::config::config()
            .agents
            .swarm_model
            .as_deref()
            .unwrap_or("provider");
        assert_eq!(
            super::SubagentTool::resolve_model(None, None, None, "provider"),
            configured_or_provider
        );
    }

    #[test]
    fn format_subagent_output_preserves_answer_without_generic_next_step_footer() {
        let output = format_subagent_output(
            "answer",
            "session_test",
            SubagentOutputMode::Answer,
            None,
            None,
        );

        assert!(output.starts_with("answer\n\n<subagent_metadata>\n"));
        assert!(output.contains("session_id: session_test\n"));
        assert!(output.contains("output_mode: answer\n"));
        assert!(!output.contains("Next step: integrate this result"));
    }

    #[test]
    fn compact_output_includes_human_readable_history() {
        let history = vec![HistoryMessage {
            role: "assistant".to_string(),
            content: "I will inspect it.".to_string(),
            tool_calls: Some(vec!["read".to_string()]),
            tool_data: None,
        }];
        let output = format_subagent_output(
            "final answer",
            "session_test",
            SubagentOutputMode::Compact,
            Some(&history),
            None,
        );

        assert!(output.contains("## Subagent transcript (compact)"));
        assert!(output.contains("### 1. assistant"));
        assert!(output.contains("I will inspect it."));
        assert!(output.contains("- `read`"));
        assert!(output.contains("output_mode: compact\n"));
    }

    #[test]
    fn full_transcript_output_includes_raw_json_section() {
        let output = format_subagent_output(
            "final answer",
            "session_test",
            SubagentOutputMode::FullTranscript,
            None,
            Some("[{\"role\":\"user\"}]"),
        );

        assert!(output.contains("## Subagent transcript (full)"));
        assert!(output.contains("```json\n[{\"role\":\"user\"}]\n```"));
        assert!(output.contains("output_mode: full_transcript\n"));
    }

    #[test]
    fn compact_history_formats_empty_transcript() {
        assert_eq!(format_compact_subagent_history(&[]), "(empty transcript)\n");
    }

    #[test]
    fn prune_allowed_tools_intersects_with_requested() {
        use std::collections::HashSet;
        let allowed: HashSet<String> = ["read", "grep", "bash", "write"]
            .into_iter()
            .map(String::from)
            .collect();

        let requested = vec!["read".to_string(), "grep".to_string()];
        let pruned = super::prune_allowed_tools(allowed, Some(&requested));

        let mut got: Vec<String> = pruned.into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["grep".to_string(), "read".to_string()]);
    }

    #[test]
    fn prune_allowed_tools_none_keeps_everything() {
        use std::collections::HashSet;
        let allowed: HashSet<String> =
            ["read", "grep"].into_iter().map(String::from).collect();

        let pruned = super::prune_allowed_tools(allowed.clone(), None);
        assert_eq!(pruned, allowed);
    }

    #[test]
    fn prune_allowed_tools_ignores_unknown_requested() {
        use std::collections::HashSet;
        let allowed: HashSet<String> =
            ["read", "grep"].into_iter().map(String::from).collect();

        // "bash" is not in the allowed set; intersection just ignores it.
        let requested = vec!["read".to_string(), "bash".to_string()];
        let pruned = super::prune_allowed_tools(allowed, Some(&requested));

        let got: Vec<String> = pruned.into_iter().collect();
        assert_eq!(got, vec!["read".to_string()]);
    }

    // --- stall watchdog ---

    #[test]
    fn stall_timeout_orchestrator_override_wins() {
        // The per-task value the orchestrator passes takes precedence (and ignores
        // any env). This is the "orchestrator decides per task" contract.
        assert_eq!(
            resolve_subagent_stall_timeout(Some(90)),
            Some(Duration::from_secs(90))
        );
    }

    #[test]
    fn stall_timeout_zero_disables_watchdog() {
        // 0 means "no stall guard" — unbounded, like the pre-watchdog behavior.
        assert_eq!(resolve_subagent_stall_timeout(Some(0)), None);
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_returns_completion_for_a_fast_task() {
        let last_activity = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let fut = async {
            tokio::time::sleep(Duration::from_secs(8)).await;
            7_i32
        };
        let res = run_until_complete_or_stalled(
            fut,
            last_activity,
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .await;
        assert!(matches!(res, StallResult::Completed(7)));
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_aborts_a_stalled_task() {
        let last_activity = Arc::new(Mutex::new(tokio::time::Instant::now()));
        // A future that never makes progress and never completes.
        let fut = std::future::pending::<i32>();
        let res = run_until_complete_or_stalled(
            fut,
            last_activity,
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .await;
        match res {
            StallResult::Stalled { idle } => assert!(idle >= Duration::from_secs(30)),
            StallResult::Completed(_) => panic!("expected the watchdog to abort a stalled task"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn watchdog_keeps_a_heartbeating_long_task_alive() {
        // The key guarantee: a task that runs WAY past the stall budget but keeps
        // emitting heartbeats is NOT killed — only true inactivity aborts.
        let last_activity = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let beat = last_activity.clone();
        let fut = async move {
            // 30s of work (>> 12s stall budget), heartbeating every 5s.
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_secs(5)).await;
                *beat.lock().unwrap() = tokio::time::Instant::now();
            }
            99_i32
        };
        let res = run_until_complete_or_stalled(
            fut,
            last_activity,
            Duration::from_secs(12),
            Duration::from_secs(5),
        )
        .await;
        assert!(
            matches!(res, StallResult::Completed(99)),
            "a continuously-heartbeating task must run to completion despite exceeding the stall budget"
        );
    }

    #[test]
    fn enforce_structured_output_accepts_valid_json() {
        let input = r#"{"key": "value", "number": 42}"#;
        let result = super::enforce_structured_output(input);
        assert!(result.is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["key"], "value");
        assert_eq!(parsed["number"], 42);
    }

    #[test]
    fn enforce_structured_output_strips_json_fence() {
        let input = r#"```json
{"key": "value"}
```"#;
        let result = super::enforce_structured_output(input);
        assert!(result.is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn enforce_structured_output_strips_code_fence() {
        let input = r#"```
{"key": "value"}
```"#;
        let result = super::enforce_structured_output(input);
        assert!(result.is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn enforce_structured_output_rejects_invalid_json() {
        let input = r#"this is not json"#;
        let result = super::enforce_structured_output(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid JSON"));
    }

    #[test]
    fn parameters_schema_includes_output_schema_property() {
        let schema = super::subagent_parameters_schema();
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("output_schema"));
        assert_eq!(props["output_schema"]["type"], "object");
        assert!(props["output_schema"]["description"]
            .as_str()
            .unwrap()
            .contains("JSON Schema"));
    }
}
