//! Tests for the cheap-routing orchestrator.
//!
//! Split out of cheap_route.rs, which is well over the code-size ratchet;
//! the test module alone was ~2200 lines.

use super::*;
use crate::agent::debate_status::NoopDebateReporter;
use jcode_provider_core::{RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource};
use std::collections::VecDeque;
use std::sync::Mutex;

/// Isolate test from user's ~/.jcode/config.toml by setting JCODE_HOME to a temp dir.
/// Returns the temp dir (must be kept alive for the test duration).
/// Guard bundling the temp dir with the process-wide env lock, so the
/// isolation cannot outlive the lock that makes it safe.
struct IsolatedConfig {
    _env: std::sync::MutexGuard<'static, ()>,
    temp: tempfile::TempDir,
}
impl IsolatedConfig {
    fn path(&self) -> &std::path::Path {
        self.temp.path()
    }
}

fn isolate_config() -> IsolatedConfig {
    // JCODE_HOME is process-global, so isolating config without holding the
    // env lock is a race, not isolation: a parallel test overwrites the
    // variable mid-run. That race made a strict-mode test observe
    // `cheap_route_strict = false` and watch a subtask execute on the
    // coordinator's model — the exact escalation the test forbids. Taking
    // the lock here rather than at each call site means no test can forget.
    let _env = jcode_base::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("failed to create temp dir");
    unsafe { std::env::set_var("JCODE_HOME", temp.path()) };
    jcode_base::config::invalidate_config_cache();
    IsolatedConfig { _env, temp }
}

#[test]
fn debate_summary_formats() {
    assert_eq!(
        debate_summary(3, 42, 0.0234),
        "gold from 3 models in 42s · $0.02"
    );
}

/// A dead credential kills every model on that account, so cooling only the
/// failing model made cheap routing walk sibling models behind the same
/// broken key (observed: OpenRouter 401 hopping free model to free model).
#[test]
fn dead_credential_cools_the_whole_provider_not_just_one_model() {
    let provider = "test-dead-provider";
    assert!(provider_is_healthy(provider));
    mark_provider_unhealthy(provider);
    assert!(!provider_is_healthy(provider));

    // Ranking must skip every route on a cooled provider while it still has
    // a healthy alternative to offer.
    let mut dead_a = priced_route("dead-a", 1);
    dead_a.provider = provider.to_string();
    let mut dead_b = priced_route("dead-b", 2);
    dead_b.provider = provider.to_string();
    let alive = priced_route("alive-model", 9_000);
    let ranked = ranked_with_preferences(vec![dead_a, dead_b, alive]);
    let models: Vec<_> = ranked.iter().map(|c| c.route.model.clone()).collect();
    assert_eq!(
        models,
        vec!["alive-model".to_string()],
        "all routes on a cooled provider must be skipped, not just the one that failed"
    );
}

/// The "everything is cooled, retry anyway" fallback must not resurrect a
/// provider cooled for a DEAD CREDENTIAL. It did, so the dead provider was
/// re-picked on every retry and cheap routing never reached a working one.
#[test]
fn credential_cooled_provider_is_not_resurrected_by_the_all_cooled_fallback() {
    let dead_provider = "test-fallback-dead-provider";
    mark_provider_unhealthy(dead_provider);

    let mut dead = priced_route("fallback-dead-model", 1);
    dead.provider = dead_provider.to_string();
    let alive = priced_route("fallback-alive-model", 9_000);
    // Cool the healthy provider's MODEL too, which triggers the
    // "no healthy routes" fallback path.
    mark_route_unhealthy("fallback-alive-model");

    let ranked = ranked_with_preferences(vec![dead, alive]);
    let models: Vec<_> = ranked.iter().map(|c| c.route.model.clone()).collect();
    assert_eq!(
        models,
        vec!["fallback-alive-model".to_string()],
        "the fallback may retry a rate-cooled model, but must never retry a dead credential"
    );
}

