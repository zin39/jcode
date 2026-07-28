use super::*;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let previous = std::env::var_os(key);
        jcode_base::env::set_var(key, value);
        Self { key, previous }
    }

    fn set_if_missing(key: &'static str, value: &str) -> Option<Self> {
        if std::env::var_os(key).is_some() {
            return None;
        }
        Some(Self::set(key, value))
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            jcode_base::env::set_var(self.key, previous);
        } else {
            jcode_base::env::remove_var(self.key);
        }
    }
}

async fn collect_live_smoke_stream(
    mut stream: EventStream,
    timeout: std::time::Duration,
) -> Result<(usize, usize, bool)> {
    tokio::time::timeout(timeout, async move {
        let mut text_bytes = 0usize;
        let mut thinking_bytes = 0usize;
        let mut saw_message_end = false;
        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::TextDelta(text) => {
                    text_bytes += text.len();
                }
                StreamEvent::ThinkingDelta(text) => {
                    thinking_bytes += text.len();
                }
                StreamEvent::MessageEnd { .. } => {
                    saw_message_end = true;
                    break;
                }
                StreamEvent::Error { message, .. } => anyhow::bail!(message),
                _ => {}
            }
        }
        Ok((text_bytes, thinking_bytes, saw_message_end))
    })
    .await
    .context("live provider smoke timed out")?
}

#[test]
fn test_parse_sse_event() {
    let mut buffer = "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".to_string();
    let event = parse_sse_event(&mut buffer).unwrap();
    assert_eq!(event.event_type, "message_start");
    assert!(buffer.is_empty());
}

#[tokio::test]
async fn test_available_models() {
    let provider = AnthropicProvider::new();
    let models = provider.available_models();
    assert!(models.contains(&"claude-opus-4-8"));
    // Opus 4.8 is native-1M, so there is no redundant `[1m]` alias.
    assert!(!models.contains(&"claude-opus-4-8[1m]"));
    assert!(models.contains(&"claude-opus-4-6"));
    assert!(models.contains(&"claude-opus-4-6[1m]"));
    assert!(models.contains(&"claude-sonnet-4-6"));
    assert!(models.contains(&"claude-sonnet-4-6[1m]"));
    assert!(models.contains(&"claude-haiku-4-5"));
}

#[test]
fn test_effectively_1m_requires_explicit_suffix() {
    assert!(!effectively_1m("claude-opus-4-6"));
    assert!(!effectively_1m("claude-sonnet-4-6"));
    assert!(effectively_1m("claude-opus-4-6[1m]"));
    assert!(effectively_1m("claude-sonnet-4-6[1m]"));
}

#[test]
fn test_oauth_beta_headers_require_explicit_1m_suffix() {
    assert_eq!(oauth_beta_headers("claude-opus-4-6"), OAUTH_BETA_HEADERS);
    assert_eq!(
        oauth_beta_headers("claude-opus-4-6[1m]"),
        OAUTH_BETA_HEADERS_1M
    );
}

#[test]
fn test_anthropic_reasoning_effort_request_parts() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-sonnet-4-6").unwrap();
    provider.set_reasoning_effort("none").unwrap();
    assert!(
        provider.set_reasoning_effort("minimal").is_err(),
        "Anthropic must reject rather than silently promote minimal to max"
    );

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "low",
            "medium",
            "high",
            "max",
            "swarm",
            "swarm-deep"
        ]
    );
    assert_eq!(provider.reasoning_effort().as_deref(), Some("none"));

    // Sonnet 4.6 supports the real `max` API level (but not `xhigh`).
    provider.set_reasoning_effort("max").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("max"));

    // `xhigh` is rejected on models that do not support it.
    assert!(provider.set_reasoning_effort("xhigh").is_err());

    provider.set_reasoning_effort("medium").unwrap();
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts("claude-sonnet-4-6", true);

    match thinking.expect("adaptive thinking should be enabled") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Claude 4.6 should use adaptive thinking"),
    }
    assert_eq!(
        output_config.expect("output_config should be set").effort,
        "medium"
    );
    assert_eq!(
        temperature, None,
        "thinking requests must omit OAuth temperature"
    );
}

