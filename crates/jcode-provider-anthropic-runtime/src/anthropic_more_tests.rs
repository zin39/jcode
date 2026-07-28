//! More Anthropic runtime tests, continued from anthropic_tests.rs.
//!
//! Split for the test-size ratchet; shares fixtures via `use super::*`.

use super::*;

#[test]
fn test_cache_breakpoint_too_few_messages() {
    let mut messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "World".to_string(),
                cache_control: None,
            }],
        },
    ];
    add_message_cache_breakpoint(&mut messages);
    // With only 2 messages, should not add cache control
    for msg in &messages {
        for block in &msg.content {
            if let ApiContentBlock::Text { cache_control, .. } = block {
                assert!(cache_control.is_none());
            }
        }
    }
}

#[test]
fn test_cache_breakpoint_adds_to_assistant_message() {
    let mut messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Identity".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "assistant".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hi there!".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "How are you?".to_string(),
                cache_control: None,
            }],
        },
    ];

    add_message_cache_breakpoint(&mut messages);

    // Assistant message (index 2) should have cache_control
    if let ApiContentBlock::Text { cache_control, .. } = &messages[2].content[0] {
        assert!(cache_control.is_some());
    } else {
        panic!("Expected Text block");
    }

    // Other messages should NOT have cache_control
    for (i, msg) in messages.iter().enumerate() {
        if i == 2 {
            continue; // Skip the assistant message we just checked
        }
        for block in &msg.content {
            if let ApiContentBlock::Text { cache_control, .. } = block {
                assert!(
                    cache_control.is_none(),
                    "Message {} should not have cache_control",
                    i
                );
            }
        }
    }
}

#[test]
fn test_cache_breakpoint_finds_text_in_mixed_content() {
    // Assistant message with tool_use followed by text
    let mut messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Identity".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Run a command".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "assistant".to_string(),
            content: vec![
                ApiContentBlock::Text {
                    text: "Running command...".to_string(),
                    cache_control: None,
                },
                ApiContentBlock::ToolUse {
                    id: "tool_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                    cache_control: None,
                },
            ],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Thanks".to_string(),
                cache_control: None,
            }],
        },
    ];

    add_message_cache_breakpoint(&mut messages);

    // The last block (ToolUse) in the assistant message should have cache_control
    // (we prefer the last block for maximum cache coverage)
    let assistant_msg = &messages[2];
    let has_cached_block = assistant_msg.content.iter().any(|block| {
        matches!(
            block,
            ApiContentBlock::ToolUse {
                cache_control: Some(_),
                ..
            }
        )
    });
    assert!(
        has_cached_block,
        "Should have added cache_control to last block (ToolUse) in assistant message"
    );
}

#[test]
fn test_system_param_split_oauth() {
    let static_content = "This is static content";
    let dynamic_content = "This is dynamic content";

    let result = build_system_param_split(static_content, dynamic_content, true);

    if let Some(ApiSystem::Blocks(blocks)) = result {
        // Should have 4 blocks: identity, notice, static (cached), dynamic (not cached)
        assert_eq!(blocks.len(), 4);

        // Block 0: identity (no cache)
        assert!(blocks[0].cache_control.is_none());

        // Block 1: notice (no cache)
        assert!(blocks[1].cache_control.is_none());

        // Block 2: static (cached)
        assert!(blocks[2].cache_control.is_some());
        assert!(blocks[2].text.contains("static"));

        // Block 3: dynamic (not cached)
        assert!(blocks[3].cache_control.is_none());
        assert!(blocks[3].text.contains("dynamic"));
    } else {
        panic!("Expected Blocks variant");
    }
}

#[test]
fn test_system_param_split_non_oauth() {
    let static_content = "This is static content";
    let dynamic_content = "This is dynamic content";

    let result = build_system_param_split(static_content, dynamic_content, false);

    if let Some(ApiSystem::Blocks(blocks)) = result {
        // Should have 2 blocks: static (cached), dynamic (not cached)
        assert_eq!(blocks.len(), 2);

        // Block 0: static (cached)
        assert!(blocks[0].cache_control.is_some());

        // Block 1: dynamic (not cached)
        assert!(blocks[1].cache_control.is_none());
    } else {
        panic!("Expected Blocks variant");
    }
}

