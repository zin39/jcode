use super::*;
use crate::tui::ui::prepare::prepare_body;

/// WP12: economy mode - messages beyond the compact threshold render in
/// COMPACT form (no per-block headers, no surface backgrounds, tighter spacing).
#[test]
fn test_economy_compact_mode_reduces_line_count() {
    let width = 80;

    // Build a conversation with many messages. Some will fall beyond the
    // default compact threshold and render in COMPACT mode.
    let mut display_messages: Vec<DisplayMessage> = Vec::new();
    for i in 0..60 {
        display_messages.push(DisplayMessage::user(format!("question {i}")));
        display_messages.push(DisplayMessage::assistant(format!(
            "answer {i} with some longer text that wraps across the line for sure"
        )));
    }

    // Full build (no compact mode) - use a very large threshold
    let state_full = TestState {
        display_messages: display_messages.clone(),
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };
    let full = prepare_body(&state_full, width, false);

    // Compact build - threshold of 20 means only last 20 messages are FULL,
    // earlier messages (0..99) are COMPACT
    let state_compact = TestState {
        display_messages: display_messages.clone(),
        messages_version: 2,
        economy_compact_threshold: 20,
        ..Default::default()
    };
    let compact = prepare_body(&state_compact, width, false);

    assert!(
        compact.wrapped_lines.len() < full.wrapped_lines.len(),
        "compact body ({} lines) should be shorter than full body ({} lines)",
        compact.wrapped_lines.len(),
        full.wrapped_lines.len()
    );

    let compact_text: String = compact
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n");
    let full_text: String = full
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n");

    // Full body has per-block "jcode ·" headers for assistant messages
    assert!(
        full_text.contains("jcode ·"),
        "full body should have assistant tag headers"
    );

    // Compact body still has headers for near-tail messages (last 20)
    assert!(
        compact_text.contains("jcode ·"),
        "compact body should keep assistant tag for near-tail messages"
    );

    // Compact body has fewer › headers (user prompt headers are stripped for
    // far messages)
    let compact_header_count = compact_text.lines().filter(|l| l.contains("›")).count();
    let full_header_count = full_text.lines().filter(|l| l.contains("›")).count();
    assert!(
        compact_header_count < full_header_count,
        "compact ({} headers) < full ({} headers)",
        compact_header_count,
        full_header_count
    );
}

/// WP12: with all messages within threshold, rendering is identical to full.
/// With threshold at 0, all messages are compacted (no headers, tight spacing).
#[test]
fn test_economy_compact_preserves_near_tail_headers() {
    let width = 80;

    let mut display_messages: Vec<DisplayMessage> = Vec::new();
    for i in 0..20 {
        display_messages.push(DisplayMessage::user(format!("user prompt {i}")));
        display_messages.push(DisplayMessage::assistant(format!(
            "assistant reply {i} with wrapping text content"
        )));
    }

    // All messages within threshold = FULL mode
    let state_full = TestState {
        display_messages: display_messages.clone(),
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };
    let full = prepare_body(&state_full, width, false);

    // Threshold = 1, only the very last message renders FULL; all others are
    // beyond it = COMPACT mode (no user headers)
    let state_compact = TestState {
        display_messages: display_messages.clone(),
        messages_version: 2,
        economy_compact_threshold: 1,
        ..Default::default()
    };
    let compact = prepare_body(&state_compact, width, false);

    let full_text: String = full
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n");
    let compact_text: String = compact
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n");

    // Full body: all user messages have "›" in their header lines
    let full_headers = full_text.lines().filter(|l| l.contains("›")).count();
    assert!(
        full_headers >= 20,
        "full body should have at least 20 › headers (one per user message), found {full_headers}"
    );

    // Compact body: only the last message has a header (threshold = 1)
    let compact_headers = compact_text.lines().filter(|l| l.contains("›")).count();
    assert!(
        compact_headers < full_headers,
        "compact body (threshold=1) should have fewer › headers than full, compact={compact_headers} full={full_headers}"
    );

    // Compact body preserves ASCII role marker (pipe gutter)
    assert!(
        compact_text.contains("│") || compact_text.contains("|"),
        "compact body should preserve role marker gutter"
    );

    // Compact body is shorter
    assert!(
        compact.wrapped_lines.len() < full.wrapped_lines.len(),
        "compact body ({} lines) should be shorter than full body ({} lines)",
        compact.wrapped_lines.len(),
        full.wrapped_lines.len()
    );
}

/// Counts rendered "jcode · <model>" header lines in a prepared body.
fn assistant_header_count(state: &TestState) -> usize {
    prepare_body(state, 80, false)
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .filter(|line| line.trim_start().starts_with("jcode ·"))
        .count()
}

/// A multi-block assistant answer should introduce itself once, not once per
/// block. Repeating the model name on every consecutive block was the single
/// loudest source of visual noise in the transcript.
#[test]
fn consecutive_assistant_blocks_render_one_model_header() {
    let width = 80;
    let state = TestState {
        display_messages: vec![
            DisplayMessage::user("one question".to_string()),
            DisplayMessage::assistant("first block of the answer".to_string()),
            DisplayMessage::assistant("second block of the same answer".to_string()),
            DisplayMessage::assistant("third block of the same answer".to_string()),
        ],
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };

    assert_eq!(
        assistant_header_count(&state),
        1,
        "three consecutive assistant blocks must share a single header, got:\n{:#?}",
        prepare_body(&state, width, false)
            .wrapped_lines
            .iter()
            .map(line_to_plain)
            .collect::<Vec<_>>()
    );
}