#[test]
fn test_anthropic_preserves_swarm_sentinels_for_cycling() {
    // Regression: storing a swarm effort must preserve which swarm mode was
    // chosen. Previously both `swarm` and `swarm-deep` collapsed to `swarm`,
    // which capped Alt+Right effort cycling at swarm-light (it could never
    // reach swarm-deep because the readback always reported `swarm`).
    let provider = AnthropicProvider::new();
    provider.set_model("claude-sonnet-4-6").unwrap();

    provider.set_reasoning_effort("swarm").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("swarm"));

    provider.set_reasoning_effort("swarm-deep").unwrap();
    assert_eq!(
        provider.reasoning_effort().as_deref(),
        Some("swarm-deep"),
        "swarm-deep must survive the round-trip so cycling can reach it"
    );

    // And cycling back down to swarm-light still works.
    provider.set_reasoning_effort("swarm").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("swarm"));
}

#[test]
fn test_anthropic_show_thinking_enables_adaptive_thinking_without_effort() {
    // With no explicit reasoning effort, an adaptive-thinking model should still
    // request summarized thinking when the user has opted into the display.
    // Crucially, `output_config` must stay None so we do not force a stronger
    // (more expensive) reasoning level than the model's default.
    //
    // We use a non-Opus model here because Opus now carries an implicit `xhigh`
    // default (see `test_anthropic_opus_defaults_to_xhigh_effort`); Sonnet keeps
    // the model's own default so this invariant stays meaningful.
    //
    // `build_reasoning_request_parts_inner` takes the model directly, so we do
    // not depend on `set_model` accepting a particular catalog entry. With no
    // effort configured, `self.reasoning_effort()` resolves to None regardless
    // of the default model.
    let provider = AnthropicProvider::new();
    // Make the test independent of the ambient config's anthropic_reasoning_effort
    // by clearing the field directly; we only exercise the show_thinking path.
    *provider.reasoning_effort.write().unwrap() = None;

    // show_thinking = false: nothing requested.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-sonnet-4-6", true, false);
    assert!(
        thinking.is_none(),
        "no thinking should be requested when both effort and show_thinking are off"
    );
    assert!(output_config.is_none());

    // show_thinking = true: adaptive thinking requested, no output_config.
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts_inner("claude-sonnet-4-6", true, true);
    match thinking.expect("show_thinking should enable adaptive thinking") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Sonnet 4.6 should use adaptive thinking"),
    }
    assert!(
        output_config.is_none(),
        "show_thinking alone must not force an output reasoning effort"
    );
    assert_eq!(
        temperature, None,
        "thinking requests must omit OAuth temperature"
    );
}

#[test]
fn test_anthropic_explicit_none_effort_disables_thinking_even_with_show_thinking() {
    // Regression: with `display.show_thinking = true` (the default), setting
    // effort to `none` still requested adaptive thinking, so the user kept
    // seeing reasoning on Fable/Sonnet. An explicit `none` must suppress the
    // thinking request entirely, on both adaptive and manual thinking models.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("none".to_string());

    // Adaptive-thinking model (Fable 5 / Sonnet 4.6 family).
    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, true);
    assert!(
        thinking.is_none(),
        "explicit effort=none must suppress thinking even when show_thinking is on"
    );
    assert!(output_config.is_none());
    assert_eq!(
        temperature,
        Some(1.0),
        "no thinking means the OAuth path restores temperature"
    );

    // Manual-thinking model.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-3-7-sonnet", false, true);
    assert!(
        thinking.is_none(),
        "explicit effort=none must suppress manual thinking budgets too"
    );
    assert!(output_config.is_none());
}

