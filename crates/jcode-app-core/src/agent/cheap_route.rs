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
    /// The model that actually produced `output` (may differ from the
    /// recommended model when cheaper routes errored and we fell back).
    pub model_used: String,
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
    /// Run one subtask on the chosen cheap model. `route_api_method` (e.g.
    /// `"openai-compatible:deepseek"`) pins the exact provider/route so the model
    /// name is not re-resolved to the wrong provider; `None` lets it resolve via
    /// the parent's active provider (used for the current-model last resort).
    async fn run_subtask(
        &self,
        subtask: &Subtask,
        model: &str,
        route_api_method: Option<&str>,
    ) -> Result<String>;
    /// Routes available for ranking into the cheapest-first menu.
    fn routes(&self) -> Vec<ModelRoute>;
    /// The parent's own current model — a known-working last-resort fallback
    /// when every ranked cheap route errors (e.g. all dead-quota).
    fn current_model(&self) -> String;
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

    // 2. Rank ALL available routes cheapest-first. The top slice is the recommend
    //    menu; the FULL ranked list is the fallback candidate order, so a working
    //    route that isn't in the cheapest 6 (e.g. deepseek sitting behind dead
    //    OpenAI nano/mini) still gets reached instead of being skipped.
    let ranked = ranked_with_preferences(backend.routes());
    if ranked.is_empty() {
        return Err(anyhow!("no available model routes to route work to"));
    }
    let menu: Vec<CheapRouteCandidate> = ranked.iter().take(MAX_MENU).cloned().collect();

    // 3. Parent recommends one model from the menu.
    let menu_str = format_menu_for_prompt(&menu);
    let recommend = backend
        .ask_parent(&build_recommend_prompt(task, &subtasks, &menu_str))
        .await?;
    let recommended_model = parse_recommended_model(&recommend, &menu)?;

    // 4. Candidate order: recommended first, then the cheapest model of EACH
    //    other provider. Quota/auth failures are per-key, so once one model of a
    //    provider errors, its siblings will too — trying only one model per
    //    provider avoids grinding through ~20 dead routes from a single exhausted
    //    key while still reaching every distinct provider (incl. cheap ones like
    //    deepseek sitting behind dead OpenAI catalog models).
    // Each candidate carries (model, route_api_method) so the spawn pins the EXACT
    // route ranking chose, instead of re-resolving the bare model name to the
    // wrong provider.
    let recommended_api = ranked
        .iter()
        .find(|c| c.route.model == recommended_model)
        .map(|c| c.route.api_method.clone());
    let mut candidates: Vec<(String, Option<String>)> =
        vec![(recommended_model.clone(), recommended_api)];
    let mut seen_providers: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(rec) = ranked.iter().find(|c| c.route.model == recommended_model) {
        seen_providers.insert(rec.route.provider.clone());
    }
    for candidate in &ranked {
        if seen_providers.insert(candidate.route.provider.clone())
            && candidate.route.model != recommended_model
        {
            candidates.push((
                candidate.route.model.clone(),
                Some(candidate.route.api_method.clone()),
            ));
        }
    }
    // Guaranteed last resort: the parent's own current model, which is known to
    // work (it just answered the decompose/recommend calls). No pinned route — it
    // resolves via the parent's active provider. This rescues runs where every
    // ranked cheap route is dead-quota.
    let current_model = backend.current_model();
    if !current_model.is_empty() && !candidates.iter().any(|(m, _)| m == &current_model) {
        candidates.push((current_model, None));
    }

    // 5. Run each subtask (with fallback) and have the parent review each result.
    let mut results = Vec::with_capacity(subtasks.len());
    for subtask in &subtasks {
        let mut chosen: Option<(String, String)> = None; // (model_used, output)
        let mut errors: Vec<String> = Vec::new();
        for (model, api_method) in &candidates {
            let attempt = tokio::time::timeout(
                SUBTASK_TIMEOUT,
                backend.run_subtask(subtask, model, api_method.as_deref()),
            )
            .await;
            match attempt {
                Ok(Ok(output)) => {
                    chosen = Some((model.clone(), output));
                    break;
                }
                Ok(Err(err)) => errors.push(format!("{model}: {err}")),
                Err(_) => errors.push(format!(
                    "{model}: timed out after {}s",
                    SUBTASK_TIMEOUT.as_secs()
                )),
            }
        }
        let (model_used, output) = chosen.ok_or_else(|| {
            anyhow!(
                "all {} candidate model(s) failed for subtask '{}': {}",
                candidates.len(),
                subtask.description,
                errors.join("; ")
            )
        })?;
        // Review is best-effort: a parent-review error must not discard a
        // subtask that already completed successfully.
        let review = match backend.ask_parent(&build_review_prompt(subtask, &output)).await {
            Ok(review) => review,
            Err(err) => format!("(review unavailable: {err})"),
        };
        results.push(SubtaskResult {
            description: subtask.description.clone(),
            output,
            review,
            model_used,
        });
    }

    Ok(CheapRouteOutcome {
        recommended_model,
        subtasks,
        results,
    })
}

