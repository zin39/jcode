# Compaction Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Long sessions keep working after compaction: a disk-persisted task-state file is re-injected every turn, and old tool-result payloads are cleared (reversibly) before any summarization happens.

**Architecture:** Feature 1 adds a per-session `*.task-state.md` file + an `update_task_state` tool + per-turn injection into the dynamic (uncached) system-prompt part. Feature 2 adds a `tool_cleared_up_to` watermark to `CompactionManager`; clearing is applied only at API-view assembly (`messages_for_api_with`), never to stored history, and is tried at the 80% threshold before summarization.

**Tech Stack:** Rust workspace. Crates touched: `jcode-base` (session storage, compaction manager), `jcode-compaction-core` (pure clearing fns), `jcode-app-core` (tool + prompt injection), `jcode-session-types` (persisted state field).

**Branch:** `feat/compaction-resilience` off `master`.

**Build/test env:** `source ~/.cargo/env` first. Known baseline: ~60 env-dependent test failures on this Mac — only new failures in touched crates matter. Prefer `cargo test -p <crate>`.

---

### Task 1: Task-state store (jcode-base)

**Files:**
- Modify: `crates/jcode-base/src/session/storage_paths.rs` (add path fn near `session_path_in_dir`, line ~8)
- Create: `crates/jcode-base/src/session/task_state.rs`
- Modify: `crates/jcode-base/src/session.rs` or the session module root that declares `mod storage_paths;` — add `pub mod task_state;` alongside it

