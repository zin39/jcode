# Context/Loop Reliability Floor (P0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a failure-mode measurement floor (E1) + error-aware compaction (C1) + always-on cache-violation counter (C4) to jcode, so later context/loop adaptations can be measured against a baseline.

**Architecture:** Three additive changes, no behavior removed. (1) Extend the process-global `session_metrics` registry with four counters and the `record_*`/snapshot plumbing. (2) Make compaction summarization mark failed tool results as `[tool failed: …]` instead of replaying their payload. (3) Flip client-cache tracking to opt-out and increment a metric on violation. A small pure `loop_detect` helper makes stuck-loop/repeated-read detection unit-testable.

**Tech Stack:** Rust, Tokio. Crates: `jcode-base` (session_metrics), `jcode-compaction-core` (summarization), `jcode-app-core` (agent loop + cache hook). Build via `source ~/.cargo/env && cargo …`.

---

## File Structure
- `crates/jcode-base/src/session_metrics.rs` — MODIFY: add 4 counters, 4 `record_*` fns, 4 snapshot fields, tests.
- `crates/jcode-compaction-core/src/lib.rs` — MODIFY: `build_compaction_conversation_text` `ToolResult` arm + test.
- `crates/jcode-app-core/src/agent/loop_detect.rs` — CREATE: pure detection helper + tests.
- `crates/jcode-app-core/src/agent.rs` — MODIFY: cache-track gate default + violation metric; register `mod loop_detect`.
- `crates/jcode-app-core/src/agent/compaction.rs` — MODIFY: increment overflow counter at `try_auto_compact_after_context_limit`.
- `crates/jcode-app-core/src/agent/turn_loops.rs` — MODIFY: call `loop_detect` once per turn, record signals.

---

## Task 1: session_metrics counters (E1 + C4 storage)

**Files:**
- Modify: `crates/jcode-base/src/session_metrics.rs`

- [ ] **Step 1: Write failing tests**

Add to the `tests` module at the bottom of `session_metrics.rs`:

```rust
    #[test]
    fn counts_failure_modes() {
        let sid = "session_metrics_test_failmodes";
        forget(sid);
        record_context_overflow(sid);
        record_context_overflow(sid);
        record_repeated_read(sid);
        record_stuck_loop(sid);
        record_cache_violation(sid);
        record_cache_violation(sid);
        record_cache_violation(sid);
        let snap = snapshot(sid, Duration::from_secs(10)).expect("snapshot");
        assert_eq!(snap.context_overflow_count, 2);
        assert_eq!(snap.repeated_read_count, 1);
        assert_eq!(snap.stuck_loop_count, 1);
        assert_eq!(snap.cache_violation_count, 3);
        forget(sid);
    }
```

- [ ] **Step 2: Run test, verify it fails**

Run: `source ~/.cargo/env && cargo test -p jcode-base session_metrics::tests::counts_failure_modes`
Expected: FAIL — `cannot find function record_context_overflow` / unknown fields.

- [ ] **Step 3: Implement counters**

In `struct SessionMetrics` (after `cumulative_output_tokens: u64,`) add:

```rust
    context_overflow_count: u64,
    repeated_read_count: u64,
    stuck_loop_count: u64,
    cache_violation_count: u64,
```

Add four record fns (place after `record_turn`):

```rust
/// Record a context-overflow event (context-limit error after a compaction attempt).
pub fn record_context_overflow(session_id: &str) {
    bump(session_id, |m| m.context_overflow_count = m.context_overflow_count.saturating_add(1));
}

/// Record a detected repeated-read / endless-file-read loop.
pub fn record_repeated_read(session_id: &str) {
    bump(session_id, |m| m.repeated_read_count = m.repeated_read_count.saturating_add(1));
}

/// Record a detected stuck-in-loop (tool-failure streak with no successes).
pub fn record_stuck_loop(session_id: &str) {
    bump(session_id, |m| m.stuck_loop_count = m.stuck_loop_count.saturating_add(1));
}

/// Record a client-side KV-cache append-only violation.
pub fn record_cache_violation(session_id: &str) {
    bump(session_id, |m| m.cache_violation_count = m.cache_violation_count.saturating_add(1));
}

fn bump(session_id: &str, f: impl FnOnce(&mut SessionMetrics)) {
    if session_id.is_empty() {
        return;
    }
    with_registry(|map| {
        let entry = map.entry(session_id.to_string()).or_default();
        f(entry);
    });
}
```

