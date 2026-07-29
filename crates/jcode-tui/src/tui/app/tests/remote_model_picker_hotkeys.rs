// Regression tests for issue #438: in remote sessions, the runtime model
// picker preview advertises Ctrl+N (toggle favorite) and Ctrl+O (set default),
// but the remote key path never routed those chords to
// `model_picker_preview_hotkey`. They fell through to the remote global
// Ctrl-key handling and were swallowed as unrecognized hotkeys.

fn remote_model_picker_preview_state() -> crate::tui::InlineInteractiveState {
    crate::tui::InlineInteractiveState {
        kind: crate::tui::PickerKind::Model,
        filtered: vec![0],
        entries: vec![crate::tui::PickerEntry {
            name: "gpt-5.5".to_string(),
            options: vec![crate::tui::PickerOption {
                provider: "OpenAI".to_string(),
                api_method: "openai-api".to_string(),
                available: true,
                detail: String::new(),
                estimated_reference_cost_micros: None,
            }],
            action: crate::tui::PickerAction::Model,
            selected_option: 0,
            is_current: false,
            is_default: false,
            is_favorite: false,
            recommended: false,
            recommendation_rank: usize::MAX,
            usage_score: 0,
            old: false,
            created_date: None,
            effort: None,
                available_efforts: Vec::new(),
                provider_group: None,
                is_recent: false,
        }],
        selected: 0,
        column: 0,
        filter: String::new(),
        preview: true,
        display_rows: vec![crate::tui::PickerDisplayRow::Entry { entry_index: 0 }],
        collapse_state: crate::tui::CollapseState::default(),
    }
}

#[test]
fn test_remote_model_picker_preview_ctrl_n_toggles_favorite() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();

        app.is_remote = true;
        app.inline_interactive_state = Some(remote_model_picker_preview_state());

        rt.block_on(app.handle_remote_key(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            &mut remote,
        ))
        .expect("Ctrl+N should be handled in the remote path");

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("picker preview should stay open after Ctrl+N");
        assert!(picker.preview, "picker should remain in preview mode");
        assert!(
            picker.entries[0].is_favorite,
            "Ctrl+N must toggle the selected model as a favorite in remote sessions"
        );
        assert!(
            app.status_notice()
                .is_some_and(|notice| notice.contains("Favorited")),
            "favorite toggle should surface a status notice, got: {:?}",
            app.status_notice()
        );

        // Toggling again must unfavorite, proving the chord is consumed by the
        // picker on every press instead of falling through once.
        rt.block_on(app.handle_remote_key(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
            &mut remote,
        ))
        .expect("second Ctrl+N should be handled in the remote path");
        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("picker preview should stay open");
        assert!(!picker.entries[0].is_favorite);
    });
}

#[test]
fn test_remote_model_picker_preview_ctrl_o_sets_default() {
    with_temp_jcode_home(|| {
        let mut app = create_test_app();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        let mut remote = crate::tui::backend::RemoteConnection::dummy();

        app.is_remote = true;
        app.inline_interactive_state = Some(remote_model_picker_preview_state());

        rt.block_on(app.handle_remote_key(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
            &mut remote,
        ))
        .expect("Ctrl+O should be handled in the remote path");

        let picker = app
            .inline_interactive_state
            .as_ref()
            .expect("picker preview should stay open after Ctrl+O");
        assert!(picker.preview, "picker should remain in preview mode");
        assert!(
            picker.entries[0].is_default,
            "Ctrl+O must mark the selected model as the default in remote sessions"
        );
        assert!(
            app.display_messages()
                .iter()
                .any(|msg| msg.content.contains("Saved default model")),
            "setting the default should confirm with a system message"
        );
    });
}

#[test]
fn test_remote_model_picker_render_shows_named_provider_profile_models() {
    with_temp_jcode_home(|| {
        let config_path = crate::storage::jcode_dir()
            .expect("test home should be configured")
            .join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[providers.sf-test]
api_base = "https://api.siliconflow.cn/v1"
api_key_env = "SILICONFLOW_API_KEY"
model_catalog = false
models = [{ id = "Qwen/Qwen2.5-72B-Instruct" }]
default_model = "Qwen/Qwen2.5-72B-Instruct"
"#,
        )
        .expect("write named provider config");
        crate::config::invalidate_config_cache();

        let mut app = create_test_app();
        configure_test_remote_models(&mut app);
        app.display_messages = vec![DisplayMessage::system("seed render state")];
        app.bump_display_messages_version();
        app.open_model_picker();
        app.wait_for_model_picker_routes_for_tests();
        wait_for_model_picker_load(&mut app);

        let picker = app
            .inline_interactive_state
            .as_mut()
            .expect("remote model picker should be open");
        picker.filter = "Qwen".to_string();
        App::apply_inline_interactive_filter(picker);

        let _render_lock = scroll_render_test_lock();
        let backend = ratatui::backend::TestBackend::new(140, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
        let text = render_and_snap(&app, &mut terminal);
        let qwen_row = text
            .lines()
            .find(|line| line.contains("Qwen") && line.contains("sf-test"))
            .unwrap_or("");

        assert!(
            qwen_row.contains("Qwen/Qwen2.5-72B-Instruct") && qwen_row.contains("sf-test"),
            "rendered remote /model picker should show named-provider models in the user-visible table, got row `{}` in:\n{}",
            qwen_row,
            text
        );
    });
}