- [ ] **Step 1: Write failing tests** (bottom of the new `task_state.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_content() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state_in_dir(dir.path(), "s1", "## Plan\n- step").unwrap();
        assert_eq!(
            read_task_state_in_dir(dir.path(), "s1").as_deref(),
            Some("## Plan\n- step")
        );
    }

    #[test]
    fn missing_file_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_task_state_in_dir(dir.path(), "nope"), None);
    }

    #[test]
    fn caps_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_TASK_STATE_CHARS + 100);
        write_task_state_in_dir(dir.path(), "s2", &big).unwrap();
        let read = read_task_state_in_dir(dir.path(), "s2").unwrap();
        assert!(read.chars().count() <= MAX_TASK_STATE_CHARS + TRUNCATION_MARKER.chars().count());
        assert!(read.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn empty_write_clears_state() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state_in_dir(dir.path(), "s3", "content").unwrap();
        write_task_state_in_dir(dir.path(), "s3", "").unwrap();
        assert_eq!(read_task_state_in_dir(dir.path(), "s3"), None);
    }
}
```

- [ ] **Step 2: Run to verify fail**

Run: `source ~/.cargo/env && cargo test -p jcode-base task_state`
Expected: compile error (module/functions don't exist)

- [ ] **Step 3: Implement `task_state.rs`**

```rust
//! Per-session task-state file: small model-maintained doc (plan / progress /
//! decisions) that survives compaction because it lives on disk and is
//! re-injected into the dynamic system prompt every turn.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Hard cap on stored task-state size. Injected every turn, so keep it small.
pub const MAX_TASK_STATE_CHARS: usize = 8_192;
pub const TRUNCATION_MARKER: &str = "\n[task state truncated by jcode at 8KB cap]";

pub fn task_state_path_in_dir(base: &Path, session_id: &str) -> PathBuf {
    base.join("sessions")
        .join(format!("{}.task-state.md", session_id))
}

pub fn task_state_path(session_id: &str) -> Result<PathBuf> {
    let base = crate::storage::jcode_dir()?;
    Ok(task_state_path_in_dir(&base, session_id))
}

pub fn read_task_state_in_dir(base: &Path, session_id: &str) -> Option<String> {
    let content = std::fs::read_to_string(task_state_path_in_dir(base, session_id)).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read the task state for a session, or `None` if absent/empty.
pub fn read_task_state(session_id: &str) -> Option<String> {
    let base = crate::storage::jcode_dir().ok()?;
    read_task_state_in_dir(&base, session_id)
}

pub fn write_task_state_in_dir(base: &Path, session_id: &str, content: &str) -> Result<()> {
    let path = task_state_path_in_dir(base, session_id);
    if content.trim().is_empty() {
        // Empty write clears the state (file removed so injection stops).
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let capped: String = if content.chars().count() > MAX_TASK_STATE_CHARS {
        let mut s: String = content.chars().take(MAX_TASK_STATE_CHARS).collect();
        s.push_str(TRUNCATION_MARKER);
        s
    } else {
        content.to_string()
    };
    std::fs::write(&path, capped)?;
    Ok(())
}

/// Write (full replace) the task state for a session. Empty content clears it.
pub fn write_task_state(session_id: &str, content: &str) -> Result<()> {
    let base = crate::storage::jcode_dir()?;
    write_task_state_in_dir(&base, session_id, content)
}
```

Declare the module where the session submodules are declared (same place as `storage_paths`): `pub mod task_state;`. If `tempfile` is not already a dev-dependency of `jcode-base`, add `tempfile = "3"` under `[dev-dependencies]`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p jcode-base task_state`
Expected: 4 passed

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base
git commit -m "feat(session): per-session task-state file store"
```

---

### Task 2: `update_task_state` tool (jcode-app-core)

**Files:**
- Create: `crates/jcode-app-core/src/tool/task_state.rs`
- Modify: `crates/jcode-app-core/src/tool/mod.rs` — `mod task_state;`, register via `Self::insert_tool` next to the `"load_tools"` registration (~line 336), and add `"update_task_state"` to the `CORE_FULL_SCHEMA_TOOLS` list (~line 60) so its schema is never deferred

- [ ] **Step 1: Write failing test** (in `crates/jcode-app-core/src/tool/tests.rs`, near the other tool tests)

```rust
#[tokio::test]
async fn update_task_state_tool_writes_and_clears() {
    let tool = super::task_state::UpdateTaskStateTool::new();
    let ctx = test_tool_context("task-state-test-session"); // reuse/adapt the existing helper used by other tool tests to build a ToolContext; session_id is the only field that matters here
    let out = tool
        .execute(serde_json::json!({"content": "## Plan\n- do thing"}), ctx.clone())
        .await
        .unwrap();
    assert!(out.output.contains("Task state updated"));
    assert_eq!(
        jcode_base::session::task_state::read_task_state("task-state-test-session").as_deref(),
        Some("## Plan\n- do thing")
    );

    let out = tool
        .execute(serde_json::json!({"content": ""}), ctx)
        .await
        .unwrap();
    assert!(out.output.contains("cleared"));
    assert_eq!(
        jcode_base::session::task_state::read_task_state("task-state-test-session"),
        None
    );
}
```

Note: if other tool tests construct `ToolContext` inline instead of via a helper, do the same here (all fields besides `session_id` can be defaults/None; `execution_mode: ToolExecutionMode::Direct`). If tests run against a sandboxed `jcode_dir` via a test-env fixture (jcode-base `test-support`), use that fixture like neighboring tests do.

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p jcode-app-core update_task_state`
Expected: compile error (tool doesn't exist)

- [ ] **Step 3: Implement the tool**

```rust
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
        let cleared = params.content.trim().is_empty();
        jcode_base::session::task_state::write_task_state(&ctx.session_id, &params.content)?;
        let msg = if cleared {
            "Task state cleared.".to_string()
        } else {
            format!(
                "Task state updated ({} chars). It will be re-injected every turn and survives compaction.",
                params.content.chars().count()
            )
        };
        Ok(ToolOutput::new(msg).with_title("update_task_state"))
    }
}
```

Adjust the `jcode_base::session::task_state::` path to however jcode-app-core imports jcode-base (check neighboring tools; it may be `crate::…` re-export or `jcode_base::…`). Register in `tool/mod.rs`:

```rust
Self::insert_tool(
    &mut tools_map,
    "update_task_state",
    task_state::UpdateTaskStateTool::new(),
);
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p jcode-app-core update_task_state`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core
git commit -m "feat(tools): update_task_state tool for compaction-surviving working state"
```

---

### Task 3: Per-turn injection into dynamic system prompt

**Files:**
- Modify: `crates/jcode-app-core/src/agent/prompting.rs` — inside `build_system_prompt_split` (line ~69), after the memory prompt is added to the dynamic part and before `append_current_turn_system_reminder`

