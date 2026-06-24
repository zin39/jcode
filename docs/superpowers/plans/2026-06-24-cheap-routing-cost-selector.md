# Cheap-Routing Cost Selector — Implementation Plan (Plan 1 of 5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a pure, well-tested function that orders available model routes cheapest-first, so cheap-routing mode can build a cheapest-first menu for the parent model to pick from.

**Architecture:** A single pure function `rank_routes_by_cost(Vec<ModelRoute>) -> Vec<CheapRouteCandidate>` in `jcode-provider-core`. It drops unavailable routes, reads each route's existing `RouteCheapnessEstimate.estimated_reference_cost_micros`, and sorts ascending (unpriced routes last, ties broken alphabetically by model for determinism). Capability judgment ("can this model do the task") is deliberately NOT here — the parent model applies the capability floor later, because jcode has no per-model quality score. Context-window fit filtering is also deferred to the orchestrator (Plan 3), which has provider hints needed by `context_limit_for_model_with_provider_and_cache`.

**Tech Stack:** Rust, the existing `ModelRoute` / `RouteCheapnessEstimate` types in `crates/jcode-provider-core/src/lib.rs`, `cargo test`.

**Spec:** `docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md` (the "Cost selector" component + flow step "rank cheapest-first by estimated_reference_cost_micros").

---

## Roadmap (context — not tasks for this plan)

This plan delivers sub-project 1. The remaining sub-plans are written when reached:

2. Subagent levers (`SubagentInput` + `SubagentTool`): `system_prompt_override`, per-spawn tool prune (`allowed_tools`), confidence self-flag — `crates/jcode-app-core/src/tool/task.rs`.
3. Orchestrator `crates/jcode-app-core/src/agent/cheap_route.rs`: decompose → ctx-fit filter (uses Plan 1) → parent recommend → spawn → review.
4. Protocol `ModelSelectionRequest`/`ModelSelectionResponse` round-trip — `crates/jcode-protocol/src/wire.rs` + server.
5. TUI `/cheap on|off|auto` command + global-pick overlay — reuse `jcode-tui-permissions`.

---

## File Structure

- Modify: `crates/jcode-provider-core/src/selection.rs` — add `CheapRouteCandidate` struct + `rank_routes_by_cost` function next to the existing route helpers (`dedupe_model_routes`, etc.). One responsibility: route selection/ordering. This file already owns route filtering/dedup, so cost ranking belongs here.
- Test: `crates/jcode-provider-core/src/selection.rs` `#[cfg(test)] mod tests` — same file, matching the existing in-file test convention (see `dedupes_model_routes_by_route_identity`).

Note on running cargo: the login shell prints `/Users/karangupta/.cargo/env: no such file` — harmless. If `cargo` is not on PATH, use the repo wrapper `scripts/dev_cargo.sh test -p jcode-provider-core ...` instead of bare `cargo`.

---

### Task 1: `rank_routes_by_cost` orders available routes cheapest-first

**Files:**
- Modify: `crates/jcode-provider-core/src/selection.rs` (add struct + function after `dedupe_model_routes`, around line 223)
- Test: `crates/jcode-provider-core/src/selection.rs` (add tests inside the existing `mod tests`, after `dedupes_model_routes_by_route_identity` ~line 522)

- [ ] **Step 1: Write the failing tests**

Add these two tests inside the existing `#[cfg(test)] mod tests { ... }` block in `crates/jcode-provider-core/src/selection.rs`:

```rust
    #[test]
    fn rank_routes_by_cost_orders_cheapest_first_and_drops_unavailable() {
        use crate::{RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource};

        fn priced(model: &str, available: bool, input_micros: u64, output_micros: u64) -> ModelRoute {
            ModelRoute {
                model: model.to_string(),
                provider: "p".to_string(),
                api_method: "a".to_string(),
                available,
                detail: String::new(),
                cheapness: Some(RouteCheapnessEstimate::metered(
                    RouteCostSource::PublicApiPricing,
                    RouteCostConfidence::Exact,
                    input_micros,
                    output_micros,
                    None,
                    None,
                )),
            }
        }

        fn unpriced(model: &str) -> ModelRoute {
            ModelRoute {
                model: model.to_string(),
                provider: "p".to_string(),
                api_method: "a".to_string(),
                available: true,
                detail: String::new(),
                cheapness: None,
            }
        }

        let routes = vec![
            priced("expensive", true, 5_000_000, 15_000_000),
            unpriced("unpriced"),
            priced("cheap", true, 200_000, 200_000),
            priced("gone", false, 1_000, 1_000), // unavailable -> dropped
        ];

        let ranked = rank_routes_by_cost(routes);
        let order: Vec<&str> = ranked.iter().map(|c| c.route.model.as_str()).collect();

        // cheapest priced first, expensive next, unpriced last; unavailable dropped.
        assert_eq!(order, vec!["cheap", "expensive", "unpriced"]);
        assert!(ranked[0].reference_cost_micros.is_some());
        assert!(ranked.last().unwrap().reference_cost_micros.is_none());
        assert!(ranked.iter().all(|c| c.route.model != "gone"));
    }

    #[test]
    fn rank_routes_by_cost_breaks_ties_alphabetically_by_model() {
        use crate::{RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource};

        fn same_price(model: &str) -> ModelRoute {
            ModelRoute {
                model: model.to_string(),
                provider: "p".to_string(),
                api_method: "a".to_string(),
                available: true,
                detail: String::new(),
                cheapness: Some(RouteCheapnessEstimate::metered(
                    RouteCostSource::PublicApiPricing,
                    RouteCostConfidence::Exact,
                    1_000_000,
                    1_000_000,
                    None,
                    None,
                )),
            }
        }

        let ranked = rank_routes_by_cost(vec![same_price("b"), same_price("a")]);
        let order: Vec<&str> = ranked.iter().map(|c| c.route.model.as_str()).collect();
        assert_eq!(order, vec!["a", "b"]);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p jcode-provider-core rank_routes_by_cost`
