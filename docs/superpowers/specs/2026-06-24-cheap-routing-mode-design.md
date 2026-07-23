# Cheap-Routing Mode (`/cheap`) — Design

**Date:** 2026-06-24
**Status:** Approved design, pending implementation plan
**Branch:** `feat/cheap-routing-mode`

## Context

When a user works with an expensive parent model (e.g. Opus), every action the
parent takes — reading files, writing code, running tools — bills at the
expensive rate. For routine coding work (60–80% of requests per industry data)
this drains an API-key budget fast for no quality gain.

The user wants: while chatting with an expensive model, offload the actual work
to the **cheapest model capable of doing it properly**, across all verified API
keys. The expensive parent should only **decompose, recommend, and review** —
never execute. Two hard requirements:

1. The expensive model must barely touch the budget.
2. Work must still be done properly.

jcode already has ~80% of the needed infrastructure (per-model pricing, provider
availability, subagent model-override, prompt caching, auto-repair). This design
adds the *decision layer*, a *selection gate*, and wires several existing-but-unused
cost/quality levers together.

## Goal

A toggleable mode (`/cheap on`) where each task the user gives the expensive
parent is decomposed, routed to the cheapest capable model (user confirms a
single global pick), executed by cheap subagents, and reviewed by the parent —
targeting ~80–90% cost reduction while preserving correctness.

## Non-Goals (v1)

- Batch API integration (50% off but async ≤24h — breaks interactive flow). Deferred; revisit for background swarms.
- Per-subtask different models by default (global pick is default; per-subtask is an escape hatch).
- Fully automatic always-on routing (mode is explicitly toggled).

## Trigger

`/cheap on | off | auto` command + session flag.

- **`on`** (confirm mode, default): every task routes through the flow below; user confirms the global model pick before spawn.
- **`auto`**: same flow but auto-accepts the parent's recommended model (no gate) — for trusted batches.
- **`off`**: normal behavior.

Command handler lives with the existing TUI slash-command handlers; flag stored on the session.

## Flow

```
/cheap on
user: "refactor auth module + add tests"
  │
  ▼
parent: decompose into subtasks          (cheap — small prompt)
  │     + rate each subtask difficulty 1–5  (lever F)
  ▼
code: verified keys → candidate routes
      → filter: ctx window fits AND affordable (verified availability)
      → rank cheapest-first by estimated_reference_cost_micros
  ▼
parent: from the cheapest-first menu, recommend ONE global model strong
      enough for the hardest subtask (capability floor = parent's job,
      since jcode has no per-model quality tier) + one-line reason   (cheap)
  ▼
TUI: global-pick overlay
      shows recommended model + price + alternatives (↑/↓ to swap)
      [enter] accept   [c] drop to per-subtask picker   (auto mode: skip)
  ▼
spawn subagents per subtask, each:
      model = chosen            (task.rs model override)
      output_mode = compact     (lever D)
      tools = only what subtask needs   (lever C — prune ~10k tok/call)
      system_prompt_override = weak-model coaching + few-shot   (lever B)
      via complete_split → prompt caching ON   (lever A — 10% cached input)
      auto-repair on for malformed weak-model output   (lever H)
  ▼
each cheap subagent self-flags confidence (high/low)   (lever E)
  ▼
parent reviews EVERY compact result:
      deep-review flagged-uncertain, skim confident   (lever E — saves parent tokens)
      accept / fix (re-spawn cheap subagent w/ correction, or fix if trivial)
  ▼
final answer to user
```

## Components

| Piece | Location | New? |
|---|---|---|
| Cost selector `rank_routes_by_cost(routes, min_ctx)` → cheapest-first capable-of-fitting routes | `crates/jcode-provider-core/src/selection.rs` | NEW — uses existing `RouteCheapnessEstimate.estimated_reference_cost_micros` + `ProviderAvailability`. Capability floor is applied by the parent, not here (no per-model quality tier exists in jcode). |
| Orchestrator (decompose → rank → recommend → spawn → review) | new `crates/jcode-app-core/src/agent/cheap_route.rs` | NEW |
| Subagent spawn w/ model + compact + tool prune + prompt override | `crates/jcode-app-core/src/tool/task.rs` | REUSE (model, output_mode, allowed_tools, system_prompt_override all exist) |
| Global-pick selection overlay | TUI, reuse `jcode-tui-permissions` overlay round-trip pattern | NEW screen, reuse infra |
| Protocol `ModelSelectionRequest` / `ModelSelectionResponse` | `crates/jcode-protocol/src/wire.rs` | NEW — mirrors existing permission-prompt round-trip |
| `/cheap on\|off\|auto` command + session flag | TUI slash-command handler + session state | NEW (small) |
| Spend guard (project batch cost, warn over cap) | orchestrator | NEW (optional) |