/// Build cheap-routing candidate routes for one configured named provider.
/// `static_ids` come from the config block's `models[]`; `cached_ids` from the
/// provider's discovered disk catalog. The union (deduped) becomes routes, each
/// priced via `price` and marked available per `key_present`.
fn build_named_provider_routes(
    name: &str,
    base_url: &str,
    static_ids: &[String],
    cached_ids: &[String],
    key_present: bool,
    price: impl Fn(&str, &str) -> Option<jcode_provider_core::RouteCheapnessEstimate>,
) -> Vec<ModelRoute> {
    let api_method = format!("openai-compatible:{name}");
    let mut seen = std::collections::HashSet::new();
    let mut routes = Vec::new();
    for id in static_ids.iter().chain(cached_ids.iter()) {
        if !seen.insert(id.clone()) {
            continue;
        }
        let cheapness = price(&api_method, id);
        routes.push(ModelRoute {
            model: id.clone(),
            provider: name.to_string(),
            api_method: api_method.clone(),
            available: key_present,
            detail: base_url.to_string(),
            cheapness,
        });
    }
    routes
}

/// Collect cheap-routing candidate routes for every configured `[providers.X]`
/// block in the user's config. Each block contributes routes from the union of
/// its static `models[]` list and its previously-discovered disk catalog.
/// Whether an ABSOLUTE `env_file` path (the form used in config `[providers.X]`
/// blocks) contains a non-empty `{env_key}=...` line. jcode's standard
/// config-dir-relative key loader rejects absolute paths, so cheap-routing checks
/// them directly to decide route availability.
fn absolute_env_file_has_key(env_key: Option<&str>, env_file: Option<&str>) -> bool {
    let (Some(env_key), Some(env_file)) = (env_key, env_file) else {
        return false;
    };
    let path = std::path::Path::new(env_file);
    if !path.is_absolute() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let prefix = format!("{env_key}=");
    content
        .lines()
        .any(|line| line.strip_prefix(&prefix).is_some_and(|v| !v.trim().is_empty()))
}

fn configured_named_provider_routes() -> Vec<ModelRoute> {
    let cfg = crate::config::config();
    let mut routes = Vec::new();
    for (name, provider_cfg) in &cfg.providers {
        let static_ids: Vec<String> =
            provider_cfg.models.iter().map(|m| m.id.clone()).collect();
        let cached_ids: Vec<String> =
            jcode_provider_openrouter::load_disk_cache_entry_for_namespace(name)
                .map(|cache| cache.models.iter().map(|m| m.id.clone()).collect())
                .unwrap_or_default();
        if static_ids.is_empty() && cached_ids.is_empty() {
            continue;
        }
        // A key is present if an inline api_key is set, or if the env-var /
        // env-file lookup finds one. We pass empty-string defaults for None
        // optional fields; load_api_key_from_env_or_config returns None for
        // invalid (empty) names, which is safe.
        // A discovered disk catalog (cached_ids) means a real /v1/models fetch
        // succeeded for this provider, which only happens with a working key — so
        // treat that as conclusive availability. Otherwise fall back to the env
        // lookups (incl. absolute env_file paths, which the config-dir-relative
        // helper rejects).
        let key_present = !cached_ids.is_empty()
            || provider_cfg.api_key.is_some()
            || crate::provider_catalog::load_api_key_from_env_or_config(
                provider_cfg.api_key_env.as_deref().unwrap_or(""),
                provider_cfg.env_file.as_deref().unwrap_or(""),
            )
            .is_some()
            || absolute_env_file_has_key(
                provider_cfg.api_key_env.as_deref(),
                provider_cfg.env_file.as_deref(),
            );
        routes.extend(build_named_provider_routes(
            name,
            &provider_cfg.base_url,
            &static_ids,
            &cached_ids,
            key_present,
            |source, model| {
                crate::provider::pricing::metered_pricing_for_source_with_tier(source, model, None)
            },
        ));
    }
    routes
}

