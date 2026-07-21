use super::*;
use crate::provider::{EventStream, Provider};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct MockSummaryProvider;

#[async_trait::async_trait]
impl Provider for MockSummaryProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[crate::message::ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "mock-summary"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockSummaryProvider)
    }

    async fn complete_simple(&self, prompt: &str, _system: &str) -> Result<String> {
        Ok(format!("summary({} chars)", prompt.len()))
    }
}

fn make_text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

#[test]
fn test_new_manager() {
    let manager = CompactionManager::new();
    assert_eq!(manager.compacted_count, 0);
    assert!(manager.active_summary.is_none());
    assert!(!manager.is_compacting());
}

#[test]
fn test_notify_message_added() {
    let mut manager = CompactionManager::new();
    manager.notify_message_added();
    manager.notify_message_added();
    assert_eq!(manager.total_turns, 2);
}

#[test]
fn test_restored_messages_do_not_trigger_compaction_immediately() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("restored {}", i)));
    }
    manager.seed_restored_messages(messages.len());
    manager.update_observed_input_tokens(900);

    assert!(
        !manager.should_compact_with(&messages),
        "restored history should not compact until a new message is added"
    );
}

#[test]
fn test_new_message_after_restore_reenables_compaction() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("restored {}", i)));
    }
    manager.seed_restored_messages(messages.len());
    manager.update_observed_input_tokens(900);
    assert!(!manager.should_compact_with(&messages));

    messages.push(make_text_message(Role::User, "new turn after restore"));
    manager.notify_message_added();

    assert!(
        manager.should_compact_with(&messages),
        "compaction should resume once a genuinely new message is added"
    );
}

#[test]
fn test_token_estimate() {
    let manager = CompactionManager::new();
    // 100 chars = ~25 tokens (plus 18k overhead for full budget)
    let messages = vec![make_text_message(Role::User, &"x".repeat(100))];
    let estimate = manager.token_estimate_with(&messages);
    // With DEFAULT_TOKEN_BUDGET and 18k overhead: 25 + 18000 = 18025
    assert!((18_000..19_000).contains(&estimate));
}

#[test]
fn test_should_compact() {
    let mut manager = CompactionManager::new().with_budget(100); // Very small budget

    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("Message {} with some content", i),
        ));
        manager.notify_message_added();
    }

    assert!(manager.should_compact_with(&messages));
}

#[test]
fn test_context_usage_prefers_observed_tokens() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let messages = vec![make_text_message(Role::User, "short message")];
    manager.notify_message_added();
    manager.update_observed_input_tokens(900);

    assert!(manager.context_usage_with(&messages) >= 0.90);
    assert!(manager.effective_token_count_with(&messages) >= 900);
}

#[test]
fn test_should_compact_uses_observed_tokens() {
    let mut manager = CompactionManager::new().with_budget(1_000);

    let mut messages = Vec::new();
    for _ in 0..12 {
        messages.push(make_text_message(Role::User, "x"));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(850);

    assert!(manager.should_compact_with(&messages));
}

#[test]
fn test_messages_for_api_no_summary() {
    let mut manager = CompactionManager::new();
    let messages = vec![
        make_text_message(Role::User, "Hello"),
        make_text_message(Role::Assistant, "Hi!"),
    ];
    manager.notify_message_added();
    manager.notify_message_added();

    let msgs = manager.messages_for_api_with(&messages);
    assert_eq!(msgs.len(), 2);
}

#[tokio::test]
async fn test_force_compact_applies_summary() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("Turn {} {}", i, "x".repeat(120)),
        ));
        manager.notify_message_added();
    }

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager
        .force_compact_with(&messages, provider)
        .expect("manual compaction should start");

    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction();
        if manager.stats().has_summary {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        manager.stats().has_summary,
        "summary should be applied after compaction task completes"
    );

    // After compaction, compacted_count should be > 0
    assert!(manager.compacted_count > 0);

    let msgs = manager.messages_for_api_with(&messages);
    assert!(msgs.len() < 30);
    let first = msgs.first().expect("summary message missing");
    assert_eq!(first.role, Role::User);
    match &first.content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
        }
        _ => panic!("expected text summary block"),
    }
}

// ── ensure_context_fits tests ──────────────────────────────

#[tokio::test]
async fn test_guard_below_80_does_nothing() {
    let mut manager = CompactionManager::new().with_budget(10_000);
    let mut messages = Vec::new();
    for i in 0..15 {
        messages.push(make_text_message(Role::User, &format!("msg {}", i)));
        manager.notify_message_added();
    }
    // Char estimate is tiny, observed tokens well below 80%
    manager.update_observed_input_tokens(5_000);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert_eq!(
        action,
        CompactionAction::None,
        "should do nothing below 80%"
    );
    assert!(
        !manager.is_compacting(),
        "should NOT start background compaction below 80%"
    );
    assert_eq!(manager.compacted_count, 0);
}

