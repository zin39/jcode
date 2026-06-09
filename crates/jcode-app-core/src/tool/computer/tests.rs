//! Tests for the computer tool. Pure-logic tests run anywhere on macOS; live
//! tests that synthesize events / capture the screen are `#[ignore]`d.

use super::*;
use jcode_tool_core::{ToolContext, ToolExecutionMode};

fn ctx() -> ToolContext {
    ToolContext {
        session_id: "test".into(),
        message_id: "test".into(),
        tool_call_id: "test".into(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

async fn run_action(v: Value) -> Result<ToolOutput> {
    ComputerTool::new().execute(v, ctx()).await
}

// ---- pure logic ----

#[tokio::test]
async fn rejects_bad_action() {
    let err = run_action(json!({ "action": "frobnicate" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Unknown computer action"));
}

#[tokio::test]
async fn move_requires_coords() {
    let err = run_action(json!({ "action": "move" })).await.unwrap_err();
    assert!(err.to_string().contains("requires"));
}

#[tokio::test]
async fn discover_all_lists_actions() {
    let out = run_action(json!({ "action": "discover", "category": "all" }))
        .await
        .unwrap();
    // Spot-check that several categories are present.
    for needle in ["press", "set_value", "run_applescript", "list_windows", "screenshot"] {
        assert!(out.output.contains(needle), "missing {needle}");
    }
}

#[tokio::test]
async fn discover_category_scopes() {
    let out = run_action(json!({ "action": "discover", "category": "ax" }))
        .await
        .unwrap();
    assert!(out.output.contains("find_element"));
    assert!(!out.output.contains("set_brightness"));
}

#[tokio::test]
async fn press_requires_element() {
    let err = run_action(json!({ "action": "press" })).await.unwrap_err();
    assert!(err.to_string().contains("element"));
}

#[test]
fn schema_is_compact() {
    // Guard against context bloat: the always-on schema + description must stay
    // small. Measured well under this bound; alert if it balloons.
    let tool = ComputerTool::new();
    let schema = serde_json::to_string(&tool.parameters_schema()).unwrap();
    let total = tool.description().len() + schema.len();
    // ~4 chars/token; keep always-on cost roughly under ~700 tokens.
    assert!(
        total < 2800,
        "computer tool always-on size grew to {total} chars (~{} tokens)",
        total / 4
    );
}

// ---- live (need GUI + permissions); run with --ignored ----

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_check_permissions() {
    let out = run_action(json!({ "action": "check_permissions" }))
        .await
        .unwrap();
    eprintln!("{}", out.output);
    assert!(out.metadata.is_some());
}

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_cursor_and_move() {
    run_action(json!({ "action": "move", "x": 400, "y": 300 }))
        .await
        .unwrap();
    let after = run_action(json!({ "action": "cursor" })).await.unwrap();
    let meta = after.metadata.unwrap();
    assert!((meta["x"].as_f64().unwrap() - 400.0).abs() < 5.0);
    assert!((meta["y"].as_f64().unwrap() - 300.0).abs() < 5.0);
}

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_screenshot() {
    let out = run_action(json!({ "action": "screenshot" })).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].media_type, "image/png");
    eprintln!("{}", out.output);
}

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_ui_tree() {
    let out = run_action(json!({ "action": "ui", "depth": 3 }))
        .await
        .unwrap();
    eprintln!("{}", out.output);
    assert!(out.output.contains("App:"));
}

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_list_windows() {
    let out = run_action(json!({ "action": "list_windows" })).await.unwrap();
    eprintln!("{}", out.output);
}

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_clipboard_roundtrip() {
    run_action(json!({ "action": "set_clipboard", "text": "jcode-clip-test" }))
        .await
        .unwrap();
    let out = run_action(json!({ "action": "get_clipboard" })).await.unwrap();
    assert!(out.output.contains("jcode-clip-test"));
}

#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_applescript() {
    let out = run_action(json!({ "action": "run_applescript", "script": "return 2 + 2" }))
        .await
        .unwrap();
    assert!(out.output.contains("4"));
}

/// Headline capability: set a TextEdit field's value via AX while TextEdit is
/// NOT frontmost, proving background control with no cursor movement.
#[tokio::test]
#[ignore = "requires GUI + permissions"]
async fn live_background_set_value() {
    // Open a fresh TextEdit document.
    run_action(json!({
        "action": "run_applescript",
        "script": "tell application \"TextEdit\" to activate\ndelay 0.4\ntell application \"TextEdit\" to make new document\ndelay 0.4"
    }))
    .await
    .unwrap();

    // Move focus away so TextEdit is in the background.
    run_action(json!({
        "action": "run_applescript",
        "script": "tell application \"System Events\" to set frontmost of (first process whose name is \"System Events\") to true"
    }))
    .await
    .ok();

    let marker = "background-ax-marker-42";
    // Set the text area value by AX path (AXScrollArea[1] -> AXTextArea[1]).
    run_action(json!({
        "action": "set_value",
        "element": { "app": "TextEdit", "path": [1, 1] },
        "value": marker
    }))
    .await
    .unwrap();

    let content = run_action(json!({
        "action": "run_applescript",
        "script": "tell application \"TextEdit\" to get text of document 1"
    }))
    .await
    .unwrap();
    assert!(content.output.contains(marker), "got: {}", content.output);

    // Cleanup.
    run_action(json!({
        "action": "run_applescript",
        "script": "tell application \"TextEdit\" to close every document saving no\ntell application \"TextEdit\" to quit"
    }))
    .await
    .ok();
}
