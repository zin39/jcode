//! Parallel tool execution.
//!
//! Independent tool calls within one assistant turn are normally executed
//! serially. The slow ones — `subagent` spawns especially — dominate wall-clock,
//! so running a turn's parallel-SAFE tool calls concurrently is a large speedup
//! for fan-out work.
//!
//! Safety model (verified against the turn loops + `tool::batch`):
//!   - `Registry::execute(&self, ...)` is concurrency-safe (read-lock, Arc-cloned).
//!   - `ToolContext` is `Clone` with no shared `&mut` state.
//!   - The ONLY serial constraint is appending results to the session, which must
//!     stay in tool-call order (`tool_use_id` ↔ `tool_result` matching). So we run
//!     execution concurrently here and the caller appends results IN ORDER after.
//!
//! Only side-effect-free / isolated tools are eligible (see
//! [`is_parallel_safe_tool`]); anything that mutates shared local state
//! (file writes, `bash`, session/todo/memory/goal) stays serial.

/// Tools that are safe to run concurrently: read-only queries plus `subagent`
/// (which runs in its OWN isolated session + forked provider, so two subagents
/// never touch each other's or the parent's mutable state). Deliberately
/// conservative — anything not listed here runs serially.
const PARALLEL_SAFE_TOOLS: &[&str] = &[
    "subagent",
    "read",
    "grep",
    "agentgrep",
    "glob",
    "ls",
    "websearch",
    "webfetch",
    "codesearch",
    "session_search",
    "lsp",
];

/// Whether a tool may run concurrently with other tools in the same turn.
pub(crate) fn is_parallel_safe_tool(name: &str) -> bool {
    PARALLEL_SAFE_TOOLS.contains(&name)
}

/// Run `items` concurrently through `run`, returning results in the ORIGINAL
/// input order regardless of completion order. This is the slow, concurrent part
/// of parallel tool execution; the caller appends the ordered results to the
/// session serially afterwards (preserving the `tool_use_id` ↔ `tool_result`
/// contract). Generic over the work fn so the ordering/concurrency is unit-
/// testable without a live `Registry`.
pub(crate) async fn run_in_parallel_ordered<I, R, F, Fut>(items: Vec<I>, run: F) -> Vec<R>
where
    F: Fn(usize, I) -> Fut,
    Fut: std::future::Future<Output = R>,
{
    use futures::stream::{FuturesUnordered, StreamExt};

    let mut stream: FuturesUnordered<_> = items
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let fut = run(index, item);
            async move { (index, fut.await) }
        })
        .collect();

    let mut out: Vec<(usize, R)> = Vec::with_capacity(stream.len());
    while let Some(indexed) = stream.next().await {
        out.push(indexed);
    }
    // Restore original order — completion order is nondeterministic.
    out.sort_by_key(|(index, _)| *index);
    out.into_iter().map(|(_, r)| r).collect()
}

use crate::agent::Agent;
use crate::tool::{ToolContext, ToolExecutionMode, ToolOutput};
use jcode_message_types::ToolCall;
use std::collections::HashMap;
use std::path::PathBuf;

