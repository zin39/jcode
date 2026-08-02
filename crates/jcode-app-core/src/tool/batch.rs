use super::{Registry, Tool, ToolContext, ToolOutput};
use crate::bus::{BatchSubcallProgress, BatchSubcallState};
use crate::message::ToolCall;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

const MAX_PARALLEL: usize = 10;

pub(crate) fn generic_batch_schema() -> Value {
    json!({
        "type": "object",
        "required": ["tool_calls"],
        "properties": {
            "intent": super::intent_schema_property(),
            "tool_calls": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["tool", "intent"],
                    "properties": {
                        "tool": {
                            "type": "string",
                            "description": "Tool name."
                        },
                        "intent": super::intent_schema_property()
                    },
                    "additionalProperties": true
                },
                "minItems": 1,
                "maxItems": 10
            }
        }
    })
}

fn ordered_batch_subcalls(
    subcalls: &[(usize, String, Value)],
    running: &HashMap<usize, ToolCall>,
    failures: &HashMap<usize, bool>,
) -> Vec<BatchSubcallProgress> {
    let mut ordered: Vec<BatchSubcallProgress> = subcalls
        .iter()
        .map(|(i, tool_name, parameters)| {
            let tool_call = running.get(i).cloned().unwrap_or_else(|| ToolCall {
                id: format!("batch-{}-{}", i + 1, tool_name),
                name: tool_name.clone(),
                input: parameters.clone(),
                intent: ToolCall::intent_from_input(parameters),
                thought_signature: None,
            });
            let state = if running.contains_key(i) {
                BatchSubcallState::Running
            } else if failures.get(i).copied().unwrap_or(false) {
                BatchSubcallState::Failed
            } else {
                BatchSubcallState::Succeeded
            };

            BatchSubcallProgress {
                index: i + 1,
                tool_call,
                state,
            }
        })
        .collect();
    ordered.sort_by_key(|entry| entry.index);
    ordered
}

pub struct BatchTool {
    registry: Registry,
}

impl BatchTool {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }
}

#[derive(Deserialize)]
struct BatchInput {
    tool_calls: Vec<BatchEntry>,
}

/// One entry of a batch, parsed so that a single malformed entry cannot
/// discard the entries beside it.
///
/// A batch is a bundle of *independent* calls, so one entry failing to parse
/// says nothing about the others. Deserializing straight into
/// `Vec<ToolCallInput>` made the whole tool call fail with a message like
/// "missing field `tool`" that named neither the offending entry nor the work
/// that was thrown away, so the model had to re-issue every sibling call to
/// retry the one it got wrong.
#[derive(Clone)]
enum BatchEntry {
    Call(ToolCallInput),
    /// Entry that could not be understood, kept so it can be reported in place
    /// (with its original position) instead of aborting its siblings.
    Malformed(String),
}

impl<'de> Deserialize<'de> for BatchEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let keys = describe_entry_keys(&value);
        Ok(match serde_json::from_value::<ToolCallInput>(value) {
            Ok(call) => BatchEntry::Call(call),
            Err(e) => BatchEntry::Malformed(format!(
                "{e}. Each entry needs a \"tool\" naming the tool to run, with that tool's own \
                 arguments beside it{keys}"
            )),
        })
    }
}

/// Render an entry's keys so a malformed-entry message points at the actual
/// payload the model sent rather than making it guess which entry broke.
fn describe_entry_keys(value: &Value) -> String {
    let Some(obj) = value.as_object() else {
        return String::new();
    };
    if obj.is_empty() {
        return String::new();
    }
    let keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    format!(" (this entry only had: {})", keys.join(", "))
}

