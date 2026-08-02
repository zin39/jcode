use super::*;
use serde_json::json;

#[test]
fn test_normalize_flat_params() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "file_path": "file1.txt"},
            {"tool": "read", "file_path": "file2.txt"}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 2);
    assert_eq!(parsed.tool_calls[0].as_call().unwrap().tool, "read");
    let params = parsed.tool_calls[0]
        .as_call()
        .unwrap()
        .parameters
        .as_ref()
        .unwrap();
    assert_eq!(params["file_path"], "file1.txt");
}

#[test]
fn test_normalize_already_nested() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "parameters": {"file_path": "file1.txt"}}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 1);
    let params = parsed.tool_calls[0]
        .as_call()
        .unwrap()
        .parameters
        .as_ref()
        .unwrap();
    assert_eq!(params["file_path"], "file1.txt");
}

#[test]
fn test_normalize_forwards_top_level_intent_into_nested_parameters() {
    let input = json!({
        "tool_calls": [{
            "tool": "read",
            "intent": "Inspect the batch renderer",
            "parameters": {"file_path": "src/tui/ui_messages.rs"}
        }]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    let params = parsed.tool_calls[0]
        .as_call()
        .unwrap()
        .parameters
        .as_ref()
        .unwrap();

    assert_eq!(params["intent"], "Inspect the batch renderer");
    assert_eq!(params["file_path"], "src/tui/ui_messages.rs");
}

#[test]
fn test_normalize_name_key_to_tool() {
    let input = json!({
        "tool_calls": [
            {"name": "read", "parameters": {"file_path": "file1.txt"}},
            {"name": "grep", "pattern": "foo", "path": "src/"}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 2);
    assert_eq!(parsed.tool_calls[0].as_call().unwrap().tool, "read");
    let params0 = parsed.tool_calls[0]
        .as_call()
        .unwrap()
        .parameters
        .as_ref()
        .unwrap();
    assert_eq!(params0["file_path"], "file1.txt");
    assert_eq!(parsed.tool_calls[1].as_call().unwrap().tool, "grep");
    let params1 = parsed.tool_calls[1]
        .as_call()
        .unwrap()
        .parameters
        .as_ref()
        .unwrap();
    assert_eq!(params1["pattern"], "foo");
}

#[test]
fn test_normalize_mixed_tool_and_name_keys() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "parameters": {"file_path": "a.rs"}},
            {"name": "read", "parameters": {"file_path": "b.rs"}},
            {"tool": "grep", "pattern": "test"}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();
    assert_eq!(parsed.tool_calls.len(), 3);
    assert_eq!(parsed.tool_calls[0].as_call().unwrap().tool, "read");
    assert_eq!(parsed.tool_calls[1].as_call().unwrap().tool, "read");
    assert_eq!(parsed.tool_calls[2].as_call().unwrap().tool, "grep");
}

#[test]
fn test_normalize_arguments_aliases_to_parameters() {
    let input = json!({
        "tool_calls": [
            {"tool": "read", "arguments": {"file_path": "a.rs"}},
            {"tool": "read", "args": {"file_path": "b.rs"}},
            {"tool": "read", "input": {"file_path": "c.rs"}}
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput = serde_json::from_value(normalized).unwrap();

    assert_eq!(parsed.tool_calls.len(), 3);
    assert_eq!(
        parsed.tool_calls[0]
            .as_call()
            .unwrap()
            .parameters
            .as_ref()
            .unwrap()["file_path"],
        "a.rs"
    );
    assert_eq!(
        parsed.tool_calls[1]
            .as_call()
            .unwrap()
            .parameters
            .as_ref()
            .unwrap()["file_path"],
        "b.rs"
    );
    assert_eq!(
        parsed.tool_calls[2]
            .as_call()
            .unwrap()
            .parameters
            .as_ref()
            .unwrap()["file_path"],
        "c.rs"
    );
}

#[test]
fn test_schema_only_requires_tool() {
    let schema = BatchTool::new(Registry {
        tools: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        skills: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::skill::SkillRegistry::default(),
        )),
        compaction: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::compaction::CompactionManager::new(),
        )),
    })
    .parameters_schema();

    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["required"],
        // Nested batch entries require `intent` alongside `tool` so every
        // fanned-out call carries a display label, matching the central
        // intent requirement in `ensure_intent_in_schema` (8505080a6).
        json!(["tool", "intent"])
    );
    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["additionalProperties"],
        json!(true)
    );
    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["properties"]["tool"]["description"],
        json!("Tool name.")
    );
    assert!(schema["properties"]["tool_calls"]["items"]["properties"]["intent"].is_object());
    assert!(schema["properties"]["tool_calls"]["items"]["properties"]["parameters"].is_null());
}

