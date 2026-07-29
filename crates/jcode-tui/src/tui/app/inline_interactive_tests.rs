//! Tests for the inline interactive (model/account) picker.
//!
//! Split out of `inline_interactive.rs` to keep that file within the
//! code-size budget. The picker owns route grouping, ordering, filtering
//! and remote-catalog caching, so its cases outgrew the module they were
//! inlined in.

use super::{
    REMOTE_MODEL_CATALOG_CACHE_MAX_AGE_SECS, REMOTE_MODEL_CATALOG_CACHE_VERSION,
    REMOTE_MODEL_CATALOG_MAX_DETAIL_BYTES, RemoteModelCatalogCache,
    filter_routes_by_provider_allowlist, key_char_eq_ignore_ascii_case,
    model_picker_route_is_current, model_picker_route_is_default,
    model_picker_route_is_recommended, picker_is_runtime_model_picker,
    remote_model_catalog_cache_build, remote_model_catalog_cache_is_fresh,
    remote_model_catalog_cache_origin, remote_model_catalog_snapshot_is_safe, route_sort_key,
    route_supports_reasoning_effort,
};
use crate::tui::{
    AgentModelTarget, App, InlineInteractiveState, PickerAction, PickerEntry, PickerKind,
    PickerOption,
};
use crossterm::event::KeyCode;

fn picker_entry(name: &str, provider: &str, usage_score: u32) -> PickerEntry {
    PickerEntry {
        name: name.to_string(),
        options: vec![picker_option(provider)],
        action: PickerAction::Model,
        selected_option: 0,
        is_current: false,
        is_default: false,
        is_favorite: false,
        recommended: false,
        recommendation_rank: usize::MAX,
        usage_score,
        old: false,
        created_date: None,
        effort: None,
        available_efforts: Vec::new(),
        provider_group: None,
        is_recent: false,
    }
}

fn picker_option_with_method(provider: &str, api_method: &str) -> PickerOption {
    PickerOption {
        provider: provider.to_string(),
        api_method: api_method.to_string(),
        available: true,
        detail: String::new(),
        estimated_reference_cost_micros: None,
    }
}

fn picker_option(provider: &str) -> PickerOption {
    picker_option_with_method(provider, "test")
}

#[test]
fn model_picker_hotkey_char_matching_is_case_insensitive() {
    assert!(key_char_eq_ignore_ascii_case(KeyCode::Char('f'), 'f'));
    assert!(key_char_eq_ignore_ascii_case(KeyCode::Char('F'), 'f'));
    assert!(key_char_eq_ignore_ascii_case(KeyCode::Char('D'), 'd'));
    assert!(!key_char_eq_ignore_ascii_case(KeyCode::Char('x'), 'f'));
}

#[test]
fn runtime_model_picker_scope_excludes_agent_model_picker() {
    let runtime = InlineInteractiveState {
        kind: PickerKind::Model,
        filtered: vec![0],
        entries: vec![picker_entry("gpt-5.5", "OpenAI", 0)],
        selected: 0,
        column: 0,
        filter: String::new(),
        preview: false,
        display_rows: vec![crate::tui::PickerDisplayRow::Entry { entry_index: 0 }],
        collapse_state: crate::tui::CollapseState::default(),
    };
    let mut agent_entry = picker_entry("Swarm / subagent", "gpt-5 default", 0);
    agent_entry.action = PickerAction::AgentTarget(AgentModelTarget::Swarm);
    let agent = InlineInteractiveState {
        kind: PickerKind::Model,
        filtered: vec![0],
        entries: vec![agent_entry],
        selected: 0,
        column: 0,
        filter: String::new(),
        preview: false,
        display_rows: vec![crate::tui::PickerDisplayRow::Entry { entry_index: 0 }],
        collapse_state: crate::tui::CollapseState::default(),
    };

    assert!(picker_is_runtime_model_picker(&runtime));
    assert!(!picker_is_runtime_model_picker(&agent));
}

#[test]
fn model_picker_fuzzy_filter_prefers_previously_selected_route() {
    let mut picker = InlineInteractiveState {
        kind: PickerKind::Model,
        filtered: vec![0, 1],
        entries: vec![
            picker_entry("claude-opus-4.6", "Cursor", 0),
            picker_entry("claude-opus-4.5", "Anthropic", 150),
        ],
        selected: 0,
        column: 0,
        filter: "opus".to_string(),
        preview: false,
        display_rows: Vec::new(),
        collapse_state: crate::tui::CollapseState::default(),
    };

    App::apply_inline_interactive_filter(&mut picker);

    assert_eq!(picker.filtered, vec![1, 0]);
}

