//! More cheap-routing tests, continued from cheap_route_tests.rs.
//!
//! Split purely for the test-size ratchet: 69 tests in one file exceeded it.
//! Shares the same imports and fixtures via `use super::*`.

use super::*;

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
            Ok(self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default())
        }
        async fn run_subtask(
            &self,
            _s: &Subtask,
            _m: &str,
            route_api_method: Option<&str>,
        ) -> Result<String> {
            self.seen
                .lock()
                .unwrap()
                .push(route_api_method.map(str::to_string));
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

#[test]
fn is_code_subtask_detects_code() {
    let code = Subtask {
        description: "edit main.rs".into(),
        prompt: "modify src/main.rs".into(),
        difficulty: 4,
        index: 0,
    };
    let reason = Subtask {
        description: "design the api".into(),
        prompt: "what is the best architecture for X".into(),
        difficulty: 4,
        index: 0,
    };
    assert!(is_code_subtask(&code));
    assert!(!is_code_subtask(&reason));
}

#[test]
fn consensus_matches_on_fence_and_case() {
    let c = vec![
        "```\nFoo Bar\n```".to_string(),
        "foo bar".to_string(),
        "other".to_string(),
    ];
    assert_eq!(consensus(&c).as_deref(), Some("```\nFoo Bar\n```"));
}

#[test]
fn consensus_none_when_all_differ() {
    let c = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert!(consensus(&c).is_none());
}

#[test]
fn truncate_tail_keeps_tail() {
    let s = "x".repeat(5000);
    let t = truncate_tail(&s, 3000);
    assert!(t.chars().count() <= 3000 + 16);
    assert!(t.ends_with("xxxxxxxxxx"));
}

#[tokio::test]
async fn ask_strong_defaults_to_ask_parent_and_gold_off() {
    let b = FakeBackend {
        parent_responses: Mutex::new(VecDeque::from(vec!["PARENT".to_string()])),
        routes: vec![],
        subtask_calls: Mutex::new(Vec::new()),
    };
    assert_eq!(b.ask_strong("q").await.unwrap(), "PARENT");
    assert!(!b.gold_mode());
}

// --- DebateBackend: builder-style fake for run_debate tests ---

struct DebateBackend {
    subtask_replies: std::collections::HashMap<String, String>,
    subtask_errors: std::collections::HashMap<String, String>,
    subtask_delays: std::collections::HashMap<String, u64>,
    strong_reply: String,
    strong_error: bool,
    strong_prompts_log: Arc<Mutex<Vec<String>>>,
}

impl DebateBackend {
    fn new() -> Self {
        Self {
            subtask_replies: std::collections::HashMap::new(),
            subtask_errors: std::collections::HashMap::new(),
            subtask_delays: std::collections::HashMap::new(),
            strong_reply: String::new(),
            strong_error: false,
            strong_prompts_log: Arc::new(Mutex::new(Vec::new())),
        }
    }
    /// Script a per-model run_subtask reply (builder).
    fn subtask(mut self, model: &str, reply: &str) -> Self {
        self.subtask_replies
            .insert(model.to_string(), reply.to_string());
        self
    }
    /// Script a per-model run_subtask error (builder).
    fn subtask_error(mut self, model: &str, msg: &str) -> Self {
        self.subtask_errors
            .insert(model.to_string(), msg.to_string());
        self
    }
    /// Script a per-model run_subtask sleep delay in seconds (builder).
    fn subtask_delay(mut self, model: &str, secs: u64) -> Self {
        self.subtask_delays.insert(model.to_string(), secs);
        self
    }
    /// Script the ask_strong reply (builder).
    fn strong(mut self, reply: &str) -> Self {
        self.strong_reply = reply.to_string();
        self
    }
    /// Make ask_strong return an error (builder).
    fn strong_err(mut self) -> Self {
        self.strong_error = true;
        self
    }
    /// Return all prompts recorded by ask_strong calls so far.
    fn strong_prompts(&self) -> Vec<String> {
        self.strong_prompts_log.lock().unwrap().clone()
    }
}

#[async_trait]
impl CheapRouteBackend for DebateBackend {
    async fn ask_parent(&self, _prompt: &str) -> Result<String> {
        Ok(String::new())
    }
    async fn run_subtask(
        &self,
        _subtask: &Subtask,
        model: &str,
        _route_api_method: Option<&str>,
    ) -> Result<String> {
        // Apply delay first (simulates a slow model).
        if let Some(&secs) = self.subtask_delays.get(model) {
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
        // Then return a scripted error if one was registered.
        if let Some(msg) = self.subtask_errors.get(model) {
            return Err(anyhow!("{msg}"));
        }
        match self.subtask_replies.get(model) {
            Some(reply) => Ok(reply.clone()),
            None => Err(anyhow!("no scripted reply for model '{model}'")),
        }
    }
    fn routes(&self) -> Vec<ModelRoute> {
        vec![]
    }
    fn current_model(&self) -> String {
        String::new()
    }
    async fn ask_strong(&self, prompt: &str) -> Result<String> {
        self.strong_prompts_log
            .lock()
            .unwrap()
            .push(prompt.to_string());
        if self.strong_error {
            return Err(anyhow!("scripted strong error"));
        }
        Ok(self.strong_reply.clone())
    }
}

#[tokio::test]
async fn run_gold_debate_falls_back_to_strong_when_no_proposers() {
    // DebateBackend::routes() is empty → 0 distinct proposers (< 2) → the
    // deterministic gold path must still return a single strong answer.
    let b = DebateBackend::new().strong("the gold answer");
    let out = run_gold_debate(&b, "which approach is best?")
        .await
        .unwrap();
    assert_eq!(out, "the gold answer");
    let prompts = b.strong_prompts();
    assert_eq!(prompts.len(), 1, "exactly one strong call");
    assert!(
        prompts[0].contains("which approach is best?"),
        "strong call gets the raw task"
    );
}

#[tokio::test]
async fn run_gold_debate_empty_task_errors() {
    let b = DebateBackend::new().strong("x");
    assert!(run_gold_debate(&b, "   ").await.is_err());
}

#[tokio::test]
async fn run_gold_debate_strong_fallback_propagates_error() {
    // No proposers AND the strong model errors → propagate (no silent empty).
    let b = DebateBackend::new().strong_err();
    assert!(run_gold_debate(&b, "x?").await.is_err());
}

#[tokio::test]
async fn run_debate_aggregates_all_candidates_one_strong_call() {
    let b = DebateBackend::new()
        .subtask("m1", "alpha")
        .subtask("m2", "beta")
        .subtask("m3", "gamma")
        .strong("GOLD");
    let st = Subtask {
        description: "d".into(),
        prompt: "p".into(),
        difficulty: 5,
        index: 0,
    };
    let models = vec![
        ("m1".to_string(), None),
        ("m2".to_string(), None),
        ("m3".to_string(), None),
    ];
    let gold = run_debate(&b, &st, &models, 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(gold, "GOLD");
    let strong = b.strong_prompts();
    assert_eq!(strong.len(), 1); // ONE strong call
    let (ia, ib, ig) = (
        strong[0].find("alpha").unwrap(),
        strong[0].find("beta").unwrap(),
        strong[0].find("gamma").unwrap(),
    );
    assert!(ia < ib && ib < ig); // candidates in order
}

// --- helpers shared by run_debate exhaustive tests ---

fn debate_st() -> Subtask {
    Subtask {
        description: "d".into(),
        prompt: "p".into(),
        difficulty: 5,
        index: 0,
    }
}

fn models3() -> Vec<(String, Option<String>)> {
    vec![
        ("m1".into(), None),
        ("m2".into(), None),
        ("m3".into(), None),
    ]
}

// === Fallback / survivor ===

#[tokio::test]
async fn debate_one_survivor_falls_back_to_strong() {
    // m2 + m3 error → only m1 survives → len < 2 → ask_strong with single_prompt.
    let b = DebateBackend::new()
        .subtask("m1", "only")
        .subtask_error("m2", "boom")
        .subtask_error("m3", "boom")
        .strong("S");
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(result, "S");
    let prompts = b.strong_prompts();
    assert!(
        !prompts.is_empty(),
        "ask_strong must be called for single survivor"
    );
    assert!(
        !prompts[0].contains("--- candidate"),
        "single-survivor path uses single_prompt (no candidate blocks); prompt was:\n{}",
        &prompts[0][..prompts[0].len().min(300)]
    );
}

#[tokio::test]
async fn debate_exactly_two_runs_full() {
    // Exactly 2 distinct candidates → no consensus → exactly 1 strong call.
    let b = DebateBackend::new()
        .subtask("m1", "x")
        .subtask("m2", "y")
        .strong("GOLD");
    let models = vec![("m1".into(), None), ("m2".into(), None)];
    let result = run_debate(&b, &debate_st(), &models, 2, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(result, "GOLD");
    assert_eq!(
        b.strong_prompts().len(),
        1,
        "exactly one strong call for two distinct candidates"
    );
}

#[tokio::test]
async fn debate_proposer_error_dropped_others_aggregate() {
    // m1 errors; m2 + m3 survive → aggregate prompt must contain m2 and m3 replies.
    let b = DebateBackend::new()
        .subtask_error("m1", "fail")
        .subtask("m2", "y")
        .subtask("m3", "z")
        .strong("GOLD");
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(result, "GOLD");
    assert_eq!(b.strong_prompts().len(), 1);
    let prompt = &b.strong_prompts()[0];
    assert!(
        prompt.contains("y"),
        "m2 reply must appear in aggregate prompt"
    );
    assert!(
        prompt.contains("z"),
        "m3 reply must appear in aggregate prompt"
    );
}

#[tokio::test]
async fn debate_all_fail_falls_back_to_strong() {
    // Zero survivors → ask_strong with single_prompt (no candidate blocks).
    let b = DebateBackend::new()
        .subtask_error("m1", "boom")
        .subtask_error("m2", "boom")
        .subtask_error("m3", "boom")
        .strong("FALLBACK");
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(result, "FALLBACK");
    let prompts = b.strong_prompts();
    assert!(
        !prompts.is_empty(),
        "ask_strong must be called with zero survivors"
    );
    assert!(
        !prompts[0].contains("--- candidate"),
        "zero-survivor path uses single_prompt, not aggregate"
    );
}

#[tokio::test]
async fn debate_aggregate_error_returns_first_candidate() {
    // Distinct candidates but ask_strong errors → fallback to candidates[0].
    let b = DebateBackend::new()
        .subtask("m1", "x")
        .subtask("m2", "y")
        .subtask("m3", "z")
        .strong_err();
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(
        result, "x",
        "aggregate error must return candidates[0] ('x')"
    );
}

// === Consensus / truncation / single-call ===

#[tokio::test]
async fn debate_consensus_skips_strong() {
    // m1 "Same" and m2 "same" agree after normalization → consensus → no strong call.
    let b = DebateBackend::new()
        .subtask("m1", "Same")
        .subtask("m2", "same")
        .subtask("m3", "Other")
        .strong("SHOULD_NOT_BE_CALLED");
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(
        result.to_ascii_lowercase(),
        "same",
        "must return one of the agreeing originals; got: {result}"
    );
    assert!(
        b.strong_prompts().is_empty(),
        "consensus path must not call ask_strong"
    );
}

#[tokio::test]
async fn debate_no_consensus_one_strong() {
    // 3 distinct candidates → exactly 1 aggregate strong call.
    let b = DebateBackend::new()
        .subtask("m1", "alpha")
        .subtask("m2", "beta")
        .subtask("m3", "gamma")
        .strong("GOLD");
    run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(
        b.strong_prompts().len(),
        1,
        "exactly one strong call for 3 distinct candidates"
    );
}

#[tokio::test]
async fn debate_truncates_long_candidate() {
    // A 5000-char candidate exceeds MAX_DEBATE_CANDIDATE_CHARS (3000).
    // The aggregate prompt must show the trimmed marker, not the full string.
    let long = "A".repeat(5000);
    let b = DebateBackend::new()
        .subtask("m1", &long)
        .subtask("m2", "b")
        .subtask("m3", "c")
        .strong("GOLD");
    run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    let prompts = b.strong_prompts();
    assert!(!prompts.is_empty());
    assert!(
        prompts[0].contains("\u{2026}(trimmed)"),
        "truncated marker '\u{2026}(trimmed)' must appear in aggregate prompt"
    );
    assert!(
        !prompts[0].contains(&long),
        "full 5000-char string must not appear verbatim in the aggregate prompt"
    );
}

// === Concurrency / timeout ===

#[tokio::test(start_paused = true)]
async fn debate_runs_proposers_concurrently() {
    // Proposers have delays of 30s, 5s, 3s. Concurrent (join_all) completes in
    // max(30,5,3)=30s; sequential would take 38s. Assert < 40s to verify no
    // hang while documenting the concurrent-execution contract.
    let b = DebateBackend::new()
        .subtask("m1", "alpha")
        .subtask_delay("m1", 30)
        .subtask("m2", "beta")
        .subtask_delay("m2", 5)
        .subtask("m3", "gamma")
        .subtask_delay("m3", 3)
        .strong("GOLD");
    let t0 = tokio::time::Instant::now();
    run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(40),
        "proposers must run concurrently (max≈30s, sequential would be 38s); elapsed={elapsed:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn debate_drops_timed_out_proposer() {
    // m1 sleeps 90s — beyond DEBATE_PROPOSER_TIMEOUT (60s) — so it is dropped.
    // m2 and m3 complete instantly; the aggregate path is taken over y and z.
    // Total virtual time ≈ 60s (the timeout), not 90s.
    let b = DebateBackend::new()
        .subtask("m1", "x")
        .subtask_delay("m1", 90)
        .subtask("m2", "y")
        .subtask("m3", "z")
        .strong("GOLD");
    let t0 = tokio::time::Instant::now();
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(result, "GOLD");
    assert_eq!(
        b.strong_prompts().len(),
        1,
        "surviving m2+m3 → one aggregate strong call"
    );
    let prompt = &b.strong_prompts()[0];
    assert!(prompt.contains("y"), "m2 reply must appear in aggregate");
    assert!(prompt.contains("z"), "m3 reply must appear in aggregate");
    assert!(
        elapsed >= std::time::Duration::from_secs(60),
        "must wait for DEBATE_PROPOSER_TIMEOUT (60s); elapsed={elapsed:?}"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(85),
        "must not wait for full 90s m1 delay; elapsed={elapsed:?}"
    );
}

// === Edge cases ===

#[tokio::test]
async fn debate_unicode_candidate_preserved() {
    // A candidate with non-ASCII content must survive truncation and appear
    // intact in the aggregate prompt.
    let unicode = "caf\u{e9} \u{1f680} \u{65e5}\u{672c}"; // "café 🚀 日本"
    let b = DebateBackend::new()
        .subtask("m1", unicode)
        .subtask("m2", "other")
        .strong("GOLD");
    let models = vec![("m1".into(), None), ("m2".into(), None)];
    run_debate(&b, &debate_st(), &models, 2, &NoopDebateReporter)
        .await
        .unwrap();
    let prompts = b.strong_prompts();
    assert!(!prompts.is_empty());
    assert!(
        prompts[0].contains(unicode),
        "unicode content must be preserved intact in the aggregate prompt"
    );
}

#[tokio::test]
async fn debate_identical_candidates_take_consensus() {
    // All three give the exact same answer → consensus → no strong call.
    let b = DebateBackend::new()
        .subtask("m1", "Same")
        .subtask("m2", "Same")
        .subtask("m3", "Same")
        .strong("SHOULD_NOT_BE_CALLED");
    let result = run_debate(&b, &debate_st(), &models3(), 3, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(result, "Same");
    assert!(
        b.strong_prompts().is_empty(),
        "no strong call when all candidates are identical"
    );
}

#[tokio::test]
async fn debate_k_caps_to_available() {
    // gold_k=5 with only 2 models → k=min(5,2)=2; must not panic.
    // Two distinct candidates → 1 strong call.
    let b = DebateBackend::new()
        .subtask("m1", "x")
        .subtask("m2", "y")
        .strong("GOLD");
    let models = vec![("m1".into(), None), ("m2".into(), None)];
    let result = run_debate(&b, &debate_st(), &models, 5, &NoopDebateReporter)
        .await
        .unwrap();
    assert_eq!(result, "GOLD");
    assert_eq!(
        b.strong_prompts().len(),
        1,
        "2 distinct candidates → one strong call"
    );
}

// --- GoldFakeBackend: fake CheapRouteBackend with gold_mode=true for gate tests ---

struct GoldFakeBackend {
    parent_responses: Mutex<VecDeque<String>>,
    routes: Vec<ModelRoute>,
    strong_reply: String,
    strong_prompts_log: Arc<Mutex<Vec<String>>>,
}

impl GoldFakeBackend {
    fn strong_prompts(&self) -> Vec<String> {
        self.strong_prompts_log.lock().unwrap().clone()
    }
}

#[async_trait]
impl CheapRouteBackend for GoldFakeBackend {
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
        // Return a distinct answer per model so proposers disagree and the
        // aggregate ask_strong path (which contains "candidate 1") is exercised.
        Ok(format!("cheap answer from {model}"))
    }

    fn routes(&self) -> Vec<ModelRoute> {
        self.routes.clone()
    }

    fn current_model(&self) -> String {
        "current".to_string()
    }

    async fn ask_strong(&self, prompt: &str) -> Result<String> {
        self.strong_prompts_log
            .lock()
            .unwrap()
            .push(prompt.to_string());
        Ok(self.strong_reply.clone())
    }

    fn gold_mode(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn gate_debates_hard_reasoning_only() {
    // 3 subtasks: hard reasoning (diff 5, non-code), hard code (diff 5, has .rs/src/),
    // trivial (diff 1). Gold mode is ON. Expected: exactly ONE debate (reasoning only).
    let decompose = r#"[
        {"description":"design the core algorithm","prompt":"what is the best approach for X","difficulty":5},
        {"description":"edit src/x.rs","prompt":"modify the file","difficulty":5},
        {"description":"rename a var","prompt":"trivial","difficulty":1}
    ]"#;
    let b = GoldFakeBackend {
        parent_responses: Mutex::new(VecDeque::from(vec![
            decompose.to_string(),
            "use model-a".to_string(), // recommend
            "OK".to_string(),          // review code subtask
            "OK".to_string(),          // review trivial subtask
        ])),
        // Two distinct routes so proposers.len() >= 2 and debate can run.
        routes: vec![priced_route("model-a", 100), priced_route("model-b", 200)],
        strong_reply: "GOLD".to_string(),
        strong_prompts_log: Arc::new(Mutex::new(Vec::new())),
    };

    let out = run_cheap_route(&b, "task").await.unwrap();

    assert_eq!(out.results.len(), 3, "all 3 subtasks produced results");
    // The reasoning subtask was debated → model_used is "debate(2)".
    assert_eq!(
        out.results[0].model_used, "debate(2)",
        "reasoning subtask debated"
    );
    // Code subtask and trivial subtask were NOT debated.
    assert!(
        !out.results[1].model_used.starts_with("debate"),
        "code subtask must NOT be debated"
    );
    assert!(
        !out.results[2].model_used.starts_with("debate"),
        "trivial subtask must NOT be debated"
    );
    // Exactly ONE aggregate ask_strong call (for the reasoning debate).
    let strong = b.strong_prompts();
    assert_eq!(
        strong.iter().filter(|p| p.contains("candidate 1")).count(),
        1,
        "exactly one aggregate debate (hard reasoning subtask only)"
    );
}

// ── Circuit breaker tests ────────────────────────────────────────────

/// Backend that scripts per-model results for circuit-breaker testing.
/// Each model has a queue of `Ok(output)` / `Err(msg)`. Models in
/// `sleep_models` sleep past the subtask timeout (use `start_paused`).
struct BreakerScriptedBackend {
    parent_responses: Mutex<VecDeque<String>>,
    routes: Vec<ModelRoute>,
    subtask_queue: Mutex<std::collections::HashMap<String, VecDeque<Result<String, String>>>>,
    attempts: Mutex<Vec<(String, String)>>, // (model, subtask description)
    sleep_models: std::collections::HashSet<String>,
    current: String,
}

impl BreakerScriptedBackend {
    fn new(parent_responses: Vec<String>, routes: Vec<ModelRoute>, current: &str) -> Self {
        Self {
            parent_responses: Mutex::new(VecDeque::from(parent_responses)),
            routes,
            subtask_queue: Mutex::new(std::collections::HashMap::new()),
            attempts: Mutex::new(Vec::new()),
            sleep_models: std::collections::HashSet::new(),
            current: current.to_string(),
        }
    }

    /// Register a queue of `Ok(output)` / `Err(msg)` results for `model`.
    fn queue(self, model: &str, results: Vec<Result<String, String>>) -> Self {
        self.subtask_queue
            .lock()
            .unwrap()
            .insert(model.to_string(), VecDeque::from(results));
        self
    }

    /// Make `model` sleep past the subtask timeout (simulates hang).
    fn hang(mut self, model: &str) -> Self {
        self.sleep_models.insert(model.to_string());
        self
    }
}

#[async_trait]
impl CheapRouteBackend for BreakerScriptedBackend {
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
        self.attempts
            .lock()
            .unwrap()
            .push((model.to_string(), subtask.description.clone()));

        if self.sleep_models.contains(model) {
            // Sleep past the subtask timeout to trigger tokio::time::timeout.
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
            return Ok("too-late".to_string());
        }

        let mut queue = self.subtask_queue.lock().unwrap();
        match queue.get_mut(model).and_then(|q| q.pop_front()) {
            Some(Ok(output)) => Ok(output),
            Some(Err(msg)) => Err(anyhow!("{msg}")),
            None => Err(anyhow!("no scripted result for model '{model}'")),
        }
    }

    fn routes(&self) -> Vec<ModelRoute> {
        self.routes.clone()
    }

    fn current_model(&self) -> String {
        self.current.clone()
    }
}

fn breaker_test_routes() -> Vec<ModelRoute> {
    vec![priced_route("route-a", 100), priced_route("route-b", 200)]
}

#[tokio::test]
async fn breaker_skips_after_config_error() {
    let _temp = isolate_config();
    // Two subtasks. route-a fails on subtask 1 with a non-retryable config
    // error → breaker trips route-a for the rest of the run.
    // route-b always works.
    let backend = BreakerScriptedBackend::new(
        vec![
            // decompose: two subtasks
            r#"[
                {"description":"task1","prompt":"p","difficulty":1},
                {"description":"task2","prompt":"p","difficulty":1}
            ]"#
            .to_string(),
            "use route-a".to_string(), // recommend
            "OK".to_string(),          // review subtask 1
            "OK".to_string(),          // review subtask 2
        ],
        breaker_test_routes(),
        "",
    )
    .queue(
        "route-a",
        vec![Err("status: 400 invalid_request".to_string())],
    )
    .queue(
        "route-b",
        vec![Ok("done-b".to_string()), Ok("done-b2".to_string())],
    );

    let outcome = run_cheap_route(&backend, "task").await.unwrap();

    assert_eq!(outcome.results.len(), 2);
    // Subtask 1: route-a errored (config), route-b succeeded.
    assert_eq!(outcome.results[0].model_used, "route-b");
    // Subtask 2: route-a was SKIPPED by breaker, route-b succeeded.
    assert_eq!(outcome.results[1].model_used, "route-b");

    let attempts = backend.attempts.lock().unwrap();
    // route-a tried once (subtask 1), then skipped on subtask 2.
    // route-b tried on both subtasks.
    assert_eq!(
        *attempts,
        vec![
            ("route-a".to_string(), "task1".to_string()),
            ("route-b".to_string(), "task1".to_string()),
            ("route-b".to_string(), "task2".to_string()),
        ],
        "route-a must be skipped on subtask 2 after config error"
    );
}

#[tokio::test(start_paused = true)]
async fn breaker_skips_after_two_timeouts() {
    // Three subtasks. route-a times out on subtask 1 (1st timeout — not
    // tripped), times out again on subtask 2 (2nd timeout — tripped), then
    // is skipped on subtask 3.  route-b always works.
    let decompose = r#"[
        {"description":"task1","prompt":"p","difficulty":1},
        {"description":"task2","prompt":"p","difficulty":1},
        {"description":"task3","prompt":"p","difficulty":1}
    ]"#;
    let backend = BreakerScriptedBackend::new(
        vec![
            decompose.to_string(),
            "use route-a".to_string(),
            "OK".to_string(),
            "OK".to_string(),
            "OK".to_string(),
        ],
        breaker_test_routes(),
        "",
    )
    .hang("route-a") // every call to route-a hangs → timeout
    .queue(
        "route-b",
        vec![
            Ok("done-b1".to_string()),
            Ok("done-b2".to_string()),
            Ok("done-b3".to_string()),
        ],
    );

    let outcome = run_cheap_route(&backend, "task").await.unwrap();

    assert_eq!(outcome.results.len(), 3);
    // All 3 subtasks ultimately succeeded via route-b.
    assert!(outcome.results.iter().all(|r| r.model_used == "route-b"));

    let attempts = backend.attempts.lock().unwrap();
    // route-a tried on subtask 1 and 2 (two timeouts), skipped on subtask 3.
    // route-b tried on all three subtasks.
    assert_eq!(
        *attempts,
        vec![
            ("route-a".to_string(), "task1".to_string()),
            ("route-b".to_string(), "task1".to_string()),
            ("route-a".to_string(), "task2".to_string()),
            ("route-b".to_string(), "task2".to_string()),
            // route-a SKIPPED on task3
            ("route-b".to_string(), "task3".to_string()),
        ],
        "route-a must be skipped on subtask 3 after 2 timeouts"
    );
}

/// Three routes: two fail with config errors on subtask 1, both tripped for
/// subtask 2.  The third route survives.  Verifies the breaker carries over
/// across subtasks but does not empty the candidate list.
#[tokio::test]
async fn breaker_carries_over_and_partial_filter() {
    let _temp = isolate_config();
    let routes = vec![
        priced_route("route-a", 100),
        priced_route("route-b", 200),
        priced_route("route-c", 300),
    ];
    let backend = BreakerScriptedBackend::new(
        vec![
            r#"[
                {"description":"task1","prompt":"p","difficulty":1},
                {"description":"task2","prompt":"p","difficulty":1}
            ]"#
            .to_string(),
            "use route-a".to_string(),
            "OK".to_string(),
            "OK".to_string(),
        ],
        routes,
        "",
    )
    .queue(
        "route-a",
        vec![
            Err("status: 400 invalid_request".to_string()),
            Err("status: 400 invalid_request".to_string()),
        ],
    )
    .queue(
        "route-b",
        vec![
            Err("status: 403 unauthorized".to_string()),
            Err("status: 403 unauthorized".to_string()),
        ],
    )
    .queue(
        "route-c",
        vec![Ok("done-c1".to_string()), Ok("done-c2".to_string())],
    );

    let outcome = run_cheap_route(&backend, "task").await.unwrap();

    assert_eq!(outcome.results.len(), 2);
    assert_eq!(outcome.results[0].model_used, "route-c");
    assert_eq!(outcome.results[1].model_used, "route-c");

    let attempts = backend.attempts.lock().unwrap();
    // Subtask 1: route-a (config err), route-b (config err), route-c (OK).
    // Subtask 2: route-a SKIPPED, route-b SKIPPED, route-c (OK).
    assert_eq!(
        *attempts,
        vec![
            ("route-a".to_string(), "task1".to_string()),
            ("route-b".to_string(), "task1".to_string()),
            ("route-c".to_string(), "task1".to_string()),
            ("route-c".to_string(), "task2".to_string()),
        ],
        "route-a and route-b tripped on subtask 1, skipped on subtask 2; route-c survives"
    );
}

// ── RouteBreaker unit tests ──────────────────────────────────────────

#[test]
fn route_breaker_config_error_trips_immediately() {
    let mut b = RouteBreaker::new();
    assert!(!b.is_tripped("m"));
    let tripped = b.record_failure("m", BreakerFailureKind::ConfigError);
    assert!(tripped, "first config error must trip the breaker");
    assert!(b.is_tripped("m"));
}

#[test]
fn route_breaker_timeout_trips_after_two() {
    let mut b = RouteBreaker::new();
    // First timeout: not yet tripped.
    assert!(!b.record_failure("m", BreakerFailureKind::Timeout));
    assert!(!b.is_tripped("m"));
    // Second timeout: tripped.
    assert!(b.record_failure("m", BreakerFailureKind::Timeout));
    assert!(b.is_tripped("m"));
}

#[test]
fn route_breaker_filter_never_empty() {
    let mut b = RouteBreaker::new();
    b.record_failure("a", BreakerFailureKind::ConfigError);
    b.record_failure("b", BreakerFailureKind::ConfigError);

    let candidates = vec![("a".to_string(), None), ("b".to_string(), None)];
    let filtered = b.filter_candidates(&candidates);
    // Both tripped → fallback returns full list.
    assert_eq!(filtered, candidates);
}

#[test]
fn route_breaker_filter_removes_tripped() {
    let mut b = RouteBreaker::new();
    b.record_failure("a", BreakerFailureKind::ConfigError);
    // b is not tripped.

    let candidates = vec![("a".to_string(), None), ("b".to_string(), None)];
    let filtered = b.filter_candidates(&candidates);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].0, "b");
}