#[tokio::test]
async fn test_guard_between_80_and_95_starts_background_only() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("msg {}", i)));
        manager.notify_message_added();
    }
    // 85% usage — above 80% threshold but below 95% critical
    manager.update_observed_input_tokens(850);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert_eq!(
        action,
        CompactionAction::BackgroundStarted {
            trigger: "reactive".to_string()
        },
        "should start background compaction at 85%"
    );
    assert!(
        manager.is_compacting(),
        "SHOULD start background compaction at 85%"
    );
    assert_eq!(
        manager.compacted_count, 0,
        "compacted_count should stay 0 (no hard compact)"
    );
}

/// Regression: a hard compact that runs while a background (reactive)
/// compaction is in flight must abort the background task and discard its
/// stale `pending_cutoff`. Otherwise, when the background task completes,
/// `check_and_apply_compaction_with` adds the stale cutoff on top of the
/// already-advanced `compacted_count`, double-compacting and wiping out all
/// live messages (observed as "kept 0 recent messages").
#[tokio::test]
async fn test_hard_compact_aborts_inflight_background_compaction() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "z".repeat(60)),
        ));
        manager.notify_message_added();
    }

    // Start a background reactive compaction (85% usage, below critical).
    manager.update_observed_input_tokens(850);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager.maybe_start_compaction_with(&messages, provider);
    assert!(
        manager.is_compacting(),
        "background compaction should be in flight"
    );
    let inflight_cutoff = manager.pending_cutoff;
    assert!(inflight_cutoff > 0, "background task should have a cutoff");

    // Now pressure spikes to critical and we hard-compact synchronously while
    // the background task is still pending.
    let dropped = manager
        .hard_compact_with(&messages)
        .expect("hard compact should succeed");
    assert!(dropped > 0);

    // The in-flight background compaction must have been aborted/discarded.
    assert!(
        !manager.is_compacting(),
        "hard compact must abort the in-flight background compaction"
    );
    assert_eq!(
        manager.pending_cutoff, 0,
        "stale pending_cutoff must be reset"
    );

    let compacted_after_hard = manager.compacted_count;

    // Simulate the (now-aborted) background task completion path. With the fix
    // there is no pending task, so this is a no-op and must NOT advance
    // compacted_count again.
    manager.check_and_apply_compaction_with(&messages);
    assert_eq!(
        manager.compacted_count, compacted_after_hard,
        "completing after abort must not double-advance compacted_count"
    );

    // Live messages must survive: active_messages_count stays positive.
    assert!(
        manager.active_messages_count() > 0,
        "must keep recent messages live, not wipe everything to 0"
    );
    let active = manager.active_messages(&messages);
    assert!(
        !active.is_empty(),
        "active message slice must not be empty after hard compact"
    );
}

/// Defense-in-depth: if `compacted_count` advances while a background
/// compaction is in flight (so its `pending_cutoff` becomes stale), applying
/// the completed result must NOT over-advance `compacted_count` and wipe the
/// live tail. The stale result should be discarded instead.
#[tokio::test]
async fn test_stale_background_result_discarded_when_context_shrinks() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "q".repeat(60)),
        ));
        manager.notify_message_added();
    }

    manager.update_observed_input_tokens(850);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    manager.maybe_start_compaction_with(&messages, provider);
    assert!(manager.is_compacting());
    let pending = manager.pending_cutoff;
    assert!(pending > 0);

    // Simulate an interleaving mutation that advances compacted_count out from
    // under the in-flight task (e.g. a hard compact via a different path),
    // leaving only a small active tail.
    manager.compacted_count = messages.len() - 3;

    // Drain the background task to completion, then apply.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction_with(&messages);
        if !manager.is_compacting() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!manager.is_compacting(), "task should have been drained");

    // The stale result must have been discarded: compacted_count stays where
    // the interleaving mutation left it, and the live tail survives.
    assert_eq!(
        manager.compacted_count,
        messages.len() - 3,
        "stale pending_cutoff must not advance compacted_count further"
    );
    assert!(
        manager.active_messages(&messages).len() >= 3,
        "live tail must survive a discarded stale compaction"
    );
    assert_eq!(manager.pending_cutoff, 0, "pending_cutoff must be reset");
}

#[tokio::test]
async fn test_guard_at_95_triggers_hard_compact() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("message {} with padding {}", i, "x".repeat(50)),
        ));
        manager.notify_message_added();
    }
    // 96% usage — above critical threshold
    manager.update_observed_input_tokens(960);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert!(
        matches!(action, CompactionAction::HardCompacted(_)),
        "SHOULD hard-compact at 96%"
    );
    assert!(
        manager.compacted_count > 0,
        "compacted_count should increase after hard compact"
    );
    assert!(
        manager.active_summary.is_some(),
        "should have an emergency summary"
    );
}

#[tokio::test]
async fn test_guard_at_100_percent_drops_messages() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "y".repeat(80)),
        ));
        manager.notify_message_added();
    }
    // Over 100% — simulates the exact bug scenario
    manager.update_observed_input_tokens(1_050);

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);
    assert!(
        matches!(action, CompactionAction::HardCompacted(_)),
        "MUST hard-compact when over 100%"
    );

    let api_messages = manager.messages_for_api_with(&messages);
    assert!(
        api_messages.len() < messages.len(),
        "API messages should be fewer after hard compact"
    );
    // First message should be the emergency summary
    match &api_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected text summary block"),
    }
}