// --- Cross-turn cache correctness tests ---
// These tests verify the two-marker sliding-window strategy that allows each turn
// to READ from the previous turn's conversation cache.

fn count_message_cache_breakpoints(messages: &[ApiMessage]) -> usize {
    messages
        .iter()
        .flat_map(|m| &m.content)
        .filter(|b| {
            matches!(
                b,
                ApiContentBlock::Text {
                    cache_control: Some(_),
                    ..
                } | ApiContentBlock::ToolUse {
                    cache_control: Some(_),
                    ..
                }
            )
        })
        .count()
}

fn cached_message_indices(messages: &[ApiMessage]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.content.iter().any(|b| {
                matches!(
                    b,
                    ApiContentBlock::Text {
                        cache_control: Some(_),
                        ..
                    } | ApiContentBlock::ToolUse {
                        cache_control: Some(_),
                        ..
                    }
                )
            })
        })
        .map(|(i, _)| i)
        .collect()
}

/// Helper to build a minimal conversation with N exchanges (user→assistant pairs).
/// Returns messages suitable for add_message_cache_breakpoint (includes a trailing user msg).
fn build_conversation(exchanges: usize) -> Vec<ApiMessage> {
    let mut messages = vec![ApiMessage {
        role: "user".to_string(),
        content: vec![ApiContentBlock::Text {
            text: "identity".to_string(),
            cache_control: None,
        }],
    }];
    for i in 0..exchanges {
        messages.push(ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: format!("Question {}", i + 1),
                cache_control: None,
            }],
        });
        messages.push(ApiMessage {
            role: "assistant".to_string(),
            content: vec![ApiContentBlock::Text {
                text: format!("Answer {}", i + 1),
                cache_control: None,
            }],
        });
    }
    // Trailing user message (the current turn's input)
    messages.push(ApiMessage {
        role: "user".to_string(),
        content: vec![ApiContentBlock::Text {
            text: format!("Question {}", exchanges + 1),
            cache_control: None,
        }],
    });
    messages
}

#[test]
fn test_cache_one_exchange_single_marker() {
    // Turn 2: only one assistant reply exists → one marker (WRITE only)
    let mut messages = build_conversation(1);
    add_message_cache_breakpoint(&mut messages);

    let indices = cached_message_indices(&messages);
    assert_eq!(indices.len(), 1, "One assistant message → one cache marker");
    // The assistant message is at index 2 (identity=0, user=1, assistant=2, user=3)
    assert_eq!(indices[0], 2);
}

#[test]
fn test_cache_two_exchanges_two_markers() {
    // Turn 3: two assistant replies → two markers (READ prev + WRITE new)
    let mut messages = build_conversation(2);
    // identity=0, user=1, assistant=2, user=3, assistant=4, user=5
    add_message_cache_breakpoint(&mut messages);

    let indices = cached_message_indices(&messages);
    assert_eq!(
        indices.len(),
        2,
        "Two assistant messages → two cache markers"
    );
    assert!(
        indices.contains(&2),
        "Second-to-last assistant (READ marker) at index 2"
    );
    assert!(
        indices.contains(&4),
        "Last assistant (WRITE marker) at index 4"
    );
}

#[test]
fn test_cache_many_exchanges_still_two_markers() {
    // 10 exchanges → still only 2 markers (within the 4-breakpoint API limit)
    let mut messages = build_conversation(10);
    add_message_cache_breakpoint(&mut messages);

    let count = count_message_cache_breakpoints(&messages);
    assert_eq!(
        count, 2,
        "Should always place exactly 2 markers regardless of conversation length"
    );
}

