# Cheap-Routing Tool — Implementation Plan (Plan 5 of 5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make cheap-routing reachable: register a `cheap_route` tool that the parent model invokes to offload a task to the cheapest capable model (decompose → recommend → run subagents → review), wrapping `run_cheap_route` + `ProviderCheapBackend`.

**Architecture:** A new tool `CheapRouteTool` in `crates/jcode-app-core/src/tool/cheap_route_tool.rs`, mirroring `SubagentTool` (holds `Arc<dyn Provider>` + `Registry`). Registered in `Registry::new` next to `subagent`. Zero changes to the TUI or the interactive turn loop — it rides the existing async tool-dispatch path, so it cannot affect existing flows unless the model calls it. Recursive use is blocked (a subagent / cheap-routed subtask cannot itself call `cheap_route`). Tests are deterministic (output formatting + recursion-block list); the orchestration and provider delegation are already tested in Plans 3–4.

**Tech Stack:** Rust edition 2024, the `Tool` trait, `async_trait`, `serde`, `crate::agent::cheap_route::{run_cheap_route, ProviderCheapBackend, CheapRouteOutcome, SubtaskResult}`.

**Run cargo with:** `. "$HOME/.cargo/env" && cargo test -p jcode-app-core ...`

**Spec:** `docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md` — the trigger/entry to the cheap-routing flow.

---

## File Structure

- Create: `crates/jcode-app-core/src/tool/cheap_route_tool.rs` — the `CheapRouteTool` + the `format_cheap_outcome` helper + tests.
- Modify: `crates/jcode-app-core/src/tool/mod.rs` — declare `mod cheap_route_tool;`, register the tool in `Registry::new`, and clone `provider` for the existing `subagent` registration.
- Modify: `crates/jcode-app-core/src/tool/task.rs` — add `"cheap_route"` to the two recursion-block lists (`SubagentTool::execute` and `ProviderCheapBackend::run_subtask`).

---

### Task 1: Add `"cheap_route"` to recursion-block lists

**Files:**
- Modify: `crates/jcode-app-core/src/tool/task.rs`
- Modify: `crates/jcode-app-core/src/agent/cheap_route.rs`

- [ ] **Step 1: Block in `SubagentTool::execute`**

In `crates/jcode-app-core/src/tool/task.rs`, find:

```rust
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread"] {
```

Replace with:

```rust
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread", "cheap_route"] {
```

- [ ] **Step 2: Block in `ProviderCheapBackend::run_subtask`**

In `crates/jcode-app-core/src/agent/cheap_route.rs`, find (inside `run_subtask`):

```rust
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread"] {
```

Replace with:

```rust
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread", "cheap_route"] {
```

- [ ] **Step 3: Verify it still compiles**

Run: `. "$HOME/.cargo/env" && cargo build -p jcode-app-core`
Expected: builds (the `cheap_route` tool doesn't exist yet; these are just string-array edits).

---

### Task 2: Create `CheapRouteTool` with a failing test

**Files:**
- Create: `crates/jcode-app-core/src/tool/cheap_route_tool.rs`
- Modify: `crates/jcode-app-core/src/tool/mod.rs`

- [ ] **Step 1: Declare the module**

In `crates/jcode-app-core/src/tool/mod.rs`, find the module declarations near the top (lines like `mod task;`, `mod batch;`, `mod skill;`). Add:

```rust
mod cheap_route_tool;
```

- [ ] **Step 2: Write the tool module with its test**

Create `crates/jcode-app-core/src/tool/cheap_route_tool.rs` with exactly:

```rust
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
            "### {}. {}\n\n{}\n\nReview: {}\n\n",
            index + 1,
            result.description,
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
            }],
        };

        let rendered = format_cheap_outcome(&outcome);
        assert!(rendered.contains("cheapo"));
        assert!(rendered.contains("edit auth"));
        assert!(rendered.contains("did it"));
        assert!(rendered.contains("Review: OK"));
    }
}
```

- [ ] **Step 3: Run the new test**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core format_cheap_outcome`
Expected: FAIL first only if not yet compiling; once it compiles, `format_cheap_outcome_lists_subtasks_and_reviews` PASSES. (The tool isn't registered yet — that's Task 3 — but the module + test compile and run independently.)

---

### Task 3: Register the tool in `Registry::new`

**Files:**
- Modify: `crates/jcode-app-core/src/tool/mod.rs`

- [ ] **Step 1: Clone provider for the subagent registration and add the cheap_route registration**

In `crates/jcode-app-core/src/tool/mod.rs`, find (around line 292-296):

```rust
        Self::insert_tool(
            &mut tools_map,
            "subagent",
            task::SubagentTool::new(provider, registry.clone()),
        );
```

Replace with:

```rust
        Self::insert_tool(
            &mut tools_map,
            "subagent",
            task::SubagentTool::new(provider.clone(), registry.clone()),
        );
        Self::insert_tool(
            &mut tools_map,
            "cheap_route",
            cheap_route_tool::CheapRouteTool::new(provider, registry.clone()),
        );
```

(`provider` is `Arc<dyn Provider>`; `.clone()` is a cheap refcount bump. The `subagent` line now clones so `provider` can be moved into `CheapRouteTool`.)

- [ ] **Step 2: Build and test the whole crate**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core cheap_route`
Expected: PASS — all `cheap_route` orchestrator tests (8) plus the tool's `format_cheap_outcome_lists_subtasks_and_reviews`.

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core 2>&1 | tail -20`
Expected: only the known pre-existing flaky failures in `tool::bash::*` and `server::*` (files untouched by this work). No NEW failures.

- [ ] **Step 3: Build the whole workspace (production-safety gate)**

Run: `. "$HOME/.cargo/env" && cargo build 2>&1 | tail -15`
Expected: the full workspace (including the `jcode` binary) compiles successfully. This proves the new tool integrates without breaking the build.

- [ ] **Step 4: Commit**

```bash
git add crates/jcode-app-core/src/tool/cheap_route_tool.rs crates/jcode-app-core/src/tool/mod.rs crates/jcode-app-core/src/tool/task.rs crates/jcode-app-core/src/agent/cheap_route.rs
git commit -m "feat(tool): add cheap_route tool to reach the cheap-routing engine

Cheap-routing Plan 5: register a cheap_route tool (mirrors subagent) that the
parent model invokes to offload a task to the cheapest capable model via
run_cheap_route + ProviderCheapBackend. Recursive use is blocked. Additive:
no turn-loop/TUI changes, only runs when invoked.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:** The trigger/entry to the cheap-routing flow is realized by `CheapRouteTool`. Combined with Plans 1–4, the full engine (rank → decompose → recommend → spawn pruned subagents → review) is now reachable end-to-end through one tool call. The interactive `/cheap` toggle UX is a thin future nicety on top of this capability (not required for the feature to work).

**2. Placeholder scan:** No TBD/TODO. Full tool code, helper, test, registration edit, and both block-list edits are given verbatim with exact anchor strings.

**3. Type consistency:** `CheapRouteTool::new(Arc<dyn Provider>, Registry)` mirrors `SubagentTool::new`. `execute` uses the `Tool` trait signature exactly (`execute(&self, Value, ToolContext) -> Result<ToolOutput>`), `ToolOutput::new(String).with_title(String).with_metadata(Value)` (verified in `task.rs`). `ProviderCheapBackend::new` / `run_cheap_route` / `CheapRouteOutcome` / `SubtaskResult` match Plans 3–4 field-for-field.

**4. Ambiguity check:** Empty-task rejection and output formatting are pinned by code + test. Recursion is blocked in both spawn paths, so `cheap_route` cannot invoke itself.
