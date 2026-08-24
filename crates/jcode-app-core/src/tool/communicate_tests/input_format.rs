#[test]
fn spawn_initial_message_accepts_prompt_alias_and_prefers_explicit_initial_message() {
    let from_prompt: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "spawn",
        "prompt": "review the diff"
    }))
    .expect("prompt alias should deserialize");
    assert_eq!(
        from_prompt.spawn_initial_message().as_deref(),
        Some("review the diff")
    );

    let preferred: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "spawn",
        "initial_message": "preferred",
        "prompt": "fallback"
    }))
    .expect("spawn payload should deserialize");
    assert_eq!(
        preferred.spawn_initial_message().as_deref(),
        Some("preferred")
    );
}

#[test]
fn communicate_input_accepts_delivery_and_share_append() {
    let delivery: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "dm",
        "message": "ping",
        "to_session": "sess-2",
        "delivery": "wake"
    }))
    .expect("delivery mode should deserialize");
    assert_eq!(
        delivery.delivery,
        Some(crate::protocol::CommDeliveryMode::Wake)
    );

    let append: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "share_append",
        "key": "task/123/notes",
        "value": "new line"
    }))
    .expect("share_append should deserialize");
    assert_eq!(append.action, "share_append");
}

#[test]
fn communicate_input_accepts_spawn_if_needed() {
    let parsed: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "assign_task",
        "spawn_if_needed": true
    }))
    .expect("spawn_if_needed should deserialize");
    assert_eq!(parsed.spawn_if_needed, Some(true));
}

#[test]
fn communicate_input_accepts_prefer_spawn() {
    let parsed: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "assign_task",
        "prefer_spawn": true
    }))
    .expect("prefer_spawn should deserialize");
    assert_eq!(parsed.prefer_spawn, Some(true));
}

#[test]
fn communicate_input_accepts_cleanup_lifecycle_flags() {
    let parsed: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "run_plan",
        "force": true,
        "retain_agents": true
    }))
    .expect("lifecycle flags should deserialize");
    assert_eq!(parsed.force, Some(true));
    assert_eq!(parsed.retain_agents, Some(true));
}

#[test]
fn cleanup_candidates_default_to_owned_terminal_workers() {
    let members = vec![
        AgentInfo {
            session_id: "coord".to_string(),
            friendly_name: Some("coord".to_string()),
            files_touched: vec![],
            status: Some("ready".to_string()),
            detail: None,
            role: Some("coordinator".to_string()),
            is_headless: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            live_attachments: None,
            status_age_secs: None,
            ..Default::default()
        },
        AgentInfo {
            session_id: "owned-done".to_string(),
            friendly_name: Some("owned".to_string()),
            files_touched: vec![],
            status: Some("completed".to_string()),
            detail: None,
            role: Some("agent".to_string()),
            is_headless: Some(true),
            report_back_to_session_id: Some("coord".to_string()),
            latest_completion_report: None,
            live_attachments: None,
            status_age_secs: None,
            ..Default::default()
        },
        AgentInfo {
            session_id: "user-created".to_string(),
            friendly_name: Some("user".to_string()),
            files_touched: vec![],
            status: Some("completed".to_string()),
            detail: None,
            role: Some("agent".to_string()),
            is_headless: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            live_attachments: None,
            status_age_secs: None,
            ..Default::default()
        },
        AgentInfo {
            session_id: "owned-running".to_string(),
            friendly_name: Some("running".to_string()),
            files_touched: vec![],
            status: Some("running".to_string()),
            detail: None,
            role: Some("agent".to_string()),
            is_headless: Some(true),
            report_back_to_session_id: Some("coord".to_string()),
            latest_completion_report: None,
            live_attachments: None,
            status_age_secs: None,
            ..Default::default()
        },
    ];
    let statuses = default_cleanup_target_statuses();
    assert_eq!(
        cleanup_candidate_session_ids("coord", &members, &statuses, &[], false),
        vec!["owned-done".to_string()]
    );
    assert_eq!(
        cleanup_candidate_session_ids("coord", &members, &statuses, &[], true),
        vec!["owned-done".to_string(), "user-created".to_string()]
    );
}

#[test]
fn format_tool_summary_includes_call_count() {
    let output = super::format_tool_summary(
        "session-123",
        &[
            ToolCallSummary {
                tool_name: "read".to_string(),
                brief_output: "Read 20 lines".to_string(),
                timestamp_secs: None,
            },
            ToolCallSummary {
                tool_name: "grep".to_string(),
                brief_output: "Found 3 matches".to_string(),
                timestamp_secs: None,
            },
        ],
    );

    assert!(
        output
            .output
            .contains("Tool call summary for session-123 (2 calls):")
    );
    assert!(output.output.contains("read — Read 20 lines"));
    assert!(output.output.contains("grep — Found 3 matches"));
}