// ── hard_compact_with edge cases ────────────────────────────────

#[test]
fn test_hard_compact_too_few_messages() {
    let mut manager = CompactionManager::new().with_budget(100);
    let messages = vec![
        make_text_message(Role::User, "hello"),
        make_text_message(Role::Assistant, "hi"),
    ];
    manager.notify_message_added();
    manager.notify_message_added();

    let result = manager.hard_compact_with(&messages);
    assert!(
        result.is_err(),
        "should fail with only 2 messages (MIN_TURNS_TO_KEEP)"
    );
}

#[test]
fn test_hard_compact_preserves_recent_turns() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..25 {
        messages.push(make_text_message(Role::User, &format!("turn {}", i)));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(950);

    let dropped = manager
        .hard_compact_with(&messages)
        .expect("should compact");
    assert!(dropped > 0, "should drop some messages");
    assert!(dropped < 25, "should not drop ALL messages");

    let api_messages = manager.messages_for_api_with(&messages);
    // Should have summary + recent turns
    assert!(
        api_messages.len() >= 2,
        "should keep at least MIN_TURNS_TO_KEEP + summary"
    );
    assert!(
        api_messages.len() <= 15,
        "should have dropped a significant number"
    );
}

// ── safe_compaction_cutoff: tool call/result pair integrity ─────────

#[test]
fn test_safe_cutoff_preserves_tool_pairs() {
    // Messages: [user, assistant(tool_use), user(tool_result), assistant, user]
    // If cutoff tries to split between tool_use and tool_result, it should back up
    let messages = vec![
        make_text_message(Role::User, "do something"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_1".to_string(),
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
                tool_use_id: "tool_1".to_string(),
                content: "file1.txt\nfile2.txt".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "I see the files"),
        make_text_message(Role::User, "thanks"),
    ];

    // Try to cut between tool_use (index 1) and tool_result (index 2)
    let cutoff = safe_compaction_cutoff(&messages, 2);
    // Should move back to include the tool_use at index 1
    assert!(
        cutoff <= 1,
        "cutoff should back up to include tool_use (got {})",
        cutoff
    );
}

#[test]
fn test_safe_cutoff_no_tool_pairs() {
    let messages = vec![
        make_text_message(Role::User, "hello"),
        make_text_message(Role::Assistant, "hi"),
        make_text_message(Role::User, "how are you"),
        make_text_message(Role::Assistant, "fine"),
    ];

    let cutoff = safe_compaction_cutoff(&messages, 2);
    assert_eq!(cutoff, 2, "no tool pairs, cutoff should stay unchanged");
}

#[test]
fn test_safe_cutoff_handles_chained_tool_dependencies_without_rescan() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_a".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file": "a.txt"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::User, "intermediate"),
        Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "tool_a".to_string(),
                    content: "a contents".to_string(),
                    is_error: Some(false),
                },
                ContentBlock::ToolUse {
                    id: "tool_b".to_string(),
                    name: "grep".to_string(),
                    input: serde_json::json!({"pattern": "foo"}),
                    thought_signature: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_b".to_string(),
                content: "foo".to_string(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "done"),
    ];

    let cutoff = safe_compaction_cutoff(&messages, 3);
    assert_eq!(
        cutoff, 0,
        "cutoff should walk back through nested tool dependencies until the kept suffix is self-contained"
    );
}

// ── emergency_truncate_with ─────────────────────────────────────

#[test]
fn test_emergency_truncate_large_tool_results() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let big_result = "x".repeat(10_000); // Way over EMERGENCY_TOOL_RESULT_MAX_CHARS (4000)
    let mut messages = vec![
        make_text_message(Role::User, "run something"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "cat bigfile"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool_1".to_string(),
                content: big_result.clone(),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        },
        make_text_message(Role::Assistant, "that's a big file"),
    ];
    for _ in &messages {
        manager.notify_message_added();
    }

    let truncated = manager.emergency_truncate_with(&mut messages);
    assert_eq!(truncated, 1, "should truncate exactly 1 tool result");

    // Check the truncated content
    if let ContentBlock::ToolResult { content, .. } = &messages[2].content[0] {
        assert!(
            content.len() < big_result.len(),
            "content should be shorter"
        );
        assert!(
            content.contains("truncated for context recovery"),
            "should have truncation marker"
        );
    } else {
        panic!("expected tool result");
    }
}

#[test]
fn test_emergency_truncate_skips_small_results() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: "tool_1".to_string(),
            content: "small output".to_string(),
            is_error: Some(false),
        }],
        timestamp: None,
        tool_duration_ms: None,
    }];
    manager.notify_message_added();

    let truncated = manager.emergency_truncate_with(&mut messages);
    assert_eq!(truncated, 0, "should not truncate small results");
}

// ── Double compaction ───────────────────────────────────────────

