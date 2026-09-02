// Local-model login must let the user point at an endpoint.
//
// llama.cpp, Ollama and LM Studio are "local" only by convention: they are
// routinely served from a LAN box, an SSH tunnel, or a non-default port, and
// llama-server's default 8080 collides with common dev servers. The TUI login
// flow used to print the endpoint as static text and go straight to the
// optional-API-key prompt, so there was no way to relocate it. Users finished
// login and then had every request fail against localhost.
//
// These tests drive the real prompt flow (`start_openai_compatible_profile_login`
// then `submit_input`) and assert on what is persisted, so they fail if the
// endpoint step is dropped or stops writing the provider's own env var.

/// Isolate `$JCODE_HOME` and the endpoint env vars this flow reads and writes.
///
/// The `JCODE_*_API_BASE` vars are genuinely process-global and take precedence
/// over the saved env file, so a stale one from the developer's shell would
/// otherwise mask what the test just wrote.
fn with_temp_local_endpoint_home<T>(f: impl FnOnce() -> T) -> T {
    let _env_guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let saved_env = [
        "JCODE_LLAMACPP_API_BASE",
        "LLAMACPP_HOST",
        "LLAMA_CPP_HOST",
        "LLAMA_SERVER_HOST",
        "JCODE_OLLAMA_API_BASE",
        "OLLAMA_HOST",
        "JCODE_LMSTUDIO_API_BASE",
        "LMSTUDIO_HOST",
    ]
    .map(|key| (key, std::env::var_os(key)));

    let _scoped_home = crate::storage::scoped_test_home(temp.path());
    for (key, _) in saved_env.iter() {
        crate::env::remove_var(key);
    }

    let result = f();

    for (key, value) in saved_env.into_iter() {
        match value {
            Some(value) => crate::env::set_var(key, value),
            None => crate::env::remove_var(key),
        }
    }
    result
}

fn llamacpp_profile() -> crate::provider_catalog::OpenAiCompatibleProfile {
    *crate::provider_catalog::openai_compatible_profiles()
        .iter()
        .find(|profile| profile.id == "llamacpp")
        .expect("llamacpp must be a known OpenAI-compatible profile")
}

/// The bug as reported: choosing llama.cpp offered no way to enter an endpoint.
#[test]
fn local_provider_login_asks_for_an_endpoint_before_the_key() {
    with_temp_local_endpoint_home(|| {
        let mut app = create_test_app();
        app.start_openai_compatible_profile_login_for_test(llamacpp_profile());

        assert!(
            matches!(
                app.pending_login,
                Some(crate::tui::app::PendingLogin::LocalEndpointApiBase { .. })
            ),
            "llama.cpp login must first ask for the endpoint, got {:?}",
            app.pending_login
        );

        let prompt = app
            .display_messages()
            .last()
            .expect("endpoint prompt message")
            .content
            .clone();
        assert!(
            prompt.contains("Endpoint"),
            "prompt must be about the endpoint, got: {prompt}"
        );
        assert!(
            prompt.contains("another host or port"),
            "prompt must tell the user relocation is possible, got: {prompt}"
        );
    });
}

/// A typed endpoint must be persisted to the provider's *own* env var, so
/// llama.cpp, Ollama and LM Studio can point at different hosts.
#[test]
fn typed_endpoint_is_saved_to_the_provider_specific_env_var() {
    with_temp_local_endpoint_home(|| {
        let mut app = create_test_app();
        app.start_openai_compatible_profile_login_for_test(llamacpp_profile());

        // `host:port` is the spelling people already use for LLAMACPP_HOST.
        app.set_input_for_test("192.168.1.50:9999");
        app.submit_input();

        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(
            llamacpp_profile(),
        );
        let env_file = crate::storage::app_config_dir()
            .unwrap()
            .join(&resolved.env_file);
        let contents = std::fs::read_to_string(&env_file)
            .unwrap_or_else(|e| panic!("{env_file:?} should exist: {e}"));
        assert!(
            contents.contains("JCODE_LLAMACPP_API_BASE=http://192.168.1.50:9999/v1"),
            "endpoint must be saved with scheme and /v1 to llamacpp's own var, got:\n{contents}"
        );

        // And the flow advances to the optional-key step rather than dead-ending.
        assert!(
            matches!(
                app.pending_login,
                Some(crate::tui::app::PendingLogin::ApiKeyProfile {
                    api_key_optional: true,
                    ..
                })
            ),
            "after the endpoint, login must continue to the optional key prompt, got {:?}",
            app.pending_login
        );
    });
}

/// The saved endpoint must actually be what the provider resolves to later.
/// Writing the file is worthless if resolution ignores it.
#[test]
fn saved_endpoint_is_what_the_provider_resolves_to() {
    with_temp_local_endpoint_home(|| {
        let mut app = create_test_app();
        let profile = llamacpp_profile();
        let default_api_base =
            crate::provider_catalog::resolve_openai_compatible_profile(profile).api_base;

        app.start_openai_compatible_profile_login_for_test(profile);
        app.set_input_for_test("http://10.0.0.7:8080/v1");
        app.submit_input();

        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
        assert_eq!(
            resolved.api_base, "http://10.0.0.7:8080/v1",
            "resolution must honor the endpoint saved during login"
        );
        assert_ne!(
            resolved.api_base, default_api_base,
            "the relocated endpoint must not fall back to the default"
        );
    });
}

