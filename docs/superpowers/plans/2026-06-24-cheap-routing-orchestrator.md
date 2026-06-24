# Cheap-Routing Orchestrator (auto mode) — Implementation Plan (Plan 3 of 5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Build the auto-mode cheap-routing orchestrator: the parent model decomposes a task into subtasks, code ranks routes into a cheapest-first menu, the parent recommends one model, cheap subagents run each subtask, and the parent reviews each result.

**Architecture:** A new module `crates/jcode-app-core/src/agent/cheap_route.rs`. The orchestration logic (`run_cheap_route`) depends on an injected `CheapRouteBackend` trait (three effects: `ask_parent`, `run_subtask`, `routes`). This makes the whole flow unit-testable with a scripted fake backend — no real provider, no spawned subagents, no disk, no flakiness. The parts most likely to break in production (parsing weak-model JSON/text output) are isolated into pure functions with thorough tests. Wiring a real backend (provider `complete_simple` + the `subagent` tool) and the `/cheap` command is deferred to Plan 5; the protocol selection round-trip is Plan 4. This plan delivers the `/cheap auto` decision core, fully green and independently testable.

**Tech Stack:** Rust edition 2024, `async_trait`, `serde` / `serde_json`, `anyhow`, and `jcode_provider_core::{ModelRoute, selection::{rank_routes_by_cost, CheapRouteCandidate}}` (app-core depends on `jcode-provider-core` directly; `selection` is a `pub mod`).

**Run cargo with:** `. "$HOME/.cargo/env" && cargo test -p jcode-app-core ...`

**Spec:** `docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md` — the "Orchestrator" component + the Flow section.

---

## File Structure

- Create: `crates/jcode-app-core/src/agent/cheap_route.rs` — the entire orchestrator (types, pure parsers/builders, backend trait, `run_cheap_route`, and tests). One responsibility: cheap-routing decision + orchestration.
- Modify: `crates/jcode-app-core/src/agent.rs` — add one module declaration `pub mod cheap_route;` next to the other `mod ...;` submodule declarations (e.g. near `mod turn_execution;`).

---

### Task 1: Create the orchestrator module (all code) with a failing test

**Files:**
- Create: `crates/jcode-app-core/src/agent/cheap_route.rs`
- Modify: `crates/jcode-app-core/src/agent.rs`

- [ ] **Step 1: Declare the module**

In `crates/jcode-app-core/src/agent.rs`, find the block of submodule declarations (lines that look like `mod turn_execution;`, `mod prompting;`, `mod response_recovery;`, etc.). Add this line alongside them:

```rust
pub mod cheap_route;
```

- [ ] **Step 2: Write the full module WITH its tests**

Create `crates/jcode-app-core/src/agent/cheap_route.rs` with exactly this content:

```rust
//! Cheap-routing orchestrator (auto mode). The expensive parent model only
//! decomposes the task, recommends one cheap model, and reviews results; cheap
//! subagents do the work. See
//! docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use jcode_provider_core::ModelRoute;
use jcode_provider_core::selection::{CheapRouteCandidate, rank_routes_by_cost};
use serde::Deserialize;

/// Largest menu the parent is asked to choose from.
const MAX_MENU: usize = 6;

fn default_difficulty() -> u8 {
    3
}

/// One independent unit of work the parent split the task into.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Subtask {
    pub description: String,
    pub prompt: String,
    /// 1 (trivial/mechanical) .. 5 (hard, needs a strong model).
    #[serde(default = "default_difficulty")]
    pub difficulty: u8,
}

/// Result of running and reviewing one subtask.
#[derive(Debug, Clone)]
pub struct SubtaskResult {
    pub description: String,
    pub output: String,
    pub review: String,
}

/// Full outcome of an auto cheap-routing run.
#[derive(Debug, Clone)]
pub struct CheapRouteOutcome {
    pub recommended_model: String,
    pub subtasks: Vec<Subtask>,
    pub results: Vec<SubtaskResult>,
}

/// Injected effects so the orchestrator is unit-testable without real providers
/// or spawning real subagents. The production implementation (Plan 5) wraps an
/// `Arc<dyn Provider>` (`ask_parent` -> `complete_simple`) and the `subagent`
/// tool (`run_subtask`).
#[async_trait]
pub trait CheapRouteBackend: Send + Sync {
    /// Ask the expensive parent model a one-shot question, returning its text.
    async fn ask_parent(&self, prompt: &str) -> Result<String>;
    /// Run one subtask on the chosen cheap model, returning the subagent output.
    async fn run_subtask(&self, subtask: &Subtask, model: &str) -> Result<String>;
    /// Routes available for ranking into the cheapest-first menu.
    fn routes(&self) -> Vec<ModelRoute>;
}

/// Strip a single surrounding markdown code fence (```json ... ```), returning
/// the inner text. Weak models routinely wrap JSON in fences.
pub fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop the optional language tag on the opening fence line.
    let body = after_open.splitn(2, '\n').nth(1).unwrap_or("");
    match body.rfind("```") {
        Some(close) => body[..close].trim(),
        None => body.trim(),
    }
}

