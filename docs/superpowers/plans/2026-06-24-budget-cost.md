# Budget Cost Tracking Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add real dollar-cost budget tracking and a configurable spending cap across all model calls including subagents, opt-in with zero behaviour change when unconfigured.

**Architecture:** A process-global spend ledger (OnceLock<Mutex<HashMap>>) keyed by root session ID accumulates microdollar spend from every TokenUsage event; a pure cost-math function mirrors the TUI's ResolvedTokenPricing math without depending on any TUI crate; enforcement runs as a guard at the top of run_turn() before the provider call.

**Tech Stack:** Rust stable, std (OnceLock, Mutex, HashMap), existing jcode-base/jcode-app-core crate structure, jcode-provider-core RouteCheapnessEstimate, serde for config deserialization.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/jcode-base/src/budget.rs` | CREATE | Spend ledger, cost math, BudgetDecision |
| `crates/jcode-base/src/lib.rs` | MODIFY | Declare `pub mod budget;` |
| `crates/jcode-config-types/src/lib.rs` | MODIFY | Add `session_budget_usd: Option<f64>` to `AgentsConfig` |
| `crates/jcode-base/src/session.rs` | MODIFY | Add `budget_root_id: Option<String>` field |
| `crates/jcode-app-core/src/tool/task.rs` | MODIFY | Propagate `budget_root_id` when creating subagent session |
| `crates/jcode-app-core/src/agent/turn_loops.rs` | MODIFY | Record spend on TokenUsage; enforce cap before provider call |

---

## Plan 1 — Shared Root-Keyed Spend Ledger

### Task 1.1: Create budget.rs with cost math

**Files:**
- Create: `crates/jcode-base/src/budget.rs`

- [ ] **Step 1: Write the file**

```rust
//! Process-global spend ledger for session budget tracking.
//!
//! Keyed by *root* session ID (the top-level session; subagents resolve to the
//! same root). All amounts are in **microdollars** (u64) to avoid f64 precision
//! loss. 1 microdollar = $0.000001 USD.
//!
//! The ledger is additive and opt-in: if no budget is configured nothing here
//! is ever consulted.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static LEDGER: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn ledger() -> &'static Mutex<HashMap<String, u64>> {
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Accumulate `micros` additional spend for `root_id`.
///
/// Uses saturating addition so pathological token counts cannot overflow.
pub fn record_spend(root_id: &str, micros: u64) {
    let mut map = ledger().lock().unwrap();
    let entry = map.entry(root_id.to_owned()).or_insert(0);
    *entry = entry.saturating_add(micros);
}

/// Total microdollars spent so far for `root_id` (0 if nothing recorded).
pub fn spent_micros(root_id: &str) -> u64 {
    ledger()
        .lock()
        .unwrap()
        .get(root_id)
        .copied()
        .unwrap_or(0)
}

/// Convert USD to microdollars (round to nearest).
pub fn usd_to_micros(usd: f64) -> u64 {
    (usd * 1_000_000.0).round() as u64
}

/// Compute the microdollar cost of one API call given token counts and pricing.
///
/// Mirrors `ResolvedTokenPricing::cost_for_usage` in `jcode-tui/misc_ui.rs` but
/// works purely in integer micros so the server side has no TUI dependency.
///
/// All `*_price_per_mtok_micros` values are **microdollars per million tokens**
/// (the unit stored in `RouteCheapnessEstimate`). Missing prices (None) are
/// treated as 0 (no cost contribution).
///
/// Cache-write premium: if `is_anthropic` is true and `cache_creation_tokens > 0`,
/// cache writes are billed at 1.25× the input rate (5-min TTL approximation,
/// same default as the TUI path).
pub fn usage_cost_micros(
    input_price_per_mtok_micros: Option<u64>,
    output_price_per_mtok_micros: Option<u64>,
    cache_read_price_per_mtok_micros: Option<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    is_anthropic: bool,
) -> u64 {
    let input_price = input_price_per_mtok_micros.unwrap_or(0);
    let output_price = output_price_per_mtok_micros.unwrap_or(0);
    let cache_read_price = cache_read_price_per_mtok_micros.unwrap_or(input_price);

    // integer micros: tokens * price_per_mtok_micros / 1_000_000
    let input_cost = input_tokens.saturating_mul(input_price) / 1_000_000;
    let output_cost = output_tokens.saturating_mul(output_price) / 1_000_000;
    let cache_read_cost = cache_read_tokens.saturating_mul(cache_read_price) / 1_000_000;

    // Cache-write premium (1.25× → ×125/100) for Anthropic split-accounting.
    let cache_write_cost = if is_anthropic && cache_creation_tokens > 0 {
        cache_creation_tokens
            .saturating_mul(input_price)
            .saturating_mul(125)
            / 100
            / 1_000_000
    } else {
        0
    };

    input_cost
        .saturating_add(output_cost)
        .saturating_add(cache_read_cost)
        .saturating_add(cache_write_cost)
}