/// Whether `entry` (case-insensitive substring) matches this route by model id,
/// provider, api_method, or `"provider/model"` composite. Used by the
/// `agents.cheap_route_prefer` / `cheap_route_ban` lists.
fn route_matches_preference(route: &ModelRoute, entry: &str) -> bool {
    let entry = entry.trim().to_lowercase();
    if entry.is_empty() {
        return false;
    }
    let composite = format!("{}/{}", route.provider, route.model).to_lowercase();
    route.model.to_lowercase().contains(&entry)
        || route.provider.to_lowercase().contains(&entry)
        || route.api_method.to_lowercase().contains(&entry)
        || composite.contains(&entry)
}

/// Drop routes matching any `ban` entry.
fn drop_banned_routes(routes: Vec<ModelRoute>, ban: &[String]) -> Vec<ModelRoute> {
    if ban.is_empty() {
        return routes;
    }
    routes
        .into_iter()
        .filter(|route| !ban.iter().any(|b| route_matches_preference(route, b)))
        .collect()
}

/// Stable-partition ranked candidates so any matching a `prefer` entry come
/// first (each group stays cheapest-first), so a preferred model is chosen even
/// when something else is marginally cheaper.
fn prioritize_preferred(
    ranked: Vec<CheapRouteCandidate>,
    prefer: &[String],
) -> Vec<CheapRouteCandidate> {
    if prefer.is_empty() {
        return ranked;
    }
    let (mut preferred, mut rest): (Vec<_>, Vec<_>) = ranked
        .into_iter()
        .partition(|c| prefer.iter().any(|p| route_matches_preference(&c.route, p)));
    preferred.append(&mut rest);
    preferred
}

/// Rank routes cheapest-first after applying the configured cheap-route `ban`
/// (drop) and `prefer` (move-to-front) lists from `agents` config.
fn ranked_with_preferences(routes: Vec<ModelRoute>) -> Vec<CheapRouteCandidate> {
    let agents = &crate::config::config().agents;
    let routes = drop_banned_routes(routes, &agents.cheap_route_ban);
    let ranked = rank_routes_by_cost(routes);
    prioritize_preferred(ranked, &agents.cheap_route_prefer)
}

/// The sentinel value users put in `agents.swarm_model` (or pass as a subagent
/// `model`) to mean "pick the cheapest available model dynamically".
pub const CHEAPEST_SENTINEL: &str = "cheapest";

/// Core file/shell tools a cheap subagent is given. The full registry (~31
/// tools) bloats the prompt and stalls cheap models, so cheap-route subtasks get
/// only these essentials. Names are intersected with the live registry.
const CHEAP_SUBAGENT_TOOLS: &[&str] = &[
    "read", "write", "edit", "multiedit", "apply_patch", "bash", "grep", "glob", "ls",
];