/// The header must come back when the user speaks again, otherwise a long
/// transcript loses track of who is talking.
#[test]
fn assistant_header_returns_after_the_user_speaks_again() {
    let width = 80;
    let state = TestState {
        display_messages: vec![
            DisplayMessage::user("first question".to_string()),
            DisplayMessage::assistant("first answer".to_string()),
            DisplayMessage::assistant("still the first answer".to_string()),
            DisplayMessage::user("second question".to_string()),
            DisplayMessage::assistant("second answer".to_string()),
        ],
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };

    assert_eq!(
        assistant_header_count(&state),
        2,
        "each assistant run after a user turn needs its own header, got:\n{:#?}",
        prepare_body(&state, width, false)
            .wrapped_lines
            .iter()
            .map(line_to_plain)
            .collect::<Vec<_>>()
    );
}

/// A tool call is work the assistant did, not a different speaker, so it must
/// not re-trigger the header mid-answer.
#[test]
fn tool_rows_do_not_reintroduce_the_assistant_header() {
    let width = 80;
    let state = TestState {
        display_messages: vec![
            DisplayMessage::user("do something".to_string()),
            DisplayMessage::assistant("let me check".to_string()),
            DisplayMessage::tool(
                "bash".to_string(),
                crate::tui::ToolCall {
                    name: "bash".to_string(),
                    ..Default::default()
                },
            ),
            DisplayMessage::assistant("here is what I found".to_string()),
        ],
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };

    assert_eq!(
        assistant_header_count(&state),
        1,
        "a tool row must not split one assistant turn into two headers, got:\n{:#?}",
        prepare_body(&state, width, false)
            .wrapped_lines
            .iter()
            .map(line_to_plain)
            .collect::<Vec<_>>()
    );
}

fn failing_tool(name: &str) -> DisplayMessage {
    DisplayMessage::tool(
        format!("{name}\nError: command failed with exit code 1"),
        crate::tui::ToolCall {
            name: name.to_string(),
            ..Default::default()
        },
    )
}

fn ok_tool(name: &str) -> DisplayMessage {
    DisplayMessage::tool(
        format!("{name}\nall good"),
        crate::tui::ToolCall {
            name: name.to_string(),
            ..Default::default()
        },
    )
}

/// Folding is positional, so a failure late in a long tool run used to be
/// hidden entirely behind "N more tool calls". An error must never be the thing
/// a summarising affordance swallows.
#[test]
fn tool_fold_never_hides_a_failed_tool() {
    let width = 100;
    let mut display_messages = vec![DisplayMessage::user("run everything".to_string())];
    for i in 0..4 {
        display_messages.push(ok_tool(&format!("ok_tool_{i}")));
    }
    // 5th tool in the run fails, well past the positional fold cutoff of 3.
    display_messages.push(failing_tool("exploding_tool"));

    let state = TestState {
        display_messages,
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };

    let text: String = prepare_body(&state, width, false)
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("exploding_tool"),
        "a failed tool must stay visible even when folded, got:\n{text}"
    );
    assert!(
        text.contains("contains a failure"),
        "the fold line must announce that it hides a failure, got:\n{text}"
    );
}

/// The calm path must stay calm: an all-successful run keeps the plain fold
/// summary with no alarming wording.
#[test]
fn tool_fold_stays_quiet_when_every_tool_succeeded() {
    let width = 100;
    let mut display_messages = vec![DisplayMessage::user("run everything".to_string())];
    for i in 0..6 {
        display_messages.push(ok_tool(&format!("ok_tool_{i}")));
    }

    let state = TestState {
        display_messages,
        messages_version: 1,
        economy_compact_threshold: usize::MAX,
        ..Default::default()
    };

    let text: String = prepare_body(&state, width, false)
        .wrapped_lines
        .iter()
        .map(line_to_plain)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        text.contains("more tool calls"),
        "successful runs should still fold, got:\n{text}"
    );
    assert!(
        !text.contains("contains a failure"),
        "must not cry wolf on a clean run, got:\n{text}"
    );
}

/// A calm tool row must not have colours competing for attention.
///
/// Reviewers independently flagged tool rows as visually noisy. The worst
/// offender was invisible rather than garish: the "normal output size" token
/// badge used rgb(118,118,118) while the tool name beside it used
/// rgb(120,120,120). Two points apart is indistinguishable to the eye, so it
/// bought no information while still making the row a three-colour object.
///
/// The budget is two: one for the row's own text, one for the dim separators.
/// Genuine signals (Warning/Danger output sizes, failures) are still allowed
/// to add a colour, because those are worth an interruption.
#[test]
fn a_routine_tool_row_stays_within_its_colour_budget() {
    use std::collections::BTreeSet;
    let mut tool = crate::tui::ToolCall::default();
    tool.name = "Bash".to_string();
    tool.input = serde_json::json!({"command": "cargo test --all"});

    let state = TestState {
        display_messages: vec![DisplayMessage::tool("Bash".to_string(), tool)],
        messages_version: 1,
        ..Default::default()
    };

    let prepared = crate::tui::ui::prepare::prepare_body(&state, 100, false);
    for line in prepared.wrapped_lines.iter() {
        let colors: BTreeSet<String> = line
            .spans
            .iter()
            .filter(|span| !span.content.trim().is_empty())
            .map(|span| format!("{:?}", span.style.fg))
            .collect();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            colors.len() <= 2,
            "a routine tool row should use at most 2 colours, found {} in {text:?}: {colors:?}",
            colors.len()
        );
    }
}