/// One line per malformed entry, in the order the caller sent them.
fn ordered_malformed_report(malformed: &HashMap<usize, String>) -> String {
    let mut entries: Vec<(&usize, &String)> = malformed.iter().collect();
    entries.sort_by_key(|(i, _)| **i);
    entries
        .into_iter()
        .map(|(i, reason)| format!("[{}] {}", i + 1, reason))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
impl BatchEntry {
    fn as_call(&self) -> Option<&ToolCallInput> {
        match self {
            BatchEntry::Call(call) => Some(call),
            BatchEntry::Malformed(_) => None,
        }
    }
}

#[derive(Deserialize, Clone)]
struct ToolCallInput {
    #[serde(alias = "name")]
    tool: String,
    #[serde(default)]
    parameters: Option<Value>,
}

impl ToolCallInput {
    fn resolved_parameters(self) -> (String, Value) {
        if let Some(params) = self.parameters {
            return (self.tool, params);
        }
        (self.tool, Value::Object(Default::default()))
    }
}

/// Try to fix common LLM mistakes in batch tool_calls:
/// - `tool_calls` sent as a JSON-encoded string instead of a real array
/// - Parameters placed at the same level as "tool" instead of nested under "parameters"
/// - "name" used instead of "tool" for the tool name key
/// - "arguments", "args", or "input" used instead of "parameters"
fn normalize_batch_input(mut input: Value) -> Value {
    // Some providers hand back tool arguments with the array re-encoded as a
    // string ("[{\"tool\": ...}]"). The payload is otherwise well formed, so
    // decode it rather than failing the call with "invalid type: string".
    if let Some(obj) = input.as_object_mut()
        && let Some(raw) = obj.get("tool_calls").and_then(Value::as_str)
        && let Ok(decoded @ Value::Array(_)) = serde_json::from_str::<Value>(raw)
    {
        obj.insert("tool_calls".to_string(), decoded);
    }

    if let Some(calls) = input.get_mut("tool_calls").and_then(|v| v.as_array_mut()) {
        for call in calls.iter_mut() {
            if let Some(obj) = call.as_object_mut() {
                // Normalize "name" -> "tool" if the model used the wrong key
                if !obj.contains_key("tool")
                    && let Some(name_val) = obj.remove("name")
                {
                    obj.insert("tool".to_string(), name_val);
                }

                if !obj.contains_key("parameters") {
                    for alias in ["arguments", "args", "input"] {
                        if let Some(alias_val) = obj.remove(alias) {
                            obj.insert("parameters".to_string(), alias_val);
                            break;
                        }
                    }
                }

                // Canonical batch calls may keep the display intent beside
                // `parameters`. Forward it into the effective tool input so
                // live progress events and the nested tool execution retain it.
                let top_level_intent = obj
                    .get("intent")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|intent| !intent.is_empty())
                    .map(ToString::to_string);
                if let Some(intent) = top_level_intent
                    && let Some(params) = obj.get_mut("parameters").and_then(Value::as_object_mut)
                {
                    params.insert("intent".to_string(), Value::String(intent));
                }

                if !obj.contains_key("parameters") && obj.contains_key("tool") {
                    let tool_name = obj.get("tool").cloned();
                    let mut params = serde_json::Map::new();
                    let keys: Vec<String> = obj.keys().filter(|k| *k != "tool").cloned().collect();
                    for key in keys {
                        if let Some(val) = obj.remove(&key) {
                            params.insert(key, val);
                        }
                    }
                    if !params.is_empty() {
                        obj.insert("parameters".to_string(), Value::Object(params));
                    }
                    if let Some(name) = tool_name {
                        obj.insert("tool".to_string(), name);
                    }
                }
            }
        }
    }
    input
}

#[async_trait]
impl Tool for BatchTool {
    fn name(&self) -> &str {
        "batch"
    }

    fn description(&self) -> &str {
        "Run tools in parallel."
    }