#[test]
fn test_cache_cross_turn_read_marker_preserved() {
    // THE KEY REGRESSION TEST: simulates turn N → turn N+1 and verifies that the
    // assistant message from turn N still has cache_control in the turn N+1 request.
    // Without this, the turn N cache snapshot is written but never read.

    // Turn 2: one assistant reply
    let mut turn2 = build_conversation(1);
    // identity=0, user=1, assistant=2, user=3
    add_message_cache_breakpoint(&mut turn2);
    let turn2_cached = cached_message_indices(&turn2);
    assert_eq!(
        turn2_cached,
        vec![2],
        "Turn 2: cache marker at assistant index 2"
    );

    // The content of the assistant message from turn 2 (what gets written to cache)
    let cached_text = match &turn2[2].content[0] {
        ApiContentBlock::Text { text, .. } => text.clone(),
        _ => panic!("Expected text block"),
    };

    // Turn 3: same conversation + one more exchange (assistant[2] is now second-to-last)
    let mut turn3 = build_conversation(2);
    // identity=0, user=1, assistant=2(same as before), user=3, assistant=4(new), user=5
    add_message_cache_breakpoint(&mut turn3);
    let turn3_cached = cached_message_indices(&turn3);

    // CRITICAL: assistant at index 2 MUST still have cache_control in turn 3,
    // so Anthropic can serve a cache READ hit for the turn-2 snapshot.
    assert!(
        turn3_cached.contains(&2),
        "Turn 3 MUST keep cache_control on the turn-2 assistant message (index 2) \
             so Anthropic can serve a cache_read hit. Without this, turn-2's cache is \
             written but never read, wasting cache_creation tokens every turn."
    );
    assert!(
        turn3_cached.contains(&4),
        "Turn 3 must add cache_control on the new assistant message (index 4) to \
             write a fresh cache snapshot for turn 4 to read"
    );

    // Verify it's actually the same content (same assistant message, not a different one)
    match &turn3[2].content[0] {
        ApiContentBlock::Text {
            text,
            cache_control,
        } => {
            assert_eq!(text, &cached_text);
            assert!(cache_control.is_some(), "Must have cache_control set");
        }
        _ => panic!("Expected text block"),
    }
}

#[test]
fn test_cache_non_oauth_path_gets_breakpoints() {
    // Non-OAuth path should now also get conversation cache breakpoints
    // (previously it returned early without calling add_message_cache_breakpoint)
    let messages = vec![
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hello".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "assistant".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Hi there!".to_string(),
                cache_control: None,
            }],
        },
        ApiMessage {
            role: "user".to_string(),
            content: vec![ApiContentBlock::Text {
                text: "Follow-up".to_string(),
                cache_control: None,
            }],
        },
    ];

    let result = format_messages_with_identity(messages, false);
    let indices = cached_message_indices(&result);
    assert_eq!(
        indices,
        vec![1],
        "Non-OAuth path should add cache breakpoint to assistant message"
    );
}

#[test]
fn test_cache_total_breakpoints_within_api_limit() {
    // Anthropic allows at most 4 cache_control parameters per request total
    // (system blocks + tool definitions + message blocks).
    // System: 1 (static block) + Tools: 1 (last tool) + Messages: up to 2 = 4 max.
    // This test verifies messages never exceed 2 breakpoints.
    for exchanges in 1..=20 {
        let mut messages = build_conversation(exchanges);
        add_message_cache_breakpoint(&mut messages);
        let count = count_message_cache_breakpoints(&messages);
        assert!(
            count <= 2,
            "Conversation with {} exchanges produced {} message breakpoints, exceeding \
                 the 2-message budget (system+tools use the other 2 of Anthropic's 4-limit)",
            exchanges,
            count
        );
    }
}

#[tokio::test]
async fn test_sanitize_tool_ids_with_dots() {
    let provider = AnthropicProvider::new();

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
                id: "chatcmpl-BF2xX.tool_call.0".to_string(),
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
                tool_use_id: "chatcmpl-BF2xX.tool_call.0".to_string(),
                content: "file1.txt".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages, false);

    let sanitized_id = "chatcmpl-BF2xX_tool_call_0";
    for msg in &formatted {
        for block in &msg.content {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    assert_eq!(id, sanitized_id);
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, sanitized_id);
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn test_sanitize_dangling_tool_ids_with_dots() {
    let provider = AnthropicProvider::new();

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
                id: "call.with.dots".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "crash"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
    ];

    let formatted = provider.format_messages(&messages, false);

    let sanitized_id = "call_with_dots";
    for msg in &formatted {
        for block in &msg.content {
            match block {
                ApiContentBlock::ToolUse { id, .. } => {
                    assert_eq!(id, sanitized_id);
                }
                ApiContentBlock::ToolResult { tool_use_id, .. } => {
                    assert_eq!(tool_use_id, sanitized_id);
                }
                _ => {}
            }
        }
    }
}