#[test]
fn test_anthropic_fable_defaults_to_high_effort() {
    // Fable 5 defaults to `high` reasoning when no explicit user effort is
    // configured. An explicit override still wins.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;

    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-fable-5").as_deref(),
        Some("high"),
    );

    // The default drives the request: output_config high + adaptive thinking.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, false);
    assert_eq!(
        output_config
            .expect("Fable should default to a forced output effort")
            .effort,
        "high",
    );
    match thinking.expect("Fable default effort should enable adaptive thinking") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Fable 5 should use adaptive thinking"),
    }

    // The surfaced status mirrors the effective default for the active model.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-fable-5".to_string();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("high"));

    // An explicit user override still wins over the Fable default.
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
    provider.set_reasoning_effort("none").unwrap();
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, true);
    assert!(
        thinking.is_none(),
        "explicit none must beat the high default and show_thinking"
    );
    assert!(output_config.is_none());
}

#[test]
fn test_anthropic_sonnet_5_supports_full_effort_ladder() {
    // `claude-sonnet-5` accepts `output_config` effort low..xhigh/max and
    // adaptive thinking (verified live 2026-07-07).
    assert!(AnthropicProvider::model_supports_output_effort(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_adaptive_thinking(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_xhigh_effort(
        "claude-sonnet-5"
    ));
    assert!(AnthropicProvider::model_supports_max_effort(
        "claude-sonnet-5"
    ));

    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = None;
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-sonnet-5".to_string();

    // No forced default: Sonnet keeps the model's own reasoning behavior.
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-sonnet-5"),
        None,
    );

    // Explicit efforts are accepted and drive the request.
    for effort in ["low", "medium", "high", "xhigh", "max"] {
        provider.set_reasoning_effort(effort).unwrap();
        assert_eq!(provider.reasoning_effort().as_deref(), Some(effort));
        let (thinking, output_config, _temp) =
            provider.build_reasoning_request_parts_inner("claude-sonnet-5", true, false);
        assert_eq!(
            output_config
                .expect("explicit effort should set output_config")
                .effort,
            effort,
        );
        assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
    }

    assert_eq!(
        provider.available_efforts(),
        vec![
            "none",
            "low",
            "medium",
            "high",
            "xhigh",
            "max",
            "swarm",
            "swarm-deep"
        ],
    );
}

#[test]
fn test_anthropic_opus_defaults_to_xhigh_effort() {
    // Opus is a reasoning-heavy flagship, so when the user has *not* configured
    // an explicit effort it should default to its strongest supported level
    // (`xhigh` on Opus 4.7/4.8). This drives both the request `output_config`
    // and the surfaced `reasoning_effort()` status.
    let provider = AnthropicProvider::new();
    // Clear any ambient config-provided effort so we exercise the model default.
    *provider.reasoning_effort.write().unwrap() = None;

    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-8").as_deref(),
        Some("xhigh"),
    );
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-7").as_deref(),
        Some("xhigh"),
    );
    // Older Opus does not support xhigh, so it clamps to high.
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-4-5").as_deref(),
        Some("high"),
    );
    // Non-Opus models keep the model's own default (no forced effort).
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-sonnet-4-6"),
        None,
    );

    // Even without show_thinking, Opus forces its strongest output effort.
    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-opus-4-8", true, false);
    assert_eq!(
        output_config
            .expect("Opus should default to a forced output effort")
            .effort,
        "xhigh",
    );
    match thinking.expect("Opus default effort should enable adaptive thinking") {
        ApiThinking::Adaptive { display } => assert_eq!(display, Some("summarized")),
        ApiThinking::Enabled { .. } => panic!("Opus 4.8 should use adaptive thinking"),
    }

    // The surfaced status mirrors the effective default for the active model.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-4-8".to_string();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("xhigh"));

    // An explicit user override still wins over the Opus default.
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));
}