#[test]
fn test_hard_compact_twice() {
    let mut manager = CompactionManager::new().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "z".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(480);

    // First hard compact
    let dropped1 = manager
        .hard_compact_with(&messages)
        .expect("first compact should work");
    assert!(dropped1 > 0);
    let count_after_first = manager.compacted_count;

    // Simulate more messages arriving after first compact
    for i in 30..45 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "z".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);

    // Second hard compact
    let dropped2 = manager
        .hard_compact_with(&messages)
        .expect("second compact should work");
    assert!(dropped2 > 0);
    assert!(
        manager.compacted_count > count_after_first,
        "compacted_count should increase"
    );

    // Summary should mention both compactions
    let api_messages = manager.messages_for_api_with(&messages);
    assert!(api_messages.len() < messages.len());
    match &api_messages[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected summary"),
    }
}

#[test]
fn test_hard_compact_clamps_pathological_compacted_count() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} content {}", i, "x".repeat(200)),
        ));
        manager.notify_message_added();
    }

    // Reproduce the #175 bad state: bookkeeping says more messages were
    // compacted than exist in the current message vector. Before the fix,
    // active_messages() returned the full transcript in this state, so each
    // hard compaction appended another emergency marker and increased
    // compacted_count even further past messages.len().
    manager.compacted_count = 100;
    manager.active_summary = Some(Summary {
        text: "# Existing summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 100,
        original_turn_count: 100,
    });
    manager.active_chars.invalidate();

    for _ in 0..3 {
        let _ = manager.hard_compact_with(&messages);
    }

    assert_eq!(
        manager.compacted_count,
        messages.len(),
        "hard compaction must clamp compacted_count to the available messages"
    );
    let summary_markers = manager
        .active_summary
        .as_ref()
        .map(|summary| summary.text.matches("[Emergency compaction]").count())
        .unwrap_or(0);
    assert_eq!(
        summary_markers, 0,
        "pathological state should not append repeated emergency markers"
    );

    let api_messages = manager.messages_for_api_with(&messages);
    assert_eq!(
        api_messages.len(),
        1,
        "all current messages should remain covered by the existing summary until new turns arrive"
    );
}

#[test]
fn test_hard_compact_reduces_api_payload_and_reports_saved_tokens() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..40 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "payload ".repeat(80)),
        ));
        manager.notify_message_added();
    }

    let pre_api_messages = manager.messages_for_api_with(&messages);
    let pre_chars: usize = pre_api_messages.iter().map(message_char_count).sum();
    let pre_tokens = manager.effective_token_count_with(&messages);

    manager
        .hard_compact_with(&messages)
        .expect("hard compaction should recover oversized context");

    let post_api_messages = manager.messages_for_api_with(&messages);
    let post_chars: usize = post_api_messages.iter().map(message_char_count).sum();
    let post_tokens = manager.effective_token_count_with(&messages);
    let event = manager
        .take_compaction_event()
        .expect("hard compaction should publish an event");

    assert!(
        post_api_messages.len() < pre_api_messages.len(),
        "hard compaction should send fewer messages"
    );
    assert!(
        post_chars < pre_chars,
        "hard compaction should reduce outgoing payload chars: pre={pre_chars}, post={post_chars}"
    );
    assert!(
        post_tokens <= pre_tokens,
        "hard compaction must not increase effective tokens: pre={pre_tokens}, post={post_tokens}"
    );
    assert!(
        event.tokens_saved.unwrap_or(0) > 0,
        "event should attribute positive token savings: {event:?}"
    );
}

#[test]
fn test_invalid_compacted_count_does_not_resurrect_full_transcript_after_new_turn() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("old turn {} {}", i, "x".repeat(120)),
        ));
        manager.notify_message_added();
    }

    manager.compacted_count = 500;
    manager.active_summary = Some(Summary {
        text: "# Existing summary".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 500,
        original_turn_count: 500,
    });
    manager.active_chars.invalidate();

    let before_new_turn = manager.messages_for_api_with(&messages);
    assert_eq!(before_new_turn.len(), 1);
    assert_eq!(manager.compacted_count(), messages.len());

    messages.push(make_text_message(Role::User, "new turn after restore"));
    manager.notify_message_added();

    let after_new_turn = manager.messages_for_api_with(&messages);
    assert_eq!(
        after_new_turn.len(),
        2,
        "request should contain summary plus only the new active turn"
    );
    match &after_new_turn[1].content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, "new turn after restore"),
        _ => panic!("expected new active text turn"),
    }
}

// ── messages_for_api_with after compaction ──────────────────────

#[test]
fn test_messages_for_api_with_summary_prepended() {
    let mut manager = CompactionManager::new().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(Role::User, &format!("turn {}", i)));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);

    manager
        .hard_compact_with(&messages)
        .expect("should compact");

    let api_msgs = manager.messages_for_api_with(&messages);
    // First message should be the summary
    assert_eq!(api_msgs[0].role, Role::User);
    match &api_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.starts_with("## Previous Conversation Summary"));
        }
        _ => panic!("expected text"),
    }
    // Remaining should be recent turns from original messages
    assert!(api_msgs.len() < messages.len());
}