/// The runtime-provider identity that `set_credential_mode` writes must decode
/// back to the exact same credential mode. This guards the model picker / header
/// widget from reporting OAuth when an API key is in use (or vice versa): the
/// env key is the single source of truth those surfaces read, so an asymmetric
/// mapping here would surface an inaccurate auth method to the user.
#[test]
fn credential_mode_runtime_provider_identity_round_trips() {
    let _guard = jcode_base::storage::lock_test_env();
    let previous = std::env::var_os("JCODE_RUNTIME_PROVIDER");

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "claude");
    assert_eq!(
        AnthropicCredentialMode::from_runtime_env(jcode_provider_core::DualAuthProvider::Anthropic),
        AnthropicCredentialMode::OAuth,
        "OAuth selection must surface as the OAuth runtime identity"
    );

    jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", "claude-api");
    assert_eq!(
        AnthropicCredentialMode::from_runtime_env(jcode_provider_core::DualAuthProvider::Anthropic),
        AnthropicCredentialMode::ApiKey,
        "API-key selection must surface as the API-key runtime identity"
    );

    match previous {
        Some(value) => jcode_base::env::set_var("JCODE_RUNTIME_PROVIDER", value),
        None => jcode_base::env::remove_var("JCODE_RUNTIME_PROVIDER"),
    }
}

#[tokio::test]
async fn auto_mode_falls_back_to_api_key_when_oauth_is_expired() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key");
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "auto");

    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "expired-oauth-access".to_string(),
        refresh: String::new(),
        expires: 0,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();

    let provider = AnthropicProvider::new();
    assert_eq!(
        provider.credential_mode_snapshot(),
        AnthropicCredentialMode::Auto
    );

    let (token, is_oauth) = provider.get_access_token().await.unwrap();
    assert_eq!(token, "test-anthropic-api-key");
    assert!(
        !is_oauth,
        "automatic fallback must use API-key request semantics"
    );
}

#[tokio::test]
async fn explicit_oauth_mode_does_not_silently_fall_back_to_api_key() {
    let _guard = jcode_base::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let _home = EnvVarGuard::set("JCODE_HOME", temp.path());
    let _api_key = EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-api-key");
    let _runtime = EnvVarGuard::set("JCODE_RUNTIME_PROVIDER", "claude");

    jcode_base::auth::claude::upsert_account(jcode_base::auth::claude::AnthropicAccount {
        label: "claude-1".to_string(),
        access: "expired-oauth-access".to_string(),
        refresh: String::new(),
        expires: 0,
        email: None,
        subscription_type: Some("max".to_string()),
        scopes: vec!["user:inference".to_string()],
    })
    .unwrap();

    let provider = AnthropicProvider::new();
    assert_eq!(
        provider.credential_mode_snapshot(),
        AnthropicCredentialMode::OAuth
    );

    let error = provider.get_access_token().await.unwrap_err().to_string();
    assert!(error.contains("expired"), "unexpected error: {error}");
}

#[test]
fn test_anthropic_fable_5_sends_reasoning_fields() {
    // `claude-fable-5` rejected reasoning fields during its preview, but the
    // released model accepts an adaptive `thinking` block and an
    // `output_config` effort (verified live 2026-07-01). The request builder
    // must send both when an effort is configured.
    let provider = AnthropicProvider::new();
    *provider.reasoning_effort.write().unwrap() = Some("high".to_string());

    let (thinking, output_config, temperature) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, false);
    assert!(
        matches!(thinking, Some(ApiThinking::Adaptive { .. })),
        "Fable 5 should send an adaptive thinking block"
    );
    assert_eq!(
        output_config.as_ref().map(|c| c.effort.as_str()),
        Some("high"),
        "Fable 5 should send the configured output_config effort"
    );
    assert_eq!(temperature, None);

    // Fable 5 supports the real `max` API level, so `max` is sent verbatim.
    *provider.reasoning_effort.write().unwrap() = Some("max".to_string());
    let (_thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-fable-5", true, false);
    assert_eq!(
        output_config.as_ref().map(|c| c.effort.as_str()),
        Some("max")
    );

    // The effort picker surfaces levels for Fable 5.
    assert!(AnthropicProvider::model_supports_reasoning_effort(
        "claude-fable-5"
    ));
}