/// When every cheap provider has a dead credential, ranking must return
/// EMPTY so the caller falls through to the parent's known-working model.
/// Returning the dead list instead guaranteed a 401 on every attempt.
#[test]
fn all_providers_credential_cooled_yields_no_routes_instead_of_dead_ones() {
    let dead_provider = "test-total-blackout-provider";
    mark_provider_unhealthy(dead_provider);
    let mut a = priced_route("blackout-a", 1);
    a.provider = dead_provider.to_string();
    let mut b = priced_route("blackout-b", 2);
    b.provider = dead_provider.to_string();
    assert!(
        ranked_with_preferences(vec![a, b]).is_empty(),
        "a total credential blackout must yield no routes so the caller uses its own model"
    );
}

/// Provider names arrive with mixed casing (`OpenRouter` display name vs
/// `openrouter` id), so cooldowns must match case-insensitively. They did
/// not, which silently disabled provider-level cooldown entirely.
#[test]
fn provider_cooldown_matches_case_insensitively() {
    mark_provider_unhealthy("TestMixedCaseProvider");
    assert!(!provider_is_healthy("testmixedcaseprovider"));
    assert!(!provider_is_healthy("TESTMIXEDCASEPROVIDER"));
    assert!(!provider_is_healthy("  TestMixedCaseProvider  "));
}

/// A 401 from a permanently-dead credential must cool the route down. It
/// previously did not, so a deleted OpenRouter account kept its route at the
/// top of the cheapest-first ranking and broke every cheap-route run.
#[test]
fn dead_credential_errors_are_detected_and_cool_routes_down() {
    for msg in [
        "OpenAI-compatible chat request failed status: 401 Unauthorized",
        "Incorrect API key provided, code invalid_api_key",
        "{\"error\":{\"message\":\"User not found.\",\"code\":401}}",
    ] {
        assert!(is_dead_credential_error(msg), "should be dead cred: {msg}");
    }
    // A genuine prompt/validation error must NOT be treated as a dead
    // credential, otherwise a bad request would cool down healthy routes.
    assert!(!is_dead_credential_error(
        "status: 400 invalid_request_error: message too long"
    ));

    let model = "test-dead-credential-route";
    assert!(route_is_healthy(model));
    note_provider_error(model, "status: 401 invalid_api_key");
    assert!(
        !route_is_healthy(model),
        "a dead credential must cool the route down so cheap routing skips it"
    );
}

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
fn parse_subtasks_strips_think_tags_and_extracts_array() {
    // Thinking models prepend reasoning; the JSON follows </think>.
    let text = "<think>\nlet me plan this out...\n[not json here]\n</think>\n\n[\n  {\"description\": \"read file\", \"prompt\": \"read it\", \"difficulty\": 1}\n]";
    let subtasks = parse_subtasks(text).unwrap();
    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0].description, "read file");

    // Prose around a bare array still parses via the span fallback.
    let text = "Here are the subtasks:\n[\n  {\"description\": \"a\", \"prompt\": \"b\", \"difficulty\": 2}\n]\nDone!";
    let subtasks = parse_subtasks(text).unwrap();
    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0].difficulty, 2);
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
        vec![
            priced_route("cheapo", 100_000),
            priced_route("pricey", 9_000_000),
        ],
        MAX_MENU,
    );
    // cheapest first
    assert_eq!(menu[0].route.model, "cheapo");
    // explicit mention wins
    assert_eq!(
        parse_recommended_model("use pricey please", &menu).unwrap(),
        "pricey"
    );
    // unparseable -> cheapest fallback
    assert_eq!(
        parse_recommended_model("hmm not sure", &menu).unwrap(),
        "cheapo"
    );
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