    fn parameters_schema(&self) -> Value {
        generic_batch_schema()
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let input = normalize_batch_input(input);
        let params: BatchInput = serde_json::from_value(input)?;

        if params.tool_calls.is_empty() {
            return Err(anyhow::anyhow!("No tool calls provided"));
        }

        if params.tool_calls.len() > MAX_PARALLEL {
            return Err(anyhow::anyhow!(
                "Maximum {} parallel tool calls allowed",
                MAX_PARALLEL
            ));
        }

        // Check for disallowed tools
        for tc in &params.tool_calls {
            if let BatchEntry::Call(tc) = tc
                && Registry::resolve_tool_name(&tc.tool) == "batch"
            {
                return Err(anyhow::anyhow!("Cannot batch the 'batch' tool"));
            }
        }

        // Execute all tools in parallel, emitting progress events as each completes
        let num_tools = params.tool_calls.len();
        use futures::StreamExt;
        // Malformed entries keep their slot so results stay aligned with the
        // positions the caller sent, and so each bad entry is reported next to
        // the sibling results that did run.
        let mut malformed: HashMap<usize, String> = HashMap::new();
        let subcalls: Vec<(usize, String, Value)> = params
            .tool_calls
            .into_iter()
            .enumerate()
            .filter_map(|(i, tc)| match tc {
                BatchEntry::Call(tc) => {
                    let (tool_name, parameters) = tc.resolved_parameters();
                    let tool_name = Registry::resolve_tool_name(&tool_name).to_string();
                    Some((i, tool_name, parameters))
                }
                BatchEntry::Malformed(reason) => {
                    malformed.insert(i, reason);
                    None
                }
            })
            .collect();

        if subcalls.is_empty() {
            let detail = ordered_malformed_report(&malformed);
            return Err(anyhow::anyhow!(
                "No usable tool calls in this batch.\n{detail}"
            ));
        }

        let mut running: HashMap<usize, ToolCall> = subcalls
            .iter()
            .map(|(i, tool_name, parameters)| {
                (
                    *i,
                    ToolCall {
                        id: format!("batch-{}-{}", i + 1, tool_name),
                        name: tool_name.clone(),
                        input: parameters.clone(),
                        intent: ToolCall::intent_from_input(parameters),
                        thought_signature: None,
                    },
                )
            })
            .collect();

        crate::bus::Bus::global().publish(crate::bus::BusEvent::BatchProgress(
            crate::bus::BatchProgress {
                session_id: ctx.session_id.clone(),
                tool_call_id: ctx.tool_call_id.clone(),
                total: num_tools,
                completed: 0,
                last_completed: None,
                running: running.values().cloned().collect(),
                subcalls: ordered_batch_subcalls(&subcalls, &running, &HashMap::new()),
            },
        ));

        let mut stream: futures::stream::FuturesUnordered<_> = subcalls
            .iter()
            .map(|(i, tool_name, parameters)| {
                let registry = self.registry.clone();
                let i = *i;
                let tool_name = tool_name.clone();
                let parameters = parameters.clone();
                let sub_ctx = ctx.for_subcall(format!("batch-{}-{}", i + 1, tool_name.clone()));
                async move {
                    let result = registry.execute(&tool_name, parameters, sub_ctx).await;
                    (i, tool_name, result)
                }
            })
            .collect();

        let mut results: Vec<(usize, String, Result<ToolOutput>)> = Vec::with_capacity(num_tools);
        let mut failures: HashMap<usize, bool> = HashMap::new();
        let mut completed_count = 0usize;
        while let Some((i, tool_name, result)) = stream.next().await {
            completed_count += 1;
            let failed = result.is_err();
            running.remove(&i);
            failures.insert(i, failed);
            crate::bus::Bus::global().publish(crate::bus::BusEvent::BatchProgress(
                crate::bus::BatchProgress {
                    session_id: ctx.session_id.clone(),
                    tool_call_id: ctx.tool_call_id.clone(),
                    total: num_tools,
                    completed: completed_count,
                    last_completed: Some(tool_name.clone()),
                    running: running.values().cloned().collect(),
                    subcalls: ordered_batch_subcalls(&subcalls, &running, &failures),
                },
            ));
            results.push((i, tool_name, result));
        }
        // Restore original order
        results.sort_by_key(|(i, _, _)| *i);

        // Format results
        let mut output = String::new();
        let mut success_count = 0;
        let mut error_count = 0;
        let mut failed_tools = Vec::new();

        // Merge executed results with the malformed entries that never ran, so
        // the caller sees one entry per slot it sent, in the order it sent them.
        let mut results = results.into_iter().peekable();
        for i in 0..num_tools {
            if let Some(reason) = malformed.get(&i) {
                error_count += 1;
                failed_tools.push(format!("[{}] <malformed>", i + 1));
                output.push_str(&format!(
                    "--- [{}] (malformed entry, not run) ---\nError: {}\n\n",
                    i + 1,
                    reason
                ));
                continue;
            }
            let Some((_, tool_name, result)) = results.next() else {
                continue;
            };
            output.push_str(&format!("--- [{}] {} ---\n", i + 1, tool_name));
            match result {
                Ok(out) => {
                    success_count += 1;
                    let max_per_tool = 50_000 / num_tools.max(1);
                    if out.output.len() > max_per_tool {
                        output.push_str(crate::util::truncate_str(&out.output, max_per_tool));
                        output.push_str("...\n(truncated)");
                    } else {
                        output.push_str(&out.output);
                    }
                }
                Err(e) => {
                    error_count += 1;
                    failed_tools.push(tool_name.clone());
                    output.push_str(&format!("Error: {}", e));
                }
            }
            output.push_str("\n\n");
        }

        if error_count > 0 {
            crate::logging::warn(&format!(
                "[tool:batch] {} of {} subcalls failed for {} in session {}: {}",
                error_count,
                num_tools,
                ctx.tool_call_id,
                ctx.session_id,
                failed_tools.join(", ")
            ));
        }

        output.push_str(&format!(
            "Completed: {} succeeded, {} failed",
            success_count, error_count
        ));

        Ok(ToolOutput::new(output))
    }
}

#[cfg(test)]
#[path = "batch_tests.rs"]
mod batch_tests;