impl Agent {
    /// Pre-execute the turn's parallel-SAFE tool calls CONCURRENTLY, returning a
    /// map of `tool_call_id -> result`. The turn loop then consumes these (exactly
    /// like an SDK-precomputed result) instead of awaiting each serially — so the
    /// slow work overlaps while the loop's append/order/event semantics are
    /// unchanged. Returns an empty map (no behavior change) unless at least two
    /// eligible tools are present, since one tool gains nothing from this path.
    ///
    /// Conservative activation: we only parallelize when EVERY tool call in the
    /// batch is parallel-safe (and valid / not SDK-executed / allowed). If any
    /// mutating tool is present we stay fully serial, so concurrent reads can
    /// never be reordered ahead of a write in the same turn. Each tool must be
    /// parallel-safe, have no validation error, not already be SDK-executed, and
    /// be allowed for this session.
    pub(crate) async fn precompute_parallel_safe_tools(
        &self,
        tool_calls: &[ToolCall],
        assistant_message_id: Option<&str>,
        sdk_tool_results: &HashMap<String, (String, bool)>,
    ) -> HashMap<String, anyhow::Result<ToolOutput>> {
        let all_eligible = tool_calls.len() >= 2
            && tool_calls.iter().all(|tc| {
                is_parallel_safe_tool(&tc.name)
                    && tc.validation_error().is_none()
                    && !sdk_tool_results.contains_key(&tc.id)
                    && self.validate_tool_allowed(&tc.name).is_ok()
            });
        if !all_eligible {
            return HashMap::new();
        }

        let jobs: Vec<(String, String, serde_json::Value, ToolContext)> = tool_calls
            .iter()
            .map(|tc| {
                let message_id = assistant_message_id
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| self.session.id.clone());
                let ctx = ToolContext {
                    session_id: self.session.id.clone(),
                    message_id,
                    tool_call_id: tc.id.clone(),
                    working_dir: self.working_dir().map(PathBuf::from),
                    stdin_request_tx: self.stdin_request_tx.clone(),
                    graceful_shutdown_signal: Some(self.graceful_shutdown.clone()),
                    execution_mode: ToolExecutionMode::AgentTurn,
                };
                (tc.id.clone(), tc.name.clone(), tc.input.clone(), ctx)
            })
            .collect();

        crate::logging::info(&format!(
            "Parallel tools: pre-executing {} parallel-safe tool call(s) concurrently",
            jobs.len()
        ));
        let registry = self.registry.clone();
        let results =
            run_in_parallel_ordered(jobs, move |_index, (id, name, input, ctx)| {
                let registry = registry.clone();
                async move {
                    let result = registry.execute(&name, input, ctx).await;
                    (id, result)
                }
            })
            .await;
        results.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{is_parallel_safe_tool, run_in_parallel_ordered};
    use std::time::Duration;

    #[test]
    fn classifier_marks_subagent_and_reads_safe() {
        assert!(is_parallel_safe_tool("subagent"));
        assert!(is_parallel_safe_tool("read"));
        assert!(is_parallel_safe_tool("websearch"));
        // Mutating / side-effecting tools are never parallel-safe.
        assert!(!is_parallel_safe_tool("write"));
        assert!(!is_parallel_safe_tool("edit"));
        assert!(!is_parallel_safe_tool("bash"));
        assert!(!is_parallel_safe_tool("memory"));
        assert!(!is_parallel_safe_tool("todo"));
        assert!(!is_parallel_safe_tool("unknown_tool"));
    }

    #[tokio::test(start_paused = true)]
    async fn preserves_input_order_despite_reverse_completion() {
        // Item 0 finishes LAST, item 2 finishes FIRST. If results came back in
        // completion order they'd be reversed; the helper must restore [0,1,2].
        let items = vec![0u64, 1, 2];
        let results = run_in_parallel_ordered(items, |_idx, item| async move {
            // earlier items sleep longer → complete later
            tokio::time::sleep(Duration::from_secs(30 - item * 10)).await;
            item * 100
        })
        .await;
        assert_eq!(results, vec![0, 100, 200]);
    }

    #[tokio::test(start_paused = true)]
    async fn runs_concurrently_not_serially() {
        // Three 10s tasks: serial would be 30s, concurrent ~10s. With paused
        // time, assert the virtual clock only advanced ~10s (concurrent), proving
        // the futures ran at the same time rather than one-after-another.
        let start = tokio::time::Instant::now();
        let results =
            run_in_parallel_ordered(vec![(), (), ()], |idx, _| async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                idx
            })
            .await;
        let elapsed = start.elapsed();
        assert_eq!(results, vec![0, 1, 2]);
        assert!(
            elapsed < Duration::from_secs(15),
            "expected ~10s concurrent, got {elapsed:?} (serial would be 30s)"
        );
    }

    #[tokio::test]
    async fn empty_input_yields_empty_output() {
        let results: Vec<u8> = run_in_parallel_ordered(Vec::<u8>::new(), |_, x| async move { x }).await;
        assert!(results.is_empty());
    }
}
