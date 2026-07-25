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