#[test]
fn test_persisted_state_round_trip_preserves_compacted_view() {
    let mut manager = CompactionManager::new().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "x".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);
    manager
        .hard_compact_with(&messages)
        .expect("should compact before persisting");

    let persisted = manager
        .persisted_state()
        .expect("compaction state should be exportable");
    let expected = manager.messages_for_api_with(&messages);

    let mut restored = CompactionManager::new().with_budget(500);
    restored.restore_persisted_state(&persisted, messages.len());
    let restored_msgs = restored.messages_for_api_with(&messages);

    assert_eq!(restored.compacted_count, persisted.compacted_count);
    assert_eq!(restored_msgs.len(), expected.len());
    match &restored_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(text.contains("Previous Conversation Summary"));
            assert!(text.contains("Emergency compaction"));
        }
        _ => panic!("expected restored summary block"),
    }
}

// ── context_usage accuracy ──────────────────────────────────────

#[test]
fn test_context_usage_with_both_estimate_and_observed() {
    let mut manager = CompactionManager::new().with_budget(200_000);
    // Build messages totalling ~50k chars = ~12.5k token estimate
    let mut messages = Vec::new();
    for i in 0..50 {
        messages.push(make_text_message(
            Role::User,
            &format!("{} {}", i, "a".repeat(1000)),
        ));
        manager.notify_message_added();
    }

    // Without observed tokens, usage should be based on char estimate
    let usage_no_observed = manager.context_usage_with(&messages);
    assert!(
        usage_no_observed < 0.2,
        "char estimate should be low: {}",
        usage_no_observed
    );

    // With observed tokens at 160k, should use observed (higher) value
    manager.update_observed_input_tokens(160_000);
    let usage_with_observed = manager.context_usage_with(&messages);
    assert!(
        usage_with_observed >= 0.79,
        "should use observed tokens: {}",
        usage_with_observed
    );
}

#[test]
fn test_context_usage_after_compaction_resets_observed() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {} pad {}", i, "x".repeat(50)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(960);

    // Hard compact should reset observed_input_tokens
    manager
        .hard_compact_with(&messages)
        .expect("should compact");
    assert!(
        manager.observed_input_tokens.is_none(),
        "observed_input_tokens should be cleared after hard compact"
    );

    // After compaction, usage should be based on char estimate of remaining messages only
    let post_usage = manager.context_usage_with(&messages);
    // The remaining messages are small, so usage should be well below the critical threshold
    assert!(
        post_usage < CRITICAL_THRESHOLD,
        "post-compaction usage should be below critical: {}",
        post_usage
    );
}

#[test]
fn test_recover_within_budget_drops_messages_without_truncation() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {} pad {}", i, "x".repeat(40)),
        ));
        manager.notify_message_added();
    }
    // Push well over budget so recovery triggers.
    manager.update_observed_input_tokens(2_000);

    let recovery = manager.recover_within_budget(&mut messages);
    assert!(
        recovery.dropped.unwrap_or(0) > 0,
        "should drop old messages"
    );
    // Dropping turns alone should fit the small remaining tail, so no
    // truncation escalation is needed.
    assert_eq!(
        recovery.truncated, 0,
        "should not truncate when dropping turns fits the budget"
    );
    assert!(recovery.did_anything());
    assert!(
        manager.context_usage_with(&messages) <= 1.0,
        "context should be back under budget after recovery"
    );
}

#[test]
fn test_recover_within_budget_truncates_when_tail_still_too_large() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let mut messages = Vec::new();
    // Build tool-use/tool-result pairs whose results are each individually
    // larger than the whole budget. After hard compaction drops down to the
    // minimum kept tail, the surviving tool result is still far over budget, so
    // recovery must escalate to truncation (which only acts on tool results).
    for i in 0..10 {
        let id = format!("tool_{i}");
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.clone(),
                name: "bash".to_string(),
                input: serde_json::json!({ "command": "cat big.log" }),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
        manager.notify_message_added();
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id,
                content: format!("huge {} {}", i, "y".repeat(20_000)),
                is_error: Some(false),
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(50_000);

    let recovery = manager.recover_within_budget(&mut messages);
    assert!(recovery.did_anything());
    assert!(
        recovery.truncated > 0,
        "should escalate to truncation when the remaining tail is still too large"
    );
}

#[test]
fn test_recover_within_budget_summary_line_variants() {
    let dropped_only = EmergencyRecovery {
        pre_usage: 1.6,
        dropped: Some(7),
        truncated: 0,
    };
    let line = dropped_only.summary_line(dropped_only.pre_usage);
    assert!(line.contains("dropped 7 old messages"));
    assert!(line.contains("160%"));
    assert!(!line.contains("truncated"));

    let dropped_and_truncated = EmergencyRecovery {
        pre_usage: 2.0,
        dropped: Some(3),
        truncated: 2,
    };
    let line = dropped_and_truncated.summary_line(dropped_and_truncated.pre_usage);
    assert!(line.contains("dropped 3 old messages"));
    assert!(line.contains("truncated 2 tool result(s)"));

    let truncate_only = EmergencyRecovery {
        pre_usage: 1.2,
        dropped: None,
        truncated: 5,
    };
    let line = truncate_only.summary_line(truncate_only.pre_usage);
    assert!(line.contains("shortened 5 large tool result(s)"));
    assert!(!line.contains("dropped"));
}