/// Hard cap on a single subtask's model call. A slow/hanging route (e.g.
/// OpenRouter stalling) must not block the whole run — on timeout the candidate
/// is treated as failed and the next route is tried.
const SUBTASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The cheapest currently-available route across the provider's own routes and
/// all configured named providers (deduped, ranked cheapest-first, prefer/ban
/// applied). Returns `(model, route_api_method)` so the spawn can PIN the exact
/// route instead of re-resolving the bare model name. `None` when no
/// priced/available route exists. Resolves the [`CHEAPEST_SENTINEL`].
pub fn cheapest_available_model(
    provider: &dyn crate::provider::Provider,
) -> Option<(String, String)> {
    let mut routes = provider.model_routes();
    routes.extend(configured_named_provider_routes());
    let routes = jcode_provider_core::selection::dedupe_model_routes(routes);
    ranked_with_preferences(routes)
        .into_iter()
        .next()
        .map(|candidate| (candidate.route.model, candidate.route.api_method))
}

use std::sync::Arc;

/// Production [`CheapRouteBackend`] backed by a real provider and tool registry.
/// `ask_parent` uses the (expensive) parent provider directly; `run_subtask`
/// spawns a one-shot subagent pinned to the chosen cheap model on an isolated
/// provider fork, mirroring `SubagentTool::execute`.
pub struct ProviderCheapBackend {
    provider: Arc<dyn crate::provider::Provider>,
    registry: crate::tool::Registry,
    parent_system: String,
}

impl ProviderCheapBackend {
    pub fn new(
        provider: Arc<dyn crate::provider::Provider>,
        registry: crate::tool::Registry,
    ) -> Self {
        Self {
            provider,
            registry,
            parent_system:
                "You are a cost-routing coordinator. Decompose, recommend a model, and review \
                 subagent work. Be terse and precise; output exactly what is asked."
                    .to_string(),
        }
    }
}

#[async_trait]
impl CheapRouteBackend for ProviderCheapBackend {
    async fn ask_parent(&self, prompt: &str) -> Result<String> {
        self.provider.complete_simple(prompt, &self.parent_system).await
    }

    async fn run_subtask(
        &self,
        subtask: &Subtask,
        model: &str,
        route_api_method: Option<&str>,
    ) -> Result<String> {
        // Mirror SubagentTool::execute: new session pinned to `model`, blocked
        // recursive tools removed, run on an isolated provider fork.
        let mut session = crate::session::Session::create(None, Some(subtask.description.clone()));
        session.model = Some(model.to_string());
        // Pin the EXACT route chosen by ranking. Without this, a bare model name
        // (e.g. "deepseek-chat") is re-resolved to whatever provider is active —
        // often the wrong one (OpenRouter) — instead of the route we selected.
        // route_api_method takes priority in model_switch_request_for_session_route.
        if let Some(api_method) = route_api_method.map(str::trim).filter(|m| !m.is_empty()) {
            session.route_api_method = Some(api_method.to_string());
        }
        session.save()?;

        // Cheap subagents only need core file/shell tools. Sending the full
        // registry (~31 tool schemas, ~9k tokens) bloats the prompt and stalls
        // cheap models — keep just the essentials so the single call is fast.
        let registry_tools: std::collections::HashSet<String> =
            self.registry.tool_names().await.into_iter().collect();
        let allowed: std::collections::HashSet<String> = CHEAP_SUBAGENT_TOOLS
            .iter()
            .map(|t| t.to_string())
            .filter(|t| registry_tools.contains(t))
            .collect();

        let mut agent = super::Agent::new_with_session(
            self.provider.fork(),
            self.registry.clone(),
            session,
            Some(allowed),
        );
        agent.run_once_capture(&subtask.prompt).await
    }

    fn routes(&self) -> Vec<ModelRoute> {
        let mut routes = self.provider.model_routes();
        routes.extend(configured_named_provider_routes());
        jcode_provider_core::selection::dedupe_model_routes(routes)
    }

    fn current_model(&self) -> String {
        self.provider.model()
    }
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
            // Distinct provider per model so the per-provider candidate dedup in
            // run_cheap_route keeps each test model as its own fallback step.
            provider: format!("prov-{model}"),
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
        assert!(rendered.contains("prov-cheapo"));
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

