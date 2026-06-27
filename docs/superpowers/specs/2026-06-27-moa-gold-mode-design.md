# Gold Mode — Multi-Model Debate ("Mixture-of-Agents") — Design

**Status:** Approved design, revised after deep verification (2 blockers fixed, 7 ambiguities resolved). Pending implementation plan.
**Date:** 2026-06-27

## Post-verification revisions (read first)

Deep verification by cheap subagents found two real blockers + several gaps the
first draft missed; all are now folded into the design below:

1. **Concurrency (compile blocker):** `run_subtask` is `#[async_trait]`, so its
   future borrows `&self`/`&subtask`/`&model` — the naive
   `run_in_parallel_ordered(|_,(m,a)| backend.run_subtask(...))` will NOT compile.
   **Resolution:** `run_debate` takes `Arc<dyn CheapRouteBackend + Send + Sync>`;
   each proposer closure `clone`s the Arc and `move`s OWNED `(model, api, subtask
   clone)` into an `async move`. `run_cheap_route` wraps its `&dyn` backend in an
   `Arc` only on the debate path.
2. **Status tiles (access blocker):** `update_member_status` /
   `broadcast_swarm_status` are `pub(super)` (server-only) and
   `ProviderCheapBackend` holds NO swarm-state Arcs — so the gallery bridge as
   first written is impossible. **Resolution:** add an optional
   `DebateStatusReporter` (trait object) to `ProviderCheapBackend`, constructed by
   `CheapRouteTool::execute` (which can receive the swarm-state Arcs at tool
   registration), plus a `pub(crate)` `report_member_status(...)` wrapper in the
   server module. If the reporter is `None` (e.g. non-server contexts), the
   debate still runs with no tiles.
3. **Critique ran on the cheap model:** `ask_parent` calls the coordinator, which
   `ProviderCheapBackend::new` switches to the CHEAPEST model — so an LLM critique
   would be weak. **Resolution:** add `ask_strong(prompt) -> String` to the
   backend (runs the strong model) and use it for the critique step.
4. **Timeout:** cheap models on hard subtasks need more than the 30s
   `SUBTASK_TIMEOUT`. **Resolution:** new `DEBATE_PROPOSER_TIMEOUT = 60s` for the
   proposer + aggregate calls on the debate path.
5. **gold_mode plumbing:** `run_cheap_route` has no `gold_mode` today.
   **Resolution:** `ProviderCheapBackend` gains `gold_mode: bool` + a
   `CheapRouteBackend::gold_mode(&self) -> bool` accessor read at `:467`.
6. **Session orphans:** K proposer sessions are `Session::create`d + saved per
   debate. **Resolution:** delete/close proposer sessions after the debate
   resolves (success or abort).

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

New `run_debate(backend: Arc<dyn CheapRouteBackend + Send + Sync>, subtask, proposer_models, k)`:

```
// proposers — own everything moved into each async (async_trait lifetime fix):
candidates = run_in_parallel_ordered(proposer_models[..k], |_, (model, api)| {
    let backend = backend.clone();          // Arc clone
    let subtask = subtask.clone();          // owned
    async move { backend.run_subtask(&subtask, &model, api.as_deref()).await }
})  // Vec<Result<String>>
candidates = surviving Ok(..) only                      // drop failed (decision #1/#7)
if candidates.len() < 2 { return backend.ask_strong(single_prompt(subtask)) }  // decision #2
critique = backend.ask_strong(critique_prompt(subtask, candidates))   // STRONG model, not cheap
gold     = backend.ask_strong(aggregate_prompt(subtask, candidates, critique))
// decision #5: if aggregate errors, return candidates[0]
return gold
```