#[test]
fn route_breaker_independent_routes() {
    let mut b = RouteBreaker::new();
    b.record_failure("a", BreakerFailureKind::ConfigError);
    // b has one timeout (not tripped yet).
    b.record_failure("b", BreakerFailureKind::Timeout);

    assert!(b.is_tripped("a"));
    assert!(!b.is_tripped("b"));
    assert!(!b.is_tripped("c")); // never seen
}

#[test]
fn classify_failure_detects_config_errors() {
    use anyhow::anyhow;

    assert_eq!(
        classify_failure(&anyhow!("status: 400 invalid_request")),
        BreakerFailureKind::ConfigError
    );
    assert_eq!(
        classify_failure(&anyhow!("product not activated")),
        BreakerFailureKind::ConfigError
    );
    assert_eq!(
        classify_failure(&anyhow!("status: 401 unauthorized")),
        BreakerFailureKind::ConfigError
    );
    assert_eq!(
        classify_failure(&anyhow!("status: 403 access denied")),
        BreakerFailureKind::ConfigError
    );
    assert_eq!(
        classify_failure(&anyhow!("model not found")),
        BreakerFailureKind::ConfigError
    );
}

#[test]
fn classify_failure_defaults_to_timeout() {
    use anyhow::anyhow;

    assert_eq!(
        classify_failure(&anyhow!("network error: connection refused")),
        BreakerFailureKind::Timeout
    );
    assert_eq!(
        classify_failure(&anyhow!("status: 429 rate limited")),
        BreakerFailureKind::Timeout
    );
    assert_eq!(
        classify_failure(&anyhow!("status: 500 internal server error")),
        BreakerFailureKind::Timeout
    );
}
