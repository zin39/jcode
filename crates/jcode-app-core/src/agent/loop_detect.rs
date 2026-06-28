//! Pure, unit-testable detection of stuck-loop / repeated-read signals over
//! recent message history. Used to feed session_metrics counters (E1).

use jcode_message_types::{ContentBlock, Message};

/// Signals detected over a recent window of messages.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LoopSignals {
    /// A (tool, input) pair was issued >= REPEAT_THRESHOLD times in the window.
    pub repeated_read: bool,
    /// Count of consecutive failed tool results with no successful result between.
    pub failure_streak: u32,
}

const WINDOW_TURNS: usize = 8;
const REPEAT_THRESHOLD: usize = 3;
const STUCK_STREAK: u32 = 3;

/// Inspect the last `WINDOW_TURNS` messages for repeated identical tool calls
/// and a trailing failed-tool streak.
pub fn detect(messages: &[Message]) -> LoopSignals {
    let start = messages.len().saturating_sub(WINDOW_TURNS);
    let window = &messages[start..];

    // Repeated identical (tool_name, input) calls.
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for msg in window {
        for block in &msg.content {
            if let ContentBlock::ToolUse { name, input, .. } = block {
                let key = format!("{name}\u{0}{input}");
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    let repeated_read = counts.values().any(|&c| c >= REPEAT_THRESHOLD);

    // Trailing failed-tool streak: count failed tool results from the end until a
    // successful tool result is seen.
    let mut failure_streak = 0u32;
    'outer: for msg in window.iter().rev() {
        for block in msg.content.iter().rev() {
            if let ContentBlock::ToolResult { is_error, .. } = block {
                if *is_error == Some(true) {
                    failure_streak += 1;
                } else {
                    break 'outer;
                }
            }
        }
    }

    LoopSignals {
        repeated_read,
        failure_streak,
    }
}

/// True when the failure streak indicates the agent is stuck.
pub fn is_stuck(signals: &LoopSignals) -> bool {
    signals.failure_streak >= STUCK_STREAK
}

#[cfg(test)]
mod tests {
    use super::*;
    use jcode_message_types::{ContentBlock, Message, Role};

    fn tool_use(name: &str, input: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: format!("u-{name}-{input}"),
                name: name.to_string(),
                input: serde_json::json!({ "path": input }),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn tool_result(err: bool) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "r".to_string(),
                content: "x".to_string(),
                is_error: if err { Some(true) } else { None },
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    #[test]
    fn detects_repeated_read() {
        let msgs = vec![
            tool_use("read", "a.rs"),
            tool_use("read", "a.rs"),
            tool_use("read", "a.rs"),
        ];
        assert!(detect(&msgs).repeated_read);
    }

    #[test]
    fn no_repeat_when_inputs_differ() {
        let msgs = vec![
            tool_use("read", "a.rs"),
            tool_use("read", "b.rs"),
            tool_use("read", "c.rs"),
        ];
        assert!(!detect(&msgs).repeated_read);
    }

    #[test]
    fn detects_stuck_streak() {
        let msgs = vec![tool_result(true), tool_result(true), tool_result(true)];
        let s = detect(&msgs);
        assert_eq!(s.failure_streak, 3);
        assert!(is_stuck(&s));
    }

    #[test]
    fn success_breaks_streak() {
        let msgs = vec![tool_result(true), tool_result(false), tool_result(true)];
        assert_eq!(detect(&msgs).failure_streak, 1);
    }
}