#[test]
fn test_bug1_active_chars_not_double_subtracted() {
    // BUG 1 regression test: verify the accounting fix for double-subtraction.
    //
    // The bug was in check_and_apply_compaction_with:
    // 1. Advance compacted_count (so active_messages() skips more messages)
    // 2. Call active_message_chars_with() which re-computes based on the new range
    // 3. Subtract compacted_chars from the re-computed (already reduced) value
    // 4. Result: double subtraction corrupts active_chars
    //
    // The fix: capture pre-advance active chars before advancing compacted_count,
    // then subtract compacted_chars from that captured value.

    let mut messages = Vec::new();
    for i in 0..5 {
        messages.push(make_text_message(
            Role::User,
            &format!("msg {}: {}", i, "x".repeat(200)),
        ));
    }

    let mut manager = CompactionManager::new();
    manager.seed_restored_messages_with(&messages);

    let last_three_chars: usize = messages[2..].iter().map(message_char_count).sum();

    // Simulate compacting the first 2 messages by manually setting state
    // (since we can't easily trigger the full background compaction flow)
    manager.compacted_count = 2;
    manager.active_chars.set_exact(last_three_chars);
    manager.active_summary = Some(Summary {
        text: "summary of first 2 messages".to_string(),
        openai_encrypted_content: None,
        covers_up_to_turn: 2,
        original_turn_count: 2,
    });

    // Now verify that the accounting is correct
    // active_message_chars_with should return the last 3 messages' chars
    let active_chars_recomputed = manager.active_message_chars_with(&messages);
    assert_eq!(
        active_chars_recomputed, last_three_chars,
        "active_message_chars_with should return chars of the 3 remaining messages"
    );

    // And the cached value should match
    let cached_value = manager.active_chars.value();
    assert_eq!(
        cached_value, last_three_chars,
        "cached active_chars should match the remaining messages"
    );

    // Verify token estimate is sensible
    let token_estimate = manager.token_estimate_with(&messages);
    assert!(token_estimate > 0, "token estimate should be positive");
}

#[test]
fn test_bug2_hard_compact_loop_checks_effective_tokens() {
    // BUG 2 regression test: verify the loop in hard_compact_with accounts
    // for SYSTEM_OVERHEAD_TOKENS and emergency summary size.
    //
    // The bug was:
    // 1. Loop compares: remaining_message_tokens <= token_budget
    // 2. But ignores SYSTEM_OVERHEAD_TOKENS (~18k)
    // 3. And ignores the emergency summary payload size
    // 4. Result: can exit loop with cutoff that still exceeds budget when
    //    those factors are included
    //
    // The fix: in the loop, compute total_effective_tokens including:
    // - message tokens
    // - summary tokens (existing + estimated emergency additions)
    // - overhead tokens
    // Then compare against budget.

    let token_budget = 30_000usize; // Small budget to trigger hard compact
    let mut manager = CompactionManager::new().with_budget(token_budget);

    // Create messages large enough to exceed budget
    // Each message: ~2000 chars = ~500 tokens
    // With 10 messages: 5000 tokens message content
    // + 18k overhead = 23k tokens total (within budget if ignoring summary)
    // But if we account for summary + overhead, we should need to drop more
    let mut messages = Vec::new();
    for i in 0..10 {
        messages.push(make_text_message(
            Role::User,
            &format!("message {}: {}", i, "x".repeat(2000)),
        ));
        manager.notify_message_added_with(&messages.last().unwrap());
    }

    // Simulate having an existing summary
    let existing_summary = Summary {
        text: "x".repeat(5000), // 5000 chars ~1250 tokens
        openai_encrypted_content: None,
        covers_up_to_turn: 100,
        original_turn_count: 100,
    };
    manager.active_summary = Some(existing_summary);

    // Hard compact should succeed and drop messages
    let result = manager.hard_compact_with(&messages);
    assert!(result.is_ok(), "hard_compact should succeed");

    let dropped = result.unwrap();
    assert!(dropped > 0, "should have dropped messages to fit budget");

    // After hard compact, verify effective tokens are within budget
    let final_tokens = manager.effective_token_count_with(&messages) as usize;
    assert!(
        final_tokens <= token_budget,
        "final tokens {} should be <= budget {} (accounting for overhead + summary)",
        final_tokens,
        token_budget
    );
}

// ── stage-1 reversible tool-result clearing ─────────────────────────────

fn make_tool_result_message(tool_use_id: &str, content: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: Some(false),
        }],
        timestamp: None,
        tool_duration_ms: None,
    }
}

struct EnvVarGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
        let prev = std::env::var_os(key);
        crate::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(prev) = &self.prev {
            crate::env::set_var(self.key, prev);
        } else {
            crate::env::remove_var(self.key);
        }
    }
}