Add four fields to `SessionMetricsSnapshot` (after `turns: u64,`):

```rust
    /// Count of context-overflow events for the session lifetime.
    pub context_overflow_count: u64,
    /// Count of detected repeated-read loops.
    pub repeated_read_count: u64,
    /// Count of detected stuck-in-loop events.
    pub stuck_loop_count: u64,
    /// Count of client-cache append-only violations.
    pub cache_violation_count: u64,
```

In `snapshot`, populate them in the returned struct (after `turns: entry.turns,`):

```rust
            context_overflow_count: entry.context_overflow_count,
            repeated_read_count: entry.repeated_read_count,
            stuck_loop_count: entry.stuck_loop_count,
            cache_violation_count: entry.cache_violation_count,
```

- [ ] **Step 4: Run tests, verify pass**

Run: `source ~/.cargo/env && cargo test -p jcode-base session_metrics`
Expected: PASS (all existing + `counts_failure_modes`).

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-base/src/session_metrics.rs
git commit -m "feat(metrics): add failure-mode + cache-violation counters"
```

---

## Task 2: error-aware compaction summarization (C1)

**Files:**
- Modify: `crates/jcode-compaction-core/src/lib.rs:180` (the `ToolResult` arm of `build_compaction_conversation_text`)

- [ ] **Step 1: Write failing test**

Add to the `tests` module in `jcode-compaction-core/src/lib.rs` (create one if absent):

```rust
    #[test]
    fn failed_tool_result_payload_excluded_from_summary() {
        use jcode_message_types::{ContentBlock, Message, Role};
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".to_string(),
                content: "ERROR: secret-stacktrace-payload at line 42".to_string(),
                is_error: Some(true),
            }],
            timestamp: None,
            tool_duration_ms: None,
        }];
        let text = build_compaction_conversation_text(&messages, None);
        assert!(!text.contains("secret-stacktrace-payload"),
            "failed-result payload must not enter summary input");
        assert!(text.contains("[tool failed:"),
            "a failure marker must remain so the summary knows an attempt happened");
    }

    #[test]
    fn successful_tool_result_payload_kept() {
        use jcode_message_types::{ContentBlock, Message, Role};
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t2".to_string(),
                content: "build succeeded: ok-payload".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }];
        let text = build_compaction_conversation_text(&messages, None);
        assert!(text.contains("ok-payload"), "successful result must be kept");
    }
```

(Confirm the `Message` literal fields match `jcode-message-types/src/lib.rs:83`. If a builder like `Message::user` is the codebase norm, use it; keep the `ToolResult` content blocks as shown.)

- [ ] **Step 2: Run test, verify it fails**

Run: `source ~/.cargo/env && cargo test -p jcode-compaction-core failed_tool_result_payload_excluded_from_summary`
Expected: FAIL — payload still present, no marker.

- [ ] **Step 3: Implement**

Replace the `ToolResult` arm (currently lines ~180-187):

```rust
                ContentBlock::ToolResult { content, is_error, .. } => {
                    if *is_error == Some(true) {
                        // Self-conditioning: keep the FACT of failure, drop the payload so
                        // the carried-forward summary is not polluted by error noise.
                        let reason = truncate_str_boundary(content, 80).replace('\n', " ");
                        conversation_text.push_str(&format!("[tool failed: {}]\n", reason.trim()));
                    } else {
                        let truncated = if content.len() > 500 {
                            format!("{}... (truncated)", truncate_str_boundary(content, 500))
                        } else {
                            content.clone()
                        };
                        conversation_text.push_str(&format!("[Result: {}]\n", truncated));
                    }
                }
```

- [ ] **Step 4: Run tests, verify pass**

Run: `source ~/.cargo/env && cargo test -p jcode-compaction-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-compaction-core/src/lib.rs
git commit -m "feat(compaction): drop failed-tool payloads from summary input (C1)"
```

---

## Task 3: wire context-overflow counter (E1)

**Files:**
- Modify: `crates/jcode-app-core/src/agent/compaction.rs` — inside `try_auto_compact_after_context_limit`, where `is_context_limit_error(error)` is true (token-overflow branch, ~line 124).

- [ ] **Step 1: Add the increment**

In the branch handling token context overflow (the one that calls `manager.hard_compact_with(...)`), immediately after the branch is entered, add:

```rust
        jcode_base::session_metrics::record_context_overflow(&self.session.id);