#[test]
fn model_picker_fuzzy_filter_tolerates_common_typos() {
    let mut picker = InlineInteractiveState {
        kind: PickerKind::Model,
        filtered: vec![0, 1],
        entries: vec![
            picker_entry("gpt-5-codex", "OpenAI", 0),
            picker_entry("claude-opus-4.6", "Anthropic", 0),
        ],
        selected: 0,
        column: 0,
        filter: "codxe".to_string(),
        preview: false,
        display_rows: Vec::new(),
        collapse_state: crate::tui::CollapseState::default(),
    };

    App::apply_inline_interactive_filter(&mut picker);

    assert_eq!(picker.filtered, vec![0]);
}

#[test]
fn model_picker_exact_name_outranks_longer_frequently_used_prefix() {
    let mut picker = InlineInteractiveState {
        kind: PickerKind::Model,
        filtered: vec![0, 1],
        entries: vec![
            picker_entry("gpt-5.5", "OpenAI", 150),
            picker_entry("gpt-5", "OpenAI", 0),
        ],
        selected: 0,
        column: 0,
        filter: "gpt-5".to_string(),
        preview: false,
        display_rows: Vec::new(),
        collapse_state: crate::tui::CollapseState::default(),
    };

    App::apply_inline_interactive_filter(&mut picker);

    assert_eq!(picker.filtered, vec![1, 0]);
}

#[test]
fn model_picker_current_route_requires_matching_provider() {
    let openai_route = picker_option("OpenAI");
    let copilot_route = picker_option("Copilot");

    assert!(model_picker_route_is_current(
        "gpt-5.5",
        &openai_route,
        "gpt-5.5",
        "OpenAI",
        None,
    ));
    assert!(!model_picker_route_is_current(
        "gpt-5.5",
        &copilot_route,
        "gpt-5.5",
        "OpenAI",
        None,
    ));
}

/// One provider, two credential paths: the picker must preselect the one the
/// session is actually on.
///
/// OpenAI serves gpt-5.5 over both ChatGPT OAuth and a metered API key, and
/// both routes carry the label "OpenAI". Matching on the label alone made
/// every route "current", so `position()` returned index 0 and the picker
/// preselected whichever the catalog emitted first. When that was the API
/// key, a paid subscription sat unused while requests billed per token.
#[test]
fn model_picker_current_route_distinguishes_auth_methods_of_one_provider() {
    let oauth = picker_option_with_method("OpenAI", "openai-oauth");
    let api_key = picker_option_with_method("OpenAI", "openai-api-key");

    // Session recorded as running on OAuth: only the OAuth route is current.
    assert!(model_picker_route_is_current(
        "gpt-5.5",
        &oauth,
        "gpt-5.5",
        "OpenAI",
        Some("openai-oauth"),
    ));
    assert!(!model_picker_route_is_current(
        "gpt-5.5",
        &api_key,
        "gpt-5.5",
        "OpenAI",
        Some("openai-oauth"),
    ));

    // And the reverse, so the fix is not just "always prefer oauth".
    assert!(model_picker_route_is_current(
        "gpt-5.5",
        &api_key,
        "gpt-5.5",
        "OpenAI",
        Some("openai-api-key"),
    ));
    assert!(!model_picker_route_is_current(
        "gpt-5.5",
        &oauth,
        "gpt-5.5",
        "OpenAI",
        Some("openai-api-key"),
    ));

    // Sessions predating `route_api_method` keep the old label-only behaviour
    // rather than losing their selection entirely.
    assert!(model_picker_route_is_current(
        "gpt-5.5", &oauth, "gpt-5.5", "OpenAI", None,
    ));
    assert!(model_picker_route_is_current(
        "gpt-5.5",
        &api_key,
        "gpt-5.5",
        "OpenAI",
        Some("   "),
    ));
}

#[test]
fn model_picker_current_route_allows_provider_aliases() {
    assert!(jcode_provider_core::model_route_provider_labels_match(
        "Anthropic",
        "Claude"
    ));
    assert!(jcode_provider_core::model_route_provider_labels_match(
        "auto",
        "OpenRouter"
    ));
    assert!(jcode_provider_core::model_route_provider_labels_match(
        "GitHub Copilot",
        "Copilot"
    ));
    assert!(jcode_provider_core::model_route_provider_labels_match(
        "AWS Bedrock",
        "Bedrock"
    ));
}

