# Budget Cost Tracking — Design Spec

**Date:** 2026-06-24  
**Status:** Approved for implementation

---

## Goal

Add real dollar-cost budget tracking and a configurable spending cap that applies across ALL model calls — including every spawned subagent — within a single jcode session tree.

---

## Problem

Today the TUI shows a running cost estimate, but there is no enforcement mechanism and no way to say "stop spending when this session (and all its subagents) reaches $X". Subagents run in independent Agent instances; they share a parent `Session.id` chain but nothing else.

---

## Non-Goals / Constraints

- **Additive + opt-in**: `None` budget = unlimited (today's default). Zero behaviour change if `session_budget_usd` is not set.
- **No TUI changes**: The new server-side tally is independent of `jcode-tui`. The TUI already has its own display path; we don't touch it.
- **No persistence**: The spend ledger is process-global in-memory only (restart = clean slate). Fine for the use case (session-scoped budget).

---

## Architecture

### Three independent layers

```
Plan 1: budget.rs (jcode-base)
  - OnceLock<Mutex<HashMap<root_id, spent_micros>>>
  - record_spend(root_id, micros)
  - spent_micros(root_id) -> u64
  - usage_cost_micros(estimate, input, output, cache_read, cache_creation) -> u64

Plan 2: Config + root-id threading
  - AgentsConfig.session_budget_usd: Option<f64>
  - Session.budget_root_id: Option<String>   (set at subagent creation time)
  - budget_root_id helper: resolve root_id from session (own id if no parent root)

Plan 3: Record + enforce in run_turn()
  - On TokenUsage event: compute micros and record_spend
  - Before provider call: check cap, return Err if exceeded, log warning at 80%
  - Pure decision fn: budget_decision(spent, cap) -> BudgetDecision { Allow | Warn | Stop }
```

### Key existing anchors (do not rebuild)

| Anchor | Location |
|--------|----------|
| `metered_pricing_for_source_with_tier` | `crates/jcode-base/src/provider/pricing.rs:131` |
| `RouteCheapnessEstimate` | `crates/jcode-provider-core/src/lib.rs:1070` — fields `input_price_per_mtok_micros`, `output_price_per_mtok_micros`, `cache_read_price_per_mtok_micros` (Option<u64>, microdollars per million tokens) |
| `Session.parent_id: Option<String>` | `crates/jcode-base/src/session.rs:93` |
| `AgentsConfig` | `crates/jcode-config-types/src/lib.rs:451` |
| `run_turn()` | `crates/jcode-app-core/src/agent/turn_loops.rs:10` |
| `StreamEvent::TokenUsage` | handled at `turn_loops.rs:375` |
| `self.provider.name()`, `.model()`, `.service_tier()` | Provider trait methods |
| `source_key_for_provider_label` | `crates/jcode-base/src/provider_activity.rs:302` |

### Cost math (mirrors misc_ui.rs ResolvedTokenPricing::cost_for_usage)

```
cost_micros = (input_tokens * input_price_per_mtok_micros) / 1_000_000
            + (output_tokens * output_price_per_mtok_micros) / 1_000_000
            + (cache_read_tokens * cache_read_price_per_mtok_micros) / 1_000_000
            + (cache_creation_tokens * input_price_per_mtok_micros * 125/100) / 1_000_000  [Anthropic only]
```

Where all prices default to 0 if `None`.

For simplicity and correctness (no TUI dependency): the server-side tally uses integer micros arithmetic (u64) and treats missing prices as 0. Cache-write premium (1.25x) is applied if `cache_creation_tokens > 0` and the provider name contains "anthropic"/"claude".

### root_id resolution

- Top-level session: `budget_root_id = None` → root is `session.id` itself
- Subagent: at creation in `tool/task.rs`, set `budget_root_id = Some(parent_budget_root_id or parent_session_id)`
- Helper `effective_budget_root(session) -> &str` returns `session.budget_root_id.as_deref().unwrap_or(&session.id)`

---

## File Map

| File | Action | Purpose |
|------|--------|---------|
| `crates/jcode-base/src/budget.rs` | CREATE | Ledger + cost math + BudgetDecision |
| `crates/jcode-base/src/lib.rs` | MODIFY | Add `pub mod budget;` |
| `crates/jcode-config-types/src/lib.rs` | MODIFY | Add `session_budget_usd: Option<f64>` to `AgentsConfig` |
| `crates/jcode-base/src/session.rs` | MODIFY | Add `budget_root_id: Option<String>` field |
| `crates/jcode-app-core/src/tool/task.rs` | MODIFY | Propagate `budget_root_id` when creating subagent session |
| `crates/jcode-app-core/src/agent/turn_loops.rs` | MODIFY | Record spend on TokenUsage; enforce cap before provider call |

---

## Testing Strategy

- `budget.rs`: pure unit tests, no I/O, no async
- `AgentsConfig`: deserialization test (missing field → None)
- `budget_root_id` propagation: unit test via Session construction
- `BudgetDecision`: pure fn test — all three outcomes
- `turn_loops.rs` wiring: kept thin (delegate to pure fns); integration covered by the pure fn tests
