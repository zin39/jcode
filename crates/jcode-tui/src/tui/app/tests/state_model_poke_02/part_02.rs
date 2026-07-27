#[test]
fn test_agents_review_picker_saves_config_override() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        configure_test_remote_models(&mut app);
        app.open_agent_model_picker(crate::tui::AgentModelTarget::Review);

        // Find the entry with the AgentModelChoice action
        let (display_row, entry_idx) = app
            .inline_interactive_state
            .as_ref()
            .and_then(|picker| {
                picker.display_rows.iter().enumerate().find_map(|(row_idx, row)| {
                    match row {
                        crate::tui::PickerDisplayRow::Entry { entry_index } => {
                            let entry = &picker.entries[*entry_index];
                            if matches!(
                                entry.action,
                                crate::tui::PickerAction::AgentModelChoice {
                                    target: crate::tui::AgentModelTarget::Review,
                                    clear_override: false,
                                }
                            ) {
                                Some((row_idx, *entry_index))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    }
                })
            })
            .expect("review picker should include at least one model option");
        app.inline_interactive_state.as_mut().unwrap().selected = display_row;
        app.inline_interactive_state.as_mut().unwrap().entries[entry_idx].options[0]
            .available = true;

        let expected = {
            let picker = app.inline_interactive_state.as_ref().unwrap();
            let entry = &picker.entries[entry_idx];
            let base = if entry.effort.is_some() {
                entry
                    .name
                    .rsplit_once(" (")
                    .map(|(base, _)| base.to_string())
                    .unwrap_or_else(|| entry.name.clone())
            } else {
                entry.name.clone()
            };
            let route = &entry.options[entry.selected_option];
            // Compare against the parsed route method, not the raw string. The
            // catalog legitimately emits aliases ("openai-api-key" for
            // "openai-api"), and the canonical alias table in
            // `ModelRouteApiMethod::parse` is what the product resolves through.
            // Matching raw strings made this expectation silently fall through
            // to the bare model name whenever an alias showed up, so the test
            // failed depending on which spelling the catalog happened to carry.
            let method = crate::provider::ModelRouteApiMethod::parse(&route.api_method);
            use crate::provider::ModelRouteApiMethod as Method;
            if matches!(method, Method::Copilot) {
                format!("copilot:{}", base)
            } else if matches!(method, Method::Cursor) {
                format!("cursor:{}", base)
            } else if matches!(method, Method::OpenAIOAuth) {
                format!("openai-oauth:{}", base)
            } else if matches!(method, Method::OpenAIApiKey) {
                format!("openai-api:{}", base)
            } else if matches!(method, Method::ClaudeOAuth) {
                format!("claude-oauth:{}", base)
            } else if matches!(method, Method::AnthropicApiKey) && route.provider == "Anthropic" {
                format!("claude-api:{}", base)
            } else if matches!(method, Method::Bedrock) {
                format!("bedrock:{}", base)
            } else if matches!(method, Method::OpenRouter) && route.provider != "auto" {
                let catalog_model = crate::provider::openrouter_catalog_model_id(&base)
                    .unwrap_or_else(|| base.clone());
                format!("{}@{}", catalog_model, route.provider)
            } else {
                base
            }
        };

        app.handle_inline_interactive_key(KeyCode::Enter, KeyModifiers::NONE)
            .expect("save agent model override");

        let cfg = crate::config::Config::load();
        assert_eq!(cfg.autoreview.model.as_deref(), Some(expected.as_str()));
        assert!(app.inline_interactive_state.is_none());
    });
}

#[test]
fn test_model_command_suggestions_include_matching_models() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);

    let suggestions = app.get_suggestions_for("/model g55");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/model gpt-5.5")
    );
}

#[test]
fn test_model_command_trailing_space_shows_model_suggestions() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);

    let suggestions = app.get_suggestions_for("/model ");
    assert!(
        suggestions
            .iter()
            .any(|(cmd, _)| cmd == "/model gpt-5.3-codex")
    );
}

#[test]
fn test_model_command_provider_suggestions_include_openrouter_routes() {
    let mut app = create_test_app();
    configure_test_remote_openrouter_provider_routes(&mut app);

    let suggestions = app.get_suggestions_for("/model anthropic/claude-sonnet-4@");
    let commands: Vec<&str> = suggestions.iter().map(|(cmd, _)| cmd.as_str()).collect();

    assert!(commands.contains(&"/model anthropic/claude-sonnet-4@auto"));
    assert!(commands.contains(&"/model anthropic/claude-sonnet-4@Fireworks"));
    assert!(commands.contains(&"/model anthropic/claude-sonnet-4@OpenAI"));
}

