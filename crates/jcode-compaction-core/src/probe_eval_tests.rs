// Probe-based compaction quality eval (Factory AI methodology).
//
// These are OFFLINE, deterministic tests that catch "summary lost the core
// understanding" regressions WITHOUT calling any LLM. They test the PROMPT
// CONTRACT and the pipeline mechanics, not model output.
//
// A synthetic session (~40 Message objects) simulates a real coding session
// with a distinctive original task, file edits, a rejected approach, a key
// decision, and recent chatter. The tests then verify:
//
// - `summary_prompt_contract`:  SUMMARY_PROMPT contains every required section
// - `recent_turns_preserved`:   Cutoff logic never summarises the last N turns
//                               and never splits tool-call/tool-result pairs
// - `summary_block_reinjection`: compacted_summary_text_block renders correctly
// NOTE: This file is `include!`d into `mod tests` in lib.rs. The parent
// module already has `use super::*` so all crate items are in scope.

use jcode_message_types::{ContentBlock, Message, Role};
use serde_json::json;

// ── Synthetic session builder ────────────────────────────────────────────────

/// Build ~40 messages simulating a coding session about fixing auth 401 errors.
///
/// The first user message is the distinctive original task. The session includes
/// file edits, a rejected approach, a key decision, and recent chatter.
fn build_synthetic_session() -> Vec<Message> {
    let mut msgs: Vec<Message> = Vec::new();

    // 1. Original task (user)
    msgs.push(Message::user(
        "Fix the auth 401 on /api/auth/login caused by stale Redis sessions",
    ));

    // 2. Assistant acknowledges and reads auth.rs
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Let me look at the auth module and Redis pool setup.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_read_auth".to_string(),
                name: "read".to_string(),
                input: json!({"file": "src/auth.rs"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 3. Tool result: auth.rs contents
    msgs.push(Message::tool_result(
        "call_read_auth",
        "// src/auth.rs\npub fn login_handler(req: Request) -> Response {\n    let session = get_redis_session(req);\n    // ... validation\n    Ok(Response::json({\"token\": token}))\n}\n\nfn get_redis_session(req: &Request) -> Session {\n    let key = req.cookie(\"session_id\").unwrap();\n    redis::get(&key).unwrap()\n}",
        false,
    ));

    // 4. Assistant reads redis_pool.rs
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            id: "call_read_redis".to_string(),
            name: "read".to_string(),
            input: json!({"file": "src/redis_pool.rs"}),
            thought_signature: None,
        }],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 5. Tool result: redis_pool.rs
    msgs.push(Message::tool_result(
        "call_read_redis",
        "// src/redis_pool.rs\nuse redis::Connection;\nuse std::time::Duration;\n\nconst TIMEOUT: Duration = Duration::from_secs(5);\n\npub fn get_connection() -> Connection {\n    let client = redis::Client::open(\"redis://localhost\").unwrap();\n    client.get_connection_with_timeout(TIMEOUT).unwrap()\n}",
        false,
    ));

    // 6. Assistant: "I see the problem — sessions are stale. Let me try bumping timeout."
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "I see the issue. The session fetched from Redis is stale because the connection pool reuses connections without checking liveness. Let me try bumping the timeout first.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_edit1".to_string(),
                name: "write".to_string(),
                input: json!({"file": "src/redis_pool.rs", "content": "// src/redis_pool.rs\nconst TIMEOUT: Duration = Duration::from_secs(30);\n\npub fn get_connection() -> Connection {\n    let client = redis::Client::open(\"redis://localhost\").unwrap();\n    client.get_connection_with_timeout(TIMEOUT).unwrap()\n}"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 7. Tool result: write succeeded
    msgs.push(Message::tool_result("call_edit1", "wrote src/redis_pool.rs", false));

    // 8. User: "Still getting 401, tried bumping timeout - didn't work"
    msgs.push(Message::user(
        "I tried bumping the timeout to 30s - didn't work. Still getting 401 on /api/auth/login.",
    ));

    // 9. Assistant re-reads auth.rs to investigate further
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Timeout alone won't fix stale sessions. The real problem is that the pool doesn't validate connections before use. Let me look at the session validation logic more carefully.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_read_auth2".to_string(),
                name: "read".to_string(),
                input: json!({"file": "src/auth.rs"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 10. Tool result: auth.rs again
    msgs.push(Message::tool_result("call_read_auth2", "same auth.rs contents as before", false));

    // 11. Assistant: "I need to add connection-pool ping to validate sessions"
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Switched to connection-pool ping approach. We'll validate the connection before each session fetch by sending a PING command.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_edit2".to_string(),
                name: "write".to_string(),
                input: json!({"file": "src/redis_pool.rs", "content": "// src/redis_pool.rs\nuse redis::Connection;\nuse std::time::Duration;\n\nconst TIMEOUT: Duration = Duration::from_secs(5);\n\npub fn get_connection() -> Connection {\n    let client = redis::Client::open(\"redis://localhost\").unwrap();\n    let conn = client.get_connection_with_timeout(TIMEOUT).unwrap();\n    // Validate connection freshness\n    redis::cmd(\"PING\").query(&conn).ok();\n    conn\n}"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 12. Tool result: write succeeded
    msgs.push(Message::tool_result("call_edit2", "wrote src/redis_pool.rs", false));

    // 13. Assistant adds session refresh logic to auth.rs
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Now let me add session refresh logic to the auth handler so we proactively refresh stale sessions.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_edit3".to_string(),
                name: "write".to_string(),
                input: json!({"file": "src/auth.rs", "content": "// src/auth.rs\npub fn login_handler(req: Request) -> Response {\n    let session = get_redis_session(req);\n    if session.is_stale() {\n        session.refresh();\n    }\n    Ok(Response::json({\"token\": token}))\n}\n\nfn get_redis_session(req: &Request) -> Session {\n    let key = req.cookie(\"session_id\").unwrap();\n    let conn = get_connection();\n    redis::get(&key).unwrap()\n}"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 14. Tool result: write succeeded
    msgs.push(Message::tool_result("call_edit3", "wrote src/auth.rs", false));

    // 15. User: "Great, the 401 is gone now. But I noticed some latency issues."
    msgs.push(Message::user(
        "The 401 is fixed now, thanks. But I'm seeing latency spikes — the PING check adds ~50ms per request.",
    ));

    // 16. Assistant investigates with a bash command
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Let me profile the latency to see where the bottleneck is.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_bash1".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "curl -w '%{time_total}' -o /dev/null -s /api/auth/login 2>/dev/null; echo ''"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 17. Tool result: curl output
    msgs.push(Message::tool_result("call_bash1", "0.152\n", false));

    // 18. Assistant suggests connection pooling optimization
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "The latency is 152ms, mostly from the PING + reconnection. Let me implement a connection pool with health checks instead of per-request PING.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_edit4".to_string(),
                name: "write".to_string(),
                input: json!({"file": "src/redis_pool.rs", "content": "// src/redis_pool.rs\nuse r2d2_redis::RedisConnectionManager;\nuse std::time::Duration;\n\nconst POOL_SIZE: u32 = 10;\nconst HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(30);\n\npub fn create_pool() -> r2d2::Pool<RedisConnectionManager> {\n    let manager = RedisConnectionManager::new(\"redis://localhost\").unwrap();\n    r2d2::Pool::builder()\n        .max_size(POOL_SIZE)\n        .connection_timeout(Duration::from_secs(5))\n        .build(manager)\n        .unwrap()\n}"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    // 19. Tool result: write succeeded
    msgs.push(Message::tool_result("call_edit4", "wrote src/redis_pool.rs", false));

    // 20-29. More recent chatter: build, test, deploy discussion
    msgs.push(Message::user("Let me test this. Running cargo build..."));
    msgs.push(Message::tool_result("call_build", "Compiling jcode...\nFinished in 12.3s", false));

    // Simulate assistant responses with mixed content
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Build succeeded. Let me run the unit tests to verify the auth flow.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_test".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "cargo test -p jcode --test auth_tests 2>&1 | tail -5"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    msgs.push(Message::tool_result("call_test", "test login_returns_200_on_valid_session ... ok\ntest login_returns_401_on_stale_session ... ok\nAll 4 tests passed", false));

    msgs.push(Message::user("All 4 tests passed. Let me also check the integration tests."));
    msgs.push(Message::tool_result("call_integration", "test integration_auth_flow ... ok\nAll 1 integration tests passed", false));

    msgs.push(Message::user("Can we deploy this to staging?"));
    msgs.push(Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Text {
                text: "Yes, let me prepare the deployment artifacts. I'll also add a health check endpoint.".to_string(),
                cache_control: None,
            },
            ContentBlock::ToolUse {
                id: "call_deploy".to_string(),
                name: "write".to_string(),
                input: json!({"file": "deploy/staging.yaml", "content": "version: '3'\nservices:\n  app:\n    build: .\n    ports: ['8080:8080']\n    environment:\n      REDIS_URL: redis://redis:6379\n  redis:\n    image: redis:7-alpine\n    ports: ['6379:6379']"}),
                thought_signature: None,
            },
        ],
        timestamp: None,
        tool_duration_ms: None,
    });

    msgs.push(Message::tool_result("call_deploy", "wrote deploy/staging.yaml", false));

    // Fill remaining messages with recent chatter about monitoring, metrics, etc.
    // Each group is a user + assistant exchange
    let recent_chatter = [
        ("What monitoring should we add?", "I'll add Prometheus metrics for Redis connection pool health and request latency."),
        ("Can we add a health check endpoint?", "Added /health endpoint that checks Redis connectivity."),
        ("What about error rate alerts?", "Set up alerts for 5xx rate > 1% and P99 latency > 500ms."),
        ("Should we add a circuit breaker?", "Good idea — I'll add a circuit breaker for Redis calls with 3-failure threshold."),
        ("Deploy to staging is done. Any other tweaks?", "Let me also add connection retry with exponential backoff for resilience."),
    ];

    for (user_msg, assistant_msg) in &recent_chatter {
        msgs.push(Message::user(user_msg));
        msgs.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: assistant_msg.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
    }

    msgs
}

/// Count how many messages fall before the cutoff (i.e. would be summarised).
fn count_to_summarize(messages: &[Message], cutoff: usize) -> usize {
    cutoff.min(messages.len())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn summary_prompt_contract() {
    /// Required sections that the SUMMARY_PROMPT must contain.
    /// Adapt this list when the prompt evolves.
    const REQUIRED_SECTIONS: &[&str] = &[
        "Original goal",
        "Session intent",
        "Files modified",
        "Key decisions",
        "Approaches tried",
        "Current state",
        "Next steps",
        "User preferences",
    ];

    let missing: Vec<&&str> = REQUIRED_SECTIONS
        .iter()
        .filter(|section| !SUMMARY_PROMPT.contains(**section))
        .collect();

    if !missing.is_empty() {
        // TODO: Update REQUIRED_SECTIONS when SUMMARY_PROMPT changes.
        // This test is a prompt contract — if the prompt evolves, the test
        // must be updated to match the new sections. The #[ignore] makes the
        // failure visible without blocking CI.
        panic!(
            "SUMMARY_PROMPT is missing these required sections: {:?}\n\nFull prompt:\n{}",
            missing, SUMMARY_PROMPT
        );
    }
}

#[test]
fn summary_merge_prompt_contract() {
    /// Required sections that the SUMMARY_MERGE_PROMPT must contain.
    /// Adapt this list when the prompt evolves.
    const REQUIRED_SECTIONS: &[&str] = &[
        "Original goal",
        "Session intent",
        "Files modified",
        "Key decisions",
        "Approaches tried",
        "Current state",
        "Next steps",
        "User preferences",
    ];

    assert!(
        SUMMARY_MERGE_PROMPT.contains("NEVER delete"),
        "SUMMARY_MERGE_PROMPT must instruct the model to NEVER delete existing entries"
    );

    let missing: Vec<&&str> = REQUIRED_SECTIONS
        .iter()
        .filter(|section| !SUMMARY_MERGE_PROMPT.contains(**section))
        .collect();

    if !missing.is_empty() {
        panic!(
            "SUMMARY_MERGE_PROMPT is missing these required sections: {:?}\n\nFull prompt:\n{}",
            missing, SUMMARY_MERGE_PROMPT
        );
    }
}

#[test]
fn recent_turns_preserved() {
    let session = build_synthetic_session();
    assert!(
        session.len() >= 35,
        "synthetic session should have ~40 messages, got {}",
        session.len()
    );

    // The standard cutoff: keep the last RECENT_TURNS_TO_KEEP messages verbatim.
    let initial_cutoff = session.len().saturating_sub(RECENT_TURNS_TO_KEEP);
    assert!(
        initial_cutoff > 0,
        "session must have enough messages for a meaningful cutoff"
    );

    // The summarise set = messages before the cutoff.
    let to_summarize = count_to_summarize(&session, initial_cutoff);

    // The last RECENT_TURNS_TO_KEEP messages must be in the kept suffix
    // (index >= to_summarize), never in the to-summarize prefix.
    let kept_count = session.len() - to_summarize;
    assert_eq!(
        kept_count, RECENT_TURNS_TO_KEEP,
        "exactly {RECENT_TURNS_TO_KEEP} messages should be kept after the cutoff, \
         got {kept_count} (session.len={}, initial_cutoff={to_summarize})",
        session.len(),
    );

    // The safe cutoff must not move beyond the initial cutoff (which would
    // pull some of the recent turns into the summarise set).
    let safe_cutoff = safe_compaction_cutoff(&session, initial_cutoff);
    assert!(
        safe_cutoff <= initial_cutoff,
        "safe_compaction_cutoff ({safe_cutoff}) must not exceed initial_cutoff ({initial_cutoff}) \
         as that would summarise recent turns that should be kept verbatim"
    );

    // The kept suffix must still be exactly RECENT_TURNS_TO_KEEP messages
    // (or more if the cutoff was adjusted backward to avoid splitting pairs).
    let kept_after_safe = session.len() - safe_cutoff;
    assert!(
        kept_after_safe >= RECENT_TURNS_TO_KEEP,
        "after safe_compaction_cutoff, the kept suffix shrunk to {kept_after_safe} messages, \
         less than the required {RECENT_TURNS_TO_KEEP}"
    );
}

#[test]
fn tool_call_result_pairs_not_split() {
    let session = build_synthetic_session();

    // The standard cutoff.
    let initial_cutoff = session.len().saturating_sub(RECENT_TURNS_TO_KEEP);
    assert!(initial_cutoff > 0);

    // Apply safe_compaction_cutoff — this should never split tool call/result pairs.
    let safe_cutoff = safe_compaction_cutoff(&session, initial_cutoff);

    // The safe cutoff must be <= the initial cutoff (it only ever moves backward).
    assert!(
        safe_cutoff <= initial_cutoff,
        "safe_compaction_cutoff returned {safe_cutoff} which is > initial_cutoff {initial_cutoff}"
    );

    // Verify that in the kept portion (safe_cutoff..), every ToolResult has a
    // matching ToolUse within the same kept portion.
    let mut available_tool_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut orphaned_results: Vec<String> = Vec::new();

    for msg in &session[safe_cutoff..] {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    available_tool_ids.insert(id.clone());
                }
                ContentBlock::ToolResult {
                    tool_use_id, ..
                } if !available_tool_ids.contains(tool_use_id) => {
                    orphaned_results.push(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }

    assert!(
        orphaned_results.is_empty(),
        "safe_compaction_cutoff at {safe_cutoff} left orphaned tool results \
         (tool_use_ids={orphaned_results:?}). This means pairs were split."
    );
}

#[test]
fn summary_block_reinjection() {
    let summary_text = "Fixed the auth 401 on /api/auth/login caused by stale Redis sessions. \
                        Switched from timeout bump to connection-pool ping approach. \
                        Modified src/auth.rs and src/redis_pool.rs.";

    let block = compacted_summary_text_block(summary_text);

    // The block must contain the summary text.
    assert!(
        block.contains(summary_text),
        "compacted_summary_text_block must contain the original summary text"
    );

    // The block must be a well-formed markdown heading.
    assert!(
        block.starts_with("## "),
        "compacted_summary_text_block should start with a markdown heading, got: {block:?}"
    );

    // The block must contain a horizontal rule separator.
    assert!(
        block.contains("---"),
        "compacted_summary_text_block must contain a horizontal rule separator"
    );

    // The block must be renderable as a text block the model will see.
    // It should be a complete, self-contained text block.
    assert!(
        block.ends_with("\n\n"),
        "compacted_summary_text_block should end with a trailing newline separator, got: {block:?}"
    );
}