#[test]
fn model_picker_provider_match_does_not_use_substring_false_positives() {
    assert!(!jcode_provider_core::model_route_provider_labels_match(
        "OpenRouter/OpenAI",
        "OpenAI"
    ));
    assert!(!jcode_provider_core::model_route_provider_labels_match(
        "OpenAI",
        "OpenRouter"
    ));
}

#[test]
fn model_picker_default_route_requires_matching_provider_when_config_has_provider() {
    let openai_route = picker_option_with_method("OpenAI", "openai-oauth");
    let copilot_route = picker_option_with_method("Copilot", "copilot");

    assert!(model_picker_route_is_default(
        "gpt-5.5",
        &openai_route,
        Some("gpt-5.5"),
        Some("openai"),
    ));
    assert!(!model_picker_route_is_default(
        "gpt-5.5",
        &copilot_route,
        Some("gpt-5.5"),
        Some("openai"),
    ));
}

#[test]
fn model_picker_default_route_marks_anthropic_api_config_provider() {
    // Regression: config `default_provider = "anthropic-api"` is the
    // dual-auth spelling of the route keyed `anthropic-api-key`. The picker
    // must still mark the Anthropic API-key route as the default ★ even
    // though the two spellings normalize differently, and must NOT mark the
    // OAuth route for the same model.
    let api_route = picker_option_with_method("Anthropic", "anthropic-api-key");
    let oauth_route = picker_option_with_method("Anthropic", "claude-oauth");

    assert!(model_picker_route_is_default(
        "claude-opus-4-8",
        &api_route,
        Some("claude-opus-4-8"),
        Some("anthropic-api"),
    ));
    assert!(!model_picker_route_is_default(
        "claude-opus-4-8",
        &oauth_route,
        Some("claude-opus-4-8"),
        Some("anthropic-api"),
    ));

    // The equivalent `claude-api` spelling behaves identically.
    assert!(model_picker_route_is_default(
        "claude-opus-4-8",
        &api_route,
        Some("claude-opus-4-8"),
        Some("claude-api"),
    ));
}

#[test]
fn model_picker_default_route_honors_provider_prefixed_model_specs() {
    let openai_route = picker_option_with_method("OpenAI", "openai-oauth");
    let copilot_route = picker_option_with_method("Copilot", "copilot");

    assert!(model_picker_route_is_default(
        "gpt-5.5",
        &copilot_route,
        Some("copilot:gpt-5.5"),
        None,
    ));
    assert!(!model_picker_route_is_default(
        "gpt-5.5",
        &openai_route,
        Some("copilot:gpt-5.5"),
        None,
    ));
}

#[test]
fn model_picker_default_route_matches_openrouter_endpoint_specs() {
    let openrouter_openai_route = picker_option_with_method("OpenAI", "openrouter");

    assert!(model_picker_route_is_default(
        "gpt-5.5",
        &openrouter_openai_route,
        Some("openai/gpt-5.5@OpenAI"),
        Some("openrouter"),
    ));
    assert!(!model_picker_route_is_default(
        "gpt-5.5",
        &openrouter_openai_route,
        Some("anthropic/gpt-5.5@OpenAI"),
        Some("openrouter"),
    ));
}