/// Pressing Enter keeps the default, so the common localhost case stays a
/// single keypress and nothing is written.
#[test]
fn empty_endpoint_input_keeps_the_default_and_writes_nothing() {
    with_temp_local_endpoint_home(|| {
        let mut app = create_test_app();
        let profile = llamacpp_profile();
        let default_api_base =
            crate::provider_catalog::resolve_openai_compatible_profile(profile).api_base;

        app.start_openai_compatible_profile_login_for_test(profile);
        app.set_input_for_test("");
        app.submit_input();

        let resolved = crate::provider_catalog::resolve_openai_compatible_profile(profile);
        assert_eq!(
            resolved.api_base, default_api_base,
            "Enter must keep the existing endpoint"
        );
        assert!(
            matches!(
                app.pending_login,
                Some(crate::tui::app::PendingLogin::ApiKeyProfile { .. })
            ),
            "Enter must advance to the key prompt"
        );
    });
}

/// A malformed endpoint must be rejected with the prompt still pending, rather
/// than silently saved and failing later at request time.
#[test]
fn invalid_endpoint_is_rejected_and_the_prompt_survives() {
    with_temp_local_endpoint_home(|| {
        let mut app = create_test_app();
        app.start_openai_compatible_profile_login_for_test(llamacpp_profile());

        // A public host over plain HTTP is refused by the shared normalizer.
        app.set_input_for_test("http://example.com:8080");
        app.submit_input();

        let last = app.display_messages().last().expect("error message").clone();
        assert_eq!(last.role, "error", "invalid endpoint must surface an error");
        assert!(
            last.content.contains("Invalid endpoint"),
            "expected an invalid-endpoint error, got: {}",
            last.content
        );
        assert!(
            matches!(
                app.pending_login,
                Some(crate::tui::app::PendingLogin::LocalEndpointApiBase { .. })
            ),
            "the endpoint prompt must survive so the user can correct it"
        );
    });
}

/// A blanket "empty input means keep waiting" guard used to swallow the bare
/// Enter on every login prompt, including the ones whose own text advertises
/// Enter as the way to accept a default. Those prompts must accept it; secret
/// and callback prompts must still repeat themselves.
#[test]
fn only_prompts_that_advertise_a_default_accept_a_bare_enter() {
    use crate::tui::app::PendingLogin;

    let optional_key = PendingLogin::ApiKeyProfile {
        provider_id: "llamacpp".to_string(),
        provider: "llama.cpp".to_string(),
        auth_method: "local_endpoint".to_string(),
        docs_url: String::new(),
        env_file: "llamacpp.env".to_string(),
        key_name: "LLAMACPP_API_KEY".to_string(),
        default_model: None,
        endpoint: None,
        api_key_optional: true,
        openai_compatible_profile: None,
    };
    let required_key = PendingLogin::ApiKeyProfile {
        provider_id: "openrouter".to_string(),
        provider: "OpenRouter".to_string(),
        auth_method: "api_key".to_string(),
        docs_url: String::new(),
        env_file: "openrouter.env".to_string(),
        key_name: "OPENROUTER_API_KEY".to_string(),
        default_model: None,
        endpoint: None,
        api_key_optional: false,
        openai_compatible_profile: None,
    };

    assert!(
        PendingLogin::LocalEndpointApiBase {
            profile: llamacpp_profile()
        }
        .accepts_empty_input(),
        "the endpoint prompt says 'Press Enter to keep the current value'"
    );
    assert!(
        optional_key.accepts_empty_input(),
        "the optional local key prompt says 'Press Enter to skip'"
    );
    assert!(
        !required_key.accepts_empty_input(),
        "a required API key prompt must keep waiting on empty input"
    );
    assert!(
        !PendingLogin::CursorApiKey.accepts_empty_input(),
        "secret prompts must keep waiting on empty input"
    );
}

/// Providers that require a key (hosted OpenAI-compatible services) must not
/// gain an endpoint step: their base URL is fixed and prompting would be noise.
#[test]
fn api_key_providers_do_not_get_a_local_endpoint_prompt() {
    with_temp_local_endpoint_home(|| {
        let hosted = crate::provider_catalog::openai_compatible_profiles()
            .iter()
            .copied()
            .find(|profile| {
                profile.id != crate::provider_catalog::OPENAI_COMPAT_PROFILE.id
                    && crate::provider_catalog::resolve_openai_compatible_profile(*profile)
                        .requires_api_key
            })
            .expect("at least one hosted OpenAI-compatible provider");

        let mut app = create_test_app();
        app.start_openai_compatible_profile_login_for_test(hosted);
        assert!(
            matches!(
                app.pending_login,
                Some(crate::tui::app::PendingLogin::ApiKeyProfile { .. })
            ),
            "hosted providers must go straight to the key prompt, got {:?}",
            app.pending_login
        );
    });
}

/// Every provider the catalog calls "local" must offer the endpoint step, so a
/// future local runtime cannot be added and silently miss it.
#[test]
fn all_local_providers_offer_the_endpoint_step() {
    with_temp_local_endpoint_home(|| {
        for profile in crate::provider_catalog::openai_compatible_profiles() {
            let resolved = crate::provider_catalog::resolve_openai_compatible_profile(*profile);
            if resolved.requires_api_key
                || profile.id == crate::provider_catalog::OPENAI_COMPAT_PROFILE.id
            {
                continue;
            }
            let mut app = create_test_app();
            app.start_openai_compatible_profile_login_for_test(*profile);
            assert!(
                matches!(
                    app.pending_login,
                    Some(crate::tui::app::PendingLogin::LocalEndpointApiBase { .. })
                ),
                "local provider '{}' must offer an endpoint prompt",
                profile.id
            );
        }
    });
}