#[test]
fn test_anthropic_show_thinking_enables_manual_thinking_without_effort() {
    // Manual-thinking models (e.g. Claude 3.7 Sonnet) need a concrete budget;
    // with only the display toggle on we fall back to the minimal budget. We use
    // a non-Opus model here because Opus now carries an implicit strongest-effort
    // default (see `test_anthropic_opus_defaults_to_xhigh_effort`). The model is
    // passed directly so this does not depend on `set_model` validation.
    let provider = AnthropicProvider::new();
    // Independent of ambient config: clear any configured effort.
    *provider.reasoning_effort.write().unwrap() = None;

    let (thinking, _output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-3-7-sonnet", false, false);
    assert!(thinking.is_none());

    let (thinking, _output_config, _temperature) =
        provider.build_reasoning_request_parts_inner("claude-3-7-sonnet", false, true);
    match thinking.expect("show_thinking should enable manual thinking") {
        ApiThinking::Enabled { budget_tokens } => assert_eq!(budget_tokens, 1_024),
        ApiThinking::Adaptive { .. } => panic!("Claude 3.7 Sonnet should use manual thinking"),
    }
}

#[test]
fn test_anthropic_max_alias_uses_strongest_real_effort() {
    // `max` is a real API level on output_config effort models.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "max"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-7", "max"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-8", "max"),
        "max"
    );
    // Manual-thinking models (no output_config) clamp max to high.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-5", "max"),
        "high"
    );
    // xhigh still clamps to high where unsupported.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "xhigh"),
        "high"
    );
    // Swarm rungs pin to the strongest supported level.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-8", "swarm"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-sonnet-4-6", "swarm-deep"),
        "max"
    );
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-4-5", "swarm"),
        "high"
    );
}

#[test]
fn test_anthropic_opus_48_fast_mode_service_tier_serializes_priority() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-8").unwrap();

    assert_eq!(provider.available_service_tiers(), vec!["off", "priority"]);
    assert_eq!(provider.service_tier(), None);

    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));

    let request = ApiRequest {
        model: strip_1m_suffix(&provider.model()).to_string(),
        max_tokens: 1024,
        system: None,
        messages: vec![],
        tools: None,
        metadata: None,
        thinking: None,
        output_config: None,
        temperature: None,
        service_tier: provider.current_service_tier_for_model(&provider.model()),
        stream: true,
    };
    let value = serde_json::to_value(&request).unwrap();

    assert_eq!(value["model"], "claude-opus-4-8");
    assert_eq!(value["service_tier"], "auto");
}

#[test]
fn test_anthropic_fast_mode_is_limited_to_opus_48() {
    let provider = AnthropicProvider::new();
    provider.set_model("claude-opus-4-6").unwrap();

    assert!(provider.available_service_tiers().is_empty());
    assert!(provider.set_service_tier("priority").is_err());
    assert_eq!(provider.service_tier(), None);

    // A stale `[1m]` alias for a native-1M model is migrated to canonical form.
    provider.set_model("claude-opus-4-8[1m]").unwrap();
    assert_eq!(provider.model(), "claude-opus-4-8");
    provider.set_service_tier("priority").unwrap();
    assert_eq!(provider.service_tier().as_deref(), Some("priority"));

    provider.set_service_tier("off").unwrap();
    assert_eq!(provider.service_tier(), None);
}

#[test]
fn test_anthropic_manual_thinking_budget_for_opus_45() {
    let provider = AnthropicProvider::new();
    // Keep this request-builder test independent of the live/persisted Anthropic
    // model catalog, which may legitimately omit older Opus 4.5 models.
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-4-5".to_string();
    provider.set_reasoning_effort("high").unwrap();

    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts("claude-opus-4-5", false);

    match thinking.expect("manual thinking should be enabled") {
        ApiThinking::Enabled { budget_tokens } => assert_eq!(budget_tokens, 8_192),
        ApiThinking::Adaptive { .. } => panic!("Claude Opus 4.5 should use manual thinking"),
    }
    assert_eq!(output_config.unwrap().effort, "high");
    assert_eq!(temperature, None);
}