- [ ] **Step 1: Write failing test** (in `crates/jcode-app-core/src/agent_tests.rs`, following the pattern of existing prompt/agent tests that build an Agent with MockProvider)

```rust
#[tokio::test]
async fn task_state_is_injected_into_dynamic_prompt() {
    // Arrange: write task state for the agent's session id, then build the split prompt.
    let (mut agent, _guard) = test_agent().await; // reuse the existing agent test constructor used by neighboring tests
    let sid = agent.session_id().to_string();
    jcode_base::session::task_state::write_task_state(&sid, "## Plan\n- finish migration").unwrap();

    let split = agent.build_system_prompt_split_for_test().await; // if no test accessor exists, mark build_system_prompt_split pub(crate) and call it directly like other tests in this file call agent internals
    assert!(split.dynamic_part.contains("# Task State"));
    assert!(split.dynamic_part.contains("finish migration"));

    jcode_base::session::task_state::write_task_state(&sid, "").unwrap();
    let split = agent.build_system_prompt_split_for_test().await;
    assert!(!split.dynamic_part.contains("# Task State"));
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p jcode-app-core task_state_is_injected`
Expected: FAIL (no injection yet)

- [ ] **Step 3: Implement injection** — add to `prompting.rs`:

```rust
fn append_task_state(&self, split: &mut crate::prompt::SplitSystemPrompt) {
    let Some(state) = jcode_base::session::task_state::read_task_state(&self.session.id) else {
        return;
    };
    if !split.dynamic_part.is_empty() {
        split.dynamic_part.push_str("\n\n");
    }
    split.dynamic_part.push_str(
        "# Task State\n\nYour saved working state (maintained via the `update_task_state` tool; survives compaction). Keep it current:\n\n",
    );
    split.dynamic_part.push_str(&state);
}
```

Call it inside `build_system_prompt_split` right before `self.append_current_turn_system_reminder(&mut split);` (match how the session id field is actually accessed in that impl — e.g. `self.session.id` vs a `session_id()` accessor). The dynamic part is uncached by design, so this costs no cache invalidation.

- [ ] **Step 4: Run tests**

Run: `cargo test -p jcode-app-core task_state`
Expected: PASS (both Task 2 + Task 3 tests)

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core
git commit -m "feat(prompt): re-inject task state into dynamic prompt every turn"
```

---

### Task 4: Pure clearing functions (jcode-compaction-core)

**Files:**
- Modify: `crates/jcode-compaction-core/src/lib.rs` (new public fns + consts next to `RECENT_TURNS_TO_KEEP`)
- Tests: same file's `#[cfg(test)]` module (16 existing tests live there — follow their style)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn cleared_content_preserves_spill_pointer() {
    let original = "first 10KB head...\n[Tool output truncated by jcode: tool `bash` produced 80000 chars; kept first 10000 inline. FULL output saved to /home/u/.jcode/tool-outputs/s/t.txt — use the read tool with start_line/limit for targeted sections.]";
    let cleared = cleared_tool_result_content(original);
    assert!(cleared.starts_with("[tool result cleared by jcode"));
    assert!(cleared.contains("FULL output saved to /home/u/.jcode/tool-outputs/s/t.txt"));
    assert!(cleared.len() < original.len());
}

#[test]
fn cleared_content_without_pointer_is_marker_only() {
    let cleared = cleared_tool_result_content("plain big output");
    assert!(cleared.starts_with("[tool result cleared by jcode"));
    assert!(!cleared.contains("FULL output saved"));
}

