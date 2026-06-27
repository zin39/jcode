# Gold Mode — Multi-Model Debate ("Mixture-of-Agents") — Design

**Status:** Approved design (UX + architecture verified against the codebase), pending implementation plan.
**Date:** 2026-06-27

## Goal

Let a jcode coordinator produce "gold" answers on hard subtasks by running 3-4
**distinct cheap models** as proposers, having them critique each other, and a
**strong model** synthesizing the result — watched **live** in the TUI, toggled
with `/gold on|off`, with no CLI-and-grep workflow. Built by **composing**
existing jcode machinery (cheap-route orchestrator, parallel tool execution,
swarm gallery, side panel, watchdogs, cost-guard), not a parallel system.

## User-facing behavior (the decided UX)

- **Toggle:** `/gold on` / `/gold off` (per-session, live, no reload). `/gold`
  shows status.
- **Model choice:** automatic — the K (default 3) **cheapest distinct** capable
  models from the user's keys are the proposers; the aggregator is the strong
  model (`cheap_route_strong_model`, else the parent/coordinator's own model).
- **Scope:** the coordinator auto-decides — a delegated subtask runs the debate
  only when its `difficulty > cheap_route_difficulty_threshold`; trivial subtasks
  stay single cheap model. "Gold where it matters."
- **Loop shape:** debate — `propose (K, parallel) → critique (each reviews the
  others) → aggregate (strong model merges answers + critiques) → gold`.
- **Live view:** the K proposers appear as **status tiles** in the existing
  inline swarm gallery (`running → done`, with a short detail), a one-line phase
  header (`propose → critique → merge`), and the gold result streams into the
  **side panel**. Status-tile visibility only — full per-proposer token
  streaming is a deliberate later enhancement, not in this scope.

## Architecture (verified against the code)

### Engine — extend `run_cheap_route` (no parallel system)

`crates/jcode-app-core/src/agent/cheap_route.rs`.

- **Hook point:** the per-subtask loop at ~`:460-498`, specifically the existing
  difficulty gate at ~`:467`. Today: hard subtask → strong model first, else
  cheapest-first fallback. New: when `gold_mode && subtask.difficulty >
  difficulty_threshold`, call `run_debate(...)` instead of the single-model
  fallback loop.
- **`run_subtask` is concurrency-safe** (`CheapRouteBackend` trait ~`:60`,
  `ProviderCheapBackend` impl ~`:1101`): `&self`, each call does
  `self.provider.fork()` + `Session::create(...)` + `run_once_capture(...)`,
  returns `Result<String>`. No `&mut self`, no shared mutable state → K calls run
  concurrently with K different `(model, route_api_method)` pins.
- **K distinct proposers:** `ranked_with_preferences(...)` (`:866`) over
  `dedupe_model_routes` (`selection.rs`) yields routes that are distinct per
  `(provider, model)`. Take the top `K = min(cheap_route_gold_k, ranked.len())`
  distinct cheapest as proposers.
- **Concurrency:** reuse `crate::agent::parallel_tools::run_in_parallel_ordered`
  (added this session) to fan the K proposer `run_subtask` calls out and collect
  `Vec<String>` candidates in order.
- **Critique:** `backend.ask_parent(<critique prompt with the K candidates>)` →
  `String`.
- **Aggregate:** run the strong model (via `run_subtask` pinned to the strong
  model, or `ask_parent` on the parent's own model) over `candidates + critique`
  → the gold `String`, which becomes the subtask result (same type as today).

### Debate orchestration (the genuinely new logic)

New `run_debate(backend, subtask, proposer_models, strong_model, k)`:

```
candidates = run_in_parallel_ordered(proposer_models[..k], |_, (model, api)|
                 backend.run_subtask(subtask, model, api))   // Vec<String>
candidates = candidates with failed ones dropped            // see error handling
if candidates.len() < 2 { fall back to single-model strong result }  // not enough to debate
critique = backend.ask_parent(critique_prompt(subtask, candidates))
gold     = backend.run_subtask_on(strong_model, aggregate_prompt(subtask, candidates, critique))
return gold
```

`critique_prompt` asks the parent to point out errors/gaps across the candidates.
`aggregate_prompt` asks the strong model to synthesize the single best answer
using the candidates + critique.

### Trigger / config (verified, mirrors existing patterns)

- **Per-session toggle:** add `gold_mode_enabled: Option<bool>` to `Session`
  (`crates/jcode-base/src/session.rs`, beside `autoreview_enabled` /
  `autojudge_enabled`; + `SessionStartupStub` + `Default`). Persisted via
  `session.save()`.
- **`/gold` command:** `handle_gold_command` in
  `crates/jcode-tui/src/tui/app/commands.rs` (mirror `handle_goals_command` /
  the `/model` handler): `on`/`off`/status, sets `session.gold_mode_enabled`,
  saves, status notice. Wired into the command dispatch (~`:1865`).
- **Global config:** add to `AgentsConfig`
  (`crates/jcode-config-types/src/lib.rs`, beside `auto_delegate`):
  `cheap_route_gold_mode: bool` (default false — master enable) and
  `cheap_route_gold_k: usize` (default 3). Update the `Default` impl.
- **Reaching the engine:** `CheapRouteTool::execute`
  (`crates/jcode-app-core/src/tool/cheap_route_tool.rs`) loads the session via
  `Session::load(&ctx.session_id)`, computes
  `gold = session.gold_mode_enabled.unwrap_or(false) && config().agents.cheap_route_gold_mode`,
  and passes it into the backend / `run_cheap_route`. `ProviderCheapBackend`
  gains a `gold_mode: bool` field.

### Live UI — status tiles via the swarm gallery (chosen bridge)

Verified: the inline swarm gallery renders entries from the server `swarm_members`
map; a member is shown when it has an entry + `SwarmStatus` is broadcast
(`output_tail` optional). The **capture path** `run_subtask` uses does **not**
stream `output_tail` (only the streaming path does), so we use **status tiles**
rather than token streaming:

- Around each proposer `run_subtask`, `run_debate` registers a lightweight
  `swarm_members` entry for the proposer session and broadcasts `SwarmStatus`
  transitions: `running` (with detail = `proposer: <model>`), then `done`/`failed`.
  Uses the existing `update_member_status` / `broadcast_swarm_status` path
  (`server/swarm.rs`); de-registers (or marks `done`) on completion. ~30 lines of
  glue; **no change to the agent execution path**.
- **Phase header:** a one-line `propose → critique → merge` indicator, via the
  existing gallery header (`info_widget_swarm_gallery.rs` `gallery_header`) or a
  thin 1-line widget above the gallery band (`ui.rs` ~`:2507`). ~50 lines max.
- **Gold result:** streamed/written into the **side panel** as a live markdown
  page via `crate::side_panel::write_markdown_page` + `set_side_panel_snapshot`
  (same path as the `/goals` overview), showing the gold answer + a collapsed
  candidate list.

## Error handling & edge cases (caught during verification)

- **Recursion guard (critical):** debate proposer/aggregator sub-runs MUST run
  with `gold_mode = false`, or a debate subtask could itself spawn a debate
  forever. `run_debate` constructs the proposer backend/context with gold off.
- **Proposer failures:** a proposer that errors/times out is dropped from
  `candidates` (it does not abort the debate). If fewer than 2 candidates
  survive, skip the debate and return a single strong-model result (no point
  "merging" one answer).
- **Timeouts:** each proposer keeps the existing `SUBTASK_TIMEOUT`; the K run
  concurrently so wall-clock ≈ one proposer + critique + aggregate (~50s worst
  case for a hard subtask), not K×.
- **Cost:** a debate is ~`K + 1 (critique) + 1 (aggregate)` model calls. Bounded
  by the **cost-guard** (daily ceiling) and never runs on trivial subtasks. The
  cost-guard records each call's spend as usual.
- **Hangs:** proposers are subagents under the existing **subagent inactivity
  watchdog**; the coordinator can't freeze on a stuck proposer.
- **`auto_delegate` interaction:** both can be on; the gold gate is independent
  (delegation decides *who* runs work, gold decides *how* hard subtasks run). The
  recursion guard prevents nested debates.
- **Gold off / not configured:** `cheap_route_gold_mode=false` (global) OR
  `gold_mode_enabled` unset → behavior is exactly today's (single-model). Purely
  additive, default-off.

## Components & boundaries

| Unit | File | Responsibility |
|---|---|---|
| `run_debate` + prompts | `agent/cheap_route.rs` | The debate algorithm (propose/critique/aggregate); pure orchestration over the backend. |
| gold gate | `agent/cheap_route.rs` ~`:467` | Decide debate vs single-model per subtask. |
| proposer tiles | `agent/cheap_route.rs` + `server/swarm.rs` | Register + status-broadcast each proposer for the gallery. |
| gold side-panel | `agent/cheap_route.rs` + `side_panel.rs` | Write the gold result + candidates to the side panel. |
| phase header | `tui/info_widget_swarm_gallery.rs` / `ui.rs` | One-line phase indicator. |
| `/gold` command | `tui/app/commands.rs` | Per-session toggle + status. |
| session flag | `jcode-base/src/session.rs` | `gold_mode_enabled` persistence. |
| config | `jcode-config-types/src/lib.rs` | `cheap_route_gold_mode`, `cheap_route_gold_k`. |
| tool wiring | `tool/cheap_route_tool.rs` | Read session flag → pass gold into the backend. |

## Testing

- **`run_debate` (unit, pure-ish via a fake `CheapRouteBackend`):** K candidates
  → aggregate called with all K + critique; order preserved; **< 2 surviving
  candidates falls back to single-model**; a failing proposer is dropped not
  fatal; **recursion guard** (proposer backend has gold off).
- **K-distinct selection (unit):** top-K from a ranked list are distinct models;
  `K = min(gold_k, available)`.
- **Config/session flag (unit):** `gold_mode_enabled` round-trips; gold runs only
  when both global config and session flag are on.
- **`/gold` command (unit):** `on`/`off`/status set + persist the session flag
  (mirror existing command tests).
- **Concurrency (unit, paused time):** proposers run concurrently (wall-clock ≈
  max, not sum) — reuse the `run_in_parallel_ordered` test pattern.
- Gallery/side-panel are status-broadcast reuse (covered by existing swarm/side
  -panel paths); a thin integration check that a debate broadcasts K member
  statuses.

## Out of scope (YAGNI)

- Full per-proposer token streaming in the gallery (status tiles only for now).
- Iterative multi-layer MoA (N rounds) — single debate round only.
- Manual model picker (auto-pick K cheapest distinct only).
- Per-message opt-in / every-turn modes (coordinator auto-decides only).