#[test]
fn model_picker_recommended_route_is_provider_aware() {
    let openai_oauth_route = picker_option_with_method("OpenAI", "openai-oauth");
    let openai_api_key_route = picker_option_with_method("OpenAI", "openai-api-key");
    let copilot_route = picker_option_with_method("Copilot", "copilot");
    let claude_oauth_route = picker_option_with_method("Anthropic", "claude-oauth");
    let claude_openrouter_route = picker_option_with_method("Anthropic", "openrouter");
    let openrouter_auto_route = picker_option_with_method("auto", "openrouter");
    let openrouter_provider_route = picker_option_with_method("DeepSeek", "openrouter");
    let deepseek_direct_route = picker_option_with_method("DeepSeek", "openai-compatible:deepseek");
    let unavailable_openai_oauth_route = PickerOption {
        available: false,
        ..openai_oauth_route.clone()
    };

    assert!(model_picker_route_is_recommended(
        "gpt-5.5",
        &openai_oauth_route
    ));
    assert!(!model_picker_route_is_recommended(
        "gpt-5.5",
        &openai_api_key_route
    ));
    assert!(!model_picker_route_is_recommended(
        "gpt-5.5",
        &copilot_route
    ));
    assert!(!model_picker_route_is_recommended(
        "gpt-5.5",
        &unavailable_openai_oauth_route,
    ));

    // Current policy (see jcode-provider-core): claude-opus-4-8 is the
    // recommended Anthropic flagship; older Opus and OpenRouter/Copilot
    // routes are not recommended.
    assert!(model_picker_route_is_recommended(
        "claude-opus-4-8",
        &claude_oauth_route,
    ));
    assert!(!model_picker_route_is_recommended(
        "claude-opus-4-7",
        &claude_oauth_route,
    ));
    assert!(!model_picker_route_is_recommended(
        "claude-opus-4-8",
        &claude_openrouter_route,
    ));
    assert!(!model_picker_route_is_recommended(
        "claude-opus-4-8",
        &copilot_route,
    ));

    // DeepSeek routes are no longer in the recommended set at all.
    assert!(!model_picker_route_is_recommended(
        "deepseek/deepseek-v4-pro",
        &openrouter_auto_route,
    ));
    assert!(!model_picker_route_is_recommended(
        "deepseek/deepseek-v4-pro",
        &deepseek_direct_route,
    ));
    assert!(!model_picker_route_is_recommended(
        "deepseek/deepseek-v4-pro",
        &openrouter_provider_route,
    ));
}

#[test]
fn remote_model_catalog_cache_keeps_flattened_legacy_schema() {
    let cache: RemoteModelCatalogCache = serde_json::from_value(serde_json::json!({
        "version": 1,
        "provider_name": "OpenAI",
        "provider_model": "gpt-5.5",
        "available_models": ["gpt-5.5"],
        "model_routes": [{
            "model": "gpt-5.5",
            "provider": "OpenAI",
            "api_method": "openai-oauth",
            "available": true,
            "detail": "OAuth"
        }],
        "observed_at_unix_secs": 123,
    }))
    .expect("legacy flattened remote cache should deserialize");

    assert_eq!(cache.snapshot.provider_name.as_deref(), Some("OpenAI"));
    assert_eq!(cache.snapshot.provider_model.as_deref(), Some("gpt-5.5"));
    assert_eq!(cache.snapshot.available_models, ["gpt-5.5"]);
    assert_eq!(cache.snapshot.model_routes.len(), 1);
    assert!(cache.origin.is_empty());
    assert_eq!(
        cache.snapshot.model_routes[0].api_method_kind(),
        crate::provider::ModelRouteApiMethod::OpenAIOAuth
    );

    let serialized = serde_json::to_value(&cache).expect("cache should serialize");
    assert_eq!(serialized["provider_name"], "OpenAI");
    assert!(serialized.get("snapshot").is_none());
}

#[test]
fn remote_model_catalog_cache_rejects_stale_and_future_timestamps() {
    let snapshot = jcode_provider_core::ModelCatalogSnapshot::new(
        Some("OpenAI".to_string()),
        Some("gpt-5.5".to_string()),
        vec!["gpt-5.5".to_string()],
        vec![model_route("gpt-5.5", "OpenAI", "openai-oauth")],
    );
    let now = REMOTE_MODEL_CATALOG_CACHE_MAX_AGE_SECS + 10_000;
    let mut cache = RemoteModelCatalogCache {
        version: REMOTE_MODEL_CATALOG_CACHE_VERSION,
        origin: remote_model_catalog_cache_origin(),
        build: remote_model_catalog_cache_build(),
        snapshot,
        observed_at_unix_secs: now,
    };

    assert!(remote_model_catalog_cache_is_fresh(&cache, now));

    // A snapshot written by a different build must be rejected however recent
    // it is. The route set is derived from the server's code, so an older
    // build's cache describes an older route vocabulary. This is exactly how a
    // cache written 77 seconds before a fix kept serving the pre-fix routes:
    // `/model` showed only "api key" for a working OAuth account, and the 24h
    // age window meant it would have kept doing so all day.
    let good_build = cache.build.clone();
    cache.build = "v0.0.0-previous (deadbee)".to_string();
    assert!(
        !remote_model_catalog_cache_is_fresh(&cache, now),
        "a cache from another build must not be served, even when brand new"
    );
    cache.build = good_build;
    assert!(remote_model_catalog_cache_is_fresh(&cache, now));

    // Caches written before the field existed deserialize with an empty build
    // and must also be rejected rather than trusted by default.
    cache.build = String::new();
    assert!(
        !remote_model_catalog_cache_is_fresh(&cache, now),
        "a pre-upgrade cache with no build stamp must be refetched"
    );
    cache.build = remote_model_catalog_cache_build();

    cache.observed_at_unix_secs = now - REMOTE_MODEL_CATALOG_CACHE_MAX_AGE_SECS - 1;
    assert!(!remote_model_catalog_cache_is_fresh(&cache, now));
    cache.observed_at_unix_secs = now + 5 * 60 + 1;
    assert!(!remote_model_catalog_cache_is_fresh(&cache, now));
}