```

(Use the crate path that resolves in `jcode-app-core`; `session_metrics` is the same module imported as `record_token_usage` elsewhere — match that import style.)

- [ ] **Step 2: Build, verify compiles**

Run: `source ~/.cargo/env && cargo build -p jcode-app-core`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/jcode-app-core/src/agent/compaction.rs
git commit -m "feat(metrics): count context-overflow events (E1)"
```

---

## Task 4: loop-signal detection (E1 repeated-read + stuck-loop)

**Files:**
- Create: `crates/jcode-app-core/src/agent/loop_detect.rs`
- Modify: `crates/jcode-app-core/src/agent.rs` — add `mod loop_detect;` near the other `agent/*` mod declarations.
- Modify: `crates/jcode-app-core/src/agent/turn_loops.rs` — call detector once per turn.

- [ ] **Step 1: Write failing test (pure helper)**

Create `crates/jcode-app-core/src/agent/loop_detect.rs`:

```rust
//! Pure, unit-testable detection of stuck-loop / repeated-read signals over
//! recent message history. Used to feed session_metrics counters (E1).

use jcode_message_types::{ContentBlock, Message};

/// Signals detected over a recent window of messages.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LoopSignals {
    /// A (tool, input) pair was issued >= REPEAT_THRESHOLD times in the window.
    pub repeated_read: bool,
    /// Count of consecutive failed tool results with no successful result between.
    pub failure_streak: u32,
}

const WINDOW_TURNS: usize = 8;
const REPEAT_THRESHOLD: usize = 3;
const STUCK_STREAK: u32 = 3;

/// Inspect the last `WINDOW_TURNS` messages for repeated identical tool calls
/// and a trailing failed-tool streak.
pub fn detect(messages: &[Message]) -> LoopSignals {
    let start = messages.len().saturating_sub(WINDOW_TURNS);
    let window = &messages[start..];

    // Repeated identical (tool_name, input) calls.
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for msg in window {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, input, .. } = block {
                let key = format!("{name}\u{0}{input}");
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    let repeated_read = counts.values().any(|&c| c >= REPEAT_THRESHOLD);

    // Trailing failed-tool streak: count failed tool results from the end until a
    // successful tool result is seen.
    let mut failure_streak = 0u32;
    'outer: for msg in window.iter().rev() {
        for block in msg.content.iter().rev() {
            if let ContentBlock::ToolResult { is_error, .. } = block {
                if *is_error == Some(true) {
                    failure_streak += 1;
                } else {
                    break 'outer;
                }
            }
        }
    }

    LoopSignals { repeated_read, failure_streak }
}

/// True when the failure streak indicates the agent is stuck.
pub fn is_stuck(signals: &LoopSignals) -> bool {
    signals.failure_streak >= STUCK_STREAK
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::{ContentBlock, Message, Role};

    fn tool_use(name: &str, input: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: format!("u-{name}-{input}"),
                name: name.to_string(),
                input: serde_json::json!({ "path": input }),
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn tool_result(err: bool) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "r".to_string(),
                content: "x".to_string(),
                is_error: if err { Some(true) } else { None },
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    #[test]
    fn detects_repeated_read() {
        let msgs = vec![tool_use("read", "a.rs"), tool_use("read", "a.rs"), tool_use("read", "a.rs")];
        assert!(detect(&msgs).repeated_read);
    }

    #[test]
    fn no_repeat_when_inputs_differ() {
        let msgs = vec![tool_use("read", "a.rs"), tool_use("read", "b.rs"), tool_use("read", "c.rs")];
        assert!(!detect(&msgs).repeated_read);
    }

    #[test]
    fn detects_stuck_streak() {
        let msgs = vec![tool_result(true), tool_result(true), tool_result(true)];
        let s = detect(&msgs);
        assert_eq!(s.failure_streak, 3);
        assert!(is_stuck(&s));
    }

    #[test]
    fn success_breaks_streak() {
        let msgs = vec![tool_result(true), tool_result(false), tool_result(true)];
        assert_eq!(detect(&msgs).failure_streak, 1);
    }
}
```

Verify the `ContentBlock::ToolUse` literal fields (`id`, `name`, `input`) match `jcode-message-types/src/lib.rs:149`; `input` is a `serde_json::Value`.

- [ ] **Step 2: Register module + run test, verify fails then passes**

Add `mod loop_detect;` (or `pub(crate) mod loop_detect;`) beside the other `agent/` submodule declarations in `crates/jcode-app-core/src/agent.rs`.