#[test]
fn detects_anthropic_reasoning_unsupported_errors() {
    // The real 400 bodies returned when Fable 5 is sent reasoning fields.
    let thinking_400 = "anthropic api error (400 bad request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"adaptive thinking is not supported on this model\"}}";
    assert!(is_reasoning_unsupported_error(thinking_400));
    let effort_400 = "anthropic api error (400 bad request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"this model does not support the effort parameter.\"}}";
    assert!(is_reasoning_unsupported_error(effort_400));

    // Unrelated 400s must not trigger the reasoning self-heal path.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (400 bad request): {\"type\":\"invalid_request_error\",\"message\":\"max_tokens too large\"}"
    ));
    // A thinking-mentioning error that is not a 400 must not match either.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (429 too many requests): rate_limit on thinking budget"
    ));
    // Model-not-found is a different recovery path.
    assert!(!is_reasoning_unsupported_error(
        "anthropic api error (404 not found): {\"type\":\"not_found_error\",\"message\":\"model not found\"}"
    ));
}

#[test]
fn detects_anthropic_model_not_found_errors() {
    // The real 404 body returned when a model id was retired (e.g. Fable 5).
    let real = "anthropic api error (404 not found): {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"claude fable 5 is not available. please use opus 4.8.\"}}";
    assert!(is_model_not_found_error(real));

    // Structural marker alone (lowercased error chain).
    assert!(is_model_not_found_error(
        "model claude-foo not found (not_found_error)"
    ));

    // Unrelated failures must not trigger the model fallback path.
    assert!(!is_model_not_found_error(
        "anthropic api error (401 unauthorized): invalid authentication credentials"
    ));
    assert!(!is_model_not_found_error(
        "anthropic api error (429 too many requests): rate_limit"
    ));
    assert!(!is_model_not_found_error(
        "anthropic api error (404 not found): resource missing"
    ));
}

#[test]
fn out_of_credits_429_is_not_retryable() {
    // The real 429 body returned when the usage-credit allowance is exhausted
    // (e.g. Fable). It is a `rate_limit_error` but permanently non-retryable.
    let real = "anthropic api error (429 too many requests): {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"usage credits are required for this model.\",\"details\":{\"error_code\":\"credits_required\",\"disabled_reason\":\"out_of_credits\"}}}";
    assert!(
        is_out_of_credits_error(real),
        "should detect the out-of-credits detail"
    );
    assert!(
        !is_retryable_error(real),
        "out-of-credits must not be retried despite the 429/rate_limit markers"
    );

    // Each marker on its own is sufficient (error chains vary by transport).
    assert!(is_out_of_credits_error("... out_of_credits ..."));
    assert!(is_out_of_credits_error("... credits_required ..."));
    assert!(is_out_of_credits_error("you're out of usage credits"));

    // A genuine transient rate limit is still retryable.
    assert!(is_retryable_error(
        "anthropic api error (429 too many requests): {\"type\":\"rate_limit_error\",\"message\":\"rate limit exceeded, please retry\"}"
    ));
}

#[test]
fn anthropic_fallback_prefers_best_available_and_skips_tried_and_retired() {
    // The fallback logic reads the process-global model catalog; lock and
    // reset it so fixture models hydrated by other tests cannot leak in.
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::provider::models::reset_model_catalog_services_for_tests();
    let known = jcode_base::provider::known_anthropic_model_ids();
    assert!(
        !known.is_empty(),
        "expected a non-empty Anthropic model catalog"
    );

    // With nothing tried, the fallback offers the highest-quality (flagship)
    // model, NOT merely the first catalog entry. The curated order ranks Opus
    // ahead of Haiku, so the chosen model must not be a Haiku/retired tier when
    // a stronger one exists.
    let first = anthropic_fallback_model(&[], "").expect("a fallback should exist");
    let first_key = AnthropicProvider::normalized_model_key(&first);
    assert!(
        !first_key.contains("haiku"),
        "fallback must not downgrade to Haiku when a flagship is available, got {first}"
    );
    assert!(
        !anthropic_model_is_retired(&first),
        "fallback must never pick a retired model, got {first}"
    );

    // A retired model in `tried` must never be re-offered, and the result must
    // skip retired families entirely.
    let next = anthropic_fallback_model(&["claude-mythos-1".to_string()], "")
        .expect("another fallback should exist");
    assert!(!anthropic_model_is_retired(&next));

    // Exhausting every viable known model yields None.
    let exhausted = anthropic_fallback_model(&known, "");
    assert!(
        exhausted.is_none(),
        "no fallback should remain once all known models are tried, got {exhausted:?}"
    );
}