#[test]
fn format_members_includes_status_and_detail() {
    let ctx = ToolContext {
        session_id: "sess-self".to_string(),
        message_id: "msg-1".to_string(),
        tool_call_id: "call-1".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    };

    let output = format_members(
        &ctx,
        &[AgentInfo {
            session_id: "sess-peer".to_string(),
            friendly_name: Some("bear".to_string()),
            files_touched: vec!["src/main.rs".to_string()],
            status: Some("running".to_string()),
            detail: Some("working on tests".to_string()),
            role: Some("agent".to_string()),
            is_headless: Some(true),
            report_back_to_session_id: Some("sess-self".to_string()),
            latest_completion_report: None,
            live_attachments: Some(0),
            status_age_secs: Some(12),
            ..Default::default()
        }],
    );

    assert!(output.output.contains("Status: running — working on tests"));
    assert!(output.output.contains("· 12s"));
    assert!(output.output.contains("Files: src/main.rs"));
    assert!(
        output
            .output
            .contains("Meta: headless · owned_by_you · attachments=0")
    );
}

#[test]
fn format_members_renders_activity_progress_churn_and_turns() {
    let ctx = test_ctx(
        "session_self_1234567890_deadbeefcafebabe",
        std::path::Path::new("."),
    );

    let output = format_members(
        &ctx,
        &[AgentInfo {
            session_id: "session_peer_1234567890_aaaaaaaaaaaa0001".to_string(),
            friendly_name: Some("otter".to_string()),
            files_touched: vec![],
            status: Some("running".to_string()),
            detail: Some("implementing".to_string()),
            task_label: None,
            role: Some("agent".to_string()),
            is_headless: Some(false),
            report_back_to_session_id: None,
            latest_completion_report: None,
            live_attachments: Some(1),
            status_age_secs: Some(8),
            last_activity_age_secs: Some(3),
            activity: Some(SessionActivitySnapshot {
                is_processing: true,
                current_tool_name: Some("edit".to_string()),
            }),
            provider_name: Some("anthropic".to_string()),
            provider_model: Some("claude-sonnet".to_string()),
            turn_count: Some(7),
            recent_total_tokens: Some(12_345),
            recent_output_tokens: Some(2_000),
            recent_window_secs: Some(10),
            cumulative_total_tokens: Some(98_765),
            todos_completed: Some(3),
            todos_total: Some(7),
        }],
    );

    let text = output.output;
    assert!(text.contains("Activity: working (edit)"), "got: {text}");
    assert!(text.contains("Progress: 3/7 todos"), "got: {text}");
    assert!(text.contains("12.3k tok/10s"), "got: {text}");
    assert!(text.contains("7 turns"), "got: {text}");
    assert!(text.contains("98.8k tok total"), "got: {text}");
    assert!(text.contains("Model: anthropic/claude-sonnet"), "got: {text}");
    // Running agent shows current-turn duration, not an "idle" label.
    assert!(text.contains("· 8s"), "got: {text}");
    // Running agent also surfaces last observed activity so a long turn does
    // not read as a dead worker.
    assert!(text.contains("· active 3s ago"), "got: {text}");
    assert!(!text.contains("idle"), "got: {text}");
}

#[test]
fn format_members_labels_idle_ready_agent() {
    let ctx = test_ctx(
        "session_self_1234567890_deadbeefcafebabe",
        std::path::Path::new("."),
    );

    let output = format_members(
        &ctx,
        &[AgentInfo {
            session_id: "session_peer_1234567890_bbbbbbbbbbbb0002".to_string(),
            friendly_name: Some("idle-one".to_string()),
            files_touched: vec![],
            status: Some("ready".to_string()),
            detail: None,
            role: Some("agent".to_string()),
            is_headless: None,
            report_back_to_session_id: None,
            latest_completion_report: None,
            live_attachments: Some(0),
            status_age_secs: Some(90),
            ..Default::default()
        }],
    );

    assert!(output.output.contains("idle 1m"), "got: {}", output.output);
}

#[test]
fn format_members_disambiguates_duplicate_friendly_names() {
    let ctx = test_ctx(
        "session_self_1234567890_deadbeefcafebabe",
        std::path::Path::new("."),
    );
    let output = format_members(
        &ctx,
        &[
            AgentInfo {
                session_id: "session_shark_1234567890_aaaaaaaaaaaa0001".to_string(),
                friendly_name: Some("shark".to_string()),
                files_touched: vec![],
                status: Some("ready".to_string()),
                detail: None,
                role: Some("agent".to_string()),
                is_headless: None,
                report_back_to_session_id: None,
                latest_completion_report: None,
                live_attachments: None,
                status_age_secs: None,
                ..Default::default()
            },
            AgentInfo {
                session_id: "session_shark_1234567890_bbbbbbbbbbbb0002".to_string(),
                friendly_name: Some("shark".to_string()),
                files_touched: vec![],
                status: Some("ready".to_string()),
                detail: None,
                role: Some("agent".to_string()),
                is_headless: None,
                report_back_to_session_id: None,
                latest_completion_report: None,
                live_attachments: None,
                status_age_secs: None,
                ..Default::default()
            },
        ],
    );

    assert!(output.output.contains("shark [aa0001]"));
    assert!(output.output.contains("shark [bb0002]"));
}