#[test]
fn remote_model_catalog_cache_rejects_forged_or_oversized_routes() {
    let safe_snapshot = jcode_provider_core::ModelCatalogSnapshot::new(
        Some("AWS Bedrock".to_string()),
        Some("us.anthropic.claude-sonnet-4-6".to_string()),
        vec!["us.anthropic.claude-sonnet-4-6".to_string()],
        vec![model_route(
            "us.anthropic.claude-sonnet-4-6",
            "AWS Bedrock",
            "bedrock",
        )],
    );
    assert!(remote_model_catalog_snapshot_is_safe(&safe_snapshot));

    let mut forged = safe_snapshot.clone();
    forged.model_routes[0].api_method = "shell:steal-credentials".to_string();
    assert!(!remote_model_catalog_snapshot_is_safe(&forged));

    let mut control = safe_snapshot.clone();
    control.model_routes[0].provider = "AWS Bedrock\nOpenAI".to_string();
    assert!(!remote_model_catalog_snapshot_is_safe(&control));

    let mut oversized = safe_snapshot;
    oversized.model_routes[0].detail = "x".repeat(REMOTE_MODEL_CATALOG_MAX_DETAIL_BYTES + 1);
    assert!(!remote_model_catalog_snapshot_is_safe(&oversized));
}

fn model_route(model: &str, provider: &str, api_method: &str) -> crate::provider::ModelRoute {
    crate::provider::ModelRoute {
        model: model.to_string(),
        provider: provider.to_string(),
        api_method: api_method.to_string(),
        available: true,
        detail: String::new(),
        cheapness: None,
    }
}

#[test]
fn route_effort_support_covers_effort_capable_runtimes_only() {
    assert!(route_supports_reasoning_effort("claude-oauth"));
    assert!(route_supports_reasoning_effort("claude-api"));
    assert!(route_supports_reasoning_effort("openai-oauth"));
    assert!(route_supports_reasoning_effort("openai-api-key"));
    assert!(route_supports_reasoning_effort("openrouter"));

    assert!(!route_supports_reasoning_effort("copilot"));
    assert!(!route_supports_reasoning_effort("bedrock"));
    assert!(!route_supports_reasoning_effort("https"));
    assert!(!route_supports_reasoning_effort(
        "openai-compatible:llamacpp"
    ));
    assert!(!route_supports_reasoning_effort("remote-catalog"));
    assert!(!route_supports_reasoning_effort("current"));
}

#[test]
fn provider_allowlist_filters_routes_by_label_method_and_profile() {
    let routes = vec![
        model_route("gpt-5.5", "OpenAI", "openai-oauth"),
        model_route("claude-fable-5", "Anthropic", "claude-oauth"),
        model_route("qwen3-coder", "llama.cpp", "openai-compatible:llamacpp"),
        model_route("deepseek/deepseek-v4-pro", "auto", "openrouter"),
    ];

    // Provider label match (normalized: case/dots/spaces insensitive).
    let filtered = filter_routes_by_provider_allowlist(
        routes.clone(),
        Some(&["Llama.CPP".to_string()]),
        "unrelated-current",
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].model, "qwen3-coder");

    // Bare openai-compatible profile id match.
    let filtered = filter_routes_by_provider_allowlist(
        routes.clone(),
        Some(&["llamacpp".to_string()]),
        "unrelated-current",
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].provider, "llama.cpp");

    // Api-method match plus alias-aware provider label match.
    let filtered = filter_routes_by_provider_allowlist(
        routes.clone(),
        Some(&["claude-oauth".to_string(), "openrouter".to_string()]),
        "unrelated-current",
    );
    let models: Vec<&str> = filtered.iter().map(|r| r.model.as_str()).collect();
    assert_eq!(models, ["claude-fable-5", "deepseek/deepseek-v4-pro"]);
}