## Cost & quality levers (all confirmed for v1)

- **A — Prompt caching on:** route every subagent through existing `complete_split()` (`jcode-provider-anthropic/src/lib.rs:442,518`) so static system + tool defs cache at 10% input price.
- **B — Weak-model prompt:** populate the existing unused `system_prompt_override` (`agent.rs:229`, applied `prompting.rs:73`) with explicit instructions + few-shot examples per subtask type.
- **C — Tool pruning:** pass each subagent only the tools its subtask needs via the `allowed_tools` filter (`tool/mod.rs:333`), cutting ~10k tokens of tool-schema overhead/call.
- **D — Compact output:** subagents return `output_mode: compact` (`task.rs`) so parent context stays small.
- **E — Confidence flag + selective review:** cheap subagent self-reports confidence; parent deep-reviews flagged-uncertain results, skims confident ones. Same coverage, fewer parent tokens.
- **F — Difficulty tiering:** parent rates each subtask 1–5 and uses that rating to set the capability floor when recommending from the cheapest-first menu (parent-side judgment — jcode has no per-model quality score, only price + context window).
- **H — Auto-repair (already on):** `response_recovery.rs` already recovers text-wrapped tool calls, truncated/malformed JSON, and missing tool outputs — critical for weak models. Confirm enabled for subagent sessions.

## Budget protections (baked in)

- Parent never executes — only decompose / recommend / review.
- All subagent output compact → parent context stays small → cheap follow-up turns.
- Capability floor (F) → avoids too-weak pick → no costly rework (rework is the real budget leak).
- Parent reviews compact diffs, not raw transcripts; selective deep-review (E).
- Optional spend cap warns before spawning if projected batch cost exceeds threshold.

## Data flow / protocol

Mirrors the existing permission-prompt round-trip:

1. Orchestrator (server side) builds candidate routes + parent recommendation.
2. Server → client: `ServerEvent::ModelSelectionRequest { subtasks, recommended_model, alternatives, est_cost }`.
3. TUI renders global-pick overlay; user accepts/swaps (or auto mode auto-accepts).
4. Client → server: `Request::ModelSelectionResponse { chosen_model, per_subtask_overrides? }`.
5. Orchestrator spawns subagents and streams results back as normal `ServerEvent`s.

## Reuse anchors

- Pricing: `jcode-provider-core/src/pricing.rs`, `RouteCheapnessEstimate` (`lib.rs:1070`), `cache_read_price_per_mtok_micros`.
- Routes + availability: `selection.rs` `ProviderAvailability`, `dedupe_model_routes()`, `ModelRoute.cheapness`.
- Subagent override: `tool/task.rs:26` `resolve_model()`, `:211` `provider.fork()`, schema `model`/`output_mode`/`subagent_type`.
- Prompt split + caching: `prompt.rs` `SplitSystemPrompt`, `prompting.rs:69` `build_system_prompt_split()`, anthropic cache breakpoints.
- Auto-repair: `agent/response_recovery.rs`.
- Permission overlay round-trip to copy for the selection UI: `jcode-tui-permissions`.

## Error handling

- No verified keys / no capable route within budget → surface clear message, fall back to normal (parent does it) or abort per user.
- Selected model unavailable at spawn (auth expired) → re-run selector excluding it, re-prompt.
- Subagent hard-fails after repair attempts → escalate that subtask to parent (or next-tier model).
- User cancels at selection overlay → abort batch cleanly, no spawns.

## Testing / verification

- Unit: `rank_routes_by_cost` ordering + tier floor + ctx filter (table-driven, mock routes w/ known prices).
- Unit: difficulty→tier mapping; confidence-flag review branch.
- Integration: `/cheap on` end-to-end with a stub provider — assert subagents spawned with the chosen model, compact output, pruned tools, and `complete_split` path (cache markers present).
- Integration: selection overlay round-trip (request → response → spawn) via the protocol, like existing permission-prompt tests.
- Manual: real run with 2–3 verified keys; confirm parent token spend is a small fraction of total, work correct. Use existing usage tracking (`jcode-base/src/usage.rs`, `session.rs:1135` token fields) to measure parent vs subagent spend.

## Future (deferred)

- **G — Batch API** (50% off) for background/non-interactive swarms.
- Full cascade auto-escalation (try cheapest, escalate on low confidence) without a parent-review pass.
- Tool-result dedup / caching across subagents.