Expected: FAIL — compile error `cannot find function rank_routes_by_cost in this scope` (and `cannot find type CheapRouteCandidate`).

- [ ] **Step 3: Write the minimal implementation**

In `crates/jcode-provider-core/src/selection.rs`, add this immediately after the `dedupe_model_routes` function (after line 223, before `fn duplicate_model_route`):

```rust
/// A model route paired with its comparable metered cost. Used by cheap-routing
/// to present a cheapest-first menu to the parent model. Capability ("can this
/// model do the task properly") is judged by the parent, not here — this orders
/// purely by price and drops routes that are not currently usable.
#[derive(Debug, Clone)]
pub struct CheapRouteCandidate {
    pub route: ModelRoute,
    /// Normalized reference-request cost in micros (lower = cheaper). `None` when
    /// the route carries no pricing estimate; such routes sort after all priced
    /// routes because an unknown cost cannot be confirmed cheap.
    pub reference_cost_micros: Option<u64>,
}

/// Order `routes` cheapest-first by each route's normalized reference-request
/// cost (`RouteCheapnessEstimate::estimated_reference_cost_micros`). Unavailable
/// routes are dropped. Priced routes sort ascending; unpriced routes sort last.
/// Ties and unpriced routes break alphabetically by model id for determinism.
pub fn rank_routes_by_cost(routes: Vec<ModelRoute>) -> Vec<CheapRouteCandidate> {
    let mut candidates: Vec<CheapRouteCandidate> = routes
        .into_iter()
        .filter(|route| route.available)
        .map(|route| {
            let reference_cost_micros = route
                .cheapness
                .as_ref()
                .and_then(|estimate| estimate.estimated_reference_cost_micros);
            CheapRouteCandidate {
                route,
                reference_cost_micros,
            }
        })
        .collect();

    candidates.sort_by(
        |a, b| match (a.reference_cost_micros, b.reference_cost_micros) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.route.model.cmp(&b.route.model)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.route.model.cmp(&b.route.model),
        },
    );

    candidates
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p jcode-provider-core rank_routes_by_cost`
Expected: PASS — both `rank_routes_by_cost_orders_cheapest_first_and_drops_unavailable` and `rank_routes_by_cost_breaks_ties_alphabetically_by_model` pass.

- [ ] **Step 5: Confirm no warnings / nothing else broke**

Run: `cargo test -p jcode-provider-core`
Expected: PASS — the whole crate's tests, including the pre-existing `dedupes_model_routes_by_route_identity` etc., still pass. No new warnings about unused code (the new items are `pub`).

- [ ] **Step 6: Commit**

```bash
git add crates/jcode-provider-core/src/selection.rs
git commit -m "feat(provider): add rank_routes_by_cost cheapest-first selector

Cheap-routing Plan 1: pure function ordering available model routes by
their existing reference-request cost estimate. Drops unavailable routes,
sorts unpriced last, ties break by model id. Capability and context-window
filtering are deferred to the orchestrator (parent-side).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:** The "Cost selector" component (`rank_routes_by_cost(routes, ...)`) and flow step "rank cheapest-first by estimated_reference_cost_micros" are implemented by Task 1. The spec's "filter ctx window fits" and "capability floor" are explicitly deferred (to Plan 3 / parent-side) and documented in the Architecture section and the function doc-comment — not dropped.

**2. Placeholder scan:** No TBD/TODO. Every code step shows complete code. Test bodies are concrete with real assertions.

**3. Type consistency:** `CheapRouteCandidate { route: ModelRoute, reference_cost_micros: Option<u64> }` is used identically in the implementation and both tests. `ModelRoute` fields (`model, provider, api_method, available, detail, cheapness`) match the real struct (verified in `selection.rs` tests + `lib.rs:527`). `RouteCheapnessEstimate::metered(source, confidence, input_micros, output_micros, cache_read, note)` matches the real signature at `lib.rs:1094`. `estimated_reference_cost_micros: Option<u64>` matches `lib.rs:1088`.

**4. Ambiguity check:** "Cheapest" is defined precisely as ascending `estimated_reference_cost_micros`; unpriced and tie behavior are pinned by tests so there is one interpretation.