#[test]
fn format_awaited_members_disambiguates_duplicate_friendly_names() {
    let output = format_awaited_members(
        true,
        "done",
        &[
            AwaitedMemberStatus {
                session_id: "session_shark_1234567890_aaaaaaaaaaaa0001".to_string(),
                friendly_name: Some("shark".to_string()),
                status: "ready".to_string(),
                done: true,
                completion_report: None,
            },
            AwaitedMemberStatus {
                session_id: "session_shark_1234567890_bbbbbbbbbbbb0002".to_string(),
                friendly_name: Some("shark".to_string()),
                status: "ready".to_string(),
                done: true,
                completion_report: None,
            },
        ],
    );

    assert!(output.output.contains("✓ shark [aa0001] (ready)"));
    assert!(output.output.contains("✓ shark [bb0002] (ready)"));
}

#[test]
fn format_status_snapshot_includes_activity_and_metadata() {
    let output = super::format_status_snapshot(&AgentStatusSnapshot {
        session_id: "sess-peer".to_string(),
        friendly_name: Some("bear".to_string()),
        swarm_id: Some("swarm-test".to_string()),
        status: Some("running".to_string()),
        detail: Some("working on observability".to_string()),
        role: Some("agent".to_string()),
        is_headless: Some(true),
        live_attachments: Some(0),
        status_age_secs: Some(7),
        last_activity_age_secs: Some(3),
        joined_age_secs: Some(42),
        files_touched: vec!["src/server/comm_sync.rs".to_string()],
        activity: Some(SessionActivitySnapshot {
            is_processing: true,
            current_tool_name: Some("bash".to_string()),
        }),
        provider_name: None,
        provider_model: None,
    });

    assert!(
        output
            .output
            .contains("Status snapshot for bear (sess-peer)")
    );
    assert!(
        output
            .output
            .contains("Lifecycle: running — working on observability")
    );
    assert!(output.output.contains("Activity: busy (bash)"));
    assert!(output.output.contains("Swarm: swarm-test"));
    assert!(
        output
            .output
            .contains("Meta: headless · attachments=0 · active=3s ago · status_age=7s · joined=42s")
    );
    assert!(output.output.contains("Files: src/server/comm_sync.rs"));
}

/// An `output_schema` on spawn must reach the worker as an explicit contract,
/// so its answer comes back machine-checkable instead of as prose.
///
/// Motivation, measured: a Sonnet worker did 16 tool calls of real research and
/// then reported "Findings with file:line evidence for each item" while
/// attaching no findings at all. The work existed; the handoff destroyed it.
#[test]
fn an_output_schema_appends_a_contract_to_the_spawned_workers_prompt() {
    let with_schema: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "spawn",
        "prompt": "map the seams",
        "output_schema": {
            "type": "object",
            "properties": {"findings": {"type": "array"}},
            "required": ["findings"]
        }
    }))
    .expect("output_schema should deserialize");

    let message = with_schema
        .spawn_initial_message()
        .expect("spawn message should exist");
    assert!(
        message.starts_with("map the seams"),
        "the caller's task must come first: {message}"
    );
    assert!(
        message.contains("## Output contract"),
        "the contract must be appended: {message}"
    );
    assert!(
        message.contains("findings"),
        "the schema must be inlined so the worker knows the fields: {message}"
    );

    // Without a schema the prompt is passed through untouched, so existing
    // spawns are unaffected.
    let without: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "spawn",
        "prompt": "map the seams"
    }))
    .expect("plain spawn should deserialize");
    assert_eq!(
        without.spawn_initial_message().as_deref(),
        Some("map the seams")
    );
}

/// `to` must work as an alias for `to_session`. Without it, serde silently
/// dropped the field and an intended DM became a subtree broadcast while the
/// sender got a success confirmation (observed live: an 11k-char "DM" logged
/// as scope=broadcast to_session=none).
#[test]
fn communicate_input_accepts_to_as_alias_for_to_session() {
    let dm: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "message",
        "message": "hello",
        "to": "hedgehog",
    }))
    .expect("'to' alias should deserialize");
    assert_eq!(dm.to_session.as_deref(), Some("hedgehog"));

    // Explicit to_session still wins over nothing; both spellings agree.
    let explicit: CommunicateInput = serde_json::from_value(serde_json::json!({
        "action": "message",
        "message": "hello",
        "to_session": "hedgehog",
    }))
    .expect("to_session should deserialize");
    assert_eq!(explicit.to_session.as_deref(), Some("hedgehog"));
}
