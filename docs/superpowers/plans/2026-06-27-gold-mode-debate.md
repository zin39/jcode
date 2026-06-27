# Gold Mode (Multi-Model Debate) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `/gold` mode — on hard *reasoning* subtasks the cheap-route orchestrator runs K distinct cheap models as proposers (concurrently), then a strong model merges them into one "gold" answer, watched live in the TUI, with consensus early-exit and code-subtask skip for cost.

**Architecture:** Purely additive, default-off. The debate is a new branch inside `run_cheap_route`'s per-subtask loop (`crates/jcode-app-core/src/agent/cheap_route.rs`), gated by a per-session `/gold` flag + global config + the existing difficulty tier. Proposers reuse `CheapRouteBackend::run_subtask` driven concurrently with `futures::future::join_all` (borrowed futures — no Arc, no signature change). Critique is folded into a single strong-model aggregate call (`ask_strong`). Live tiles use an optional `DebateStatusReporter` injected by `CheapRouteTool`.

**Tech Stack:** Rust, tokio, async_trait, futures, ratatui (TUI), serde.

**Reference spec:** `docs/superpowers/specs/2026-06-27-moa-gold-mode-design.md` (test matrix A–H, the 7 resolved decisions, cost opts).

**Run tests with:** `source ~/.cargo/env && cargo test -p <crate> --lib <filter>`. Cheap-route tests live in `crates/jcode-app-core/src/agent/cheap_route.rs` `#[cfg(test)] mod tests`.

---

## File structure (what changes, and why)

| File | Change | Responsibility |
|---|---|---|
| `crates/jcode-config-types/src/lib.rs` | add `cheap_route_gold_mode: bool`, `cheap_route_gold_k: usize` to `AgentsConfig` + defaults | global enable + K |
| `crates/jcode-base/src/session.rs` | add `gold_mode_enabled: Option<bool>` (field + stub + Default) | per-session toggle persistence |
| `crates/jcode-tui/src/tui/app/commands.rs` | add `handle_gold_command` + dispatch | `/gold on\|off\|status\|k=N` |
| `crates/jcode-app-core/src/agent/cheap_route.rs` | `ask_strong` + `gold_mode` on the trait; `DEBATE_PROPOSER_TIMEOUT`, `MAX_DEBATE_CANDIDATE_CHARS`; `consensus`, `truncate_tail`, `is_code_subtask`, `run_debate`; the gate branch | debate engine |
| `crates/jcode-app-core/src/agent/debate_status.rs` (new) | `DebateStatusReporter` trait + no-op | gallery tile callback |
| `crates/jcode-app-core/src/server/swarm.rs` | `pub(crate) report_debate_member(...)` wrapper | expose status to the reporter |
| `crates/jcode-app-core/src/tool/cheap_route_tool.rs` | read session flag, build reporter, pass `gold_mode`/`k` | wiring + run summary |

The orchestrator (`run_debate`) is **pure over the `CheapRouteBackend` trait** → unit-testable with a fake backend (the trait already exists for this; see its doc comment at `cheap_route.rs:50`).

---

## Phase 1 — Config, session flag, `/gold` command (isolated, low risk)

### Task 1: Global config fields

**Files:**
- Modify: `crates/jcode-config-types/src/lib.rs` (AgentsConfig, beside `auto_delegate`; Default impl)