#[test]
fn clearing_applies_in_api_view_but_not_history() {
    let mut manager = CompactionManager::new().with_budget(50_000);
    let mut messages = Vec::new();
    // 3 old messages carrying big, clearable tool results.
    for i in 0..3 {
        messages.push(make_tool_result_message(
            &format!("t{}", i),
            &"A".repeat(900),
        ));
        manager.notify_message_added();
    }
    // Padding so the watermark (len - RECENT_TURNS_TO_KEEP) lands past the
    // big tool results and into the small recent tail.
    for i in 0..12 {
        messages.push(make_text_message(Role::User, &format!("recent {}", i)));
        manager.notify_message_added();
    }
    assert_eq!(messages.len(), 15);

    let watermark = messages.len() - RECENT_TURNS_TO_KEEP; // 5
    manager.set_tool_cleared_up_to(watermark);

    let view = manager.messages_for_api_with(&messages);
    assert_eq!(view.len(), messages.len(), "no summary yet, view keeps all messages");

    // The 3 big tool results (indexes 0..3) must be cleared in the view.
    for i in 0..3 {
        match &view[i].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(
                    content.starts_with("[tool result cleared by jcode"),
                    "message {} should be cleared in the view, got: {}",
                    i,
                    content
                );
                assert!(content.len() < 900, "cleared content should be much smaller");
            }
            other => panic!("expected tool result at index {}, got {:?}", i, other),
        }
    }

    // Messages 3..5 (still below the watermark but plain text) must be
    // untouched — clearing only ever rewrites ToolResult blocks.
    for i in 3..5 {
        match &view[i].content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.starts_with("recent"))
            }
            other => panic!("expected text at index {}, got {:?}", i, other),
        }
    }

    // The recent tail (>= watermark) must be byte-for-byte identical to the
    // original messages.
    for i in watermark..messages.len() {
        assert_eq!(view[i].role, messages[i].role, "recent message {} role changed", i);
        match (&view[i].content[0], &messages[i].content[0]) {
            (ContentBlock::Text { text: a, .. }, ContentBlock::Text { text: b, .. }) => {
                assert_eq!(a, b, "recent message {} must be untouched", i);
            }
            other => panic!("expected matching text content at index {}, got {:?}", i, other),
        }
    }

    // Stored history (the caller's own Vec) must never be mutated — clearing
    // only ever rewrites the cloned view returned above.
    for i in 0..3 {
        match &messages[i].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert_eq!(content, &"A".repeat(900), "stored history must keep full content");
            }
            other => panic!("expected tool result at index {}, got {:?}", i, other),
        }
    }
}

#[tokio::test]
async fn stage1_clearing_skips_summarization_when_it_frees_enough() {
    // Serialize against env_kill_switch_disables_stage1: this test relies on
    // JCODE_DISABLE_TOOL_RESULT_CLEARING being unset, and that test sets it
    // under the same lock.
    let _env_lock = crate::storage::lock_test_env();

    // Small budget so 3 big (but individually clearable) tool results push
    // usage above 80%, but the cleared marker text is small enough to bring
    // it back down once stage-1 clears them.
    let mut manager = CompactionManager::new().with_budget(2_000);
    let mut messages = Vec::new();
    for i in 0..3 {
        messages.push(make_tool_result_message(
            &format!("t{}", i),
            &"A".repeat(900),
        ));
        manager.notify_message_added();
    }
    for i in 0..10 {
        messages.push(make_text_message(Role::User, &format!("recent {}", i)));
        manager.notify_message_added();
    }
    assert_eq!(messages.len(), 13);

    // Sanity: raw usage (before any clearing) should already be in the
    // reactive-but-not-critical band so we reach the stage-1 code path.
    let raw_usage = manager.context_usage_with(&messages);
    assert!(
        (COMPACTION_THRESHOLD..CRITICAL_THRESHOLD).contains(&raw_usage),
        "test setup should land usage in [80%, 95%), got {:.3}",
        raw_usage
    );

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);

    assert_eq!(
        action,
        CompactionAction::ToolResultsCleared { cleared: 3 },
        "stage-1 clearing should free enough headroom to skip summarization"
    );
    assert!(
        !manager.is_compacting(),
        "background summarization should not have started"
    );
    assert_eq!(manager.compacted_count, 0, "stage-1 clearing must not touch compacted_count");
    assert!(
        manager.context_usage_with(&messages) < COMPACTION_THRESHOLD,
        "usage should have dropped below the threshold after clearing"
    );
}

#[tokio::test]
async fn stage1_falls_through_to_summarization_when_not_enough() {
    // Serialize against env_kill_switch_disables_stage1 (see
    // stage1_clearing_skips_summarization_when_it_frees_enough).
    let _env_lock = crate::storage::lock_test_env();

    // Big plain-text messages (no tool results at all) push usage above 80%.
    // Stage-1 clearing has nothing to clear, so it must fall through to
    // background summarization instead of silently doing nothing.
    let mut manager = CompactionManager::new().with_budget(2_000);
    let mut messages = Vec::new();
    for i in 0..3 {
        messages.push(make_text_message(
            Role::User,
            &format!("old {} {}", i, "x".repeat(850)),
        ));
        manager.notify_message_added();
    }
    for i in 0..10 {
        messages.push(make_text_message(Role::User, &format!("recent {}", i)));
        manager.notify_message_added();
    }
    assert_eq!(messages.len(), 13);

    let raw_usage = manager.context_usage_with(&messages);
    assert!(
        (COMPACTION_THRESHOLD..CRITICAL_THRESHOLD).contains(&raw_usage),
        "test setup should land usage in [80%, 95%), got {:.3}",
        raw_usage
    );

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);

    assert!(
        matches!(action, CompactionAction::BackgroundStarted { .. }),
        "expected fallthrough to background summarization, got {:?}",
        action
    );
    assert!(
        manager.is_compacting(),
        "background summarization should be in flight"
    );
    // The watermark still advances (nothing to clear doesn't mean nothing to
    // mark), but it had zero effect since there were no clearable results.
    assert_eq!(manager.tool_cleared_up_to(), 3);
}