#[test]
fn clear_tool_results_respects_watermark_and_skips_errors_short_results() {
    // messages[0]: tool result 1000 chars -> cleared
    // messages[1]: tool result under MIN_CLEARABLE_TOOL_RESULT_CHARS -> untouched
    // messages[2]: tool result beyond watermark -> untouched
    let mut messages = vec![
        tool_result_message("t1", &"a".repeat(1000)),
        tool_result_message("t2", "short"),
        tool_result_message("t3", &"b".repeat(1000)),
    ];
    let cleared = clear_tool_results_up_to(&mut messages, 2);
    assert_eq!(cleared, 1);
    assert!(tool_result_text(&messages[0]).starts_with("[tool result cleared"));
    assert_eq!(tool_result_text(&messages[1]), "short");
    assert!(tool_result_text(&messages[2]).starts_with("bbb"));
    // Idempotent: second pass clears nothing new.
    assert_eq!(clear_tool_results_up_to(&mut messages, 2), 0);
}
```

(`tool_result_message` / `tool_result_text` are small local test helpers building a `Message` with a single `ContentBlock::ToolResult` — write them in the test module using `Message::tool_result_with_duration` or a literal struct, matching how existing tests in this file build messages.)

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p jcode-compaction-core clear`
Expected: compile error

- [ ] **Step 3: Implement**

```rust
/// Tool results below this size are left alone — clearing them saves nothing
/// and destroys cheap, useful context.
pub const MIN_CLEARABLE_TOOL_RESULT_CHARS: usize = 200;

const CLEARED_MARKER_PREFIX: &str = "[tool result cleared by jcode";
const SPILL_POINTER_NEEDLE: &str = "FULL output saved to ";

/// Replacement text for a cleared tool result. Preserves the spill-pointer
/// line (if the original was spilled to disk) so the agent can still retrieve
/// the full output with the read tool.
pub fn cleared_tool_result_content(original: &str) -> String {
    let pointer_line = original
        .lines()
        .find(|line| line.contains(SPILL_POINTER_NEEDLE));
    match pointer_line {
        Some(line) => format!(
            "{} under context pressure: {} chars removed. {}]",
            CLEARED_MARKER_PREFIX,
            original.chars().count(),
            line.trim_start_matches('[').trim_end_matches(']'),
        ),
        None => format!(
            "{} under context pressure: {} chars removed. Re-run the tool if this output is needed again.]",
            CLEARED_MARKER_PREFIX,
            original.chars().count(),
        ),
    }
}

/// Clear tool-result payloads in `messages[..up_to_index]` (view-time only —
/// callers pass a cloned API view, never stored history). Returns how many
/// results were cleared. Skips already-cleared markers and small results.
pub fn clear_tool_results_up_to(messages: &mut [Message], up_to_index: usize) -> usize {
    let mut cleared = 0;
    let end = up_to_index.min(messages.len());
    for message in &mut messages[..end] {
        for block in &mut message.content {
            if let ContentBlock::ToolResult { content, .. } = block {
                clear_tool_result_block_content(content, &mut cleared);
            }
        }
    }
    cleared
}
```

`ContentBlock::ToolResult.content`'s concrete type must be checked in `jcode-message-types` (it may be a `String`, or a nested enum of text blocks). Implement `clear_tool_result_block_content` accordingly: for each text payload, if `chars >= MIN_CLEARABLE_TOOL_RESULT_CHARS` and it doesn't already start with `CLEARED_MARKER_PREFIX`, replace it with `cleared_tool_result_content(...)` and bump the counter. Reuse whatever accessor pattern `build_compaction_conversation_text` (same file, ~line 181) already uses to read tool-result text — that function demonstrably handles the real shape.

- [ ] **Step 4: Run tests**

Run: `cargo test -p jcode-compaction-core`
Expected: all pass (16 existing + 3 new)

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-compaction-core
git commit -m "feat(compaction-core): reversible tool-result clearing primitives"
```

---

### Task 5: Manager integration — clear at threshold, before summarization

**Files:**
- Modify: `crates/jcode-base/src/compaction.rs` — `CompactionManager` struct (~line 134), `ensure_context_fits` (~line 932), `messages_for_api_with` (~line 1297), `persisted_state()` (~line 378), `restore_persisted_state*` (~line 318), `CompactionAction` enum
- Modify: `crates/jcode-session-types/src/lib.rs` — `StoredCompactionState` (~line 261): add field
- Tests: `crates/jcode-base/src/compaction_tests.rs`

- [ ] **Step 1: Write failing tests** (in `compaction_tests.rs`, using the same helpers existing threshold tests use to build a manager + message list with known token sizes)

```rust
#[test]
fn clearing_applies_in_api_view_but_not_history() {
    let mut manager = test_manager(); // existing helper pattern in this file
    let messages = many_messages_with_big_tool_results(20); // helper: 20 messages, each with a 4000-char tool result
    manager.set_tool_cleared_up_to(messages.len() - RECENT_TURNS_TO_KEEP);
    let api_view = manager.messages_for_api_with(&messages);
    // Old tool results cleared in the view...
    assert!(first_tool_result_text(&api_view).starts_with("[tool result cleared"));
    // ...but the caller's history is untouched.
    assert!(!first_tool_result_text(&messages).starts_with("[tool result cleared"));
    // Recent suffix untouched in the view too.
    assert!(!last_tool_result_text(&api_view).starts_with("[tool result cleared"));
}

