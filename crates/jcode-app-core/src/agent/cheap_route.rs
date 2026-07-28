//! Cheap-routing orchestrator (auto mode). The expensive parent model only
//! decomposes the task, recommends one cheap model, and reviews results; cheap
//! subagents do the work. See
//! docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md.

use crate::agent::debate_status::{DebatePhase, DebateStatusReporter};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::future::join_all;
use jcode_provider_core::ModelRoute;
use jcode_provider_core::selection::{CheapRouteCandidate, rank_routes_by_cost};

mod strict;
use serde::Deserialize;
use strict::{blackout_error, coordinator_is_allowed_as_last_resort, strong_model_or_empty};

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
    /// Runtime-only position of this subtask in the plan, used to route the
    /// worker's live output tail to the matching side-panel row. Not part of the
    /// model's JSON contract (skipped in (de)serialization).
    #[serde(skip)]
    pub index: usize,
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

// ── Run-scoped circuit breaker ──────────────────────────────────────────

/// What kind of failure occurred, for circuit-breaker classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakerFailureKind {
    /// Non-retryable config/auth error (4xx): invalid_request, product not
    /// activated, auth failures. One strike trips the breaker.
    ConfigError,
    /// The candidate timed out. Two consecutive timeouts trip the breaker.
    Timeout,
}

/// Per-route breaker state within one [`run_cheap_route`] invocation.
#[derive(Debug, Clone, Default)]
struct RouteBreakerState {
    consecutive_failures: usize,
    last_failure_kind: Option<BreakerFailureKind>,
}

/// Run-scoped circuit breaker that remembers which routes have failed
/// and skips dead ones for the remainder of the run.
struct RouteBreaker {
    map: std::collections::HashMap<String, RouteBreakerState>,
}

impl RouteBreaker {
    fn new() -> Self {
        Self {
            map: std::collections::HashMap::new(),
        }
    }

    /// Record a failure for `route_key` (model name). Returns `true` if
    /// this failure tripped the breaker (the route should now be skipped).
    fn record_failure(&mut self, route_key: &str, kind: BreakerFailureKind) -> bool {
        let entry = self.map.entry(route_key.to_string()).or_default();

        // If the failure kind changed (e.g. timeout after a config error),
        // reset the consecutive count — we track each kind independently.
        if entry.last_failure_kind != Some(kind) {
            entry.consecutive_failures = 0;
        }
        entry.consecutive_failures += 1;
        entry.last_failure_kind = Some(kind);

        // Trip thresholds:
        // - One ConfigError (invalid_request/product-not-activated/auth)
        // - Two consecutive Timeouts
        match kind {
            BreakerFailureKind::ConfigError => entry.consecutive_failures >= 1,
            BreakerFailureKind::Timeout => entry.consecutive_failures >= 2,
        }
    }

    /// Whether this route is currently tripped and should be skipped.
    fn is_tripped(&self, route_key: &str) -> bool {
        self.map
            .get(route_key)
            .map_or(false, |s| match s.last_failure_kind {
                Some(BreakerFailureKind::ConfigError) => s.consecutive_failures >= 1,
                Some(BreakerFailureKind::Timeout) => s.consecutive_failures >= 2,
                None => false,
            })
    }

    /// Filter `candidates` through the breaker. Returns the survivors.
    /// If the breaker would empty the list, returns the original candidates
    /// unchanged (never-empty fallback).
    fn filter_candidates(
        &self,
        candidates: &[(String, Option<String>)],
    ) -> Vec<(String, Option<String>)> {
        let survivors: Vec<_> = candidates
            .iter()
            .filter(|(model, _)| !self.is_tripped(model))
            .cloned()
            .collect();
        if survivors.is_empty() {
            candidates.to_vec() // fallback: never skip ALL candidates
        } else {
            survivors
        }
    }
}

/// Classify a provider error string for circuit-breaker purposes.
fn classify_failure(error: &anyhow::Error) -> BreakerFailureKind {
    let msg = error.to_string().to_ascii_lowercase();
    if msg.contains("status: 400")
        || msg.contains("status: 401")
        || msg.contains("status: 403")
        || msg.contains("status: 404")
        || msg.contains("invalid_request")
        || msg.contains("invalid request")
        || msg.contains("product not activated")
        || msg.contains("product-not-activated")
        || msg.contains("not activated")
        || msg.contains("unauthorized")
        || msg.contains("authentication")
        || msg.contains("invalid api key")
        || msg.contains("invalid key")
        || msg.contains("access denied")
        || msg.contains("forbidden")
        || msg.contains("model_not_found")
        || msg.contains("model not found")
    {
        BreakerFailureKind::ConfigError
    } else {
        // Everything else is treated as a transient error. The only other
        // failure that reaches the breaker is a timeout, which is
        // classified by the caller.
        BreakerFailureKind::Timeout
    }
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
    /// Run the configured verification command (e.g. `cargo check`) after a
    /// subtask's edits, returning `(passed, combined_output)`. The default impl
    /// shells out in the working directory; test backends override it to script
    /// outcomes without spawning a process.
    async fn verify_edits(&self, command: &str) -> Result<(bool, String)> {
        run_verify_command(command).await
    }
    /// Run the STRONG model for one-shot text (used for debate aggregate). Default delegates to ask_parent.
    async fn ask_strong(&self, prompt: &str) -> Result<String> {
        self.ask_parent(prompt).await
    }
    /// Whether this backend runs gold-mode debates. Default false. Production impl reads session+config.
    fn gold_mode(&self) -> bool {
        false
    }
    /// Number of distinct proposers for a gold debate. Default 3. Prod impl reads config.
    fn gold_k(&self) -> usize {
        3
    }
    /// Debate status reporter. Default returns a static no-op so test backends
    /// need not implement this method.
    fn reporter(&self) -> &dyn crate::agent::debate_status::DebateStatusReporter {
        static NOOP: crate::agent::debate_status::NoopDebateReporter =
            crate::agent::debate_status::NoopDebateReporter;
        &NOOP
    }
}

/// Run a verification shell command in the current working directory, capturing
/// stdout+stderr and the exit status. Returns `(passed, combined_output)` where
/// `passed` is true only on a zero exit code. Mirrors the spawn/capture pattern
/// used by the bash tool (kill-on-drop, piped stdout/stderr read concurrently).
/// Output is truncated so a noisy build log can't blow up the repair prompt.
async fn run_verify_command(command: &str) -> Result<(bool, String)> {
    use tokio::io::AsyncReadExt;
    let mut cmd = if cfg!(windows) {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("failed to spawn verify command '{command}': {e}"))?;
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut out) = stdout_handle.take() {
            let _ = out.read_to_string(&mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut err) = stderr_handle.take() {
            let _ = err.read_to_string(&mut buf).await;
        }
        buf
    });
    let status = child.wait().await?;
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();
    let mut combined = String::new();
    if !stdout.trim().is_empty() {
        combined.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim_end());
    }
    // Keep only the tail — build/test failures put the useful error at the end,
    // and the repair prompt must stay small for a cheap model. Advance the cut
    // point to the next char boundary so slicing never panics on a multibyte
    // UTF-8 sequence (build logs contain non-ASCII).
    const MAX_VERIFY_OUTPUT: usize = 4000;
    if combined.len() > MAX_VERIFY_OUTPUT {
        let mut start = combined.len() - MAX_VERIFY_OUTPUT;
        while start < combined.len() && !combined.is_char_boundary(start) {
            start += 1;
        }
        combined = format!("…(truncated)…\n{}", &combined[start..]);
    }
    Ok((status.success(), combined))
}