#[test]
fn message_start_warns_when_server_substitutes_a_different_model() {
    // Anthropic can silently alias an unavailable model id to a different model
    // (observed: claude-fable-5 -> claude-haiku-4-5). When the served model
    // differs from the requested base id, we must surface a StatusDetail warning
    // so the user is not misled about which model answered.
    let mut state = SseStreamState {
        requested_model_base: "claude-fable-5".to_string(),
        ..SseStreamState::default()
    };
    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: serde_json::json!({
            "type": "message_start",
            "message": {"model": "claude-haiku-4-5-20251001", "usage": {"input_tokens": 1}}
        })
        .to_string(),
    };
    let events = process_sse_event(&event, &mut state, true);
    let warned = events.iter().any(|e| {
        matches!(e, StreamEvent::StatusDetail { detail }
            if detail.contains("claude-haiku-4-5") && detail.contains("claude-fable-5"))
    });
    assert!(
        warned,
        "expected a substitution StatusDetail, got {events:?}"
    );
    assert!(state.warned_model_substitution);

    // A matching served model must NOT warn.
    let mut state = SseStreamState {
        requested_model_base: "claude-opus-4-8".to_string(),
        ..SseStreamState::default()
    };
    let event = SseEvent {
        event_type: "message_start".to_string(),
        data: serde_json::json!({
            "type": "message_start",
            "message": {"model": "claude-opus-4-8", "usage": {"input_tokens": 1}}
        })
        .to_string(),
    };
    let events = process_sse_event(&event, &mut state, true);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, StreamEvent::StatusDetail { .. })),
        "served model matched request; must not warn"
    );
    assert!(!state.warned_model_substitution);
}

#[test]
fn test_anthropic_thinking_sse_events() {
    let mut state = SseStreamState::default();
    let start = SseEvent {
        event_type: "content_block_start".to_string(),
        data: serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "thinking", "thinking": "", "signature": "sig"}
        })
        .to_string(),
    };
    let events = process_sse_event(&start, &mut state, false);
    assert!(matches!(events.as_slice(), [StreamEvent::ThinkingStart]));
    assert!(state.current_thinking_block);

    let delta = SseEvent {
        event_type: "content_block_delta".to_string(),
        data: serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "reasoning text"}
        })
        .to_string(),
    };
    let events = process_sse_event(&delta, &mut state, false);
    assert!(
        matches!(events.as_slice(), [StreamEvent::ThinkingDelta(text)] if text == "reasoning text")
    );

    let signature = SseEvent {
        event_type: "content_block_delta".to_string(),
        data: serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "signature_delta", "signature": "signed"}
        })
        .to_string(),
    };
    let events = process_sse_event(&signature, &mut state, false);
    assert!(
        matches!(events.as_slice(), [StreamEvent::ThinkingSignatureDelta(sig)] if sig == "signed")
    );

    let stop = SseEvent {
        event_type: "content_block_stop".to_string(),
        data: serde_json::json!({"type": "content_block_stop", "index": 0}).to_string(),
    };
    let events = process_sse_event(&stop, &mut state, false);
    assert!(matches!(events.as_slice(), [StreamEvent::ThinkingEnd]));
    assert!(!state.current_thinking_block);
}

#[test]
fn test_anthropic_signed_thinking_replayed_in_request_blocks() {
    let provider = AnthropicProvider::new();
    let blocks = provider.format_content_blocks(
        &[ContentBlock::AnthropicThinking {
            thinking: "reasoning text".to_string(),
            signature: "signed".to_string(),
        }],
        false,
    );

    let value = serde_json::to_value(&blocks).expect("serialize content blocks");
    assert_eq!(
        value,
        serde_json::json!([
            {
                "type": "thinking",
                "thinking": "reasoning text",
                "signature": "signed"
            }
        ])
    );
}