#[test]
fn stage1_clearing_skips_summarization_when_it_frees_enough() {
    // Manager sized so that big tool results push usage over COMPACTION_THRESHOLD,
    // but clearing them drops usage below it.
    let mut manager = test_manager_with_small_context_limit();
    let messages = many_messages_with_big_tool_results(20);
    let action = manager.ensure_context_fits(&messages, mock_provider());
    match action {
        CompactionAction::ToolResultsCleared { cleared } => assert!(cleared > 0),
        other => panic!("expected ToolResultsCleared, got {:?}", other),
    }
    assert!(!manager.is_compacting()); // no background summarization started
}

#[test]
fn tool_cleared_watermark_round_trips_through_persistence() {
    let mut manager = test_manager();
    manager.set_tool_cleared_up_to(7);
    let stored = manager.persisted_state();
    let mut restored = test_manager();
    restored.restore_persisted_state(stored); // match the real restore fn name/signature
    assert_eq!(restored.tool_cleared_up_to(), 7);
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p jcode-base compaction_tests`
Expected: compile errors (new methods/variant missing)

- [ ] **Step 3: Implement**

3a. `CompactionManager`: add field + accessors.

```rust
/// Absolute message index below which tool-result payloads are cleared in the
/// provider-visible view (stage-1 compaction). Stored history is never mutated.
tool_cleared_up_to: usize,
```

```rust
pub fn tool_cleared_up_to(&self) -> usize {
    self.tool_cleared_up_to
}

pub fn set_tool_cleared_up_to(&mut self, up_to: usize) {
    self.tool_cleared_up_to = self.tool_cleared_up_to.max(up_to);
}

fn tool_result_clearing_disabled() -> bool {
    std::env::var("JCODE_DISABLE_TOOL_RESULT_CLEARING").is_ok_and(|v| v == "1")
}
```

3b. `messages_for_api_with`: after assembling `result` (both summary and no-summary arms produce owned `Vec<Message>` — change the `None` arm from `active.to_vec()` to a `let mut result = active.to_vec()`), apply clearing before returning. The watermark is an absolute index into `all_messages`; the view drops the first `compacted_count` messages and (in the summary arm) prepends 1 summary message, so translate it:

```rust
// Translate absolute watermark -> index in the assembled view.
let compacted = self.compacted_count();
let offset = if self.active_summary.is_some() { 1 } else { 0 };
let view_watermark = self
    .tool_cleared_up_to
    .saturating_sub(compacted)
    .saturating_add(offset)
    .min(result.len());
if view_watermark > offset {
    jcode_compaction_core::clear_tool_results_up_to(&mut result[..view_watermark], view_watermark);
}
```

(Use whatever accessor exposes `compacted_count` inside the impl — the field is on `self`.)

3c. `ensure_context_fits`: stage-1 before starting background compaction. Insert after the critical-threshold block, before `maybe_start_compaction_with`:

```rust
// Stage 1: reversible tool-result clearing. Cheaper than summarization and
// preserves the verbatim recent window; only escalate to summarization if
// clearing can't get us back under the threshold.
let usage = self.context_usage_with(all_messages);
if usage >= COMPACTION_THRESHOLD
    && !Self::tool_result_clearing_disabled()
    && all_messages.len() > RECENT_TURNS_TO_KEEP
{
    let candidate = all_messages.len() - RECENT_TURNS_TO_KEEP;
    if candidate > self.tool_cleared_up_to {
        let before = self.tool_cleared_up_to;
        self.set_tool_cleared_up_to(candidate);
        let cleared = self.count_clearable_tool_results(all_messages, before, candidate);
        let post_usage = self.context_usage_with(all_messages);
        crate::logging::info(&format!(
            "[compaction] Stage-1 tool-result clearing: {} results cleared, usage {:.1}% -> {:.1}%",
            cleared,
            usage * 100.0,
            post_usage * 100.0,
        ));
        if cleared > 0 && post_usage < COMPACTION_THRESHOLD {
            return CompactionAction::ToolResultsCleared { cleared };
        }
    }
}
```

**Requirement this creates:** `context_usage_with` must reflect the watermark, or `post_usage` never drops. Read `context_usage_with`; it estimates tokens over the same messages the API sees. Route its estimation through the cleared view (cheapest correct option: extract a helper that computes the per-message char/token estimate and, for messages below `tool_cleared_up_to`, estimates the tool-result blocks at `cleared_tool_result_content(...)` size instead of full size — add `pub fn cleared_estimate_chars(original_chars: usize) -> usize` to jcode-compaction-core if a precomputed size is easier than building the string). Add `count_clearable_tool_results(&self, all_messages, from, to) -> usize` as a read-only counter using the same `MIN_CLEARABLE_TOOL_RESULT_CHARS` / already-cleared rules (expose a small helper from jcode-compaction-core rather than duplicating the predicate).

3d. New `CompactionAction` variant: `ToolResultsCleared { cleared: usize }`. Chase `match` exhaustiveness errors at call sites (turn_loops, TUI event mapping): map it to a log/no-op or an info-level status line — follow whatever `BackgroundStarted` does at each site, but do not block the turn.

3e. Persistence: `StoredCompactionState` gets

```rust
#[serde(default)]
pub tool_cleared_up_to: Option<usize>,
```

Set it in `persisted_state()` (`Some(self.tool_cleared_up_to).filter(|v| *v > 0)`), restore with `.unwrap_or(0)` in `restore_persisted_state*`. `serde(default)` keeps old session files loading.

3f. Hard compact / manual compact interaction: after `hard_compact_with` or applying a summary advances `compacted_count`, the watermark may lag behind (`tool_cleared_up_to < compacted_count`); the translation in 3b already saturates to 0 in that case — no extra handling needed, but add one assertion test if cheap.

- [ ] **Step 4: Run tests**

Run: `cargo test -p jcode-base compaction && cargo test -p jcode-app-core`
Expected: new tests pass; pre-existing compaction tests still green

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base crates/jcode-session-types crates/jcode-app-core
git commit -m "feat(compaction): stage-1 reversible tool-result clearing before summarization"
```

---

### Task 6: Workspace verification + docs

**Files:**
- Modify: `docs/` — if a features/config doc lists tools or env vars, add `update_task_state` + `JCODE_DISABLE_TOOL_RESULT_CLEARING` entries (grep `docs/` for an existing env-var table; skip if none)

- [ ] **Step 1: Full build + touched-crate tests**

Run: `source ~/.cargo/env && cargo build && cargo test -p jcode-compaction-core -p jcode-base -p jcode-app-core 2>&1 | tail -20`
Expected: build clean; no NEW failures vs the known env-dependent baseline (bash stdin forwarding, server reload tests, etc.)

- [ ] **Step 2: Clippy on touched crates**

Run: `cargo clippy -p jcode-compaction-core -p jcode-base -p jcode-app-core 2>&1 | grep -E '^(warning|error)' | head`
Expected: no new warnings from the new code

- [ ] **Step 3: Manual smoke (end-to-end)**

Run `./target/debug/jcode` in a scratch dir; ask it to "save a task state saying we are testing compaction survival, then read a big file". Verify:
- `~/.jcode/sessions/<id>.task-state.md` exists after the tool call
- Second turn's behavior shows it knows the task state (it's in the dynamic prompt)
- `JCODE_DISABLE_TOOL_RESULT_CLEARING=1` still runs normally

- [ ] **Step 4: Commit any doc changes**

```bash
git add docs
git commit -m "docs: task-state tool + tool-result clearing kill switch"
```