#[test]
fn tool_cleared_watermark_round_trips_through_persistence() {
    let mut manager = CompactionManager::new().with_budget(500);
    let mut messages = Vec::new();
    for i in 0..20 {
        messages.push(make_text_message(
            Role::User,
            &format!("turn {} {}", i, "x".repeat(40)),
        ));
        manager.notify_message_added();
    }
    manager.update_observed_input_tokens(490);
    manager
        .hard_compact_with(&messages)
        .expect("should compact before persisting");

    manager.set_tool_cleared_up_to(7);

    let persisted = manager
        .persisted_state()
        .expect("compaction state should be exportable");
    assert_eq!(persisted.tool_cleared_up_to, Some(7));

    let mut restored = CompactionManager::new().with_budget(500);
    restored.restore_persisted_state(&persisted, messages.len());

    assert_eq!(restored.tool_cleared_up_to(), 7);
}

#[tokio::test]
async fn env_kill_switch_disables_stage1() {
    let _env_lock = crate::storage::lock_test_env();
    let _guard = EnvVarGuard::set("JCODE_DISABLE_TOOL_RESULT_CLEARING", "1");

    // Same setup as `stage1_clearing_skips_summarization_when_it_frees_enough`:
    // without the kill switch this would return ToolResultsCleared.
    let mut manager = CompactionManager::new().with_budget(2_000);
    let mut messages = Vec::new();
    for i in 0..3 {
        messages.push(make_tool_result_message(
            &format!("t{}", i),
            &"A".repeat(900),
        ));
        manager.notify_message_added();
    }
    for i in 0..10 {
        messages.push(make_text_message(Role::User, &format!("recent {}", i)));
        manager.notify_message_added();
    }

    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);
    let action = manager.ensure_context_fits(&messages, provider);

    assert!(
        !matches!(action, CompactionAction::ToolResultsCleared { .. }),
        "kill switch must disable stage-1, got {:?}",
        action
    );
    assert_eq!(
        manager.tool_cleared_up_to(),
        0,
        "watermark must not advance while stage-1 is disabled"
    );
}

#[tokio::test]
async fn test_two_cycle_compaction() {
    let mut manager = CompactionManager::new().with_budget(1_000);
    let provider: Arc<dyn Provider> = Arc::new(MockSummaryProvider);

    // ── Cycle 1: create ~30 messages and force-compact ──────────────
    let mut messages = Vec::new();
    for i in 0..30 {
        messages.push(make_text_message(
            Role::User,
            &format!("Turn {} {}", i, "x".repeat(120)),
        ));
        manager.notify_message_added();
    }

    manager
        .force_compact_with(&messages, provider.clone())
        .expect("first force_compact should start");

    // Poll until the first compaction completes
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction();
        if manager.stats().has_summary {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        manager.active_summary.is_some(),
        "cycle 1: active_summary should be set"
    );
    let compacted_after_first = manager.compacted_count;
    assert!(
        compacted_after_first > 0,
        "cycle 1: compacted_count should be > 0, got {}",
        compacted_after_first,
    );

    // ── Cycle 2: add ~15 more messages and force-compact again ──────
    for i in 30..45 {
        messages.push(make_text_message(
            Role::User,
            &format!("Turn {} {}", i, "y".repeat(120)),
        ));
        manager.notify_message_added();
    }

    manager
        .force_compact_with(&messages, provider.clone())
        .expect("second force_compact should start");

    // Poll until the second compaction completes
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        manager.check_and_apply_compaction();
        if manager.compacted_count > compacted_after_first {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        manager.active_summary.is_some(),
        "cycle 2: active_summary should still be set"
    );
    assert!(
        manager.compacted_count > compacted_after_first,
        "cycle 2: compacted_count should have increased beyond {} (got {})",
        compacted_after_first,
        manager.compacted_count,
    );

    // The final API view should still have a summary + recent messages
    let api_msgs = manager.messages_for_api_with(&messages);
    assert!(
        api_msgs.len() < messages.len(),
        "after two cycles, API messages should be fewer than total"
    );
    match &api_msgs[0].content[0] {
        ContentBlock::Text { text, .. } => {
            assert!(
                text.contains("Previous Conversation Summary"),
                "first message should be the merged summary"
            );
        }
        _ => panic!("expected text summary block after two compaction cycles"),
    }
}