#[tokio::test]
#[ignore = "live smoke: requires ANTHROPIC_API_KEY, or set JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH=1 to use Claude OAuth credentials"]
async fn live_anthropic_reasoning_smoke() -> Result<()> {
    let _env_lock = jcode_base::storage::lock_test_env();
    let using_api_key = std::env::var_os("ANTHROPIC_API_KEY").is_some();
    let allow_oauth = std::env::var_os("JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH").is_some();
    if !using_api_key && !allow_oauth {
        eprintln!(
            "skipping live Anthropic smoke: set ANTHROPIC_API_KEY or JCODE_LIVE_ANTHROPIC_ALLOW_OAUTH=1"
        );
        return Ok(());
    }

    let _max_tokens = EnvVarGuard::set_if_missing("JCODE_ANTHROPIC_MAX_TOKENS", "2048");
    let model = std::env::var("JCODE_LIVE_ANTHROPIC_MODEL")
        .or_else(|_| std::env::var("JCODE_ANTHROPIC_MODEL"))
        .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
    let effort = std::env::var("JCODE_LIVE_ANTHROPIC_REASONING_EFFORT")
        .unwrap_or_else(|_| "low".to_string());
    let prompt = std::env::var("JCODE_LIVE_ANTHROPIC_PROMPT")
        .unwrap_or_else(|_| "Live smoke test: answer exactly OK.".to_string());
    let system = std::env::var("JCODE_LIVE_ANTHROPIC_SYSTEM").unwrap_or_else(|_| {
        "You are a live provider smoke test. Keep the answer tiny.".to_string()
    });
    let require_thinking = std::env::var_os("JCODE_LIVE_ANTHROPIC_REQUIRE_THINKING").is_some();

    let provider = AnthropicProvider::new();
    provider.set_model(&model)?;
    // Some models (e.g. Fable 5) legitimately reject any reasoning effort. Treat
    // that as "use the model default" so the live call still exercises the model
    // rather than aborting the smoke test before any request is sent.
    if let Err(err) = provider.set_reasoning_effort(&effort) {
        eprintln!(
            "model {model} does not support reasoning effort '{effort}' ({err}); using model default"
        );
    }

    let messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: prompt,
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];

    let stream = provider.complete(&messages, &[], &system, None).await?;
    let (text_bytes, thinking_bytes, saw_message_end) =
        collect_live_smoke_stream(stream, std::time::Duration::from_secs(90)).await?;

    eprintln!(
        "live Anthropic reasoning smoke passed: model={model}, effort={effort}, text_bytes={text_bytes}, thinking_bytes={thinking_bytes}, message_end={saw_message_end}"
    );
    assert!(
        text_bytes > 0 || thinking_bytes > 0,
        "live Anthropic response contained neither text nor thinking deltas"
    );
    if require_thinking {
        assert!(
            thinking_bytes > 0,
            "live Anthropic response did not include thinking deltas despite JCODE_LIVE_ANTHROPIC_REQUIRE_THINKING"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_dangling_tool_use_repair() {
    let provider = AnthropicProvider::new();

    // Create messages with a dangling tool_use (no corresponding tool_result)
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "Let me check".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_123".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_456".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "/tmp/test"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        // Missing tool_results for tool_123 and tool_456!
    ];

    let formatted = provider.format_messages(&messages, false);

    // Should have 3 messages:
    // 1. User: "Hello"
    // 2. Assistant: text + tool_uses
    // 3. User: synthetic tool_results for the dangling tool_uses
    assert_eq!(formatted.len(), 3);

    // Check the synthetic tool_result message
    let synthetic_msg = &formatted[2];
    assert_eq!(synthetic_msg.role, "user");
    assert_eq!(synthetic_msg.content.len(), 2);

    // Verify both tool_results are present
    let mut found_ids = std::collections::HashSet::new();
    for block in &synthetic_msg.content {
        if let ApiContentBlock::ToolResult {
            tool_use_id,
            is_error,
            content,
        } = block
        {
            found_ids.insert(tool_use_id.clone());
            assert!(is_error);
            match content {
                ToolResultContent::Text(t) => assert!(t.contains("interrupted")),
                ToolResultContent::Blocks(_) => panic!("Expected text content"),
            }
        } else {
            panic!("Expected ToolResult block");
        }
    }
    assert!(found_ids.contains("tool_123"));
    assert!(found_ids.contains("tool_456"));
}

#[tokio::test]
async fn test_no_repair_when_tool_results_present() {
    let provider = AnthropicProvider::new();

    // Create messages where tool_use has a corresponding tool_result
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_123".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_123".to_string(),
                content: "file1.txt\nfile2.txt".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages, false);

    // Should have exactly 3 messages (no synthetic ones added)
    assert_eq!(formatted.len(), 3);

    // The last message should be the actual tool_result, not synthetic
    let last_msg = &formatted[2];
    if let ApiContentBlock::ToolResult { content, .. } = &last_msg.content[0] {
        match content {
            ToolResultContent::Text(t) => assert!(t.contains("file1.txt")),
            ToolResultContent::Blocks(_) => panic!("Expected text content"),
        }
    } else {
        panic!("Expected ToolResult block");
    }
}