/// Strip a single surrounding markdown code fence (```json ... ```), returning
/// the inner text. Weak models routinely wrap JSON in fences.
pub fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop the optional language tag on the opening fence line.
    let body = after_open.split_once('\n').map(|x| x.1).unwrap_or("");
    match body.rfind("```") {
        Some(close) => body[..close].trim(),
        None => body.trim(),
    }
}

/// Parse the parent's decompose response into subtasks. Accepts a raw JSON array
/// or one wrapped in a markdown code fence. Thinking models (DeepSeek/GLM style)
/// also prepend `<think>…</think>` reasoning before the JSON; strip that and, as
/// a last resort, extract the outermost `[…]` span so stray prose never sinks
/// an otherwise-valid decomposition.
pub fn parse_subtasks(text: &str) -> Result<Vec<Subtask>> {
    let without_think = match (text.find("<think>"), text.rfind("</think>")) {
        (Some(_), Some(end)) => &text[end + "</think>".len()..],
        _ => text,
    };
    let json = strip_code_fence(without_think);
    let parsed: Result<Vec<Subtask>, _> = serde_json::from_str(json);
    let subtasks: Vec<Subtask> = match parsed {
        Ok(subtasks) => subtasks,
        Err(first_err) => {
            // Fallback: extract the outermost JSON array span.
            let start = json.find('[');
            let end = json.rfind(']');
            match (start, end) {
                (Some(start), Some(end)) if start < end => serde_json::from_str(&json[start..=end])
                    .map_err(|e| anyhow!("failed to parse subtasks JSON: {e}; raw: {json}"))?,
                _ => {
                    return Err(anyhow!(
                        "failed to parse subtasks JSON: {first_err}; raw: {json}"
                    ));
                }
            }
        }
    };
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
///
/// The independence requirement is stated explicitly and with an example,
/// because "independent" alone was not enough: parents emitted chained
/// subtasks referring to `{previous output}`, and each subtask prompt is sent
/// verbatim to a worker in a FRESH session with no prior context, so the
/// placeholder arrived unsubstituted and the worker answered by asking for the
/// missing input. One good subtask followed by two confused ones is worse than
/// a single self-contained subtask.
pub fn build_decompose_prompt(task: &str) -> String {
    format!(
        "Split the following coding task into the smallest SELF-CONTAINED subtasks. \
Each subtask runs in a SEPARATE session with NO memory of the others and NO access \
to their output, so every prompt must stand alone: repeat any file paths and context \
it needs, and never refer to a previous subtask or use placeholders like \
'{{previous output}}'. If a step genuinely needs an earlier step's result, merge them \
into ONE subtask instead of splitting. Prefer fewer, complete subtasks. \
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

/// Prompt for the single repair attempt after a verification command failed.
/// Gives the model the original subtask, what it produced, the exact command
/// that failed, and the failure output, and asks it to fix the actual cause so
/// the command passes — not to paper over the symptom.
pub fn build_repair_prompt(
    subtask: &Subtask,
    previous_output: &str,
    verify_cmd: &str,
    failure_output: &str,
) -> String {
    format!(
        "Your previous attempt at this subtask did not pass verification. \
Fix the underlying cause so `{verify_cmd}` succeeds. Re-read and edit the \
relevant files; do not just describe the fix.\n\n\
SUBTASK: {}\n\n\
YOUR PREVIOUS RESULT:\n{}\n\n\
VERIFY COMMAND: {verify_cmd}\n\
VERIFY FAILURE OUTPUT:\n{}",
        subtask.description, previous_output, failure_output
    )
}

/// Verify a completed subtask's edits with `verify_cmd`; on failure, retry the
/// subtask ONCE on the same route (`model`/`api_method`) with the failure output
/// fed back, then re-verify. Returns `(final_output, note)` where `note` is a
/// short human-readable verdict prepended to the review. Best-effort: any
/// verify-infrastructure error leaves the original output intact.
/// Upper bound on a single verify command. A hanging test suite must not block
/// the subtask loop forever; generous enough for a real build/test, bounded
/// enough to fail fast on a deadlock.
const VERIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// Outcome of one bounded verify run, normalizing timeout/spawn-error/exit into
/// three cases the caller can match without nested `Result<Result<…>>`.
enum VerifyOutcome {
    Passed,
    Failed(String),
    Unavailable(String),
}

async fn run_bounded_verify(backend: &dyn CheapRouteBackend, cmd: &str) -> VerifyOutcome {
    match tokio::time::timeout(VERIFY_TIMEOUT, backend.verify_edits(cmd)).await {
        Ok(Ok((true, _))) => VerifyOutcome::Passed,
        Ok(Ok((false, out))) => VerifyOutcome::Failed(out),
        Ok(Err(e)) => VerifyOutcome::Unavailable(format!("could not run `{cmd}`: {e}")),
        Err(_) => VerifyOutcome::Unavailable(format!(
            "`{cmd}` timed out after {}s",
            VERIFY_TIMEOUT.as_secs()
        )),
    }
}

async fn verify_and_maybe_repair(
    backend: &dyn CheapRouteBackend,
    subtask: &Subtask,
    model: &str,
    api_method: Option<&str>,
    output: String,
    verify_cmd: &str,
) -> (String, String) {
    let fail_out = match run_bounded_verify(backend, verify_cmd).await {
        VerifyOutcome::Passed => {
            return (output, format!("[verify: `{verify_cmd}` passed] "));
        }
        VerifyOutcome::Unavailable(msg) => {
            return (output, format!("[verify: {msg}] "));
        }
        VerifyOutcome::Failed(out) => out,
    };

    // One repair attempt on the same pinned route, failure fed back.
    let repair = Subtask {
        description: subtask.description.clone(),
        prompt: build_repair_prompt(subtask, &output, verify_cmd, &fail_out),
        difficulty: subtask.difficulty,
        index: subtask.index,
    };
    let repaired = tokio::time::timeout(
        SUBTASK_TIMEOUT,
        backend.run_subtask(&repair, model, api_method),
    )
    .await;
    match repaired {
        Ok(Ok(new_output)) => match run_bounded_verify(backend, verify_cmd).await {
            VerifyOutcome::Passed => (
                new_output,
                format!("[verify: `{verify_cmd}` failed, repaired, now passes] "),
            ),
            VerifyOutcome::Failed(_) => (
                new_output,
                format!("[verify: `{verify_cmd}` still failing after one repair attempt] "),
            ),
            VerifyOutcome::Unavailable(msg) => (new_output, format!("[verify re-check: {msg}] ")),
        },
        Ok(Err(e)) => (
            output,
            format!("[verify: `{verify_cmd}` failed; repair attempt errored: {e}] "),
        ),
        Err(_) => (
            output,
            format!(
                "[verify: `{verify_cmd}` failed; repair timed out after {}s] ",
                SUBTASK_TIMEOUT.as_secs()
            ),
        ),
    }
}

/// Auto-mode cheap routing: decompose -> rank -> recommend -> spawn -> review.
pub async fn run_cheap_route(
    backend: &dyn CheapRouteBackend,
    task: &str,
) -> Result<CheapRouteOutcome> {
    // 1. Parent decomposes the task.
    let decompose = backend.ask_parent(&build_decompose_prompt(task)).await?;
    let mut subtasks = parse_subtasks(&decompose)?;
    // Stamp each subtask with its plan position so the worker's live output tail
    // routes to the matching side-panel row.
    for (i, st) in subtasks.iter_mut().enumerate() {
        st.index = i;
    }
    // Publish the plan so the user can watch progress (side panel page).
    backend.reporter().plan(
        &subtasks
            .iter()
            .map(|s| (s.description.clone(), s.difficulty))
            .collect::<Vec<_>>(),
    );

    // 2. Rank ALL available routes cheapest-first. The top slice is the recommend
    //    menu; the FULL ranked list is the fallback candidate order, so a working
    //    route that isn't in the cheapest 6 (e.g. deepseek sitting behind dead
    //    OpenAI nano/mini) still gets reached instead of being skipped.
    let ranked = ranked_with_preferences(backend.routes());
    let current_model = backend.current_model();
    // A genuine absence of routes is fatal. But when routes EXIST and are merely
    // all cooled down by health tracking (e.g. a transient quota blip across
    // every cheap provider), don't fail the run — fall through to the parent's
    // current model as the last resort (the candidate list below appends it).
    let strict = crate::config::config().agents.cheap_route_strict;
    if ranked.is_empty() && current_model.trim().is_empty() {
        return Err(anyhow!("no available model routes to route work to"));
    }
    // Strict: a total blackout must FAIL rather than escalate silently.
    if ranked.is_empty() && strict {
        return Err(blackout_error(&current_model));
    }
    let menu: Vec<CheapRouteCandidate> = ranked.iter().take(MAX_MENU).cloned().collect();

    // 3. Pick the primary model. When the user pinned a preference
    //    (agents.cheap_route_prefer), it is a HARD override: use the
    //    prefer-prioritized cheapest route directly and SKIP the parent recommend
    //    round-trip entirely (faster + deterministic, and it stops the parent
    //    from picking its own expensive model over the cheap one the user wanted).
    //    Otherwise, ask the parent to recommend from the cheapest-first menu.
    let recommended_model = if ranked.is_empty() {
        // Every cheap route is cooled down: route directly to the parent's
        // known-working current model instead of erroring.
        current_model.clone()
    } else if !crate::config::config().agents.cheap_route_prefer.is_empty() {
        ranked
            .first()
            .map(|c| c.route.model.clone())
            .ok_or_else(|| anyhow!("no candidate models to route to"))?
    } else {
        let menu_str = format_menu_for_prompt(&menu);
        let recommend = backend
            .ask_parent(&build_recommend_prompt(task, &subtasks, &menu_str))
            .await?;
        parse_recommended_model(&recommend, &menu)?
    };

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
    // Difficulty-tiered routing: hard subtasks (difficulty ABOVE the threshold)
    // try a stronger model FIRST — the configured cheap_route_strong_model, or
    // the parent's own (main) model — so the expensive model only spends on
    // complex work. Trivial subtasks stay on the cheapest-first list. Captured
    // before current_model is moved into the candidate list below.
    let difficulty_threshold = crate::config::config()
        .agents
        .cheap_route_difficulty_threshold;
    let strong_model = strong_model_or_empty(&current_model, strict);
    let strong_api = ranked
        .iter()
        .find(|c| c.route.model == strong_model)
        .map(|c| c.route.api_method.clone());

    // Last resort: the parent's own current model. EXCEPT when it is banned via
    // cheap_route_ban (e.g. the user banned Claude): never silently burn an
    // expensive banned coordinator just because every cheap route is dead — the
    // subtask should fail loudly instead. The cheap candidates above are already
    // a cooled-but-cheap fallback, so this only drops the truly-expensive escape.
    if coordinator_is_allowed_as_last_resort(&current_model, strict)
        && !candidates.iter().any(|(m, _)| m == &current_model)
    {
        candidates.push((current_model, None));
    }

    // 5. Run each subtask (with fallback) and have the parent review each result.
    //    A run-scoped circuit breaker skips routes that repeatedly fail across
    //    subtasks so dead routes (e.g. product-not-activated, persistent timeouts)
    //    don't waste a full timeout on every subtask.
    let mut breaker = RouteBreaker::new();
    let mut results = Vec::with_capacity(subtasks.len());
    for (subtask_index, subtask) in subtasks.iter().enumerate() {
        // Gold mode: run a multi-model debate on a HARD REASONING subtask
        // (code subtasks use strong + verify+repair instead — execution-grounded).
        if backend.gold_mode()
            && subtask.difficulty > difficulty_threshold
            && !is_code_subtask(subtask)
        {
            let proposers: Vec<(String, Option<String>)> = ranked
                .iter()
                .map(|c| (c.route.model.clone(), Some(c.route.api_method.clone())))
                .take(backend.gold_k().max(2))
                .collect();
            if proposers.len() >= 2 {
                if let Ok(output) = run_debate(
                    backend,
                    subtask,
                    &proposers,
                    backend.gold_k(),
                    backend.reporter(),
                )
                .await
                {
                    results.push(SubtaskResult {
                        description: subtask.description.clone(),
                        output,
                        review: String::new(),
                        model_used: format!("debate({})", proposers.len()),
                    });
                    continue;
                }
                // on debate Err: fall through to the normal single-model path below
            }
        }

        // Hard subtasks try the strong model first (then cheap routes as
        // fallback, so a dead strong model still completes). Trivial subtasks
        // use the cheapest-first list directly.
        let task_candidates: Vec<(String, Option<String>)> =
            if subtask.difficulty > difficulty_threshold && !strong_model.is_empty() {
                // Strong model FIRST, then the cheap routes as fallback (skipping
                // a duplicate of the strong model, which is otherwise present as
                // the last-resort entry).
                let mut tiered = Vec::with_capacity(candidates.len() + 1);
                tiered.push((strong_model.clone(), strong_api.clone()));
                tiered.extend(
                    candidates
                        .iter()
                        .filter(|(m, _)| m != &strong_model)
                        .cloned(),
                );
                tiered
            } else {
                candidates.clone()
            };

        // Circuit breaker: skip routes that have already been tripped by
        // failures on earlier subtasks (e.g. product-not-activated, 2x timeouts).
        let active_candidates = breaker.filter_candidates(&task_candidates);
        let mut errors: Vec<String> = Vec::new();
        // Report skipped routes so final error messages stay informative.
        for (model, _) in &task_candidates {
            if breaker.is_tripped(model) {
                errors.push(format!(
                    "{model}: skipped (circuit breaker tripped — route dead)"
                ));
            }
        }

        let mut chosen: Option<(String, Option<String>, String)> = None; // (model, api_method, output)
        let attempt_timeout = subtask_attempt_timeout(subtask.difficulty, difficulty_threshold);
        for (model, api_method) in &active_candidates {
            backend.reporter().subtask(
                subtask_index,
                DebatePhase::Running,
                &format!("running on {model}"),
            );
            let attempt = tokio::time::timeout(
                attempt_timeout,
                backend.run_subtask(subtask, model, api_method.as_deref()),
            )
            .await;
            match attempt {
                Ok(Ok(output)) => {
                    chosen = Some((model.to_string(), api_method.clone(), output));
                    break;
                }
                Ok(Err(err)) => {
                    let kind = classify_failure(&err);
                    let tripped = breaker.record_failure(model, kind);
                    let suffix = if tripped {
                        " (circuit breaker tripped)"
                    } else {
                        ""
                    };
                    errors.push(format!("{model}: {err}{suffix}"));
                }
                Err(_) => {
                    let tripped = breaker.record_failure(model, BreakerFailureKind::Timeout);
                    let suffix = if tripped {
                        " (circuit breaker tripped)"
                    } else {
                        ""
                    };
                    errors.push(format!(
                        "{model}: timed out after {}s{suffix}",
                        attempt_timeout.as_secs()
                    ));
                }
            }
        }
        let (model_used, api_method_used, mut output) = match chosen {
            Some(chosen) => chosen,
            None => {
                backend.reporter().subtask(
                    subtask_index,
                    DebatePhase::Failed,
                    "all candidates failed",
                );
                return Err(anyhow!(
                    "all {} candidate model(s) failed for subtask '{}': {}",
                    task_candidates.len(),
                    subtask.description,
                    errors.join("; ")
                ));
            }
        };
        backend
            .reporter()
            .subtask(subtask_index, DebatePhase::Done, &model_used);

        // Execution-grounded verification: if a verify command is configured,
        // run it after the subtask's edits, retrying the subtask once on failure
        // with the failure fed back (see `verify_and_maybe_repair`). No command
        // configured => no-op, preserving prior behavior.
        let mut verify_note = String::new();
        if let Some(cmd) = crate::config::config()
            .agents
            .cheap_route_verify_cmd
            .clone()
            && !cmd.trim().is_empty()
        {
            let (new_output, note) = verify_and_maybe_repair(
                backend,
                subtask,
                &model_used,
                api_method_used.as_deref(),
                output,
                &cmd,
            )
            .await;
            output = new_output;
            verify_note = note;
        }

        // Review is best-effort: a parent-review error must not discard a
        // subtask that already completed successfully.
        let review = match backend
            .ask_parent(&build_review_prompt(subtask, &output))
            .await
        {
            Ok(review) => format!("{verify_note}{review}"),
            Err(err) => format!("{verify_note}(review unavailable: {err})"),
        };
        results.push(SubtaskResult {
            description: subtask.description.clone(),
            output,
            review,
            model_used,
        });
    }

    // Mark the run complete so the side-panel phase label stops saying
    // "running subtasks" once every subtask has finished.
    backend.reporter().phase("complete");

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
    let api_method = jcode_base::provider_catalog::openai_compatible_api_method(name);
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
    content.lines().any(|line| {
        line.strip_prefix(&prefix)
            .is_some_and(|v| !v.trim().is_empty())
    })
}

/// Build a metered cheapness estimate from a user-configured per-million-token
/// price hint (USD). Lets providers absent from the models.dev catalog
/// (modelscope, dashscope, …) participate in cost ranking. Negative inputs are
/// clamped to 0.
fn cheapness_from_price_hint(
    input_usd_per_mtok: f64,
    output_usd_per_mtok: f64,
) -> jcode_provider_core::RouteCheapnessEstimate {
    let to_micros = |usd: f64| (usd.max(0.0) * 1_000_000.0).round() as u64;
    jcode_provider_core::RouteCheapnessEstimate::metered(
        jcode_provider_core::RouteCostSource::PublicApiPricing,
        jcode_provider_core::RouteCostConfidence::High,
        to_micros(input_usd_per_mtok),
        to_micros(output_usd_per_mtok),
        None,
        Some("config price hint".to_string()),
    )
}

fn configured_named_provider_routes() -> Vec<ModelRoute> {
    let cfg = crate::config::config();
    let mut routes = Vec::new();
    for (name, provider_cfg) in &cfg.providers {
        let static_ids: Vec<String> = provider_cfg.models.iter().map(|m| m.id.clone()).collect();
        let cached_ids: Vec<String> =
            crate::provider::openrouter::load_disk_cache_entry_for_namespace(name)
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
        // Per-model price hints from config let a provider whose pricing is NOT
        // in the models.dev catalog (e.g. modelscope, dashscope) still be
        // cost-ranked instead of sorting last as "price unknown". Both input and
        // output must be set and non-negative.
        let price_hints: std::collections::HashMap<String, (f64, f64)> = provider_cfg
            .models
            .iter()
            .filter_map(
                |m| match (m.price_input_per_mtok, m.price_output_per_mtok) {
                    (Some(input), Some(output)) if input >= 0.0 && output >= 0.0 => {
                        Some((m.id.clone(), (input, output)))
                    }
                    _ => None,
                },
            )
            .collect();
        routes.extend(build_named_provider_routes(
            name,
            &provider_cfg.base_url,
            &static_ids,
            &cached_ids,
            key_present,
            |source, model| {
                // Config price hint wins over the catalog: the user is declaring
                // the price for a key the catalog doesn't cover.
                if let Some(&(input, output)) = price_hints.get(model) {
                    return Some(cheapness_from_price_hint(input, output));
                }
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

/// Model-name fragments for legacy COMPLETION / base models and non-chat
/// endpoints (embeddings, audio, image, moderation) that are cheap by price but
/// unusable for agentic chat — e.g. OpenAI `babbage-002` / `davinci-002`, which a
/// pure cost ranker would pick as "cheapest" and then strand a worker on a
/// `model_not_found` (the cause of a real Claude-session worker dying after a
/// 120s stall). Matched as substrings, case-insensitive, and dropped
/// UNCONDITIONALLY (independent of config `ban`) so no user has to enumerate junk
/// models by hand. Deliberately does NOT include "instruct": many legitimate chat
/// models (qwen/llama `*-instruct`) carry that suffix; only the specific OpenAI
/// completion model `gpt-3.5-turbo-instruct` is listed.
const NON_CHAT_MODEL_FRAGMENTS: &[&str] = &[
    "babbage",
    "davinci",
    "curie",
    "gpt-3.5-turbo-instruct",
    "text-embedding",
    "whisper",
    "dall-e",
    "tts-",
    "moderation",
];

/// Drop models that are not usable for agentic chat (see
/// [`NON_CHAT_MODEL_FRAGMENTS`]). Applied to every cheap-route ranking so a junk
/// completion/base model can never be selected as the "cheapest" worker.
fn drop_non_chat_models(routes: Vec<ModelRoute>) -> Vec<ModelRoute> {
    routes
        .into_iter()
        .filter(|route| {
            let m = route.model.to_ascii_lowercase();
            !NON_CHAT_MODEL_FRAGMENTS.iter().any(|frag| m.contains(frag))
        })
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
/// Process-global cheap-route health: model id -> unix-seconds-until-eligible.
/// When a cheap session's API call fails with a quota / rate / availability
/// error (402/429/403/5xx, "insufficient balance"), the route is cooled down so
/// subsequent cheap spawns skip it and pick the next-cheapest *healthy* route
/// instead of falling through to the expensive coordinator model. Recovers
/// automatically once the cooldown expires (e.g. after the user tops up balance
/// or a rate window resets).
fn cheap_route_health() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    static HEALTH: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    HEALTH.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// How long a route is skipped after a quota/rate failure. Long enough to route
/// around a drained balance or outage, short enough to recover on its own.
const CHEAP_ROUTE_COOLDOWN_SECS: u64 = 300;

/// How many distinct cheap routes the coordinator will try before giving up on
/// a coordination call. Bounded so a fully-dead credential set fails fast
/// instead of grinding through every provider.
const COORDINATOR_ROUTE_ATTEMPTS: usize = 4;

fn cheap_route_now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cool a cheap route down after a quota/rate/availability failure so future
/// cheap spawns route around it. Called from the agent turn loop.
pub fn mark_route_unhealthy(model: &str) {
    let model = model.trim();
    if model.is_empty() {
        return;
    }
    let now = cheap_route_now_unix();
    let until = now.saturating_add(CHEAP_ROUTE_COOLDOWN_SECS);
    // Recover from a poisoned lock rather than silently dropping the update —
    // the body is a trivial map mutation, so the data is still consistent.
    let mut health = cheap_route_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Prune expired entries so the map can't grow unbounded over a long session.
    health.retain(|_, &mut until| until > now);
    health.insert(model.to_string(), until);
    crate::logging::info(&format!(
        "Cheap route '{model}' marked unhealthy for {CHEAP_ROUTE_COOLDOWN_SECS}s \
         (quota/rate/availability failure); cheap spawns will skip it and use the next-cheapest route"
    ));
}

fn route_is_healthy(model: &str) -> bool {
    let health = cheap_route_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    health
        .get(model)
        .copied()
        .map(|until| until <= cheap_route_now_unix())
        .unwrap_or(true)
}

/// Cooldowns keyed by PROVIDER rather than model. A dead or unpaid credential is
/// a property of the whole account, so every model behind it fails identically.
/// Cooling only the individual model made cheap routing walk through dozens of
/// sibling models on the same broken key before making progress.
fn cheap_provider_health() -> &'static std::sync::Mutex<std::collections::HashMap<String, u64>> {
    static HEALTH: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    HEALTH.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Cool an entire provider down after a credential-level failure (401 / dead
/// key), so cheap routing skips all of its models instead of retrying siblings
/// that share the same broken credential.
pub fn mark_provider_unhealthy(provider: &str) {
    let provider = normalize_provider_key(provider);
    let provider = provider.as_str();
    if provider.is_empty() {
        return;
    }
    let now = cheap_route_now_unix();
    let until = now.saturating_add(CHEAP_ROUTE_COOLDOWN_SECS);
    let mut health = cheap_provider_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    health.retain(|_, &mut until| until > now);
    health.insert(provider.to_string(), until);
    crate::logging::info(&format!(
        "Cheap provider '{provider}' marked unhealthy for {CHEAP_ROUTE_COOLDOWN_SECS}s \
         (dead credential); cheap routing will skip all of its models"
    ));
}

/// Provider names reach this module with inconsistent casing (display names like
/// `OpenRouter` from route metadata vs ids like `openrouter` from config), so
/// cooldowns must be keyed case-insensitively. Keying on the raw string made
/// `mark_provider_unhealthy("OpenRouter")` invisible to a route tagged
/// `openrouter`, silently disabling provider-level cooldown.
fn normalize_provider_key(provider: &str) -> String {
    provider.trim().to_ascii_lowercase()
}

fn provider_is_healthy(provider: &str) -> bool {
    let provider = normalize_provider_key(provider);
    let health = cheap_provider_health()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    health
        .get(&provider)
        .copied()
        .map(|until| until <= cheap_route_now_unix())
        .unwrap_or(true)
}

/// Inspect a provider error string; if it indicates the route is out of quota,
/// rate-limited, or unavailable, cool the route down. Quota/payment errors are
/// the common case (e.g. DeepSeek `402 Payment Required` when the balance is
/// drained), which previously made every cheap spawn fall back to the expensive
/// coordinator model instead of the next-cheapest cheap route.
pub fn note_provider_error(model: &str, error: &str) {
    if is_rate_or_quota_error(error) || is_dead_credential_error(error) {
        mark_route_unhealthy(model);
    }
}

/// Whether a provider error means the route's CREDENTIAL is dead (401 /
/// `invalid_api_key` / "user not found"), as opposed to a transient quota blip.
/// A dead key fails identically on every retry, so the route must be cooled down
/// and routed around. Without this, a single expired key (e.g. an OpenRouter key
/// whose account was deleted) sat at the top of the cheapest-first ranking and
/// aborted every cheap-route run on the very first coordinator call.
pub fn is_dead_credential_error(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("status: 401")
        || e.contains("status: 407")
        || e.contains("invalid_api_key")
        || e.contains("invalid api key")
        || e.contains("unauthorized")
        || e.contains("user not found")
}

/// Whether a provider error looks like a route is out of quota, rate-limited, or
/// transiently unavailable (402/429/403/5xx, "insufficient balance", "rate
/// limit", "quota"). Used both to cool a route down and to decide whether a
/// cheap worker should auto-reroute to another model.
pub fn is_rate_or_quota_error(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("status: 402")
        || e.contains("status: 429")
        || e.contains("status: 403")
        || e.contains("status: 500")
        || e.contains("status: 502")
        || e.contains("status: 503")
        || e.contains("status: 529")
        || e.contains("insufficient balance")
        || e.contains("payment required")
        || e.contains("rate limit")
        || e.contains("rate-limit")
        || e.contains("quota")
        // A model that doesn't exist / was deprovisioned (e.g. a junk completion
        // model that slipped through) must fail over to the next cheap route, not
        // strand the worker on a dead model after a stream stall.
        || e.contains("model_not_found")
        || e.contains("model not found")
        || e.contains("does not exist")
}

fn ranked_with_preferences(routes: Vec<ModelRoute>) -> Vec<CheapRouteCandidate> {
    let agents = &crate::config::config().agents;
    let routes = drop_banned_routes(routes, &agents.cheap_route_ban);
    let routes = drop_non_chat_models(routes);
    let ranked = rank_routes_by_cost(routes);
    let ranked = prioritize_preferred(ranked, &agents.cheap_route_prefer);
    // Prefer routes NOT cooled down by a recent quota/rate failure. But if EVERY
    // cheap route is cooled, return them anyway (cheapest-first) instead of an
    // empty list. An empty list makes callers escalate to the coordinator model
    // — which can be expensive AND ban-exempt (the last-resort path bypasses
    // cheap_route_ban), so all-cheap-dead used to silently burn an expensive
    // coordinator (e.g. Claude). Retrying a cooled CHEAP route is always safer.
    // `ranked` already had banned routes dropped, so nothing returned here is a
    // banned model.
    //
    // A provider cooled for a DEAD CREDENTIAL is different: retrying it cannot
    // succeed, because the key is rejected identically every time. So it is
    // dropped unconditionally, before the "everything is cooled" fallback. Without
    // this, an unusable key on the cheapest provider (e.g. free OpenRouter models
    // on a deleted account) was re-admitted by the fallback and re-picked on every
    // retry, so cheap routing exhausted its attempt budget without ever reaching a
    // provider that actually works.
    let ranked: Vec<CheapRouteCandidate> = {
        let usable: Vec<CheapRouteCandidate> = ranked
            .iter()
            .filter(|c| provider_is_healthy(&c.route.provider))
            .cloned()
            .collect();
        // When EVERY cheap provider is credential-cooled, return nothing rather
        // than the dead list. `run_cheap_route` already treats an empty ranking
        // as "fall through to the parent's current model", which is a route we
        // know works. Returning the dead list instead guaranteed a 401.
        usable
    };
    let healthy: Vec<CheapRouteCandidate> = ranked
        .iter()
        .filter(|c| route_is_healthy(&c.route.model))
        .cloned()
        .collect();
    if healthy.is_empty() { ranked } else { healthy }
}

/// Resolve an explicit (non-"cheapest") model spec to a concrete
/// `(model, api_method)` route from the available routes, so a provider-qualified
/// cheap model (e.g. `"deepseek/deepseek-chat"`) gets its route PINNED instead of
/// silently running on the coordinator's own provider. Matches on the bare model
/// id after any `"provider/"` prefix. Returns None if no available route matches.
pub fn resolve_model_route(
    provider: &dyn crate::provider::Provider,
    model_spec: &str,
) -> Option<(String, String)> {
    let bare = model_spec
        .rsplit('/')
        .next()
        .unwrap_or(model_spec)
        .trim()
        .to_ascii_lowercase();
    if bare.is_empty() {
        return None;
    }
    let mut routes = provider.model_routes();
    routes.extend(configured_named_provider_routes());
    let routes = jcode_provider_core::selection::dedupe_model_routes(routes);
    routes
        .into_iter()
        .find(|r| r.available && r.model.to_ascii_lowercase() == bare)
        .map(|r| (r.model, r.api_method))
}

/// Whether a model id is excluded by `agents.cheap_route_ban`. Used to keep the
/// last-resort coordinator fallback from EVER using a banned model (e.g. Claude)
/// when every cheap route is dead, and to refuse running a spawned subagent on a
/// banned model.
pub fn model_is_cheap_route_banned(model: &str) -> bool {
    let agents = &crate::config::config().agents;
    let probe = ModelRoute {
        model: model.to_string(),
        provider: String::new(),
        api_method: String::new(),
        available: true,
        detail: String::new(),
        cheapness: None,
    };
    drop_banned_routes(vec![probe], &agents.cheap_route_ban).is_empty()
}

/// THE single decision point for a spawned worker's `(model, route_api_method)`.
///
/// Root-cause fix for the Claude-burn class of bugs: a spawned worker is a fork of
/// the coordinator's provider, whose DEFAULT backend is the coordinator's own
/// (often expensive) model. Every past leak was a separate code path that left the
/// route unresolved, so the fork silently defaulted to that backend. Instead of
/// guarding each path, ALL worker-route decisions funnel through here, and the
/// final resolved model is gated against `cheap_route_ban` exactly once. The
/// invariant: this returns a non-banned route, or it returns `Err` — it can NEVER
/// return a route that bills the coordinator's banned model.
///
/// `requested_model` is the already-resolved model string (may be the `cheapest`
/// sentinel). `route_already_pinned` is `session.route_api_method.is_some()`.
/// Returns `(model, Some(api_method))` when a route should be pinned, or
/// `(model, None)` to inherit the already-pinned/coordinator route.
pub fn resolve_worker_route(
    provider: &dyn crate::provider::Provider,
    requested_model: &str,
    route_already_pinned: bool,
) -> anyhow::Result<(String, Option<String>)> {
    let provider_model = provider.model();
    let (model, route_api): (String, Option<String>) = if requested_model
        .eq_ignore_ascii_case(CHEAPEST_SENTINEL)
    {
        // "cheapest": pick the dynamically-cheapest available route.
        match cheapest_available_model(provider) {
            Some((m, api)) => (m, Some(api)),
            None => {
                return Err(anyhow::anyhow!(
                    "no cheap route available for a 'cheapest' worker; refusing to fall back to the coordinator's model"
                ));
            }
        }
    } else if !route_already_pinned && !requested_model.eq_ignore_ascii_case(&provider_model) {
        // An EXPLICIT model that isn't the coordinator's own (e.g.
        // "deepseek/deepseek-chat"): resolve and PIN its route so the forked
        // coordinator provider actually switches backend.
        match resolve_model_route(provider, requested_model) {
            Some((m, api)) => (m, Some(api)),
            None => {
                return Err(anyhow::anyhow!(
                    "subagent model '{}' has no resolvable provider route; refusing to fall back to the coordinator's model",
                    requested_model
                ));
            }
        }
    } else {
        // Inherit: model is the coordinator's own and/or a route is already
        // pinned. Keep the existing route; the ban gate below still applies.
        (requested_model.to_string(), None)
    };

    // THE single backend gate. Whatever path resolved the model, it must not be
    // excluded by cheap_route_ban (e.g. Claude). Fail loudly instead of billing.
    if model_is_cheap_route_banned(&model) {
        return Err(anyhow::anyhow!(
            "worker model '{}' is excluded by cheap_route_ban; refusing to run on a banned (expensive) model",
            model
        ));
    }
    Ok((model, route_api))
}

/// The sentinel value users put in `agents.swarm_model` (or pass as a subagent
/// `model`) to mean "pick the cheapest available model dynamically".
pub const CHEAPEST_SENTINEL: &str = "cheapest";

/// Default tool set for a cheap subagent: core file/shell work plus web
/// search/fetch (so research subtasks work), but not the full ~31-tool registry
/// that bloats the prompt and stalls cheap models. Overridable via
/// `agents.cheap_route_tools`. Names are intersected with the live registry.
const CHEAP_SUBAGENT_TOOLS: &[&str] = &[
    "read",
    "write",
    "edit",
    "multiedit",
    "apply_patch",
    "bash",
    "grep",
    "glob",
    "ls",
    "websearch",
    "webfetch",
];

/// Resolve the cheap-subagent tool allowlist: the configured
/// `agents.cheap_route_tools` if non-empty, else [`CHEAP_SUBAGENT_TOOLS`].
fn cheap_subagent_tool_allowlist(
    registry_tools: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let configured = &crate::config::config().agents.cheap_route_tools;
    let wanted: Vec<String> = if configured.is_empty() {
        CHEAP_SUBAGENT_TOOLS.iter().map(|t| t.to_string()).collect()
    } else {
        configured.clone()
    };
    wanted
        .into_iter()
        .filter(|t| registry_tools.contains(t))
        .collect()
}

/// Hard cap on a single subtask's model call. A slow/hanging route (e.g. a
/// stalled provider) must not block the whole run — on timeout the candidate is
/// treated as failed and the next route is tried. A healthy cheap model answers
/// a text-only subtask in seconds, but subtasks that exercise TOOLS (reading
/// files, running scripts, writing outputs) legitimately take 1-2 minutes even
/// on fast models; the old 30s budget made every tool-using subtask "time out"
/// across all candidates and fail the whole run.
const SUBTASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Timeout for a HARD subtask (difficulty above the routing threshold) running
/// on a strong reasoning model. Frontier reasoning models (Fable, Kimi K3,
/// GPT-5.6 Sol) legitimately think for 1-3 minutes on difficulty-4/5 work;
/// killing them at the cheap 30s budget guaranteed the strong tier could never
/// answer and every hard subtask fell back to cheap models or failed.
const STRONG_SUBTASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(240);

/// Pick the per-attempt timeout for a subtask: hard subtasks get the strong
/// (reasoning) budget, trivial ones stay on the tight cheap budget.
fn subtask_attempt_timeout(difficulty: u8, threshold: u8) -> std::time::Duration {
    if difficulty > threshold {
        STRONG_SUBTASK_TIMEOUT
    } else {
        SUBTASK_TIMEOUT
    }
}
#[allow(dead_code)]
const DEBATE_PROPOSER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
#[allow(dead_code)]
const MAX_DEBATE_CANDIDATE_CHARS: usize = 3000;

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
    /// Provider used for coordination calls (decompose/review). It is a fork of
    /// `provider` switched to the cheapest available model, so coordination is
    /// fast and cheap regardless of how slow the session/chat model is.
    coordinator: Arc<dyn crate::provider::Provider>,
    registry: crate::tool::Registry,
    parent_system: String,
    /// Whether to run multi-model gold debates on hard reasoning subtasks.
    /// Computed from the per-session `/gold` flag AND the global config gate.
    gold_mode: bool,
    /// Number of distinct proposer models for a gold debate.
    gold_k: usize,
    /// Live reporter for debate progress (side panel + bus). Defaults to no-op.
    reporter: Arc<dyn crate::agent::debate_status::DebateStatusReporter>,
}

impl ProviderCheapBackend {
    pub fn new(
        provider: Arc<dyn crate::provider::Provider>,
        registry: crate::tool::Registry,
    ) -> Self {
        // Coordination (decompose/review) must not run on a slow session model
        // (e.g. qwen3.7-max, ~10-20s/call). Fork the provider and switch it to the
        // cheapest available route; complete_simple sends no tools so even a
        // slow-with-tools model opens fast for these small text calls.
        let coordinator = provider.fork();
        if let Some((model, route_api_method)) = cheapest_available_model(provider.as_ref()) {
            let request = crate::provider::MultiProvider::model_switch_request_for_session_route(
                &model,
                None,
                Some(&route_api_method),
            );
            let _ = crate::provider::set_model_with_auth_refresh(coordinator.as_ref(), &request);
        }
        Self {
            provider,
            coordinator,
            registry,
            parent_system:
                "You are a cost-routing coordinator. Decompose, recommend a model, and review \
                 subagent work. Be terse and precise; output exactly what is asked."
                    .to_string(),
            gold_mode: false,
            gold_k: 3,
            reporter: Arc::new(crate::agent::debate_status::NoopDebateReporter),
        }
    }

    /// Set the gold-mode flags on this backend. Call after `new` to wire in the
    /// per-session flag combined with the global config gate.
    pub fn with_gold(mut self, gold_mode: bool, gold_k: usize) -> Self {
        self.gold_mode = gold_mode;
        self.gold_k = gold_k;
        self
    }

    /// Attach a debate status reporter. Used by the production tool to surface
    /// live debate progress to the side panel.
    pub fn with_reporter(
        mut self,
        r: Arc<dyn crate::agent::debate_status::DebateStatusReporter>,
    ) -> Self {
        self.reporter = r;
        self
    }
}

#[async_trait]
impl CheapRouteBackend for ProviderCheapBackend {
    async fn ask_parent(&self, prompt: &str) -> Result<String> {
        // The coordinator is pinned to the cheapest route, which may be dead
        // (expired key, drained balance). A single failure used to abort the
        // whole cheap-route run before any subtask ran. Instead, cool the broken
        // route down and re-pin to the next-cheapest healthy route, so one bad
        // credential degrades to the next provider rather than to a hard error.
        let mut last_err: Option<anyhow::Error> = None;
        for _ in 0..COORDINATOR_ROUTE_ATTEMPTS {
            match self
                .coordinator
                .complete_simple(prompt, &self.parent_system)
                .await
            {
                Ok(reply) => return Ok(reply),
                Err(err) => {
                    let text = err.to_string();
                    let model = self.coordinator.model();
                    // Only route around failures that are inherent to this route
                    // (dead key / drained quota). Genuine prompt or network
                    // errors would fail identically everywhere, so surface them.
                    if !(is_dead_credential_error(&text) || is_rate_or_quota_error(&text)) {
                        return Err(err);
                    }
                    mark_route_unhealthy(&model);
                    // A dead credential takes down the whole account, so cool the
                    // provider too; otherwise the next-cheapest pick is just a
                    // sibling model behind the same broken key.
                    if is_dead_credential_error(&text)
                        && let Some(provider) = self
                            .routes()
                            .into_iter()
                            .find(|r| r.model == model)
                            .map(|r| r.provider)
                    {
                        mark_provider_unhealthy(&provider);
                    }
                    last_err = Some(err);
                    let Some((next_model, next_api)) =
                        cheapest_available_model(self.provider.as_ref())
                            .filter(|(next, _)| *next != model)
                    else {
                        break;
                    };
                    let request =
                        crate::provider::MultiProvider::model_switch_request_for_session_route(
                            &next_model,
                            None,
                            Some(&next_api),
                        );
                    if crate::provider::set_model_with_auth_refresh(
                        self.coordinator.as_ref(),
                        &request,
                    )
                    .is_err()
                    {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no usable coordinator route for cheap routing")))
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
        let allowed = cheap_subagent_tool_allowlist(&registry_tools);

        let mut agent = super::Agent::new_with_session(
            self.provider.fork(),
            self.registry.clone(),
            session,
            Some(allowed),
        );
        // Cheap workers may auto-switch to the next-cheapest healthy model if
        // their pinned model rate-limits/quota-fails mid-run, instead of failing.
        agent.set_allow_auto_reroute(true);
        // Stream the worker's live output tail directly into the side-panel
        // "Cheap Route" page, routed to this subtask's row, so the user can open
        // the panel and watch exactly what the cheap model is doing (streaming
        // text + tool markers) in real time. Cheap workers are not swarm members,
        // so we use the direct sink rather than the global-bus route.
        let reporter = self.reporter.clone();
        let index = subtask.index;
        agent.set_inline_tail_sink(std::sync::Arc::new(move |tail: &str| {
            reporter.subtask_live(index, tail);
        }));
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

    fn gold_mode(&self) -> bool {
        self.gold_mode
    }

    fn gold_k(&self) -> usize {
        self.gold_k
    }

    /// Run the strong model for one-shot text (used for debate aggregate).
    /// Uses `cheap_route_strong_model` from config if set and non-empty, else
    /// falls back to the parent's own current model.  Forks the provider to
    /// avoid mutating the coordinator/session model in-place.
    async fn ask_strong(&self, prompt: &str) -> Result<String> {
        let strong_model = crate::config::config()
            .agents
            .cheap_route_strong_model
            .clone()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| self.current_model());

        // Pin the exact route for the strong model (avoids re-resolving the
        // bare model name to the wrong provider).
        let routes = self.routes();
        let strong_api = routes
            .iter()
            .find(|r| r.model == strong_model)
            .map(|r| r.api_method.clone());

        let strong_provider = self.provider.fork();
        let request = crate::provider::MultiProvider::model_switch_request_for_session_route(
            &strong_model,
            None,
            strong_api.as_deref(),
        );
        let _ = crate::provider::set_model_with_auth_refresh(strong_provider.as_ref(), &request);
        strong_provider
            .complete_simple(prompt, &self.parent_system)
            .await
    }

    fn reporter(&self) -> &dyn crate::agent::debate_status::DebateStatusReporter {
        self.reporter.as_ref()
    }
}

/// If >=2 candidates agree after normalize (strip_code_fence + casefold + whitespace-collapse),
/// return one ORIGINAL agreeing candidate; else None.
#[allow(dead_code)]
fn consensus(candidates: &[String]) -> Option<String> {
    fn norm(s: &str) -> String {
        strip_code_fence(s)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
    }
    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            let ni = norm(&candidates[i]);
            if !ni.is_empty() && ni == norm(&candidates[j]) {
                return Some(candidates[i].clone());
            }
        }
    }
    None
}

/// Keep the last `max` chars (tail holds the conclusion), with a marker when truncated.
#[allow(dead_code)]
fn truncate_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let tail: String = {
        let v: Vec<char> = s.chars().collect();
        v[v.len() - max..].iter().collect()
    };
    format!("…(trimmed)\n{tail}")
}

/// Heuristic: a subtask is "code" if it edits/creates files or names code paths.
/// Debate is skipped for code (verify+repair is the stronger signal).
fn is_code_subtask(s: &Subtask) -> bool {
    let t = format!("{} {}", s.description, s.prompt).to_ascii_lowercase();
    const CODE_HINTS: &[&str] = &[
        "edit",
        "write",
        "modify",
        "implement",
        "refactor",
        "fix the",
        "function",
        "```",
        ".rs",
        ".ts",
        ".py",
        ".go",
        ".js",
        "src/",
        "compile",
        "cargo",
    ];
    CODE_HINTS.iter().any(|h| t.contains(h))
}

/// Deterministic gold entry point for `/gold <task>`: run a multi-model debate
/// DIRECTLY on `task` — no decomposition, no difficulty/code gate. Picks the K
/// cheapest DISTINCT proposer models, runs the debate (proposers → consensus or
/// one strong aggregate), and returns the synthesized answer.
///
/// Edge cases:
/// - fewer than 2 distinct proposers available → fall back to a single strong
///   answer (`ask_strong`) so the user still gets a result;
/// - the debate itself erroring → same single-strong fallback;
/// - both unavailable → propagate the error.
pub async fn run_gold_debate(backend: &dyn CheapRouteBackend, task: &str) -> Result<String> {
    let task = task.trim();
    if task.is_empty() {
        return Err(anyhow!("gold: task is empty"));
    }

    // Cheapest-first, DISTINCT models (debate value comes from diversity, so
    // never seed two proposers with the same model).
    let ranked = ranked_with_preferences(backend.routes());
    let mut seen = std::collections::HashSet::new();
    let proposers: Vec<(String, Option<String>)> = ranked
        .iter()
        .filter(|c| seen.insert(c.route.model.clone()))
        .map(|c| (c.route.model.clone(), Some(c.route.api_method.clone())))
        .take(backend.gold_k().max(2))
        .collect();

    let subtask = Subtask {
        description: task.to_string(),
        prompt: task.to_string(),
        difficulty: 5,
        index: 0,
    };

    if proposers.len() >= 2 {
        match run_debate(
            backend,
            &subtask,
            &proposers,
            backend.gold_k(),
            backend.reporter(),
        )
        .await
        {
            Ok(output) => return Ok(output),
            Err(e) => {
                crate::logging::warn(&format!(
                    "run_gold_debate: debate failed ({}); falling back to single strong answer",
                    e
                ));
            }
        }
    } else {
        crate::logging::warn(&format!(
            "run_gold_debate: only {} distinct proposer(s) available; using single strong answer",
            proposers.len()
        ));
    }

    backend.ask_strong(task).await
}

/// Run K proposer models concurrently (each under `DEBATE_PROPOSER_TIMEOUT`),
/// drop failures/timeouts, early-exit if 2+ agree (consensus), else make ONE
/// strong aggregate call. The strong model sees all candidates in input order.
async fn run_debate(
    backend: &dyn CheapRouteBackend,
    subtask: &Subtask,
    proposer_models: &[(String, Option<String>)],
    gold_k: usize,
    reporter: &dyn DebateStatusReporter,
) -> Result<String> {
    let k = gold_k.min(proposer_models.len());

    reporter.phase("propose");
    for (model, _) in &proposer_models[..k] {
        reporter.proposer(model, DebatePhase::Running);
    }

    let futs = proposer_models[..k].iter().map(|(model, api)| async move {
        tokio::time::timeout(
            DEBATE_PROPOSER_TIMEOUT,
            backend.run_subtask(subtask, model, api.as_deref()),
        )
        .await
    });
    let raw = join_all(futs).await; // Vec<Result<Result<String>, Elapsed>>, in input order

    // Emit Done/Failed per proposer, aligned by index.
    for ((model, _), result) in proposer_models[..k].iter().zip(raw.iter()) {
        let phase = if matches!(result, Ok(Ok(_))) {
            DebatePhase::Done
        } else {
            DebatePhase::Failed
        };
        reporter.proposer(model, phase);
    }

    let candidates: Vec<String> = raw
        .into_iter()
        .filter_map(|r| r.ok().and_then(|inner| inner.ok()))
        .collect();
    if candidates.len() < 2 {
        let result = backend
            .ask_strong(&build_debate_single_prompt(subtask))
            .await?;
        reporter.gold(&result);
        return Ok(result);
    }
    if let Some(agreed) = consensus(&candidates) {
        reporter.gold(&agreed);
        return Ok(agreed);
    }
    reporter.phase("merge");
    let trimmed: Vec<String> = candidates
        .iter()
        .map(|c| truncate_tail(strip_code_fence(c), MAX_DEBATE_CANDIDATE_CHARS))
        .collect();
    match backend
        .ask_strong(&build_debate_aggregate_prompt(subtask, &trimmed))
        .await
    {
        Ok(gold) => {
            reporter.gold(&gold);
            Ok(gold)
        }
        Err(_) => {
            reporter.gold(&candidates[0]);
            Ok(candidates[0].clone())
        }
    }
}

#[allow(dead_code)]
fn build_debate_single_prompt(s: &Subtask) -> String {
    format!(
        "Complete this task as best you can.\n\nTASK: {}\n",
        s.prompt
    )
}

#[allow(dead_code)]
fn build_debate_aggregate_prompt(s: &Subtask, candidates: &[String]) -> String {
    let mut p = format!(
        "You are the aggregator in a multi-model debate. {n} models each answered the task below.\n\
         First note errors/gaps across the candidates, then write the single BEST answer (do not mention the debate). \
         End with one line: 'why: <=1 sentence on what made it best'.\n\nTASK: {task}\n\n",
        n = candidates.len(),
        task = s.prompt
    );
    for (i, c) in candidates.iter().enumerate() {
        p.push_str(&format!("--- candidate {} ---\n{}\n\n", i + 1, c));
    }
    p
}

/// Format a one-line run summary for a completed gold debate.
/// Example: `"gold from 3 models in 42s · $0.02"`
pub fn debate_summary(models: usize, elapsed_secs: u64, usd: f64) -> String {
    format!(
        "gold from {} models in {}s · ${:.2}",
        models, elapsed_secs, usd
    )
}

#[cfg(test)]
#[path = "cheap_route_tests.rs"]
mod tests;

#[cfg(test)]
mod decompose_prompt_tests {
    use super::build_decompose_prompt;

    /// Each subtask prompt is sent verbatim to a worker in a FRESH session with
    /// no prior context (see `run_subtask`, which passes `&subtask.prompt`
    /// unchanged). A live run proved "independent" was too weak a word: the
    /// parent emitted chained subtasks referencing `{previous output}`, the
    /// placeholder was never substituted, and two of three workers replied by
    /// asking for the missing input instead of doing the work.
    #[test]
    fn decompose_prompt_forbids_cross_subtask_references() {
        let prompt = build_decompose_prompt("read a file and summarize it");

        assert!(
            prompt.contains("SELF-CONTAINED"),
            "must demand self-contained subtasks, not merely 'independent'"
        );
        assert!(
            prompt.contains("SEPARATE session"),
            "must state WHY: each subtask runs with no memory of the others"
        );
        assert!(
            prompt.contains("previous output"),
            "must name the exact placeholder pattern that broke a real run"
        );
        assert!(
            prompt.contains("merge them"),
            "must give the fix for genuinely dependent steps, not just a ban"
        );
        // The task itself must still reach the parent.
        assert!(prompt.contains("read a file and summarize it"));
    }
}