#[test]
fn test_model_command_provider_suggestions_rank_matching_provider_prefix() {
    let mut app = create_test_app();
    configure_test_remote_openrouter_provider_routes(&mut app);

    let suggestions = app.get_suggestions_for("/model anthropic/claude-sonnet-4@fi");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/model anthropic/claude-sonnet-4@Fireworks")
    );
}

#[test]
fn test_model_command_provider_suggestions_normalize_bare_openai_model_to_openrouter_catalog_id() {
    let (app, _set_model_calls) = create_openrouter_spec_capture_test_app();

    let suggestions = app.get_suggestions_for("/model gpt-5.4@op");
    assert_eq!(
        suggestions.first().map(|(cmd, _)| cmd.as_str()),
        Some("/model openai/gpt-5.4@OpenAI")
    );
}

#[test]
fn test_model_command_provider_suggestions_include_auto_for_normalized_bare_openai_model() {
    let (app, _set_model_calls) = create_openrouter_spec_capture_test_app();

    let suggestions = app.get_suggestions_for("/model gpt-5.4@");
    let commands: Vec<&str> = suggestions.iter().map(|(cmd, _)| cmd.as_str()).collect();

    assert!(commands.contains(&"/model openai/gpt-5.4@auto"));
    assert!(commands.contains(&"/model openai/gpt-5.4@OpenAI"));
}

#[test]
fn test_remote_fallback_provider_suggestions_normalize_bare_openai_openrouter_routes() {
    with_temp_jcode_home(|| {
        let prev_api_key = std::env::var_os("OPENROUTER_API_KEY");
        crate::env::set_var("OPENROUTER_API_KEY", "test-openrouter-key");
        crate::auth::AuthStatus::invalidate_cache();

        let mut app = create_test_app();
        app.is_remote = true;
        app.remote_provider_model = Some("gpt-5.4".to_string());
        app.remote_available_entries = vec!["gpt-5.4".to_string()];
        app.remote_model_options.clear();

        let suggestions = app.get_suggestions_for("/model gpt-5.4@");
        let commands: Vec<&str> = suggestions.iter().map(|(cmd, _)| cmd.as_str()).collect();

        assert!(commands.contains(&"/model openai/gpt-5.4@auto"));
        assert!(commands.contains(&"/model openai/gpt-5.4@OpenAI"));

        if let Some(prev_api_key) = prev_api_key {
            crate::env::set_var("OPENROUTER_API_KEY", prev_api_key);
        } else {
            crate::env::remove_var("OPENROUTER_API_KEY");
        }
        crate::auth::AuthStatus::invalidate_cache();
    });
}

#[test]
fn test_login_command_suggestions_follow_provider_catalog() {
    let app = create_test_app();
    let suggestions = app.get_suggestions_for("/login ");

    for provider in crate::provider_catalog::tui_login_providers() {
        assert!(
            suggestions
                .iter()
                .any(|(cmd, detail)| cmd == &format!("/login {}", provider.id)
                    && detail == &provider.menu_detail),
            "missing /login suggestion for provider {}",
            provider.id
        );
    }
}

#[test]
fn test_model_autocomplete_completes_unique_match() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);
    app.input = "/model g55".to_string();
    app.cursor_pos = app.input.len();

    assert!(app.autocomplete());
    assert_eq!(app.input(), "/model gpt-5.5");
}

#[test]
fn test_model_autocomplete_completes_unique_provider_match() {
    let mut app = create_test_app();
    configure_test_remote_openrouter_provider_routes(&mut app);

    app.input = "/model anthropic/claude-sonnet-4@fi".to_string();
    app.cursor_pos = app.input.len();

    assert!(app.autocomplete());
    assert_eq!(app.input(), "/model anthropic/claude-sonnet-4@Fireworks");
}

#[test]
fn test_model_picker_preview_stays_open_and_updates_filter() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);

    for c in "/model g55".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    app.wait_for_model_picker_routes_for_tests();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("model picker preview should be open");
    assert!(picker.preview);
    assert_eq!(picker.filter, "g55");
    // NEW DESIGN: One row per model, no effort suffix
    assert!(
        picker
            .filtered
            .iter()
            .any(|&i| picker.entries[i].name == "gpt-5.5"),
        "gpt-5.5 should match filter g55"
    );
    assert_eq!(app.input(), "/model g55");
}

#[test]
fn test_model_picker_preview_enter_selects_model() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);

    for c in "/model g55".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::empty())
        .unwrap();

    // Enter from preview mode selects the model and closes the picker
    assert!(app.inline_interactive_state.is_none());
    assert!(app.input().is_empty());
    assert_eq!(app.cursor_pos(), 0);
}

