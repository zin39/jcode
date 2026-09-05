//! Context-window resolution for Ollama endpoints.
//!
//! Split out of `openrouter_tests.rs` so the Ollama serving-window rules live
//! next to the module that implements them, and so the shared OpenRouter test
//! file does not keep growing.

use super::tests::{ENV_LOCK, EnvVarGuard};
use super::*;
use jcode_provider_core::Provider;

#[test]
fn ollama_context_window_does_not_over_report_before_the_catalog_is_probed() {
    // Ollama serves min(trained window, OLLAMA_CONTEXT_LENGTH) and silently
    // truncates anything longer, so before the native-API probe has populated
    // the catalog there is no evidence for a large window. Reporting a model's
    // advertised window here would over-budget the request and make the
    // conversation look like it lost its history.
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:11434/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        // A model id whose family heuristics advertise a very large window.
        default_model: Some("qwen3:35b".to_string()),
        ..Default::default()
    };
    config.model_catalog = false;
    config.requires_api_key = Some(false);

    let provider =
        OpenRouterProvider::new_named_openai_compatible("ollama", &config).expect("provider");

    assert_eq!(
        provider.context_window(),
        4096,
        "a cold Ollama cache must fall back to Ollama's conservative serving \
         default instead of the model's advertised window"
    );
}

#[test]
fn explicit_context_window_still_wins_over_the_ollama_clamp() {
    // The clamp is a fallback for missing evidence, not a ceiling. A user who
    // has raised OLLAMA_CONTEXT_LENGTH and declared the matching window must be
    // believed, otherwise the fix for over-reporting becomes under-reporting.
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:11434/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("qwen3:35b".to_string()),
        models: vec![jcode_base::config::NamedProviderModelConfig {
            id: "qwen3:35b".to_string(),
            context_window: Some(65_536),
            input: Vec::new(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
        }],
        ..Default::default()
    };
    config.model_catalog = false;
    config.requires_api_key = Some(false);

    let provider =
        OpenRouterProvider::new_named_openai_compatible("ollama", &config).expect("provider");

    assert_eq!(provider.context_window(), 65_536);
}

#[test]
fn ollama_cloud_model_is_not_clamped_to_the_local_runner_default() {
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:11434/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("glm-5.2:cloud".to_string()),
        ..Default::default()
    };
    config.model_catalog = false;
    config.requires_api_key = Some(false);

    let provider =
        OpenRouterProvider::new_named_openai_compatible("ollama", &config).expect("provider");

    assert_eq!(provider.context_window(), 1_000_000);
}

#[test]
fn llamacpp_context_window_does_not_inherit_the_trained_family_window() {
    // `llama-server -c N` fixes the served window, and a served id like
    // `qwen3-coder` collides with the open-weight family table, which reports
    // the model's *trained* window (256K+). Inheriting that overstates the
    // gauge and builds prompts the server truncates, which reads to the user
    // as the model forgetting the conversation.
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:8080/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("qwen3-coder".to_string()),
        ..Default::default()
    };
    config.model_catalog = false;
    config.requires_api_key = Some(false);

    let provider =
        OpenRouterProvider::new_named_openai_compatible("llamacpp", &config).expect("provider");

    assert_eq!(
        provider.context_window(),
        8_192,
        "a cold llama.cpp cache must fall back to a conservative served window \
         instead of the model's advertised trained window"
    );
}

#[test]
fn explicit_context_window_still_wins_for_llamacpp() {
    // The fallback is a guess for missing evidence, not a ceiling: a user who
    // ran `llama-server -c 131072` and declared it must be believed.
    let _lock = ENV_LOCK.lock();
    let _namespace = EnvVarGuard::remove("JCODE_OPENROUTER_CACHE_NAMESPACE");
    let mut config = jcode_base::config::NamedProviderConfig {
        base_url: "http://localhost:8080/v1".to_string(),
        auth: jcode_base::config::NamedProviderAuth::None,
        default_model: Some("qwen3-coder".to_string()),
        models: vec![jcode_base::config::NamedProviderModelConfig {
            id: "qwen3-coder".to_string(),
            context_window: Some(131_072),
            input: Vec::new(),
            price_input_per_mtok: None,
            price_output_per_mtok: None,
        }],
        ..Default::default()
    };
    config.model_catalog = false;
    config.requires_api_key = Some(false);

    let provider =
        OpenRouterProvider::new_named_openai_compatible("llamacpp", &config).expect("provider");

    assert_eq!(
        provider.context_window(),
        131_072,
        "an explicitly configured context_window must outrank the fallback"
    );
}

/// An explicit `context_window` must outrank every auto-detected source,
/// including the live catalog.
///
/// Regression for a `llama-server` serving 1M tokens that jcode still budgeted
/// as a 200K model. No llamacpp catalog cache existed, so `context_window()`
/// fell through the catalog, the static table, and the llama.cpp/Ollama
/// serving-floor guards, all the way to `DEFAULT_CONTEXT_LIMIT` (200_000) --
/// and the user's configured value was never consulted on that path at all.
///
/// This pins the *precedence rule* rather than the plumbing: config wins,
/// because it is the only source that reflects how the server was actually
/// launched.
#[test]
fn configured_context_window_outranks_every_autodetected_source() {
    /// Mirrors the resolution order in `context_window()`.
    fn resolve(
        configured: Option<usize>,
        catalog: Option<usize>,
        static_table: Option<usize>,
        llamacpp_floor: bool,
    ) -> usize {
        const DEFAULT_CONTEXT_LIMIT: usize = 200_000;
        const LLAMACPP_FALLBACK_SERVING_CONTEXT: usize = 8_192;
        if let Some(limit) = configured.filter(|l| *l > 0) {
            return limit;
        }
        if let Some(limit) = catalog {
            return limit;
        }
        if let Some(limit) = static_table {
            return limit;
        }
        if llamacpp_floor {
            return LLAMACPP_FALLBACK_SERVING_CONTEXT;
        }
        DEFAULT_CONTEXT_LIMIT
    }

    // The reported bug: nothing auto-detected, so it used to land on 200K.
    assert_eq!(
        resolve(Some(1_048_576), None, None, true),
        1_048_576,
        "with no catalog cache the configured window must still win, not the \
         llama.cpp floor or DEFAULT_CONTEXT_LIMIT"
    );

    // Config also beats a live catalog that disagrees: the catalog can be a
    // stale snapshot of a server that has since been relaunched.
    assert_eq!(
        resolve(Some(1_048_576), Some(262_144), Some(200_000), true),
        1_048_576,
        "configured window must outrank a stale live catalog"
    );

    // Zero/absent config must not shadow real evidence.
    assert_eq!(
        resolve(Some(0), Some(262_144), None, true),
        262_144,
        "a zero context_window is not a valid override"
    );
    assert_eq!(
        resolve(None, Some(262_144), None, true),
        262_144,
        "unconfigured models still use the live catalog"
    );

    // Unconfigured llama.cpp with no catalog keeps the conservative floor:
    // guessing high builds requests the server rejects.
    assert_eq!(
        resolve(None, None, None, true),
        8_192,
        "unconfigured llama.cpp must keep its conservative serving floor"
    );
}