/// Parse the parent's decompose response into subtasks. Accepts a raw JSON array
/// or one wrapped in a markdown code fence.
pub fn parse_subtasks(text: &str) -> Result<Vec<Subtask>> {
    let json = strip_code_fence(text);
    let subtasks: Vec<Subtask> = serde_json::from_str(json)
        .map_err(|e| anyhow!("failed to parse subtasks JSON: {e}; raw: {json}"))?;
    if subtasks.is_empty() {
        return Err(anyhow!("decompose returned zero subtasks"));
    }
    Ok(subtasks)
}

/// Build a cheapest-first candidate menu from the provider routes, capped to
/// `max` entries.
pub fn build_menu(routes: Vec<ModelRoute>, max: usize) -> Vec<CheapRouteCandidate> {
    let mut ranked = rank_routes_by_cost(routes);
    ranked.truncate(max);
    ranked
}

/// Render the menu as LLM-readable lines for the recommend prompt.
pub fn format_menu_for_prompt(menu: &[CheapRouteCandidate]) -> String {
    if menu.is_empty() {
        return "(no routes available)".to_string();
    }
    let mut out = String::new();
    for candidate in menu {
        let price = match candidate.reference_cost_micros {
            Some(micros) => format!("${:.4}/ref-req", micros as f64 / 1_000_000.0),
            None => "price unknown".to_string(),
        };
        out.push_str(&format!(
            "- {} (via {}, {})\n",
            candidate.route.model, candidate.route.provider, price
        ));
    }
    out.trim_end().to_string()
}

/// Pick the model the parent recommended: the first menu model whose id appears
/// in `text`. If none match, fall back to the cheapest (first) menu entry so the
/// run never stalls on an unparseable recommendation.
pub fn parse_recommended_model(text: &str, menu: &[CheapRouteCandidate]) -> Result<String> {
    if menu.is_empty() {
        return Err(anyhow!("no candidate models to recommend from"));
    }
    let lowered = text.to_lowercase();
    for candidate in menu {
        if lowered.contains(&candidate.route.model.to_lowercase()) {
            return Ok(candidate.route.model.clone());
        }
    }
    Ok(menu[0].route.model.clone())
}

/// Instruction asking the parent to split the task into difficulty-rated subtasks.
pub fn build_decompose_prompt(task: &str) -> String {
    format!(
        "Split the following coding task into the smallest independent subtasks. \
For each subtask rate difficulty 1 (trivial/mechanical) to 5 (hard, needs a strong model). \
Respond with ONLY a JSON array, no prose. Each element: \
{{\"description\": string, \"prompt\": string, \"difficulty\": integer}}.\n\nTASK:\n{task}"
    )
}

/// Instruction asking the parent to pick one model from the cheapest-first menu.
pub fn build_recommend_prompt(task: &str, subtasks: &[Subtask], menu_str: &str) -> String {
    let hardest = subtasks.iter().map(|s| s.difficulty).max().unwrap_or(3);
    format!(
        "You are routing work to the cheapest capable model. The hardest subtask is difficulty {hardest}/5. \
From the menu below (cheapest first) pick exactly ONE model strong enough for the hardest subtask. \
Reply with ONLY the model id.\n\nTASK: {task}\n\nMENU:\n{menu_str}"
    )
}

/// Instruction asking the parent to review one cheap-model result.
pub fn build_review_prompt(subtask: &Subtask, result: &str) -> String {
    format!(
        "A cheap model completed this subtask. Review for correctness. \
If correct reply 'OK'. If not, reply 'FIX:' then what is wrong.\n\nSUBTASK: {}\n\nRESULT:\n{}",
        subtask.description, result
    )
}

