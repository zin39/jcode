// Routing tests for profile-less `openai-compatible:<model>` route specs.
//
// Split out of model_resolution.rs, which is already over the oversized-test
// budget.

/// Regression for the live failure: the model picker emitted
/// `api_method=openai-compatible` (no profile id), which
/// `RouteSelection::routed_model_spec` turned into
/// `openai-compatible:qwen3.7-max`.
///
/// The catalog has a catch-all profile whose id is literally
/// "openai-compatible" pointed at api.openai.com, so the pin parser claimed the
/// transport segment as that profile. Unconfigured, it errored and the caller
/// fell onward to the active provider, producing "Model qwen3.7-max not
/// supported by Anthropic provider" while Claude was active. Configured, it
/// would have been billed to api.openai.com for a model DashScope serves.
///
/// The switch must never end up on Anthropic, and the error must name the
/// unresolved route rather than a provider the user never selected.
#[test]
fn transport_only_openai_compatible_spec_never_falls_through_to_the_active_provider() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();
        let provider = MultiProvider {
            claude: RwLock::new(None),
            anthropic: RwLock::new(None),
            openai: RwLock::new(None),
            copilot_api: RwLock::new(None),
            antigravity: RwLock::new(None),
            gemini: RwLock::new(None),
            cursor: RwLock::new(None),
            bedrock: RwLock::new(None),
            openrouter: RwLock::new(None),
            openai_compatible_profiles: RwLock::new(std::collections::HashMap::new()),
            active_openai_compatible_profile: RwLock::new(None),
            // The failing case: Claude active when the switch arrives.
            active: RwLock::new(ActiveProvider::Claude),
            use_claude_cli: false,
            startup_notices: RwLock::new(Vec::new()),
            initial_provider: None,
            routes_memo: std::sync::Mutex::new(None),
            post_auth_refreshes_pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };

        // No provider serving this model is configured here, so the switch must
        // fail; what matters is HOW it fails.
        let err = provider
            .set_model("openai-compatible:qwen3.7-max")
            .expect_err("no configured provider serves this model in the test env");
        let message = err.to_string();

        assert!(
            !message.contains("Anthropic"),
            "must not blame the provider the user never selected: {message}"
        );
        assert!(
            message.contains("qwen3.7-max"),
            "error should name the unresolved model: {message}"
        );
        assert!(
            message.contains("OpenAI-compatible"),
            "error should name the route that could not be resolved: {message}"
        );
        assert_eq!(
            provider.active_provider(),
            ActiveProvider::Claude,
            "a failed switch must not silently move the active provider"
        );
    });
}

/// The resolver must handle profiles defined in config (`[providers.<name>]`),
/// not just built-in catalog profiles.
///
/// This is the user's real setup: `dashscope` is a named config profile, so its
/// routes carry `api_method = "openai-compatible:dashscope"` but
/// `openai_compatible_profile_by_id("dashscope")` finds nothing in the static
/// catalog. A catalog-only lookup would resolve the route id and then drop it,
/// sending the switch back to the active provider.
#[test]
fn transport_only_spec_resolves_a_named_config_profile() {
    with_clean_provider_test_env(|| {
        let rt = enter_test_runtime();
        let _runtime_guard = rt.enter();

        let owning = model_pin::openai_compatible_profile_id_owning_model(
            "qwen3.7-max",
            None,
            || {
                vec![ModelRoute {
                    model: "qwen3.7-max".to_string(),
                    provider: "DashScope".to_string(),
                    api_method: "openai-compatible:dashscope".to_string(),
                    available: true,
                    detail: String::new(),
                    cheapness: None,
                }]
            },
        );

        assert_eq!(
            owning.as_deref(),
            Some("dashscope"),
            "a route whose api_method names a config-defined profile must resolve; \
             a catalog-only lookup dropped it and the switch fell back to the active provider"
        );

        // And the resolver must hand it to the named-profile binding path,
        // since `dashscope` has no catalog entry.
        let resolved = model_pin::resolve_openai_compatible_target(
            "openai-compatible:qwen3.7-max",
            |_| Some("dashscope".to_string()),
        )
        .expect("resolution should succeed")
        .expect("spec names an OpenAI-compatible target");
        assert!(
            matches!(resolved.0, model_pin::OpenAiCompatibleTarget::Named(ref n) if n == "dashscope"),
            "a config-defined profile must route to the named-profile path"
        );
        assert_eq!(resolved.1, "qwen3.7-max");
    });
}