- [ ] **Step 1: Write the failing test** (add to that crate's tests, or a new `#[cfg(test)]` block)

```rust
#[test]
fn agents_config_gold_defaults() {
    let c = AgentsConfig::default();
    assert!(!c.cheap_route_gold_mode);
    assert_eq!(c.cheap_route_gold_k, 3);
}
```

- [ ] **Step 2: Run to verify it fails**
Run: `source ~/.cargo/env && cargo test -p jcode-config-types --lib agents_config_gold_defaults`
Expected: FAIL — no field `cheap_route_gold_mode`.

- [ ] **Step 3: Implement** — add after the `auto_delegate` field:

```rust
    /// Master enable for gold mode (multi-model debate on hard subtasks). The
    /// per-session `/gold` flag only takes effect when this is also true. Off by default.
    #[serde(default)]
    pub cheap_route_gold_mode: bool,
    /// Number of distinct cheap-model proposers in a debate. Default 3.
    #[serde(default = "default_cheap_route_gold_k")]
    pub cheap_route_gold_k: usize,
```
Add the default fn (beside the other `default_*` fns):
```rust
fn default_cheap_route_gold_k() -> usize { 3 }
```
Add to the `Default for AgentsConfig` impl (beside `auto_delegate: false`):
```rust
            cheap_route_gold_mode: false,
            cheap_route_gold_k: default_cheap_route_gold_k(),
```

- [ ] **Step 4: Run to verify it passes**
Run: `source ~/.cargo/env && cargo test -p jcode-config-types --lib agents_config_gold_defaults` — Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/jcode-config-types/src/lib.rs
git commit -m "feat(config): add cheap_route_gold_mode + cheap_route_gold_k"
```

### Task 2: Per-session `gold_mode_enabled` flag

**Files:**
- Modify: `crates/jcode-base/src/session.rs` (Session struct beside `autoreview_enabled`; `SessionStartupStub`; `Default`)

- [ ] **Step 1: Failing test**
```rust
#[test]
fn gold_mode_enabled_roundtrips() {
    let mut s = Session::create(None, Some("t".into()));
    s.gold_mode_enabled = Some(true);
    s.save().unwrap();
    let loaded = Session::load(&s.id).unwrap();
    assert_eq!(loaded.gold_mode_enabled, Some(true));
}
```
- [ ] **Step 2: Run → fail** (`cargo test -p jcode-base --lib gold_mode_enabled_roundtrips`): no field.
- [ ] **Step 3: Implement** — add beside `autoreview_enabled`:
```rust
    /// Whether gold mode (debate on hard subtasks) is on for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gold_mode_enabled: Option<bool>,
```
Add `#[serde(default)] gold_mode_enabled: Option<bool>,` to `SessionStartupStub`, and `gold_mode_enabled: None,` to the `Default`/`create` constructor(s) — match the exact pattern `autoreview_enabled` uses in each of the three places.
- [ ] **Step 4: Run → pass.**
- [ ] **Step 5: Commit** `feat(session): add gold_mode_enabled per-session flag`.

### Task 3: `/gold` command

**Files:**
- Modify: `crates/jcode-tui/src/tui/app/commands.rs` (new `handle_gold_command`, dispatch ~`:1865`)

- [ ] **Step 1: Failing tests** (mirror existing command tests in that file's test module)
```rust
#[test]
fn gold_command_on_off_set_session_flag() {
    let mut app = test_app(); // existing test harness builder used by other command tests
    assert!(super::handle_gold_command(&mut app, "/gold on"));
    assert_eq!(app.session.gold_mode_enabled, Some(true));
    assert!(super::handle_gold_command(&mut app, "/gold off"));
    assert_eq!(app.session.gold_mode_enabled, Some(false));
}
#[test]
fn gold_command_k_override_and_junk() {
    let mut app = test_app();
    assert!(super::handle_gold_command(&mut app, "/gold k=5"));
    assert_eq!(app.gold_k_override, Some(5));
    assert!(super::handle_gold_command(&mut app, "/gold k=0")); // rejected, no change
    assert_eq!(app.gold_k_override, Some(5));
    assert!(!super::handle_gold_command(&mut app, "/notgold"));
}
```
(If `test_app()` differs, copy the exact constructor the neighbouring `handle_goals_command` tests use.)
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement** `handle_gold_command` (mirror `handle_goals_command` at `commands.rs:2224` + the `/model` handler): parse `/gold`, `/gold status` (show state from `app.session.gold_mode_enabled` + `config().agents.cheap_route_gold_mode`), `/gold on|off` (set `app.session.gold_mode_enabled`, `app.session.save()`, status notice), `/gold k=N` (parse N; if `N>=2` set `app.gold_k_override = Some(N)` else error). Return `false` for non-`/gold`. Add a `gold_k_override: Option<usize>` field to `App` (default `None`). Wire `if handle_gold_command(app, trimmed) { return true; }` into the dispatch near the `handle_goals_command` call (~`:1865`), and register `/gold` in the command list (`state_ui_input_helpers.rs`, like `/goals`).
- [ ] **Step 4: Run → pass.**
- [ ] **Step 5: Commit** `feat(tui): /gold on|off|status|k=N command`.

---

## Phase 2 — Backend extensions + pure helpers

### Task 4: `ask_strong` + `gold_mode` on `CheapRouteBackend`; debate constants

**Files:**
- Modify: `crates/jcode-app-core/src/agent/cheap_route.rs` (trait ~`:52`; constants near `:1029`)

- [ ] **Step 1: Failing test** (fake backend in the test module; the module already has fake backends — extend one)
```rust
#[tokio::test]
async fn ask_strong_defaults_to_ask_parent() {
    let b = FakeBackend::new().with_parent_reply("PARENT");
    assert_eq!(b.ask_strong("q").await.unwrap(), "PARENT");
    assert!(!b.gold_mode()); // default false
}
```
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement** — add to the `#[async_trait] pub trait CheapRouteBackend` (defaults so existing/fake impls don't break):
```rust
    /// Run the STRONG model (cheap_route_strong_model, else the coordinator's own
    /// model) for one-shot text — used for the debate critique+aggregate so they
    /// are NOT on the cheapest model. Default delegates to `ask_parent`.
    async fn ask_strong(&self, prompt: &str) -> Result<String> { self.ask_parent(prompt).await }
    /// Whether this backend should run gold-mode debates. Default false (so a
    /// proposer/aggregator sub-backend never recurses). Production impl reads the
    /// session+config flag.
    fn gold_mode(&self) -> bool { false }
```
Add constants near `SUBTASK_TIMEOUT`:
```rust
const DEBATE_PROPOSER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const MAX_DEBATE_CANDIDATE_CHARS: usize = 3000;
```
- [ ] **Step 4: Run → pass.** **Step 5: Commit** `feat(cheap_route): ask_strong + gold_mode on backend; debate constants`.

### Task 5: `consensus` + `truncate_tail` helpers (pure)

**Files:** Modify `cheap_route.rs` (free fns near `strip_code_fence` `:148`).

- [ ] **Step 1: Failing tests**
```rust
#[test]
fn consensus_matches_on_fence_and_case() {
    let c = vec!["```\nFoo Bar\n```".to_string(), "foo bar".to_string(), "totally other".to_string()];
    assert_eq!(consensus(&c).as_deref(), Some("```\nFoo Bar\n```")); // returns an ORIGINAL agreeing candidate
}
#[test]
fn consensus_none_when_all_differ() {
    let c = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert!(consensus(&c).is_none());
}
#[test]
fn truncate_tail_keeps_tail_when_over_limit() {
    let s = "x".repeat(5000);
    let t = truncate_tail(&s, 3000);
    assert!(t.len() <= 3000 + 16); // tail kept, small marker allowance
    assert!(t.ends_with(&"x".repeat(10)));
}
```
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement**
```rust
/// If >=2 candidates agree after normalize (strip_code_fence + casefold +
/// whitespace-collapse), return one ORIGINAL agreeing candidate; else None.
fn consensus(candidates: &[String]) -> Option<String> {
    fn norm(s: &str) -> String {
        strip_code_fence(s).split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_lowercase()
    }
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            if !norm(&candidates[i]).is_empty() && norm(&candidates[i]) == norm(&candidates[j]) {
                return Some(candidates[i].clone());
            }
        }
    }
    None
}
/// Keep the last `max` chars (the tail holds the conclusion / return value),
/// prefixed with a marker when truncated. Mirrors MAX_VERIFY_OUTPUT handling.
fn truncate_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_string(); }
    let tail: String = s.chars().rev().take(max).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…(trimmed)\n{tail}")
}
```
- [ ] **Step 4: Run → pass.** **Step 5: Commit** `feat(cheap_route): consensus + truncate_tail helpers`.

### Task 6: `is_code_subtask` helper (pure)

**Files:** Modify `cheap_route.rs`.

- [ ] **Step 1: Failing tests**
```rust
#[test]
fn is_code_subtask_detects_code() {
    let code = Subtask { description: "edit main.rs".into(), prompt: "modify src/main.rs to add a fn".into(), difficulty: 4 };
    let reason = Subtask { description: "design the api".into(), prompt: "what's the best architecture for X".into(), difficulty: 4 };
    assert!(is_code_subtask(&code));
    assert!(!is_code_subtask(&reason));
}
```
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement** (heuristic; cheap + good-enough — the decomposer `difficulty` already gates hardness):
```rust
/// Heuristic: a subtask is "code" if it edits/creates files or names code paths.
/// Used to skip the LLM debate for code (verify+repair is the better signal).
fn is_code_subtask(s: &Subtask) -> bool {
    let t = format!("{} {}", s.description, s.prompt).to_ascii_lowercase();
    const CODE_HINTS: &[&str] = &[
        "edit", "write", "modify", "implement", "refactor", "fix the", "add a fn",
        "function", "```", ".rs", ".ts", ".py", ".go", ".js", "src/", "compile", "cargo",
    ];
    CODE_HINTS.iter().any(|h| t.contains(h))
}
```
- [ ] **Step 4: Run → pass.** **Step 5: Commit** `feat(cheap_route): is_code_subtask heuristic`.

---

## Phase 3 — `run_debate` core (the engine; heavily tested)

### Task 7: `run_debate` happy path (A1, A8) + single-strong-call (H2)

**Files:** Modify `cheap_route.rs` (new `run_debate` + `build_debate_aggregate_prompt`). Test module: extend the fake backend to record `ask_strong` calls + script `run_subtask` per-model replies.

- [ ] **Step 1: Failing tests**
```rust
#[tokio::test]
async fn run_debate_aggregates_all_candidates_one_strong_call() {
    let b = FakeBackend::new()
        .with_subtask_reply("m1", "alpha").with_subtask_reply("m2", "beta").with_subtask_reply("m3", "gamma")
        .with_strong_reply("GOLD");
    let st = hard_subtask();
    let models = vec![("m1".into(), None), ("m2".into(), None), ("m3".into(), None)];
    let gold = run_debate(&b, &st, &models, 3).await.unwrap();
    assert_eq!(gold, "GOLD");
    // exactly ONE strong call, and its prompt contains all three candidates in order
    let strong = b.strong_prompts();
    assert_eq!(strong.len(), 1);
    let idx_a = strong[0].find("alpha").unwrap();
    let idx_b = strong[0].find("beta").unwrap();
    let idx_g = strong[0].find("gamma").unwrap();
    assert!(idx_a < idx_b && idx_b < idx_g);
}
```
- [ ] **Step 2: Run → fail** (no `run_debate`).
- [ ] **Step 3: Implement** (concurrency = `join_all` of borrowed, per-proposer-timeout futures — no Arc):
```rust
use futures::future::join_all;

async fn run_debate(
    backend: &dyn CheapRouteBackend,
    subtask: &Subtask,
    proposer_models: &[(String, Option<String>)],
    gold_k: usize,
) -> Result<String> {
    let k = gold_k.min(proposer_models.len());
    // proposers — concurrent, ordered, borrowing (async_trait-safe):
    let futs = proposer_models[..k].iter().map(|(model, api)| async move {
        tokio::time::timeout(
            DEBATE_PROPOSER_TIMEOUT,
            backend.run_subtask(subtask, model, api.as_deref()),
        ).await
    });
    let raw = join_all(futs).await; // Vec<Result<Result<String>, Elapsed>>, in order
    let candidates: Vec<String> = raw.into_iter()
        .filter_map(|r| r.ok().and_then(|inner| inner.ok())) // drop timeouts + errors (decision #1)
        .collect();
    // decision #2: need >=2 to debate; else single strong result
    if candidates.len() < 2 {
        return backend.ask_strong(&build_debate_single_prompt(subtask)).await;
    }
    // cost opt 2: consensus early-exit (no strong calls)
    if let Some(agreed) = consensus(&candidates) {
        return Ok(agreed);
    }
    // cost opt 4: bound aggregate input
    let trimmed: Vec<String> = candidates.iter()
        .map(|c| truncate_tail(strip_code_fence(c), MAX_DEBATE_CANDIDATE_CHARS))
        .collect();
    // cost opt 1: ONE strong call (critique folded in)
    match backend.ask_strong(&build_debate_aggregate_prompt(subtask, &trimmed)).await {
        Ok(gold) => Ok(gold),
        Err(_) => Ok(candidates[0].clone()), // decision #5
    }
}

fn build_debate_single_prompt(s: &Subtask) -> String {
    format!("Complete this task as best you can.\n\nTASK: {}\n", s.prompt)
}
fn build_debate_aggregate_prompt(s: &Subtask, candidates: &[String]) -> String {
    let mut p = format!(
        "You are the aggregator in a multi-model debate. {n} models each answered the task below.\n\
         First note any errors or gaps across the candidate answers, then write the single BEST \
         answer (do not mention the debate). End with one line: 'why: <=1 sentence on what made it best'.\n\n\
         TASK: {task}\n\n", n = candidates.len(), task = s.prompt);
    for (i, c) in candidates.iter().enumerate() {
        p.push_str(&format!("--- candidate {} ---\n{}\n\n", i + 1, c));
    }
    p
}
```
- [ ] **Step 4: Run → pass.** **Step 5: Commit** `feat(cheap_route): run_debate (concurrent proposers + folded aggregate)`.

### Task 8: Fallback + survivor tests (A2, A3, A4, A5, B2/B3, decision #2)

**Files:** `cheap_route.rs` test module (no impl change expected; verifies Task 7's branches).

- [ ] **Step 1: Failing/asserting tests** — add, each via the fake backend:
```rust
#[tokio::test] async fn debate_one_survivor_falls_back_to_strong() { /* m2,m3 error; with_strong_reply("S"); assert gold=="S" and strong_prompts contains no candidate markers */ }
#[tokio::test] async fn debate_exactly_two_runs_full() { /* 2 distinct candidates -> 1 strong call, gold from strong */ }
#[tokio::test] async fn debate_proposer_error_dropped_others_aggregate() { /* m1 Err -> aggregate over m2,m3 */ }
#[tokio::test] async fn debate_all_fail_falls_back_to_strong() { /* all Err -> single strong prompt path */ }
```
Implement each per the matrix (A2–A5) using the fake backend (`with_subtask_error("m2", ...)`, etc.). These should PASS against Task 7's code; if any fails, fix `run_debate`.
- [ ] **Step 2: Run → confirm pass** (TDD-after-the-fact is acceptable for branch coverage of one function). **Step 3: Commit** `test(cheap_route): debate fallback + survivor cases`.

### Task 9: Consensus + truncation + single-strong behavior (H1, H3, H6)

- [ ] Tests: `debate_consensus_skips_strong` (≥2 agree → `b.strong_prompts().is_empty()`, gold == agreed); `debate_no_consensus_one_strong` (distinct → exactly 1 strong call); `debate_truncates_long_candidate` (a >3000-char candidate appears trimmed in the strong prompt). Run → pass (covered by Task 7). Commit `test(cheap_route): consensus + truncation`.

### Task 10: Concurrency + timeout (E1, E2, E4)

- [ ] **Step 1:** `#[tokio::test(start_paused = true)]` — fake backend whose `run_subtask` sleeps per model (m1 30s, m2 5s, m3 3s); assert wall-clock ≈ max (≈30s) not sum, and a proposer exceeding `DEBATE_PROPOSER_TIMEOUT` (60s) is dropped while faster ones still aggregate. (Mirror `parallel_tools` paused-time tests.)
- [ ] **Step 2-4:** Run → pass. **Step 5: Commit** `test(cheap_route): debate concurrency + timeout`.

### Task 11: Edge cases (A7, G1, G2, G13, G14, H8)

- [ ] Tests: empty/whitespace candidate passes through; unicode preserved; 100KB candidate handled (truncated, no panic); asymmetric sizes aggregated; identical candidates → consensus path; aggregate prompt asks for the "why:" rationale. Run → pass. Commit `test(cheap_route): debate edge cases`.

---

## Phase 4 — Gold gate + recursion guard + session cleanup

### Task 12: Gate the debate into the subtask loop (C1, C2, C5, B5, code-skip H4/H5)

**Files:** Modify `cheap_route.rs` — the `for subtask in &subtasks` loop (~`:466`). Add `gold_mode`+`gold_k` reads and the branch BEFORE the existing `task_candidates` fallback loop.

- [ ] **Step 1: Failing test** — a fake backend with `gold_mode()==true`, config gold on, a hard reasoning subtask → `run_cheap_route` produces the debate's gold; a hard CODE subtask → no debate (single model); a trivial subtask → no debate.
```rust
#[tokio::test]
async fn gate_runs_debate_only_for_hard_reasoning_when_gold_on() {
    set_test_config_gold(true, 3);                  // helper sets cheap_route_gold_mode
    let b = FakeBackend::new().gold_on()
        .with_decompose(vec![("design core algo", 5 /*reasoning*/), ("edit src/x.rs", 5 /*code*/), ("rename var", 1)])
        .with_subtask_reply_any("cheap").with_strong_reply("GOLD");
    let out = run_cheap_route(&b, "task").await.unwrap();
    // reasoning hard subtask used the debate (one strong aggregate call); code + trivial did not
    assert_eq!(b.strong_prompts().len(), 1);
}
```
- [ ] **Step 2: Run → fail.**
- [ ] **Step 3: Implement** — inside the loop, compute once before building `task_candidates`:
```rust
        let gold_on = backend.gold_mode() && crate::config::config().agents.cheap_route_gold_mode;
        let gold_k = crate::config::config().agents.cheap_route_gold_k;
        if gold_on
            && subtask.difficulty > difficulty_threshold
            && !is_code_subtask(subtask)            // code -> strong + verify+repair (cost opt 3)
        {
            // top-K DISTINCT cheap models as proposers (ranked is already distinct via dedupe)
            let proposers: Vec<(String, Option<String>)> = ranked.iter()
                .map(|c| (c.route.model.clone(), Some(c.route.api_method.clone())))
                .take(gold_k.max(2))
                .collect();
            if proposers.len() >= 2 {
                match run_debate(backend, subtask, &proposers, gold_k).await {
                    Ok(output) => {
                        results.push(SubtaskResult { /* same shape the loop builds: model_used = "debate", output, .. */ });
                        continue; // skip the single-model fallback loop for this subtask
                    }
                    Err(_) => { /* fall through to the existing single-model loop */ }
                }
            }
        }
```
(Match `SubtaskResult`'s exact fields from the surrounding code; set `model_used` to a label like `format!("debate({})", proposers.len())`.)
- [ ] **Step 4: Run → pass.** **Step 5: Commit** `feat(cheap_route): gate gold debate on hard reasoning subtasks`.

### Task 13: Gold off + recursion guard (C3, C4, A6, D7, D8)

**Files:** `cheap_route.rs` (`ProviderCheapBackend::gold_mode`) + test module.

- [ ] **Step 1: Tests** — gold off (session unset OR global false) never debates; the proposer/aggregator sub-runs see `gold_mode()==false`. The recursion guard is structural: proposers are run via `run_subtask` (which forks the provider into a fresh subagent that does NOT carry a gold backend), and the gate only triggers in the top `run_cheap_route`. Add a test that `ProviderCheapBackend::gold_mode()` returns the stored flag and that a debate-spawned subagent's own cheap_route (if any) has it false. Assert via the fake backend that `run_subtask`/`ask_strong` calls during a debate never re-enter `run_debate`.
- [ ] **Step 2-4:** Implement `gold_mode` on `ProviderCheapBackend` to return the `gold_mode: bool` field set at construction (Task 18). Run → pass. **Step 5: Commit** `feat(cheap_route): gold_mode accessor + recursion-guard tests`.

### Task 14: Proposer session cleanup (G15)

**Files:** `cheap_route.rs` `ProviderCheapBackend::run_subtask` (~`:1101`).

- [ ] **Step 1: Test** — after a debate, the K proposer sessions are deleted/closed (count proposer sessions before/after, assert cleaned). Use a fake `Session` dir or assert `Session::delete` called.
- [ ] **Step 2-4:** In `run_subtask`, after `run_once_capture` returns, delete the just-created proposer session (`Session::delete(&session_id)` or the existing close path) so K-per-subtask sessions don't accumulate. Run → pass. **Step 5: Commit** `fix(cheap_route): clean up proposer sessions after a debate`.

---

## Phase 5 — Live UI: status tiles, side panel, run summary

### Task 15: `DebateStatusReporter` trait + no-op

**Files:** Create `crates/jcode-app-core/src/agent/debate_status.rs`; declare `mod debate_status;` in `agent.rs`.

- [ ] **Step 1: Test**
```rust
#[test]
fn noop_reporter_is_safe() {
    let r = NoopDebateReporter;
    r.proposer("deepseek", DebatePhase::Running); // must not panic
}
```
- [ ] **Step 2-4: Implement**
```rust
pub enum DebatePhase { Running, Done, Failed }
pub trait DebateStatusReporter: Send + Sync {
    fn proposer(&self, model: &str, phase: DebatePhase);
    fn phase(&self, label: &str);              // "propose" | "critique" | "merge"
    fn gold(&self, markdown: &str);            // write gold + candidates to side panel
}
pub struct NoopDebateReporter;
impl DebateStatusReporter for NoopDebateReporter {
    fn proposer(&self, _m: &str, _p: DebatePhase) {}
    fn phase(&self, _l: &str) {}
    fn gold(&self, _md: &str) {}
}
```
`run_debate` gains an `&dyn DebateStatusReporter` param (callers pass `&NoopDebateReporter` in tests). Update Task 7's signature + the gate call accordingly; the `F5` test (reporter None/noop → debate still runs) is satisfied. Run → pass. **Step 5: Commit** `feat(cheap_route): DebateStatusReporter (no-op default)`.

### Task 16: Server-side reporter wrapper

**Files:** `crates/jcode-app-core/src/server/swarm.rs` (add `pub(crate) fn report_debate_member(...)` thin wrapper over `update_member_status` + `broadcast_swarm_status`), and a concrete `SwarmDebateReporter { swarm_members, swarms_by_id, ... }` impl of `DebateStatusReporter` (in `server/` so it can hold the Arcs).

- [ ] **Step 1: Test (integration)** — construct a `SwarmDebateReporter` with empty swarm-state Arcs, call `.proposer("m", Running)`, assert a `swarm_members` entry + a `SwarmStatus` broadcast happened (F1). Use the existing swarm test scaffolding (`swarm_member` helper).
- [ ] **Step 2-4: Implement** the wrapper + reporter; on `proposer(Running)` register/update a lightweight member (`proposer:<model>`), on `Done/Failed` mark terminal; `phase`/`gold` route to the side panel (`write_markdown_page` for `gold`). Run → pass. **Step 5: Commit** `feat(server): SwarmDebateReporter status tiles + side-panel gold`.

### Task 17: Run summary + cost line

**Files:** `crates/jcode-app-core/src/tool/cheap_route_tool.rs` (after a debate completes) — compose `gold from N models in {elapsed}s · ${cost}` using the cost-guard spend snapshot delta + a timer; emit via `reporter.gold(...)` footer / a system message. Skip note when `<2 models` or code-skip (H10).

- [ ] **Step 1-4:** test that the summary string is built from a recorded (models, elapsed, cost) tuple (pure formatter `fn debate_summary(models: usize, elapsed_s: u64, usd: f64) -> String`), then wire it. Run → pass. **Step 5: Commit** `feat(cheap_route): debate run summary + cost line`.

---

## Phase 6 — Wire the tool

### Task 18: `CheapRouteTool` reads the flag + injects the reporter (D-series, gold_mode wiring)

**Files:** `crates/jcode-app-core/src/tool/cheap_route_tool.rs` (`execute`); `ProviderCheapBackend::new*` (add `gold_mode: bool` field + setter).

- [ ] **Step 1: Test (integration)** — with session `gold_mode_enabled=Some(true)` + config on, `CheapRouteTool::execute` builds a backend whose `gold_mode()==true`; with either off, `false`.
- [ ] **Step 2-4: Implement** — in `execute`: `let session = Session::load(&ctx.session_id).ok();`
  `let gold = session.as_ref().and_then(|s| s.gold_mode_enabled).unwrap_or(false) && config().agents.cheap_route_gold_mode;`
  construct `ProviderCheapBackend` with `gold` + the `SwarmDebateReporter` (server context) or `NoopDebateReporter`. `ProviderCheapBackend` stores `gold_mode: bool` and returns it from the `gold_mode()` trait method. Run → pass. **Step 5: Commit** `feat(cheap_route): wire gold-mode flag + reporter through the tool`.

### Task 19: Full build + suite + manual smoke

- [ ] `source ~/.cargo/env && cargo build --release -p jcode --bin jcode` → clean.
- [ ] `cargo test -p jcode-app-core -p jcode-config-types -p jcode-base --lib` → all green (ignore the known pre-existing `tool::bash::tests::test_stdin_forwarding_*` + reload parallel-contention flakes; confirm they fail identically on a clean tree).
- [ ] Manual: set `cheap_route_gold_mode = true` in config, `/gold on`, send a hard reasoning task, watch proposer tiles + gold in the side panel + the summary line. **Commit** any fixups.

---

## Self-review notes

- **Spec coverage:** config (T1), session+`/gold` (T2-3), `ask_strong`/`gold_mode`/timeouts (T4), helpers (T5-6), `run_debate` incl. consensus/fold/truncate/fallbacks (T7-11), gate + code-skip + recursion + cleanup (T12-14), reporter+side panel+summary (T15-17), tool wiring (T18). Test matrix A–H mapped across T7-T18.
- **Concurrency:** uses `join_all` of borrowed per-proposer-timeout futures — avoids the `async_trait` lifetime blocker and needs no Arc / no `run_cheap_route` signature change.
- **Type consistency:** `run_debate(&dyn CheapRouteBackend, &Subtask, &[(String, Option<String>)], usize, &dyn DebateStatusReporter)`; `consensus(&[String]) -> Option<String>`; `truncate_tail(&str, usize) -> String`; `is_code_subtask(&Subtask) -> bool`; trait adds `ask_strong`, `gold_mode`. Match `SubtaskResult`'s real fields when pushing the debate result in T12 (read the struct at implementation time).
- **Default-off:** every gate requires both global config + session flag; trait defaults keep existing/fake backends unchanged.