/// L4 RUNTIME proof: a total cheap-route blackout under strict mode must
/// FAIL, not escalate.
///
/// This drives the real `run_cheap_route` entry point rather than the pure
/// helpers, because the helpers cannot catch the failure mode that actually
/// shipped: a gate that is correct but never reached. `resolve_worker_route`
/// existed with ZERO callers and enforced nothing.
///
/// Regression guard for 70035ce0b, which made an empty ranking mean "use the
/// parent model", so a drained balance silently billed the frontier model.
#[tokio::test]
async fn blackout_under_strict_fails_instead_of_escalating() {
    let temp = isolate_config();
    std::fs::write(
        temp.path().join("config.toml"),
        "[agents]\ncheap_route_strict = true\n",
    )
    .expect("write strict config");
    jcode_base::config::invalidate_config_cache();

    // The blackout condition in production: every cheap provider is
    // credential-cooled or banned so ranking yields nothing, but a real
    // coordinator model IS available. That is precisely the state
    // 70035ce0b escalated into silently.
    struct BlackoutBackend {
        parent: Mutex<VecDeque<String>>,
        ran: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl CheapRouteBackend for BlackoutBackend {
        async fn ask_parent(&self, _p: &str) -> Result<String> {
            Ok(self.parent.lock().unwrap().pop_front().unwrap_or_default())
        }
        async fn run_subtask(&self, st: &Subtask, model: &str, _r: Option<&str>) -> Result<String> {
            self.ran.lock().unwrap().push(model.to_string());
            Ok(format!("done: {}", st.description))
        }
        fn routes(&self) -> Vec<ModelRoute> {
            Vec::new()
        }
        fn current_model(&self) -> String {
            // An EXPENSIVE coordinator, so escalation would be billable.
            "claude-opus-4-8".to_string()
        }
    }
    let backend = BlackoutBackend {
        parent: Mutex::new(VecDeque::from(vec![
            r#"[{"description":"do it","prompt":"do it","difficulty":1}]"#.to_string(),
        ])),
        ran: Mutex::new(Vec::new()),
    };

    let err = run_cheap_route(&backend, "任务")
        .await
        .expect_err("strict mode must refuse to escalate on a blackout");
    let msg = err.to_string();
    assert!(
        msg.contains("cheap-route blackout"),
        "must fail with the blackout error, got: {msg}"
    );
    assert!(
        msg.contains("cheap_route_strict"),
        "error must name the setting so the user can opt back in: {msg}"
    );

    // The decisive assertion: NO subtask ran. Escalation would have executed
    // the subtask on the coordinator's model and billed it.
    assert!(
        backend.ran.lock().unwrap().is_empty(),
        "no work may run when strict mode refuses to escalate; ran on: {:?}",
        backend.ran.lock().unwrap()
    );
}

#[tokio::test]
async fn run_cheap_route_decomposes_recommends_spawns_and_reviews() {
    let _temp = isolate_config();
    let decompose = r#"[
        {"description":"edit auth","prompt":"edit it","difficulty":2},
        {"description":"write tests","prompt":"test it","difficulty":3}
    ]"#;
    let backend = FakeBackend {
        parent_responses: Mutex::new(VecDeque::from(vec![
            decompose.to_string(),    // decompose
            "use cheapo".to_string(), // recommend
            "OK".to_string(),         // review subtask 1
            "OK".to_string(),         // review subtask 2
        ])),
        routes: vec![
            priced_route("cheapo", 100_000),
            priced_route("pricey", 9_000_000),
        ],
        subtask_calls: Mutex::new(Vec::new()),
    };

    let outcome = run_cheap_route(&backend, "refactor auth + tests")
        .await
        .unwrap();

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

/// Backend for testing execution-grounded verify+repair: scripted
/// `verify_edits` outcomes and a `run_subtask` call counter.
struct VerifyBackend {
    verify_results: Mutex<VecDeque<(bool, String)>>,
    subtask_outputs: Mutex<VecDeque<String>>,
    subtask_calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl CheapRouteBackend for VerifyBackend {
    async fn ask_parent(&self, _prompt: &str) -> Result<String> {
        Ok("OK".to_string())
    }
    async fn run_subtask(
        &self,
        _subtask: &Subtask,
        _model: &str,
        _route_api_method: Option<&str>,
    ) -> Result<String> {
        self.subtask_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self
            .subtask_outputs
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| "repaired".to_string()))
    }
    fn routes(&self) -> Vec<ModelRoute> {
        vec![]
    }
    fn current_model(&self) -> String {
        "parent".to_string()
    }
    async fn verify_edits(&self, _command: &str) -> Result<(bool, String)> {
        Ok(self
            .verify_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or((true, String::new())))
    }
}

fn verify_subtask() -> Subtask {
    Subtask {
        description: "t".to_string(),
        prompt: "do it".to_string(),
        difficulty: 1,
        index: 0,
    }
}