/// Auto-mode cheap routing: decompose -> rank -> recommend -> spawn -> review.
pub async fn run_cheap_route(
    backend: &dyn CheapRouteBackend,
    task: &str,
) -> Result<CheapRouteOutcome> {
    // 1. Parent decomposes the task.
    let decompose = backend.ask_parent(&build_decompose_prompt(task)).await?;
    let subtasks = parse_subtasks(&decompose)?;

    // 2. Code ranks routes into a cheapest-first menu.
    let menu = build_menu(backend.routes(), MAX_MENU);
    if menu.is_empty() {
        return Err(anyhow!("no available model routes to route work to"));
    }

    // 3. Parent recommends one model from the menu.
    let menu_str = format_menu_for_prompt(&menu);
    let recommend = backend
        .ask_parent(&build_recommend_prompt(task, &subtasks, &menu_str))
        .await?;
    let recommended_model = parse_recommended_model(&recommend, &menu)?;

    // 4. Spawn each subtask on the chosen model; 5. parent reviews each result.
    let mut results = Vec::with_capacity(subtasks.len());
    for subtask in &subtasks {
        let output = backend.run_subtask(subtask, &recommended_model).await?;
        let review = backend.ask_parent(&build_review_prompt(subtask, &output)).await?;
        results.push(SubtaskResult {
            description: subtask.description.clone(),
            output,
            review,
        });
    }

    Ok(CheapRouteOutcome {
        recommended_model,
        subtasks,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_provider_core::{RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn priced_route(model: &str, input_micros: u64) -> ModelRoute {
        ModelRoute {
            model: model.to_string(),
            provider: "testprov".to_string(),
            api_method: "a".to_string(),
            available: true,
            detail: String::new(),
            cheapness: Some(RouteCheapnessEstimate::metered(
                RouteCostSource::PublicApiPricing,
                RouteCostConfidence::Exact,
                input_micros,
                input_micros,
                None,
                None,
            )),
        }
    }

    #[test]
    fn strip_code_fence_unwraps_json_block() {
        let fenced = "```json\n[{\"a\":1}]\n```";
        assert_eq!(strip_code_fence(fenced), "[{\"a\":1}]");
        assert_eq!(strip_code_fence("[1,2]"), "[1,2]");
    }

    #[test]
    fn parse_subtasks_accepts_fenced_and_plain_json() {
        let plain = r#"[{"description":"edit","prompt":"do it","difficulty":2}]"#;
        let parsed = parse_subtasks(plain).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].description, "edit");
        assert_eq!(parsed[0].difficulty, 2);

        let fenced = "```json\n[{\"description\":\"x\",\"prompt\":\"p\"}]\n```";
        let parsed2 = parse_subtasks(fenced).unwrap();
        assert_eq!(parsed2.len(), 1);
        // difficulty defaults to 3 when omitted.
        assert_eq!(parsed2[0].difficulty, 3);
    }

    #[test]
    fn parse_subtasks_rejects_empty_and_bad_json() {
        assert!(parse_subtasks("[]").is_err());
        assert!(parse_subtasks("not json").is_err());
    }

    #[test]
    fn format_menu_lists_models_with_price() {
        let menu = build_menu(vec![priced_route("cheapo", 100_000)], MAX_MENU);
        let rendered = format_menu_for_prompt(&menu);
        assert!(rendered.contains("cheapo"));
        assert!(rendered.contains("testprov"));
    }

    #[test]
    fn parse_recommended_model_matches_listed_else_falls_back_to_cheapest() {
        let menu = build_menu(
            vec![priced_route("cheapo", 100_000), priced_route("pricey", 9_000_000)],
            MAX_MENU,
        );
        // cheapest first
        assert_eq!(menu[0].route.model, "cheapo");
        // explicit mention wins
        assert_eq!(parse_recommended_model("use pricey please", &menu).unwrap(), "pricey");
        // unparseable -> cheapest fallback
        assert_eq!(parse_recommended_model("hmm not sure", &menu).unwrap(), "cheapo");
    }

    struct FakeBackend {
        parent_responses: Mutex<VecDeque<String>>,
        routes: Vec<ModelRoute>,
        subtask_calls: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl CheapRouteBackend for FakeBackend {
        async fn ask_parent(&self, _prompt: &str) -> Result<String> {
            Ok(self
                .parent_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default())
        }

        async fn run_subtask(&self, subtask: &Subtask, model: &str) -> Result<String> {
            self.subtask_calls
                .lock()
                .unwrap()
                .push((subtask.description.clone(), model.to_string()));
            Ok(format!("done: {}", subtask.description))
        }

        fn routes(&self) -> Vec<ModelRoute> {
            self.routes.clone()
        }
    }

    #[tokio::test]
    async fn run_cheap_route_decomposes_recommends_spawns_and_reviews() {
        let decompose = r#"[
            {"description":"edit auth","prompt":"edit it","difficulty":2},
            {"description":"write tests","prompt":"test it","difficulty":3}
        ]"#;
        let backend = FakeBackend {
            parent_responses: Mutex::new(VecDeque::from(vec![
                decompose.to_string(),  // decompose
                "use cheapo".to_string(), // recommend
                "OK".to_string(),         // review subtask 1
                "OK".to_string(),         // review subtask 2
            ])),
            routes: vec![priced_route("cheapo", 100_000), priced_route("pricey", 9_000_000)],
            subtask_calls: Mutex::new(Vec::new()),
        };

        let outcome = run_cheap_route(&backend, "refactor auth + tests").await.unwrap();

        assert_eq!(outcome.recommended_model, "cheapo");
        assert_eq!(outcome.subtasks.len(), 2);
        assert_eq!(outcome.results.len(), 2);
        assert_eq!(outcome.results[0].review, "OK");

        // both subtasks ran on the chosen cheap model
        let calls = backend.subtask_calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|(_, model)| model == "cheapo"));
        assert_eq!(calls[0].0, "edit auth");
    }

    #[tokio::test]
    async fn run_cheap_route_errors_when_no_routes() {
        let backend = FakeBackend {
            parent_responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"x","prompt":"p","difficulty":1}]"#.to_string(),
            ])),
            routes: vec![],
            subtask_calls: Mutex::new(Vec::new()),
        };
        let err = run_cheap_route(&backend, "task").await.unwrap_err();
        assert!(err.to_string().contains("no available model routes"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they pass (after compile)**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core cheap_route`
Expected: PASS — all `cheap_route::tests::*` pass: `strip_code_fence_unwraps_json_block`, `parse_subtasks_accepts_fenced_and_plain_json`, `parse_subtasks_rejects_empty_and_bad_json`, `format_menu_lists_models_with_price`, `parse_recommended_model_matches_listed_else_falls_back_to_cheapest`, `run_cheap_route_decomposes_recommends_spawns_and_reviews`, `run_cheap_route_errors_when_no_routes`.

If `tokio::test` is not found: confirm `tokio` is a dev-dependency or normal dependency of `jcode-app-core` with the `macros` + `rt` features (it is — the crate is async throughout and other tests use `#[tokio::test]`). If `RouteCheapnessEstimate` / `RouteCostSource` / `RouteCostConfidence` are not importable from `jcode_provider_core`, they are re-exported at the crate root of `jcode-provider-core` (defined in `crates/jcode-provider-core/src/lib.rs:1070`, `:1049`, `:1062`).

- [ ] **Step 4: Confirm the whole crate still builds and tests pass**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core`
Expected: PASS — full crate compiles, all tests green (new `cheap_route` tests + all pre-existing). Dead-code warnings for the not-yet-wired public items are acceptable (the crate does not deny warnings), but there should be none because every item is exercised by a test.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core/src/agent.rs crates/jcode-app-core/src/agent/cheap_route.rs
git commit -m "feat(agent): add cheap-routing orchestrator (auto mode)

Cheap-routing Plan 3: decompose -> rank -> recommend -> spawn -> review,
behind an injected CheapRouteBackend trait so the flow and weak-model output
parsing are fully unit-tested with a scripted fake (no provider/disk). Real
backend + /cheap command wiring come in Plan 5.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:** Spec "Orchestrator (decompose → rank → recommend → spawn → review)" and the Flow section are implemented by `run_cheap_route`. Lever F (difficulty 1–5 → capability floor for the recommend prompt) is in `Subtask.difficulty` + `build_recommend_prompt` (uses `hardest`). Levers A/B/C/E and the user selection gate are deferred to Plans 4/5 (documented in Architecture) — they are wiring/protocol/UI, not orchestration logic.

**2. Placeholder scan:** No TBD/TODO. The full module body and all tests are given verbatim. The one module-declaration edit names the exact line to add and where.

**3. Type consistency:** `CheapRouteBackend` (`ask_parent`/`run_subtask`/`routes`) is defined once and used identically in `run_cheap_route` and `FakeBackend`. `Subtask`/`SubtaskResult`/`CheapRouteOutcome` field names match between definition, `run_cheap_route`, and assertions. `build_menu` returns `Vec<CheapRouteCandidate>` (from Plan 1) and `format_menu_for_prompt`/`parse_recommended_model` consume `&[CheapRouteCandidate]` reading `.route.model` and `.reference_cost_micros` — matching Plan 1's struct exactly. `ModelRoute` literal fields match the real struct.

**4. Ambiguity check:** Recommendation fallback (unparseable → cheapest) and decompose failures (empty/bad JSON → error) are pinned by tests, so behavior is unambiguous. `MAX_MENU` caps menu size deterministically.