Run: `source ~/.cargo/env && cargo test -p jcode-app-core loop_detect`
Expected: PASS (tests are self-contained).

- [ ] **Step 3: Wire into the turn loop**

In `crates/jcode-app-core/src/agent/turn_loops.rs`, once per turn after tool results are appended to the session (near the end of the loop body, where `repair_missing_tool_outputs` / tool execution completes), add:

```rust
        let signals = crate::agent::loop_detect::detect(&self.session.messages_as_provider());
        if signals.repeated_read {
            jcode_base::session_metrics::record_repeated_read(&self.session.id);
        }
        if crate::agent::loop_detect::is_stuck(&signals) {
            jcode_base::session_metrics::record_stuck_loop(&self.session.id);
        }
```

Use whatever accessor returns `&[Message]` for the session (e.g. `self.session.provider_messages()` as used in `messages_for_provider`). Match the existing call style. Keep this observational — do NOT interrupt the turn in P0.

- [ ] **Step 4: Build + test**

Run: `source ~/.cargo/env && cargo test -p jcode-app-core loop_detect && cargo build -p jcode-app-core`
Expected: PASS + build success.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core/src/agent/loop_detect.rs crates/jcode-app-core/src/agent.rs crates/jcode-app-core/src/agent/turn_loops.rs
git commit -m "feat(metrics): detect repeated-read + stuck-loop signals (E1)"
```

---

## Task 5: always-on cache-violation counter (C4)

**Files:**
- Modify: `crates/jcode-app-core/src/agent.rs:254` (`should_track_client_cache`) and `:763` (violation handler).

- [ ] **Step 1: Flip the gate to opt-out**

Change `should_track_client_cache` so an unset env var defaults to ON (lightweight prefix hashing is cheap; this surfaces violations in prod). Replace `Err(_) => false,` with `Err(_) => true,`:

```rust
    fn should_track_client_cache(&self) -> bool {
        match std::env::var("JCODE_TRACK_CLIENT_CACHE") {
            Ok(value) => {
                let value = value.trim();
                value.is_empty() || (value != "0" && !value.eq_ignore_ascii_case("false"))
            }
            Err(_) => true,
        }
    }
```

(Note: an explicit empty value now also counts as ON; explicit `0`/`false` still disables.)

- [ ] **Step 2: Increment metric on violation**

In the `if let Some(violation) = violation {` block (line ~763), add the metric call alongside the existing warn:

```rust
        if let Some(violation) = violation {
            jcode_base::session_metrics::record_cache_violation(&self.session.id);
            logging::warn(&format!(
                "CLIENT_CACHE_VIOLATION: {} | turn={} messages={}",
                violation.reason, violation.turn, violation.message_count
            ));
        }
```

- [ ] **Step 3: Build + run the existing cache-tracker tests**

Run: `source ~/.cargo/env && cargo test -p jcode-base cache_tracker && cargo build -p jcode-app-core`
Expected: PASS + build success (append-only invariant tests unaffected; healthy runs report 0 violations).

- [ ] **Step 4: Commit**

```bash
git add crates/jcode-app-core/src/agent.rs
git commit -m "feat(cache): always-on append-only tracking + violation metric (C4)"
```

---

## Final verification

- [ ] **Full build + targeted tests**

Run:
```bash
source ~/.cargo/env && cargo test -p jcode-base -p jcode-compaction-core -p jcode-app-core
```
Expected: all PASS.

- [ ] **Manual smoke (optional but recommended)**

Drive a long session that triggers a real compaction with at least one failed tool call; confirm via logs/inspection that the failed payload is absent from the summary and `session_metrics::snapshot` reports nonzero counts when overflow/loops are provoked.

---

## Self-review notes
- **Spec coverage:** E1 (Tasks 1,3,4), C1 (Task 2), C4 (Tasks 1 storage + 5 wiring) — all covered.
- **Type consistency:** `record_context_overflow/repeated_read/stuck_loop/cache_violation` names identical across Tasks 1/3/4/5; `LoopSignals{repeated_read,failure_streak}` consistent in Task 4.
- **Known adaptation points for the implementer:** exact session-messages accessor in Task 4 Step 3 and the precise overflow branch in Task 3 must be confirmed against current code (paths given). `Message`/`ContentBlock` literal fields must match `jcode-message-types/src/lib.rs:83/114` — use the codebase's builders if present.