#[tokio::test]
async fn test_parallel_image_tool_results_stay_contiguous() {
    // Regression for Anthropic 400: "`tool_use` ids were found without `tool_result`
    // blocks immediately after". When the assistant issues several parallel `read`
    // calls that return images, each tool result is stored as its own user message in
    // the form [tool_result, image, "[Attached image ...]" text]. After merging the
    // consecutive user messages, the sibling label text blocks were wedged between the
    // tool_results, which Anthropic rejects. The label must be folded into the
    // tool_result content so every tool_result stays contiguous.
    let provider = AnthropicProvider::new();

    let make_image_result = |id: &str, label: &str| Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: format!("Image: {label}"),
                is_error: None,
            },
            ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "AAAA".to_string(),
            },
            ContentBlock::Text {
                text: format!(
                    "[Attached image associated with the preceding tool result: {label}]"
                ),
                cache_control: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    };

    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "tool_a".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "a.png"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_b".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "b.png"}),
                    thought_signature: None,
                },
                ContentBlock::ToolUse {
                    id: "tool_c".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file_path": "c.png"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_image_result("tool_a", "a.png"),
        make_image_result("tool_b", "b.png"),
        make_image_result("tool_c", "c.png"),
    ];

    let formatted = provider.format_messages(&messages, false);

    // assistant message + merged user tool_result message
    assert_eq!(formatted.len(), 2);
    let user_msg = &formatted[1];
    assert_eq!(user_msg.role, "user");

    // Every block in the user message must be a tool_result (no sibling text blocks
    // wedged between them), and all three tool_use ids must be present.
    let mut seen = std::collections::HashSet::new();
    for block in &user_msg.content {
        match block {
            ApiContentBlock::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                seen.insert(tool_use_id.clone());
                // Each image tool_result should carry its image and folded label text.
                match content {
                    ToolResultContent::Blocks(blocks) => {
                        assert!(
                            blocks
                                .iter()
                                .any(|b| matches!(b, ToolResultContentBlock::Image { .. })),
                            "image tool_result should contain an image block"
                        );
                        assert!(
                            blocks.iter().any(|b| matches!(
                                b,
                                ToolResultContentBlock::Text { text }
                                    if text.contains("[Attached image associated")
                            )),
                            "label text should be folded into the tool_result content"
                        );
                    }
                    ToolResultContent::Text(_) => {
                        panic!("image tool_result should use block content")
                    }
                }
            }
            _ => panic!("expected only tool_result blocks in the user message"),
        }
    }
    assert_eq!(
        seen,
        ["tool_a", "tool_b", "tool_c"]
            .iter()
            .map(|s| s.to_string())
            .collect::<std::collections::HashSet<_>>()
    );
}

#[test]
fn test_cache_breakpoint_no_messages() {
    let mut messages: Vec<ApiMessage> = vec![];
    add_message_cache_breakpoint(&mut messages);
    // Should not panic, just return early
    assert!(messages.is_empty());
}

#[cfg(test)]
#[path = "anthropic_more_tests.rs"]
mod more;