/// Left/Right in the PREVIEW picker (the state while "/model" is still in the
/// composer) must dial the focused row's effort, not move the composer cursor.
/// This was the live regression: the hint said "←/→ effort" but the keys just
/// wandered through the typed text.
#[test]
fn test_model_picker_preview_arrows_cycle_effort_not_cursor() {
    let mut app = create_test_app();
    configure_test_remote_models(&mut app);

    for c in "/model g55".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    app.wait_for_model_picker_routes_for_tests();

    let cursor_before = app.cursor_pos;
    let effort_before = {
        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("preview picker open");
        assert!(picker.preview);
        let entry_idx = picker
            .entry_index_for_display_row(picker.selected)
            .expect("selected row should be an entry");
        assert!(
            !picker.entries[entry_idx].available_efforts.is_empty(),
            "focused row should support efforts for this test"
        );
        picker.entries[entry_idx].effort.clone()
    };

    app.handle_key(KeyCode::Right, KeyModifiers::empty()).unwrap();

    let picker = app
        .inline_interactive_state
        .as_ref()
        .expect("picker still open");
    let entry_idx = picker
        .entry_index_for_display_row(picker.selected)
        .expect("selected row should be an entry");
    assert_ne!(
        picker.entries[entry_idx].effort, effort_before,
        "Right must cycle the focused row's effort"
    );
    assert_eq!(
        app.cursor_pos, cursor_before,
        "Right must not move the composer cursor while the model picker is open"
    );
}

/// Effort cycling must wrap at both ends and Enter must stage the dialed
/// effort into the switch request, or the ladder is decorative.
#[test]
fn test_model_picker_effort_wraps_and_enter_stages_choice() {
    let mut app = create_test_app();
    // Real hydrated routes: with only placeholder "remote-catalog" routes,
    // Enter starts a catalog refresh instead of staging a switch.
    configure_test_remote_models_with_openai_recommendations(&mut app);

    for c in "/model g55".chars() {
        app.handle_key(KeyCode::Char(c), KeyModifiers::empty())
            .unwrap();
    }
    app.wait_for_model_picker_routes_for_tests();

    // Wrap forward: cycling len(available) times returns to the start.
    let (start_effort, n) = {
        let picker = app.inline_interactive_state.as_ref().unwrap();
        let entry_idx = picker
            .entry_index_for_display_row(picker.selected)
            .expect("selected row should be an entry");
        (
            picker.entries[entry_idx].effort.clone(),
            picker.entries[entry_idx].available_efforts.len(),
        )
    };
    assert!(n >= 2, "need at least two efforts to test wrap");
    for _ in 0..n {
        app.handle_key(KeyCode::Right, KeyModifiers::empty()).unwrap();
    }
    {
        let picker = app.inline_interactive_state.as_ref().unwrap();
        let entry_idx = picker
            .entry_index_for_display_row(picker.selected)
            .expect("selected row should be an entry");
        assert_eq!(
            picker.entries[entry_idx].effort, start_effort,
            "cycling through all efforts must wrap back to the start"
        );
    }

    // Left is the inverse of Right: one step back from the start effort.
    app.handle_key(KeyCode::Left, KeyModifiers::empty()).unwrap();
    let dialed = {
        let picker = app.inline_interactive_state.as_ref().unwrap();
        let entry_idx = picker
            .entry_index_for_display_row(picker.selected)
            .expect("selected row should be an entry");
        let e = picker.entries[entry_idx].effort.clone().expect("effort dialed");
        let ladder = &picker.entries[entry_idx].available_efforts;
        let start_idx = ladder
            .iter()
            .position(|a| Some(a) == start_effort.as_ref())
            .expect("start effort on ladder");
        let expected = &ladder[(start_idx + ladder.len() - 1) % ladder.len()];
        assert_eq!(
            &e, expected,
            "Left must step one back on the ladder (wrapping at the ends)"
        );
        e
    };

    // Enter stages the dialed effort.
    app.handle_key(KeyCode::Enter, KeyModifiers::empty()).unwrap();
    assert!(
        app.pending_model_switch.is_some() || app.pending_reasoning_effort.is_some(),
        "Enter should stage a switch"
    );
    assert_eq!(
        app.pending_reasoning_effort.as_deref(),
        Some(dialed.as_str()),
        "staged switch should carry the dialed effort, got model={:?} effort={:?}",
        app.pending_model_switch,
        app.pending_reasoning_effort
    );
}