        async fn run_subtask(
            &self,
            subtask: &Subtask,
            model: &str,
            _route_api_method: Option<&str>,
        ) -> Result<String> {
            self.subtask_calls
                .lock()
                .unwrap()
                .push((subtask.description.clone(), model.to_string()));
            Ok(format!("done: {}", subtask.description))
        }

        fn routes(&self) -> Vec<ModelRoute> {
            self.routes.clone()
        }

        fn current_model(&self) -> String {
            String::new()
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

    /// Backend where `run_subtask` errors for any model in `dead_models`,
    /// simulating a dead-quota / unauthorized route.
    struct FallbackBackend {
        parent_responses: Mutex<VecDeque<String>>,
        routes: Vec<ModelRoute>,
        dead_models: std::collections::HashSet<String>,
        attempts: Mutex<Vec<String>>,
        current: String,
    }

    #[async_trait]
    impl CheapRouteBackend for FallbackBackend {
        async fn ask_parent(&self, _prompt: &str) -> Result<String> {
            Ok(self.parent_responses.lock().unwrap().pop_front().unwrap_or_default())
        }

        async fn run_subtask(
            &self,
            _subtask: &Subtask,
            model: &str,
            _route_api_method: Option<&str>,
        ) -> Result<String> {
            self.attempts.lock().unwrap().push(model.to_string());
            if self.dead_models.contains(model) {
                Err(anyhow!("insufficient_quota"))
            } else {
                Ok(format!("done via {model}"))
            }
        }

        fn routes(&self) -> Vec<ModelRoute> {
            self.routes.clone()
        }

        fn current_model(&self) -> String {
            self.current.clone()
        }
    }

    #[tokio::test]
    async fn run_cheap_route_falls_back_when_cheapest_model_errors() {
        // Menu: cheapo (cheapest, DEAD) + pricey (works). Recommend -> cheapo.
        let backend = FallbackBackend {
            parent_responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"do x","prompt":"p","difficulty":1}]"#.to_string(), // decompose
                "use cheapo".to_string(), // recommend the dead one
                "OK".to_string(),         // review
            ])),
            routes: vec![priced_route("cheapo", 100_000), priced_route("pricey", 9_000_000)],
            dead_models: ["cheapo".to_string()].into_iter().collect(),
            attempts: Mutex::new(Vec::new()),
            current: "qwen-current".to_string(),
        };

        let outcome = run_cheap_route(&backend, "task").await.unwrap();

        assert_eq!(outcome.results.len(), 1);
        // Fell back from the dead cheapo to the working pricey.
        assert_eq!(outcome.results[0].model_used, "pricey");
        assert!(outcome.results[0].output.contains("done via pricey"));
        assert_eq!(outcome.results[0].review, "OK");
        // It tried cheapo first, then pricey.
        let attempts = backend.attempts.lock().unwrap();
        assert_eq!(*attempts, vec!["cheapo".to_string(), "pricey".to_string()]);
    }

    #[tokio::test]
    async fn run_cheap_route_errors_when_all_candidates_dead() {
        let backend = FallbackBackend {
            parent_responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"do x","prompt":"p","difficulty":1}]"#.to_string(),
                "use cheapo".to_string(),
            ])),
            routes: vec![priced_route("cheapo", 100_000), priced_route("pricey", 9_000_000)],
            dead_models: ["cheapo".to_string(), "pricey".to_string()].into_iter().collect(),
            attempts: Mutex::new(Vec::new()),
            current: String::new(), // no last-resort model available
        };

        let err = run_cheap_route(&backend, "task").await.unwrap_err();
        assert!(err.to_string().contains("all 2 candidate model(s) failed"));
    }

    #[test]
    fn build_named_provider_routes_unions_static_and_cached_models_with_availability() {
        // name="modelscope", static model deepseek-v4-flash, cached model qwen-x.
        let routes = build_named_provider_routes(
            "modelscope",
            "https://api-inference.modelscope.cn/v1",
            &["deepseek-v4-flash".to_string()],   // static (config) ids
            &["qwen-x".to_string(), "deepseek-v4-flash".to_string()], // discovered (cache) ids
            true,                                  // key present -> available
            |_source, _model| None,                // pricing lookup stub
        );

        let models: std::collections::BTreeSet<&str> =
            routes.iter().map(|r| r.model.as_str()).collect();
        // union, deduped
        assert!(models.contains("deepseek-v4-flash"));
        assert!(models.contains("qwen-x"));
        assert_eq!(routes.len(), 2);
        // all carry the named-provider api_method + availability + base url detail
        assert!(routes.iter().all(|r| r.api_method == "openai-compatible:modelscope"));
        assert!(routes.iter().all(|r| r.available));
        assert!(routes.iter().all(|r| r.detail.contains("modelscope")));
    }

    #[test]
    fn build_named_provider_routes_marks_unavailable_when_no_key() {
        let routes = build_named_provider_routes(
            "deepseek",
            "https://api.deepseek.com/v1",
            &["deepseek-chat".to_string()],
            &[],
            false, // no key
            |_s, _m| None,
        );
        assert_eq!(routes.len(), 1);
        assert!(!routes[0].available);
    }

    #[test]
    fn absolute_env_file_has_key_reads_absolute_path() {
        use std::io::Write;
        let path = std::env::temp_dir().join("jcode_cheap_route_absenv_test.env");
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "DEEPSEEK_API_KEY=sk-abc123").unwrap();
        drop(file);
        let abs = path.to_str().unwrap();

        assert!(absolute_env_file_has_key(Some("DEEPSEEK_API_KEY"), Some(abs)));
        assert!(!absolute_env_file_has_key(Some("MISSING_KEY"), Some(abs)));
        // relative path is not handled here (config-dir helper covers those)
        assert!(!absolute_env_file_has_key(Some("DEEPSEEK_API_KEY"), Some("rel.env")));
        // missing args
        assert!(!absolute_env_file_has_key(None, Some(abs)));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn run_cheap_route_rescues_via_current_model_when_all_ranked_dead() {
        // Every ranked route is dead (mirrors the real case: all 6 cheapest are
        // one exhausted key). The parent's own current model still works.
        let backend = FallbackBackend {
            parent_responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"do x","prompt":"p","difficulty":1}]"#.to_string(),
                "use cheapo".to_string(),
                "OK".to_string(),
            ])),
            routes: vec![priced_route("cheapo", 100_000), priced_route("pricey", 9_000_000)],
            dead_models: ["cheapo".to_string(), "pricey".to_string()].into_iter().collect(),
            attempts: Mutex::new(Vec::new()),
            current: "qwen-live".to_string(),
        };

        let outcome = run_cheap_route(&backend, "task").await.unwrap();

        assert_eq!(outcome.results[0].model_used, "qwen-live");
        assert!(outcome.results[0].output.contains("done via qwen-live"));
        // Tried both dead ranked routes, then rescued via the current model.
        let attempts = backend.attempts.lock().unwrap();
        assert_eq!(
            *attempts,
            vec!["cheapo".to_string(), "pricey".to_string(), "qwen-live".to_string()]
        );
    }

    #[tokio::test]
    async fn run_cheap_route_tries_one_model_per_provider() {
        fn route(model: &str, provider: &str, micros: u64) -> ModelRoute {
            ModelRoute {
                model: model.to_string(),
                provider: provider.to_string(),
                api_method: "a".to_string(),
                available: true,
                detail: String::new(),
                cheapness: Some(RouteCheapnessEstimate::metered(
                    RouteCostSource::PublicApiPricing,
                    RouteCostConfidence::Exact,
                    micros,
                    micros,
                    None,
                    None,
                )),
            }
        }
        // 3 dead OpenAI models (one key) + a working deepseek model.
        let backend = FallbackBackend {
            parent_responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"x","prompt":"p","difficulty":1}]"#.to_string(),
                "use gpt-nano".to_string(),
                "OK".to_string(),
            ])),
            routes: vec![
                route("gpt-nano", "openai", 10),
                route("gpt-mini", "openai", 20),
                route("gpt-small", "openai", 30),
                route("deepseek-chat", "deepseek", 100),
            ],
            dead_models: ["gpt-nano", "gpt-mini", "gpt-small"]
                .into_iter()
                .map(String::from)
                .collect(),
            attempts: Mutex::new(Vec::new()),
            current: "qwen".to_string(),
        };

        let outcome = run_cheap_route(&backend, "task").await.unwrap();

        assert_eq!(outcome.results[0].model_used, "deepseek-chat");
        // Only ONE OpenAI model tried (not all 3), then deepseek — per-provider cap.
        let attempts = backend.attempts.lock().unwrap();
        assert_eq!(
            *attempts,
            vec!["gpt-nano".to_string(), "deepseek-chat".to_string()]
        );
    }

    // --- minimal provider mock (mirrors agent_tests::DelayedProvider) ---
    struct ParentMock {
        reply: String,
        routes: Vec<ModelRoute>,
    }

    #[async_trait]
    impl crate::provider::Provider for ParentMock {
        async fn complete(
            &self,
            _messages: &[jcode_message_types::Message],
            _tools: &[jcode_message_types::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            let reply = self.reply.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<jcode_message_types::StreamEvent>>(8);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(jcode_message_types::StreamEvent::TextDelta(reply)))
                    .await;
                let _ = tx
                    .send(Ok(jcode_message_types::StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            });
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        }

        fn name(&self) -> &str {
            "parentmock"
        }

        fn fork(&self) -> std::sync::Arc<dyn crate::provider::Provider> {
            std::sync::Arc::new(Self {
                reply: self.reply.clone(),
                routes: self.routes.clone(),
            })
        }

        fn model_routes(&self) -> Vec<ModelRoute> {
            self.routes.clone()
        }
    }

    #[tokio::test]
    async fn provider_backend_delegates_ask_parent_and_routes() {
        let provider: std::sync::Arc<dyn crate::provider::Provider> =
            std::sync::Arc::new(ParentMock {
                reply: "PARENT_REPLY".to_string(),
                routes: vec![priced_route("cheapo", 100_000)],
            });
        let registry = crate::tool::Registry::empty();
        let backend = ProviderCheapBackend::new(provider, registry);

        // ask_parent drains the provider stream into text.
        let answer = backend.ask_parent("anything").await.unwrap();
        assert_eq!(answer, "PARENT_REPLY");

        // routes delegates to provider.model_routes().
        let routes = backend.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].model, "cheapo");
    }

    #[test]
    fn cheapest_available_model_returns_cheapest_route() {
        let provider = ParentMock {
            reply: String::new(),
            routes: vec![
                priced_route("pricey", 9_000_000),
                priced_route("cheapo", 100_000),
            ],
        };
        assert_eq!(
            cheapest_available_model(&provider),
            Some(("cheapo".to_string(), "a".to_string())) // priced_route api_method = "a"
        );
    }

    #[test]
    fn drop_banned_routes_removes_matching_models() {
        let routes = vec![
            priced_route("deepseek-chat", 100),
            priced_route("deepseek-v4-flash", 50),
        ];
        let kept = drop_banned_routes(routes, &["deepseek-v4-flash".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].model, "deepseek-chat");
    }

    #[test]
    fn prioritize_preferred_moves_matches_to_front() {
        let ranked = rank_routes_by_cost(vec![
            priced_route("cheapo", 100),
            priced_route("pricey", 9_000_000),
        ]);
        assert_eq!(ranked[0].route.model, "cheapo");
        // Preferring "pricey" moves it ahead of the cheaper "cheapo".
        let reordered = prioritize_preferred(ranked, &["pricey".to_string()]);
        assert_eq!(reordered[0].route.model, "pricey");
        assert_eq!(reordered[1].route.model, "cheapo");
    }

    #[test]
    fn route_matches_preference_handles_model_and_composite() {
        let route = priced_route("deepseek-chat", 100); // provider "prov-deepseek-chat"
        assert!(route_matches_preference(&route, "deepseek-chat"));
        assert!(route_matches_preference(&route, "prov-deepseek-chat/deepseek-chat"));
        assert!(!route_matches_preference(&route, "gpt-5-nano"));
        assert!(!route_matches_preference(&route, ""));
    }

    #[tokio::test(start_paused = true)]
    async fn run_cheap_route_times_out_hanging_route_and_falls_back() {
        struct HangBackend {
            responses: Mutex<VecDeque<String>>,
            routes: Vec<ModelRoute>,
            attempts: Mutex<Vec<String>>,
        }
        #[async_trait]
        impl CheapRouteBackend for HangBackend {
            async fn ask_parent(&self, _p: &str) -> Result<String> {
                Ok(self.responses.lock().unwrap().pop_front().unwrap_or_default())
            }
            async fn run_subtask(
                &self,
                _s: &Subtask,
                model: &str,
                _r: Option<&str>,
            ) -> Result<String> {
                self.attempts.lock().unwrap().push(model.to_string());
                if model == "hang" {
                    // Never returns within the timeout (paused clock auto-advances).
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    Ok("late".to_string())
                } else {
                    Ok(format!("done via {model}"))
                }
            }
            fn routes(&self) -> Vec<ModelRoute> {
                self.routes.clone()
            }
            fn current_model(&self) -> String {
                String::new()
            }
        }

        // "hang" is cheapest (recommended) but stalls; "good" is the next route.
        let backend = HangBackend {
            responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"x","prompt":"p","difficulty":1}]"#.to_string(),
                "use hang".to_string(),
                "OK".to_string(),
            ])),
            routes: vec![priced_route("hang", 100), priced_route("good", 9_000_000)],
            attempts: Mutex::new(Vec::new()),
        };

        let outcome = run_cheap_route(&backend, "task").await.unwrap();

        // Timed out on the hanging route, fell back to the working one.
        assert_eq!(outcome.results[0].model_used, "good");
        let attempts = backend.attempts.lock().unwrap();
        assert_eq!(*attempts, vec!["hang".to_string(), "good".to_string()]);
    }

    #[tokio::test]
    async fn run_cheap_route_pins_chosen_route_api_method() {
        struct RouteRecordingBackend {
            seen: Mutex<Vec<Option<String>>>,
            routes: Vec<ModelRoute>,
            responses: Mutex<VecDeque<String>>,
        }
        #[async_trait]
        impl CheapRouteBackend for RouteRecordingBackend {
            async fn ask_parent(&self, _p: &str) -> Result<String> {
                Ok(self.responses.lock().unwrap().pop_front().unwrap_or_default())
            }
            async fn run_subtask(
                &self,
                _s: &Subtask,
                _m: &str,
                route_api_method: Option<&str>,
            ) -> Result<String> {
                self.seen.lock().unwrap().push(route_api_method.map(str::to_string));
                Ok("done".to_string())
            }
            fn routes(&self) -> Vec<ModelRoute> {
                self.routes.clone()
            }
            fn current_model(&self) -> String {
                String::new()
            }
        }

        let route = ModelRoute {
            model: "deepseek-chat".to_string(),
            provider: "deepseek".to_string(),
            api_method: "openai-compatible:deepseek".to_string(),
            available: true,
            detail: String::new(),
            cheapness: Some(RouteCheapnessEstimate::metered(
                RouteCostSource::PublicApiPricing,
                RouteCostConfidence::Exact,
                100,
                100,
                None,
                None,
            )),
        };
        let backend = RouteRecordingBackend {
            seen: Mutex::new(Vec::new()),
            routes: vec![route],
            responses: Mutex::new(VecDeque::from(vec![
                r#"[{"description":"x","prompt":"p","difficulty":1}]"#.to_string(),
                "use deepseek-chat".to_string(),
                "OK".to_string(),
            ])),
        };

        run_cheap_route(&backend, "task").await.unwrap();

        // The chosen route's api_method was pinned through to the spawn.
        let seen = backend.seen.lock().unwrap();
        assert_eq!(seen[0].as_deref(), Some("openai-compatible:deepseek"));
    }
}