/// Decision returned by `budget_decision()`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BudgetDecision {
    /// Spend is within budget; proceed normally.
    Allow,
    /// Spend has reached 80% of budget; proceed but emit a warning.
    Warn,
    /// Spend has met or exceeded budget cap; the call must be blocked.
    Stop,
}

/// Pure budget enforcement decision.
///
/// `spent_micros`: accumulated microdollar spend for this root.
/// `cap_micros`: the configured cap in microdollars.
///
/// Returns `Stop` when `spent >= cap`, `Warn` when `spent >= 0.8 * cap`,
/// `Allow` otherwise.
pub fn budget_decision(spent_micros: u64, cap_micros: u64) -> BudgetDecision {
    if spent_micros >= cap_micros {
        BudgetDecision::Stop
    } else if spent_micros * 10 >= cap_micros * 8 {
        // spent/cap >= 0.8 without floating point
        BudgetDecision::Warn
    } else {
        BudgetDecision::Allow
    }
}

/// Reset the ledger entry for `root_id` (useful for tests; no-op in production).
#[doc(hidden)]
pub fn reset_for_test(root_id: &str) {
    let mut map = ledger().lock().unwrap();
    map.remove(root_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a unique prefix per test to avoid interference from the global ledger.

    #[test]
    fn test_record_and_spent_micros_start_at_zero() {
        let id = "test-root-zero";
        reset_for_test(id);
        assert_eq!(spent_micros(id), 0);
    }

    #[test]
    fn test_record_spend_accumulates() {
        let id = "test-root-accum";
        reset_for_test(id);
        record_spend(id, 1_000);
        record_spend(id, 2_000);
        assert_eq!(spent_micros(id), 3_000);
    }

    #[test]
    fn test_record_spend_saturates_on_overflow() {
        let id = "test-root-overflow";
        reset_for_test(id);
        record_spend(id, u64::MAX - 1);
        record_spend(id, 10); // would overflow; saturating_add clamps to u64::MAX
        assert_eq!(spent_micros(id), u64::MAX);
    }

    #[test]
    fn test_usage_cost_micros_zero_tokens() {
        let cost = usage_cost_micros(Some(3_000_000), Some(15_000_000), None, 0, 0, 0, 0, false);
        assert_eq!(cost, 0);
    }

    #[test]
    fn test_usage_cost_micros_basic_input_output() {
        // 1M input tokens at $3/MTok = $3 = 3_000_000 micros
        // 1M output tokens at $15/MTok = $15 = 15_000_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None,
            1_000_000,
            1_000_000,
            0,
            0,
            false,
        );
        assert_eq!(cost, 18_000_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_read_cheaper() {
        // 1M cache-read tokens at $0.30/MTok = $0.30 = 300_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            Some(300_000),
            0,
            0,
            1_000_000,
            0,
            false,
        );
        assert_eq!(cost, 300_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_read_falls_back_to_input_price() {
        // cache_read_price_per_mtok_micros = None → falls back to input price
        // 1M cache-read at input price $3/MTok = 3_000_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None, // no cache read price
            0,
            0,
            1_000_000,
            0,
            false,
        );
        assert_eq!(cost, 3_000_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_write_anthropic_premium() {
        // 1M cache-creation tokens at $3/MTok * 1.25 = $3.75 = 3_750_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None,
            0,
            0,
            0,
            1_000_000,
            true, // is_anthropic
        );
        assert_eq!(cost, 3_750_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_write_no_premium_non_anthropic() {
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None,
            0,
            0,
            0,
            1_000_000,
            false, // not anthropic
        );
        assert_eq!(cost, 0); // no write cost for non-anthropic
    }

    #[test]
    fn test_usage_cost_micros_missing_prices_are_zero() {
        let cost = usage_cost_micros(None, None, None, 1_000_000, 1_000_000, 1_000_000, 0, false);
        assert_eq!(cost, 0);
    }

    #[test]
    fn test_budget_decision_allow() {
        assert_eq!(budget_decision(0, 1_000_000), BudgetDecision::Allow);
        assert_eq!(budget_decision(799_999, 1_000_000), BudgetDecision::Allow);
    }

    #[test]
    fn test_budget_decision_warn_at_80_percent() {
        assert_eq!(budget_decision(800_000, 1_000_000), BudgetDecision::Warn);
        assert_eq!(budget_decision(999_999, 1_000_000), BudgetDecision::Warn);
    }

    #[test]
    fn test_budget_decision_stop_at_100_percent() {
        assert_eq!(budget_decision(1_000_000, 1_000_000), BudgetDecision::Stop);
        assert_eq!(budget_decision(1_500_000, 1_000_000), BudgetDecision::Stop);
    }

    #[test]
    fn test_usd_to_micros() {
        assert_eq!(usd_to_micros(1.0), 1_000_000);
        assert_eq!(usd_to_micros(0.5), 500_000);
        assert_eq!(usd_to_micros(0.0), 0);
    }
}
```

- [ ] **Step 2: Declare module in lib.rs**

Add `pub mod budget;` to `crates/jcode-base/src/lib.rs` after the existing `pub mod bus;` line.

Read the file to find the exact insertion point, then add the line. The existing `pub mod bus;` declaration is around line 22. Insert after it:

```
pub mod budget;
```

- [ ] **Step 3: Run tests to verify the module compiles and tests pass**

```bash
. "$HOME/.cargo/env" && cargo test -p jcode-base budget 2>&1 | tail -30
```

Expected output: all tests in `budget` module pass.

- [ ] **Step 4: Commit**

```bash
git add crates/jcode-base/src/budget.rs crates/jcode-base/src/lib.rs
git commit -m "feat(budget): add spend ledger, cost math, and BudgetDecision

- OnceLock<Mutex<HashMap>> keyed by root_session_id → spent_micros
- record_spend / spent_micros / reset_for_test helpers  
- usage_cost_micros: integer micros math mirroring TUI cost_for_usage
- budget_decision: pure fn returning Allow/Warn/Stop at 80%/100% cap
- 13 unit tests covering zero, accumulation, overflow, cache math, decision"
```

---

## Plan 2 — Budget Config + Root-ID Threading

### Task 2.1: Add session_budget_usd to AgentsConfig

**Files:**
- Modify: `crates/jcode-config-types/src/lib.rs`

- [ ] **Step 1: Read the current AgentsConfig Default impl**

Read `crates/jcode-config-types/src/lib.rs` around line 500 to find the `impl Default for AgentsConfig` block and its closing brace so you know the exact insertion point.

- [ ] **Step 2: Add field to AgentsConfig struct**

In `AgentsConfig`, add after the last existing field (which is `memory_embedding_dim: Option<usize>`):

```rust
    /// Optional per-session dollar budget cap applied across the session tree
    /// (top-level session + all spawned subagents). When set, the total spend
    /// for a root session and all its descendants is tracked and capped at this
    /// value. `None` = unlimited (the default).
    ///
    /// Example: `session_budget_usd = 0.50` caps each session tree at $0.50.
    #[serde(default)]
    pub session_budget_usd: Option<f64>,
```

- [ ] **Step 3: Add to Default impl**

In `impl Default for AgentsConfig`, add inside the struct literal:

```rust
            session_budget_usd: None,
```

- [ ] **Step 4: Verify it compiles**

```bash
. "$HOME/.cargo/env" && cargo build -p jcode-config-types 2>&1 | tail -10
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-config-types/src/lib.rs
git commit -m "feat(budget): add session_budget_usd to AgentsConfig

Optional f64 field, None by default (serde(default)), unlimited when absent."
```

### Task 2.2: Add budget_root_id field to Session

**Files:**
- Modify: `crates/jcode-base/src/session.rs`

- [ ] **Step 1: Read the Session struct definition**

Read `crates/jcode-base/src/session.rs` lines 88-135 to see the current Session struct fields so you know the exact insertion point.

- [ ] **Step 2: Add budget_root_id field**

In the `Session` struct (after `parent_id: Option<String>` at line 93), add:

```rust
    /// Budget root for spend tracking. `None` means this session IS the root;
    /// subagents set this to the ancestor's root id so all spend accumulates
    /// under a single ledger key.
    #[serde(default)]
    pub budget_root_id: Option<String>,
```

- [ ] **Step 3: Verify Session still serializes/deserializes correctly**

The field has `#[serde(default)]` so existing persisted sessions without the field deserialize to `None`. Verify compile:

```bash
. "$HOME/.cargo/env" && cargo build -p jcode-base 2>&1 | tail -20
```

Expected: no errors.

- [ ] **Step 4: Add helper method to Session**

After the `token_usage_totals` method (around line 1152), add:

```rust
    /// The effective budget root id for spend tracking.
    ///
    /// If this session has a `budget_root_id`, use that (it was set by the
    /// subagent spawner to point at the tree root). Otherwise this session IS
    /// the root, so return its own `id`.
    pub fn effective_budget_root(&self) -> &str {
        self.budget_root_id.as_deref().unwrap_or(&self.id)
    }
```

- [ ] **Step 5: Write unit test for effective_budget_root**

Add in the same file's test module (search for `#[cfg(test)]` at the bottom of session.rs):

```rust
    #[test]
    fn test_effective_budget_root_own_id_when_no_parent_root() {
        let mut s = Session::default_for_test("sess-abc");
        s.budget_root_id = None;
        assert_eq!(s.effective_budget_root(), "sess-abc");
    }

    #[test]
    fn test_effective_budget_root_uses_budget_root_id_when_set() {
        let mut s = Session::default_for_test("sess-child");
        s.budget_root_id = Some("sess-root".to_owned());
        assert_eq!(s.effective_budget_root(), "sess-root");
    }
```

NOTE: `Session::default_for_test` may not exist. If not, search the test module for how tests currently construct a Session. You may need to use a different constructor. If there's no test helper, construct it directly:

```rust
    #[cfg(test)]
    fn make_session(id: &str) -> Session {
        Session {
            id: id.to_owned(),
            parent_id: None,
            budget_root_id: None,
            title: None,
            custom_title: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            messages: vec![],
            compaction: None,
            provider_session_id: None,
            provider_key: None,
            model: None,
            route_api_method: None,
            reasoning_effort: None,
            subagent_model: None,
            improve_mode: None,
            autoreview_enabled: None,
        }
    }
```

Then write the test using `make_session`:

```rust
    #[test]
    fn test_effective_budget_root_own_id_when_no_parent_root() {
        let s = make_session("sess-abc");
        assert_eq!(s.effective_budget_root(), "sess-abc");
    }

    #[test]
    fn test_effective_budget_root_uses_budget_root_id_when_set() {
        let mut s = make_session("sess-child");
        s.budget_root_id = Some("sess-root".to_owned());
        assert_eq!(s.effective_budget_root(), "sess-root");
    }
```

- [ ] **Step 6: Run the tests**

```bash
. "$HOME/.cargo/env" && cargo test -p jcode-base session 2>&1 | tail -30
```

Expected: new tests pass, no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/jcode-base/src/session.rs
git commit -m "feat(budget): add budget_root_id to Session + effective_budget_root()

Subagent sessions carry budget_root_id pointing to the tree root.
effective_budget_root() returns own id for root sessions."
```

### Task 2.3: Propagate budget_root_id at subagent creation

**Files:**
- Modify: `crates/jcode-app-core/src/tool/task.rs`

- [ ] **Step 1: Read task.rs around subagent Session creation**

Read `crates/jcode-app-core/src/tool/task.rs` lines 120-160 to see the exact code where `Session::create(Some(ctx.session_id.clone()), ...)` is called.

- [ ] **Step 2: Understand the ctx object**

The `ctx` object passed to the tool has `ctx.session_id`. We need to also read the parent session to get its `budget_root_id`. Look for how `ctx.session` or session loading is done nearby. Also check what `ctx` type is by searching for `ToolContext` or `TaskContext` struct definition.

Run:
```bash
grep -n "ctx\.session\|ToolContext\|fn execute\|ctx: " /Users/karangupta/new_projects/jcode/.claude/worktrees/agent-a454b4377aa9c65b8/crates/jcode-app-core/src/tool/task.rs | head -30
```

- [ ] **Step 3: After `Session::create(...)`, set budget_root_id on the new session**

The new subagent session is created at approximately:
```rust
Session::create(Some(ctx.session_id.clone()), Some(subagent_title(&params)))
```

After this call returns `new_session`, add:

```rust
// Propagate budget root: subagents share the root session's budget.
// We need the parent's effective root — load it briefly to check.
if let Ok(parent_session) = crate::session::Session::load(&ctx.session_id) {
    let root_id = parent_session.effective_budget_root().to_owned();
    new_session.budget_root_id = Some(root_id);
    // Persist the updated session so downstream Agent picks it up.
    let _ = new_session.save();
}
```

NOTE: Look at the actual variable name for the created session in task.rs (it may not be `new_session`). Adapt accordingly.

- [ ] **Step 4: Build to verify**

```bash
. "$HOME/.cargo/env" && cargo build -p jcode-app-core 2>&1 | tail -20
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core/src/tool/task.rs
git commit -m "feat(budget): propagate budget_root_id to subagent sessions

When creating a subagent session, read the parent's effective budget root
and store it on the child so all subagents accumulate spend under one key."
```

---

## Plan 3 — Record Spend + Enforce at the Choke Point

### Task 3.1: Record spend on TokenUsage in run_turn

**Files:**
- Modify: `crates/jcode-app-core/src/agent/turn_loops.rs`

- [ ] **Step 1: Read the TokenUsage event handler in turn_loops.rs**

Read `crates/jcode-app-core/src/agent/turn_loops.rs` lines 370-415 to see the exact `StreamEvent::TokenUsage` match arm and its surrounding code. Note the variable names: `usage_input`, `usage_output`, `usage_cache_read`, `usage_cache_creation`.

- [ ] **Step 2: Add spend recording after all four usage variables are updated**

Inside the `StreamEvent::TokenUsage` match arm, after all four `usage_*` variables are updated (after the `if trace { eprintln! }` block at the end of the arm), add:

```rust
// Record spend for budget tracking (no-op if no budget is configured).
{
    let provider_name = self.provider.name().to_string();
    let model = self.provider.model();
    let service_tier = self.provider.service_tier();
    let source_key = crate::provider_activity::source_key_for_provider_label(
        &provider_name,
        Some(&provider_name),
    );
    if let Some(estimate) = crate::provider::pricing::metered_pricing_for_source_with_tier(
        &source_key,
        &model,
        service_tier.as_deref(),
    ) {
        let is_anthropic = provider_name.to_ascii_lowercase().contains("anthropic")
            || provider_name.to_ascii_lowercase().contains("claude");
        let micros = crate::budget::usage_cost_micros(
            estimate.input_price_per_mtok_micros,
            estimate.output_price_per_mtok_micros,
            estimate.cache_read_price_per_mtok_micros,
            usage_input.unwrap_or(0),
            usage_output.unwrap_or(0),
            usage_cache_read.unwrap_or(0),
            usage_cache_creation.unwrap_or(0),
            is_anthropic,
        );
        if micros > 0 {
            crate::budget::record_spend(
                self.session.effective_budget_root(),
                micros,
            );
        }
    }
}
```

- [ ] **Step 3: Build to verify**

```bash
. "$HOME/.cargo/env" && cargo build -p jcode-app-core 2>&1 | tail -20
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/jcode-app-core/src/agent/turn_loops.rs
git commit -m "feat(budget): record spend from TokenUsage events in run_turn

Looks up pricing via metered_pricing_for_source_with_tier, computes
micros via usage_cost_micros, records under effective_budget_root."
```

### Task 3.2: Enforce budget cap before provider call

**Files:**
- Modify: `crates/jcode-app-core/src/agent/turn_loops.rs`

- [ ] **Step 1: Read the area before the provider.complete_split call**

Read `crates/jcode-app-core/src/agent/turn_loops.rs` lines 85-115 to see exactly where the provider call happens and what comes just before it (the `let mut stream = match self.provider.complete_split(...)` block).

- [ ] **Step 2: Add budget enforcement before the provider call**

Just before the `let mut stream = match self.provider.complete_split(...)` block, add:

```rust
// Budget enforcement: check cap before each provider call.
// This covers the main session AND every subagent (all go through run_turn).
{
    let budget_micros = crate::config::config()
        .agents
        .session_budget_usd
        .map(crate::budget::usd_to_micros);
    if let Some(cap) = budget_micros {
        let root_id = self.session.effective_budget_root();
        let spent = crate::budget::spent_micros(root_id);
        match crate::budget::budget_decision(spent, cap) {
            crate::budget::BudgetDecision::Stop => {
                let spent_usd = spent as f64 / 1_000_000.0;
                let cap_usd = cap as f64 / 1_000_000.0;
                return Err(anyhow::anyhow!(
                    "Session budget of ${:.2} exceeded (spent ${:.2}). \
                     Configure a higher agents.session_budget_usd to continue.",
                    cap_usd, spent_usd
                ));
            }
            crate::budget::BudgetDecision::Warn => {
                let spent_usd = spent as f64 / 1_000_000.0;
                let cap_usd = cap as f64 / 1_000_000.0;
                // Log once per run_turn call; not rate-limited further since
                // run_turn is called once per assistant turn.
                crate::logging::warn(&format!(
                    "Budget warning: spent ${:.2} of ${:.2} cap (≥80%)",
                    spent_usd, cap_usd
                ));
            }
            crate::budget::BudgetDecision::Allow => {}
        }
    }
}
```

- [ ] **Step 3: Build to verify**

```bash
. "$HOME/.cargo/env" && cargo build -p jcode-app-core 2>&1 | tail -20
```

Expected: no errors.

- [ ] **Step 4: Run full test suite for both changed crates**

```bash
. "$HOME/.cargo/env" && cargo test -p jcode-base 2>&1 | tail -30
. "$HOME/.cargo/env" && cargo test -p jcode-app-core 2>&1 | tail -30
```

Expected: all budget tests pass, no new failures (known flaky: `tool::bash::*` stdin, `server::*` queue/reload/socket).

- [ ] **Step 5: Build the jcode binary**

```bash
. "$HOME/.cargo/env" && cargo build -p jcode --bin jcode 2>&1 | tail -20
```

Expected: success.

- [ ] **Step 6: Commit**

```bash
git add crates/jcode-app-core/src/agent/turn_loops.rs
git commit -m "feat(budget): enforce budget cap in run_turn before provider call

Reads agents.session_budget_usd from config; if set, checks
budget_decision(spent, cap) before each API call:
- Stop: returns Err with clear message
- Warn: logs warning (≥80% of cap)
- Allow: proceeds normally
Covers main session and all subagents via the shared choke point."
```

### Task 3.3: Final verification

- [ ] **Step 1: Run full tests**

```bash
. "$HOME/.cargo/env" && cargo test -p jcode-base 2>&1 | tail -50
. "$HOME/.cargo/env" && cargo test -p jcode-config-types 2>&1 | tail -30
. "$HOME/.cargo/env" && cargo test -p jcode-app-core 2>&1 | tail -50
```

- [ ] **Step 2: Build the final binary**

```bash
. "$HOME/.cargo/env" && cargo build -p jcode --bin jcode 2>&1 | tail -10
```

- [ ] **Step 3: Review git log**

```bash
git log --oneline feat/budget-cost
```

Expected: spec commit + 6 feature commits (one per task).