#[test]
fn provider_allowlist_keeps_current_model_and_never_empties_picker() {
    let routes = vec![
        model_route("gpt-5.5", "OpenAI", "openai-oauth"),
        model_route("qwen3-coder", "llama.cpp", "openai-compatible:llamacpp"),
    ];

    // Current model's route survives even when its provider is filtered out.
    let filtered = filter_routes_by_provider_allowlist(
        routes.clone(),
        Some(&["llamacpp".to_string()]),
        "gpt-5.5",
    );
    let models: Vec<&str> = filtered.iter().map(|r| r.model.as_str()).collect();
    assert_eq!(models, ["gpt-5.5", "qwen3-coder"]);

    // A filter matching nothing falls back to the full list.
    let filtered = filter_routes_by_provider_allowlist(
        routes.clone(),
        Some(&["nonexistent".to_string()]),
        "unrelated-current",
    );
    assert_eq!(filtered.len(), routes.len());

    // None / empty / blank-entry allowlists are no-ops.
    assert_eq!(
        filter_routes_by_provider_allowlist(routes.clone(), None, "x").len(),
        2
    );
    assert_eq!(
        filter_routes_by_provider_allowlist(routes.clone(), Some(&[]), "x").len(),
        2
    );
    assert_eq!(
        filter_routes_by_provider_allowlist(routes, Some(&["  ".to_string()]), "x").len(),
        2
    );
}

/// A paid subscription must outrank a metered API key for the same model.
///
/// The bug: OpenAI OAuth and OpenAI API key both sat at method rank 0, and
/// both report `estimated_reference_cost_micros = None`, so the cheapness
/// tiebreak compared MAX to MAX and fell through to the provider label,
/// which is the identical string "OpenAI". Ordering was therefore
/// arbitrary, and the API-key route won: every OpenAI model in `/model`
/// showed "api key" as its default route even for a user with a working
/// ChatGPT OAuth login, silently billing per token for requests the
/// subscription already covered.
#[test]
fn oauth_routes_outrank_api_key_routes_for_the_same_model() {
    fn opt(api_method: &str, provider: &str) -> crate::tui::PickerOption {
        crate::tui::PickerOption {
            provider: provider.to_string(),
            api_method: api_method.to_string(),
            available: true,
            detail: String::new(),
            // Both subscription and (unpriced) key routes report None here,
            // which is exactly why cheapness could not break the tie.
            estimated_reference_cost_micros: None,
        }
    }

    let mut openai = vec![
        opt("openai-api-key", "OpenAI"),
        opt("openai-oauth", "OpenAI"),
    ];
    openai.sort_by_key(route_sort_key);
    assert_eq!(
        openai[0].api_method, "openai-oauth",
        "a ChatGPT subscription is already paid for; defaulting to the \
         metered key bills twice for the same request"
    );

    // Sort must not depend on input order.
    let mut reversed = vec![
        opt("openai-oauth", "OpenAI"),
        opt("openai-api-key", "OpenAI"),
    ];
    reversed.sort_by_key(route_sort_key);
    assert_eq!(reversed[0].api_method, "openai-oauth");

    // The Anthropic pair already behaved; pin it so the shared ranking
    // cannot regress while fixing the OpenAI side.
    let mut anthropic = vec![
        opt("api-key", "Anthropic"),
        opt("claude-oauth", "Anthropic"),
    ];
    anthropic.sort_by_key(route_sort_key);
    assert_eq!(anthropic[0].api_method, "claude-oauth");
}

/// Availability still dominates: an unavailable OAuth route must not be
/// preferred over a working API key, or the picker would default to a
/// route that cannot serve the request.
#[test]
fn an_unavailable_oauth_route_loses_to_a_working_api_key() {
    fn opt(api_method: &str, available: bool) -> crate::tui::PickerOption {
        crate::tui::PickerOption {
            provider: "OpenAI".to_string(),
            api_method: api_method.to_string(),
            available,
            detail: String::new(),
            estimated_reference_cost_micros: None,
        }
    }

    let mut routes = vec![opt("openai-oauth", false), opt("openai-api-key", true)];
    routes.sort_by_key(route_sort_key);
    assert_eq!(
        routes[0].api_method, "openai-api-key",
        "preferring a subscription must never mean preferring a dead route"
    );
}