`ask_strong(prompt) -> Result<String>` is a NEW backend method that runs the
strong model (`cheap_route_strong_model`, else the coordinator's own model) — used
for critique + aggregate so they are NOT on the cheapest model. Proposer + strong
calls use `DEBATE_PROPOSER_TIMEOUT` (60s).

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

- **Reporter plumbing (the real cost — `update_member_status` is `pub(super)`):**
  add an optional `DebateStatusReporter` trait object to `ProviderCheapBackend`.
  `CheapRouteTool::execute` constructs it with the server's swarm-state Arcs +
  calls a new `pub(crate)` `server::report_member_status(...)` wrapper. Around
  each proposer, `run_debate` calls `reporter.proposer(model, Running|Done|Failed)`
  which registers/updates a lightweight `swarm_members` entry + broadcasts
  `SwarmStatus`. If the reporter is `None` (non-server context, or not injected),
  the debate runs with no tiles (test F5). This is NOT a change to the agent
  execution path, but it IS new plumbing across the tool/server boundary.
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

### Resolved design decisions (the 7 ambiguities)

1. **All proposers fail (0 survivors):** fall back to a single strong-model
   result for the subtask. Never error the whole task on debate failure.
2. **< 2 surviving / K < 2 / 0-1 models / K=0 config:** no debate (you can't
   debate one answer) → single strong-model result. Debate requires ≥ 2 distinct
   models.
3. **"Distinct" key = `(provider, model)`** (what `dedupe_model_routes` uses).
   `api_method` variants of the same model collapse to one proposer.
4. **Mid-debate toggle:** the gold flag is sampled ONCE at the start of each
   subtask. Flipping `/gold` mid-subtask does not interrupt an in-flight debate;
   it affects the next subtask. Predictable, no torn state.
5. **Aggregate (strong model) errors:** recoverable — return the first surviving
   candidate as the result rather than failing the subtask. Log a warning.
6. **No strong model AND no parent:** the aggregator falls back to the
   coordinator's own model (`ask_strong` defaults to it); if even that is
   unavailable, return the first candidate. Never panic.
7. **Critique empty/errors:** advisory only — aggregate proceeds with an empty
   critique. The gold result is still produced.

## Value & scope (honest)

Verification's honest take: the debate adds the most value on **reasoning/design-
heavy** subtasks where K diverse models explore different approaches and the
critique catches consistent errors (~10-25% quality lift, ~+40% cost vs the
strong model alone). It adds **little** on **code-writing** subtasks, where
jcode's existing execution-grounded `verify+repair` loop (`cheap_route_verify_cmd`)
is already a stronger correctness signal than an LLM critique. Implication:
default to debate on hard *reasoning* subtasks; rely on verify+repair for code.
This is a deliberate scoping note, not a blocker — the difficulty gate already
limits when debate runs, and it is default-off.

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

## Testing — full matrix (54 cases; [U]=unit, [I]=integration)

All `run_debate` tests use a **fake `CheapRouteBackend`** (records calls, scripts
proposer outputs/errors/delays) so the orchestration is unit-testable without
real models. Concurrency tests use `tokio::time` paused.

**A. `run_debate` orchestration**
- A1 [U] K candidates collected; aggregate gets all K + critique; order preserved.
- A2 [U] <2 survivors → single strong-model fallback; no critique call.
- A3 [U] exactly 2 candidates → full debate runs (not a fallback).
- A4 [U] a proposer Err/timeout → dropped; others still aggregate.
- A5 [U] ALL proposers fail → single strong-model fallback (decision #1).
- A6 [U] recursion guard: proposer + aggregator sub-runs have `gold_mode=false`.
- A7 [U] empty/whitespace candidate text passed through, no panic.
- A8 [U] candidate order preserved end-to-end.

**B. K-distinct model selection**
- B1 [U] top-K distinct by `(provider,model)`; dups collapsed.
- B2 [U] fewer than K available → `K = available`.
- B3 [U] 0 or 1 models → no debate (fallback).
- B4 [U] banned/unhealthy models excluded from proposers.
- B5 [U] `cheap_route_gold_k`=1 → no debate (decision #2).

**C. Gold gate**
- C1 [U] difficulty>threshold + gold on → debate.
- C2 [U] difficulty≤threshold → single model even with gold on.
- C3 [U] gold off (session unset OR global false) → never debates.
- C4 [I] `auto_delegate`+gold both on → no nested debate (recursion guard).
- C5 [U] difficulty == threshold (boundary) → no debate.

**D. Config + session flag + `/gold`**
- D1 [U] defaults: `cheap_route_gold_mode=false`, `cheap_route_gold_k=3`.
- D2 [U] `gold_mode_enabled` round-trips save/load.
- D3 [U] `/gold on` sets+persists. D4 [U] `/gold off` sets+persists.
- D5 [U] `/gold status`/`/gold` shows state. D6 [U] `/gold <junk>` → usage error, no change.
- D7 [U] global vs session: debate only when BOTH on.
- D8 [U] session `None` + global true → debate (`unwrap_or(false) && global`).

**E. Concurrency / timeout / cost**
- E1 [U] K proposers concurrent: wall-clock ≈ max not sum (paused time).
- E2 [U] per-proposer `DEBATE_PROPOSER_TIMEOUT` fires independently.
- E3 [I] cost-guard records all K + critique + aggregate calls.
- E4 [U] one slow proposer doesn't stall the others past its own timeout.
- E5 [I] critique call cost recorded.

**F. UI status tiles + side panel**
- F1 [I] K member-status broadcasts (running→done/failed) on a debate.
- F2 [I] phase header transitions propose→critique→merge.
- F3 [I] gold result written to side panel (with candidate list).
- F4 [I] tiles de-register / marked done after the debate (no stale "running").
- F5 [U] reporter `None` → debate still runs, no broadcasts, no panic.

**G. Edge cases**
- G1 [U] unicode candidate text preserved. G2 [U] very large (100KB) candidate handled.
- G3 [U] same model, two api_methods → one proposer (decision #3).
- G4 [U] only model == strong model, K=3 → no debate (need ≥2 distinct).
- G5 [U] `gold_k`=0 → no debate, no panic. G6 [U] `gold_k`≫available → `K=available`.
- G7 [I] `/gold off` mid-debate → in-flight debate completes (flag sampled at start, decision #4).
- G8 [U] critique empty/Err → aggregate still proceeds (decision #7).
- G9 [U] aggregate Err → return first surviving candidate (decision #5).
- G10 [U] difficulty unset/0 → single model, no crash.
- G11 [U] `cheap_route_strong_model`=None → aggregator uses coordinator model.
- G12 [U] no strong + no parent → first candidate returned, no panic (decision #6).
- G13 [U] asymmetric candidate sizes (1 word vs paragraph) → all aggregated.
- G14 [U] all candidates identical → debate still runs, no dedup.
- G15 [U] proposer session cleanup: K sessions deleted/closed after the debate.

**Totals:** ~45 unit + ~9 integration. The fake-backend harness covers A/B/C/E/G;
the server-plumbed reporter + side panel are the integration set (F, C4, E3/E5, G7).

## Out of scope (YAGNI)

- Full per-proposer token streaming in the gallery (status tiles only for now).
- Iterative multi-layer MoA (N rounds) — single debate round only.
- Manual model picker (auto-pick K cheapest distinct only).
- Per-message opt-in / every-turn modes (coordinator auto-decides only).