#[tokio::test]
async fn verify_passes_means_no_repair() {
    let backend = VerifyBackend {
        verify_results: Mutex::new(VecDeque::from(vec![(true, String::new())])),
        subtask_outputs: Mutex::new(VecDeque::new()),
        subtask_calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let (out, note) = verify_and_maybe_repair(
        &backend,
        &verify_subtask(),
        "m",
        None,
        "orig".to_string(),
        "cargo check",
    )
    .await;
    assert_eq!(out, "orig");
    assert!(note.contains("passed"), "note was: {note}");
    assert_eq!(
        backend
            .subtask_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no repair attempt when verify passes"
    );
}

#[tokio::test]
async fn verify_fails_then_repair_passes() {
    let backend = VerifyBackend {
        verify_results: Mutex::new(VecDeque::from(vec![
            (false, "error[E0308] mismatched types".to_string()),
            (true, String::new()),
        ])),
        subtask_outputs: Mutex::new(VecDeque::from(vec!["repaired-output".to_string()])),
        subtask_calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let (out, note) = verify_and_maybe_repair(
        &backend,
        &verify_subtask(),
        "m",
        None,
        "orig".to_string(),
        "cargo check",
    )
    .await;
    assert_eq!(out, "repaired-output", "output replaced by repaired result");
    assert!(note.contains("repaired, now passes"), "note was: {note}");
    assert_eq!(
        backend
            .subtask_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one repair attempt"
    );
}

#[tokio::test]
async fn verify_fails_and_repair_still_fails() {
    let backend = VerifyBackend {
        verify_results: Mutex::new(VecDeque::from(vec![
            (false, "err".to_string()),
            (false, "still err".to_string()),
        ])),
        subtask_outputs: Mutex::new(VecDeque::from(vec!["repaired".to_string()])),
        subtask_calls: std::sync::atomic::AtomicUsize::new(0),
    };
    let (out, note) = verify_and_maybe_repair(
        &backend,
        &verify_subtask(),
        "m",
        None,
        "orig".to_string(),
        "cargo check",
    )
    .await;
    assert_eq!(out, "repaired");
    assert!(note.contains("still failing"), "note was: {note}");
    assert_eq!(
        backend
            .subtask_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn run_verify_command_captures_exit_and_output() {
    let (passed, out) = run_verify_command("echo hello && exit 0").await.unwrap();
    assert!(passed);
    assert!(out.contains("hello"));
    let (failed, _) = run_verify_command("echo boom 1>&2; exit 7").await.unwrap();
    assert!(!failed, "non-zero exit must report not-passed");
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
        Ok(self
            .parent_responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default())
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
    let _temp = isolate_config();
    // Menu: cheapo (cheapest, DEAD) + pricey (works). Recommend -> cheapo.
    let backend = FallbackBackend {
        parent_responses: Mutex::new(VecDeque::from(vec![
            r#"[{"description":"do x","prompt":"p","difficulty":1}]"#.to_string(), // decompose
            "use cheapo".to_string(), // recommend the dead one
            "OK".to_string(),         // review
        ])),
        routes: vec![
            priced_route("cheapo", 100_000),
            priced_route("pricey", 9_000_000),
        ],
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
        routes: vec![
            priced_route("cheapo", 100_000),
            priced_route("pricey", 9_000_000),
        ],
        dead_models: ["cheapo".to_string(), "pricey".to_string()]
            .into_iter()
            .collect(),
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
        &["deepseek-v4-flash".to_string()], // static (config) ids
        &["qwen-x".to_string(), "deepseek-v4-flash".to_string()], // discovered (cache) ids
        true,                               // key present -> available
        |_source, _model| None,             // pricing lookup stub
    );

    let models: std::collections::BTreeSet<&str> =
        routes.iter().map(|r| r.model.as_str()).collect();
    // union, deduped
    assert!(models.contains("deepseek-v4-flash"));
    assert!(models.contains("qwen-x"));
    assert_eq!(routes.len(), 2);
    // all carry the named-provider api_method + availability + base url detail
    assert!(
        routes
            .iter()
            .all(|r| r.api_method == "openai-compatible:modelscope")
    );
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

    assert!(absolute_env_file_has_key(
        Some("DEEPSEEK_API_KEY"),
        Some(abs)
    ));
    assert!(!absolute_env_file_has_key(Some("MISSING_KEY"), Some(abs)));
    // relative path is not handled here (config-dir helper covers those)
    assert!(!absolute_env_file_has_key(
        Some("DEEPSEEK_API_KEY"),
        Some("rel.env")
    ));
    // missing args
    assert!(!absolute_env_file_has_key(None, Some(abs)));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn run_cheap_route_rescues_via_current_model_when_all_ranked_dead() {
    // The last-resort rescue is gated on `agents.cheap_route_ban`, which is
    // read from config, so without isolation this test consults the developer's
    // real ~/.jcode/config.toml. A ban list containing the fake current model's
    // family (or simply `strict = true`) suppresses the rescue and the test
    // fails locally while passing in CI.
    let _config = isolate_config();

    // Every ranked route is dead (mirrors the real case: all 6 cheapest are
    // one exhausted key). The parent's own current model still works.
    let backend = FallbackBackend {
        parent_responses: Mutex::new(VecDeque::from(vec![
            r#"[{"description":"do x","prompt":"p","difficulty":1}]"#.to_string(),
            "use cheapo".to_string(),
            "OK".to_string(),
        ])),
        routes: vec![
            priced_route("cheapo", 100_000),
            priced_route("pricey", 9_000_000),
        ],
        dead_models: ["cheapo".to_string(), "pricey".to_string()]
            .into_iter()
            .collect(),
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
        vec![
            "cheapo".to_string(),
            "pricey".to_string(),
            "qwen-live".to_string()
        ]
    );
}

#[tokio::test]
async fn uses_cooled_cheap_route_not_parent_when_all_cooled() {
    // SAFETY (Claude-burn fix): when EVERY cheap route is cooled, run on the
    // cheapest cooled CHEAP route — do NOT escalate to the parent/coordinator
    // model, which can be expensive and ban-exempt. Retrying a cooled cheap
    // route is always safer than burning an expensive coordinator.
    // Unique route names so the process-global health map stays isolated.
    mark_route_unhealthy("cooled-route-x");
    mark_route_unhealthy("cooled-route-y");
    let backend = FallbackBackend {
        parent_responses: Mutex::new(VecDeque::from(vec![
            r#"[{"description":"do x","prompt":"p","difficulty":1}]"#.to_string(),
            "OK".to_string(), // review
        ])),
        routes: vec![
            priced_route("cooled-route-x", 100),
            priced_route("cooled-route-y", 200),
        ],
        dead_models: std::collections::HashSet::new(),
        attempts: Mutex::new(Vec::new()),
        current: "parent-live".to_string(),
    };

    let outcome = run_cheap_route(&backend, "task").await.unwrap();

    // Ran on the cheapest cooled CHEAP route, NOT the parent model.
    assert_eq!(outcome.results[0].model_used, "cooled-route-x");
    let attempts = backend.attempts.lock().unwrap();
    assert_eq!(attempts[0], "cooled-route-x");
    assert!(
        !attempts.contains(&"parent-live".to_string()),
        "must never escalate to the parent/coordinator when cheap routes exist"
    );
}

#[tokio::test]
async fn difficulty_routes_hard_subtask_to_strong_model() {
    let _temp = isolate_config();
    // Default threshold is 3: difficulty<=3 -> cheapest, >3 -> strong model
    // (here the parent's current model, since cheap_route_strong_model unset).
    let backend = FallbackBackend {
        parent_responses: Mutex::new(VecDeque::from(vec![
            r#"[{"description":"easy","prompt":"p","difficulty":2},{"description":"hard","prompt":"p","difficulty":5}]"#.to_string(),
            "cheapo".to_string(), // recommend (cheapest)
            "OK".to_string(),     // review easy
            "OK".to_string(),     // review hard
        ])),
        routes: vec![priced_route("cheapo", 100), priced_route("pricey", 9_000_000)],
        dead_models: std::collections::HashSet::new(),
        attempts: Mutex::new(Vec::new()),
        current: "strong-main".to_string(),
    };

    let outcome = run_cheap_route(&backend, "task").await.unwrap();

    assert_eq!(outcome.results.len(), 2);
    let attempts = backend.attempts.lock().unwrap();
    // Easy (diff 2) ran on the cheapest model; hard (diff 5) ran on the
    // strong/current model first — the expensive model only touched the hard
    // subtask.
    assert_eq!(
        *attempts,
        vec!["cheapo".to_string(), "strong-main".to_string()]
    );
}

#[tokio::test]
async fn run_cheap_route_tries_one_model_per_provider() {
    let _temp = isolate_config();
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
    let _temp = isolate_config();
    let provider: std::sync::Arc<dyn crate::provider::Provider> = std::sync::Arc::new(ParentMock {
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
    let _temp = isolate_config();
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
fn unhealthy_route_is_skipped_then_recovers() {
    let model = "zzz-health-test-model";
    assert!(
        route_is_healthy(model),
        "unknown route is healthy by default"
    );
    mark_route_unhealthy(model);
    assert!(!route_is_healthy(model), "cooled-down route is unhealthy");
    // Simulate an expired cooldown (until in the past) -> healthy again.
    if let Ok(mut h) = cheap_route_health().lock() {
        h.insert(model.to_string(), 1);
    }
    assert!(route_is_healthy(model), "expired cooldown recovers");
}

#[test]
fn note_provider_error_cools_only_quota_rate_errors() {
    let model = "zzz-quota-test-model";
    // Clear any prior state for determinism.
    if let Ok(mut h) = cheap_route_health().lock() {
        h.remove(model);
    }
    note_provider_error(model, "some transient network blip");
    assert!(
        route_is_healthy(model),
        "non-quota errors must not cool a route"
    );
    note_provider_error(
        model,
        "OpenAI-compatible chat request failed status: 402 Payment Required",
    );
    assert!(
        !route_is_healthy(model),
        "402 Payment Required must cool the route"
    );
}

#[test]
fn ranked_with_preferences_filters_unhealthy() {
    let model = "zzz-ranked-health-model";
    if let Ok(mut h) = cheap_route_health().lock() {
        h.remove(model);
    }
    let routes = vec![priced_route(model, 100), priced_route("other-cheap", 200)];
    let before = ranked_with_preferences(routes.clone());
    assert!(
        before.iter().any(|c| c.route.model == model),
        "healthy route present"
    );
    mark_route_unhealthy(model);
    let after = ranked_with_preferences(routes);
    assert!(
        !after.iter().any(|c| c.route.model == model),
        "unhealthy route filtered out of ranked candidates"
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
fn price_hint_converts_usd_per_mtok_to_micros() {
    let est = cheapness_from_price_hint(0.14, 0.28);
    assert_eq!(est.input_price_per_mtok_micros, Some(140_000));
    assert_eq!(est.output_price_per_mtok_micros, Some(280_000));
    assert_eq!(est.source, RouteCostSource::PublicApiPricing);
    assert!(
        est.estimated_reference_cost_micros.is_some(),
        "must have a reference cost so it sorts"
    );
    // Negative is clamped to 0 (a free/garbage value can't underflow).
    assert_eq!(
        cheapness_from_price_hint(-5.0, 0.0).input_price_per_mtok_micros,
        Some(0)
    );
}

#[test]
fn price_hinted_route_ranks_before_unpriced() {
    let hinted = ModelRoute {
        model: "modelscope-cheap".to_string(),
        provider: "modelscope".to_string(),
        api_method: "openai-compatible:modelscope".to_string(),
        available: true,
        detail: String::new(),
        cheapness: Some(cheapness_from_price_hint(0.1, 0.2)),
    };
    let unpriced = ModelRoute {
        model: "mystery-model".to_string(),
        provider: "other".to_string(),
        api_method: "a".to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
    };
    let ranked = rank_routes_by_cost(vec![unpriced, hinted]);
    assert_eq!(
        ranked[0].route.model, "modelscope-cheap",
        "a config-priced route must rank ahead of an unpriced one"
    );
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
    assert!(route_matches_preference(
        &route,
        "prov-deepseek-chat/deepseek-chat"
    ));
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
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default())
        }
        async fn run_subtask(&self, _s: &Subtask, model: &str, _r: Option<&str>) -> Result<String> {
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

#[cfg(test)]
#[path = "cheap_route_more_tests.rs"]
mod more;