#[test]
fn anthropic_fallback_honors_server_recommendation() {
    // The recommendation matcher scores hints against the process-global model
    // catalog; lock and reset it so fixture models hydrated by other tests
    // (e.g. claude-opus-5-preview) cannot outrank the real catalog entries.
    let _guard = jcode_base::storage::lock_test_env();
    jcode_base::provider::models::reset_model_catalog_services_for_tests();
    // The real 404 body recommends a specific replacement model. We must honor
    // it over the generic quality ranking.
    let body = "anthropic api error (404 not found): {\"type\":\"error\",\"error\":{\"type\":\"not_found_error\",\"message\":\"claude fable 5 is not available. please use opus 4.8. learn more: https://anthropic.com\"}}";
    let recommended =
        anthropic_recommended_model_from_error(body).expect("should parse a recommendation");
    assert_eq!(
        AnthropicProvider::normalized_model_key(&recommended),
        "claude-opus-4-8",
        "server recommendation 'Opus 4.8' should map to claude-opus-4-8"
    );

    // The full fallback also returns the recommended model.
    let fallback = anthropic_fallback_model(&["claude-mythos-1".to_string()], body)
        .expect("a fallback should exist");
    assert_eq!(
        AnthropicProvider::normalized_model_key(&fallback),
        "claude-opus-4-8"
    );

    // A recommendation pointing at a retired model is ignored (falls through to
    // quality ranking).
    let retired_rec = "model x not available. please use mythos 1.";
    assert!(
        anthropic_recommended_model_from_error(retired_rec).is_none()
            || !anthropic_model_is_retired(
                &anthropic_recommended_model_from_error(retired_rec).unwrap()
            )
    );

    // No recommendation phrase -> None.
    assert!(anthropic_recommended_model_from_error("429 too many requests").is_none());
}

#[test]
fn anthropic_quality_rank_orders_opus_before_haiku_and_retired_last() {
    let opus = anthropic_model_quality_rank("claude-opus-4-8");
    let sonnet = anthropic_model_quality_rank("claude-sonnet-4-6");
    let haiku = anthropic_model_quality_rank("claude-haiku-4-5");
    let retired = anthropic_model_quality_rank("claude-mythos-1");
    // Fable 5 is live again and curated as the flagship, so it ranks first.
    let fable = anthropic_model_quality_rank("claude-fable-5");
    assert!(
        fable <= opus,
        "Fable 5 should rank at or ahead of Opus ({fable} vs {opus})"
    );
    assert!(
        opus < sonnet,
        "Opus should outrank Sonnet ({opus} vs {sonnet})"
    );
    assert!(
        sonnet < haiku,
        "Sonnet should outrank Haiku ({sonnet} vs {haiku})"
    );
    assert!(
        haiku < retired,
        "retired models must sort last ({haiku} vs {retired})"
    );
    assert_eq!(retired, usize::MAX);
    // Dated live ids must rank like their canonical base.
    assert_eq!(
        anthropic_model_quality_rank("claude-haiku-4-5-20251001"),
        haiku
    );
}

#[test]
fn ping_keepalive_emits_streaming_phase_event() {
    // Issue #451: during silent reasoning phases, `ping` events can be the
    // only upstream traffic. They must surface as a StreamEvent so the client
    // stall guard sees activity instead of cancelling a healthy stream.
    let mut state = SseStreamState::default();
    let event = SseEvent {
        event_type: "ping".to_string(),
        data: r#"{"type": "ping"}"#.to_string(),
    };
    let events = process_sse_event(&event, &mut state, true);
    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ConnectionPhase {
                phase: jcode_message_types::ConnectionPhase::Streaming
            }
        )),
        "expected ping to emit a Streaming ConnectionPhase event, got {events:?}"
    );
}