#[test]
fn test_schema_keeps_flat_generic_subcall_shape() {
    let schema = generic_batch_schema();

    assert!(schema["properties"]["tool_calls"]["description"].is_null());
    assert!(schema["properties"]["tool_calls"]["items"]["description"].is_null());
    assert_eq!(
        schema["properties"]["tool_calls"]["items"]["properties"]
            .as_object()
            .map(|props| props.len()),
        Some(2)
    );
    assert!(schema["properties"]["tool_calls"]["items"]["oneOf"].is_null());
}

/// Reproduces the logged production failure: a batch where one entry omitted
/// `tool` made the WHOLE call fail with "missing field `tool`", discarding the
/// sibling entries that were perfectly valid.
///
/// Raw input recovered from session_shark_1785647144977 (toolu_01UCY3YVkXgxQG4K1jJSEbiM).
#[test]
fn one_entry_missing_tool_does_not_discard_its_siblings() {
    let input = json!({
        "intent": "Read format_sender_answer, synthesize_answer, and test file listing",
        "tool_calls": [
            {
                "file_path": "/tmp/chat_backend.py",
                "intent": "read format_sender_answer and nearby functions",
                "limit": 120,
                "offset": 1168
            },
            {
                "intent": "list test files",
                "path": "/tmp/tests",
                "tool": "ls"
            }
        ]
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput =
        serde_json::from_value(normalized).expect("a malformed entry must not fail the batch");

    assert_eq!(parsed.tool_calls.len(), 2);
    assert!(
        parsed.tool_calls[0].as_call().is_none(),
        "entry without `tool` should be retained as malformed, not silently dropped"
    );
    let survivor = parsed.tool_calls[1]
        .as_call()
        .expect("the valid sibling must still be runnable");
    assert_eq!(survivor.tool, "ls");
}

/// The malformed-entry message must name what the model actually sent, so it
/// can fix the one bad entry instead of guessing across the whole batch.
#[test]
fn malformed_entry_reports_the_keys_it_received() {
    let input = json!({
        "tool_calls": [{"file_path": "/tmp/a.py", "limit": 120}]
    });

    let parsed: BatchInput = serde_json::from_value(normalize_batch_input(input)).unwrap();
    let BatchEntry::Malformed(reason) = &parsed.tool_calls[0] else {
        panic!("expected a malformed entry");
    };

    assert!(
        reason.contains("tool"),
        "should name the missing field: {reason}"
    );
    assert!(
        reason.contains("file_path") && reason.contains("limit"),
        "should echo the keys the caller actually sent: {reason}"
    );
}

/// Reproduces the logged "invalid type: string" failure: some providers hand
/// back `tool_calls` as a JSON-encoded string rather than a real array.
///
/// Observed in session_orangutan_1785300869101 (toolu_01Ck2Tm97aGMcwKMinaUoCzN).
#[test]
fn tool_calls_encoded_as_a_json_string_is_decoded() {
    let input = json!({
        "intent": "Check liveness",
        "tool_calls": "[{\"tool\": \"swarm\", \"action\": \"status\", \"intent\": \"Check shark liveness\"}]"
    });

    let normalized = normalize_batch_input(input);
    let parsed: BatchInput =
        serde_json::from_value(normalized).expect("stringified tool_calls should be decoded");

    assert_eq!(parsed.tool_calls.len(), 1);
    let call = parsed.tool_calls[0]
        .as_call()
        .expect("should parse as a call");
    assert_eq!(call.tool, "swarm");
    assert_eq!(call.parameters.as_ref().unwrap()["action"], "status");
}

/// A string that is not a JSON array must stay a hard error rather than being
/// coerced into something surprising.
#[test]
fn non_json_string_tool_calls_still_fails() {
    let input = json!({"tool_calls": "read the file please"});
    let normalized = normalize_batch_input(input);
    assert!(serde_json::from_value::<BatchInput>(normalized).is_err());
}