#[test]
fn test_anthropic_opus_5_low_effort_reaches_the_wire() {
    // Benchmark campaigns pin `claude-opus-5` at `low` effort. Opus defaults to
    // `xhigh`, so an explicit `low` must survive normalization, must NOT be
    // silently promoted, and must land in `output_config.effort` on the request.
    assert!(AnthropicProvider::model_supports_output_effort(
        "claude-opus-5"
    ));
    assert_eq!(
        AnthropicProvider::default_reasoning_effort_for_model("claude-opus-5").as_deref(),
        Some("xhigh"),
    );
    assert_eq!(
        AnthropicProvider::normalize_reasoning_effort("low").as_deref(),
        Some("low"),
    );
    // Downward selection is never clamped upward toward the model default.
    assert_eq!(
        AnthropicProvider::actual_effort_for_model("claude-opus-5", "low"),
        "low",
    );
    assert_eq!(
        AnthropicProvider::store_effort_for_model("claude-opus-5", "low"),
        "low",
    );

    let provider = AnthropicProvider::new();
    *provider
        .model
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = "claude-opus-5".to_string();
    provider.set_reasoning_effort("low").unwrap();
    assert_eq!(provider.reasoning_effort().as_deref(), Some("low"));

    let (thinking, output_config, _temp) =
        provider.build_reasoning_request_parts_inner("claude-opus-5", true, false);
    assert_eq!(
        output_config
            .expect("explicit low effort should set output_config")
            .effort,
        "low",
    );
    // Opus 5 rejects `thinking.type.enabled`; it requires adaptive thinking.
    assert!(matches!(thinking, Some(ApiThinking::Adaptive { .. })));
}

// ── is_oauth_org_policy_error classifier tests ──────────────────────────

#[test]
fn test_is_oauth_org_policy_error_matches_real_error() {
    // The exact error string that stream_response produces for the org-policy 403.
    // error_str is lowercased before classification.
    let error = "anthropic api error (403 forbidden): {\"type\":\"error\",\"error\":{\"type\":\"permission_error\",\"message\":\"oauth authentication is currently not allowed for this organization.\"}}";
    assert!(
        is_oauth_org_policy_error(error),
        "must match the real Anthropic org-policy 403 response"
    );
}

#[test]
fn test_is_oauth_org_policy_error_matches_snake_case_code() {
    // If Anthropic returns the snake_case machine-readable code instead.
    let error = "anthropic api error (403 forbidden): oauth_not_allowed_for_organization";
    assert!(is_oauth_org_policy_error(error));
}

#[test]
fn test_is_oauth_org_policy_error_rejects_normal_oauth_failures() {
    // Expired token is NOT an org-policy error; is_oauth_auth_error handles it.
    assert!(!is_oauth_org_policy_error(
        "anthropic api error (401 unauthorized): oauth token has expired"
    ));

    // Generic 401 is not an org-policy error.
    assert!(!is_oauth_org_policy_error(
        "anthropic api error (401 unauthorized): invalid token"
    ));

    // 403 without the org-policy message is not an org-policy error.
    assert!(!is_oauth_org_policy_error(
        "anthropic api error (403 forbidden): insufficient permissions"
    ));

    // Network error is not an org-policy error.
    assert!(!is_oauth_org_policy_error(
        "failed to send request to anthropic api: connection reset"
    ));
}

#[test]
fn test_is_oauth_org_policy_error_not_triggered_by_200_body() {
    // A successful response body containing "403" as page content is not an error.
    assert!(!is_oauth_org_policy_error(
        "http 200 ok with 403 bytes of content"
    ));
}

#[test]
fn test_catalog_classifier_excludes_org_policy_error() {
    // The catalog path force-refreshes the token whenever this returns true.
    // An org-policy 403 is not refreshable, so it must be excluded — otherwise
    // the real cause is masked by a "failed to refresh token" error.
    let org_policy = "Anthropic API error (403 Forbidden): {\"error\":{\"message\":\"OAuth authentication is currently not allowed for this organization.\"}}";
    assert!(is_oauth_org_policy_error(&org_policy.to_lowercase()));
    assert!(
        !is_oauth_catalog_auth_error(org_policy),
        "org-policy 403 must not trigger a pointless catalog token refresh"
    );

    // A plain 403/401 without the org-policy marker still triggers a refresh.
    assert!(is_oauth_catalog_auth_error(
        "Anthropic API error (403 Forbidden): insufficient permissions"
    ));
    assert!(is_oauth_catalog_auth_error(
        "Anthropic API error (401 Unauthorized): oauth token has expired"
    ));
}
