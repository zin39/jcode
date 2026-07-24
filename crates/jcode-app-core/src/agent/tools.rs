use crate::message::{ContentBlock, ToolCall};
use crate::tool::ToolOutput;

/// Legacy high-water mark for the remote protocol / session history.  Kept as a
/// separate, larger backstop so individual per-tool caps set via config
/// (`max_tool_result_chars`) don't accidentally raise the ceiling.
pub(super) const MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY: usize = 512 * 1024;

/// Truncate a native tool output before it enters the conversation transcript.
///
/// Two caps apply (in order):
/// 1. `agents.max_tool_result_chars` (config, default 60_000).  When exceeded the
///    first 75 % and last 25 % are kept with a marker; full output is written to a
///    temp file under the session directory so the agent can read it later.
/// 2. `MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY` (512 KiB) — a hard safety ceiling that
///    protects the remote protocol and session persistence.  Exceeding it
///    truncates the output to just the prefix kept.
pub(super) fn cap_tool_output_for_history(
    tool_name: &str,
    session_id: &str,
    mut output: ToolOutput,
) -> ToolOutput {
    let max_result_chars = crate::config::config().agents.max_tool_result_chars;

    // --- config-driven truncation (head 75 % + tail 25 %) ---
    if max_result_chars > 0 && output.output.chars().count() > max_result_chars {
        let original_chars = output.output.chars().count();
        let head_chars = (max_result_chars as f64 * 0.75) as usize;
        let tail_chars = max_result_chars - head_chars;

        let head = crate::util::truncate_str(&output.output, head_chars);
        let tail = tail_str(&output.output, tail_chars);

        let truncated_path = save_truncated_output(
            session_id,
            tool_name,
            &output.output,
        );

        output.output = format!(
            "{head}\n\n[...truncated {dropped} chars; full output saved to {path}]\n\n{tail}",
            head = head,
            dropped = original_chars - max_result_chars,
            path = truncated_path,
            tail = tail,
        );
    }

    // --- hard protocol ceiling ---
    if output.output.chars().count() > MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY {
        let original_chars = output.output.chars().count();
        let kept =
            crate::util::truncate_str(&output.output, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY);
        output.output = format!(
            "{}\n\n[Tool output truncated by jcode: tool `{}` produced {} chars; kept first {} chars to protect the remote protocol, session history, and prompt cache. Redirect large logs to a file and read targeted sections.]",
            kept, tool_name, original_chars, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY,
        );
    }

    output
}

/// Truncate SDK-supplied tool content before it enters the conversation
/// transcript.  Same caps as [`cap_tool_output_for_history`].
pub(super) fn cap_sdk_tool_content_for_history(
    tool_name: &str,
    session_id: &str,
    content: String,
) -> String {
    let max_result_chars = crate::config::config().agents.max_tool_result_chars;

    // --- config-driven truncation (head 75 % + tail 25 %) ---
    if max_result_chars > 0 && content.chars().count() > max_result_chars {
        let original_chars = content.chars().count();
        let head_chars = (max_result_chars as f64 * 0.75) as usize;
        let tail_chars = max_result_chars - head_chars;

        let head = crate::util::truncate_str(&content, head_chars);
        let tail = tail_str(&content, tail_chars);

        let truncated_path = save_truncated_output(
            session_id,
            tool_name,
            &content,
        );

        return format!(
            "{head}\n\n[...truncated {dropped} chars; full output saved to {path}]\n\n{tail}",
            head = head,
            dropped = original_chars - max_result_chars,
            path = truncated_path,
            tail = tail,
        );
    }

    // --- hard protocol ceiling ---
    if content.chars().count() > MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY {
        let original_chars = content.chars().count();
        let kept = crate::util::truncate_str(&content, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY);
        return format!(
            "{}\n\n[Tool output truncated by jcode: tool `{}` produced {} chars; kept first {} chars to protect the remote protocol, session history, and prompt cache. Redirect large logs to a file and read targeted sections.]",
            kept, tool_name, original_chars, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY,
        );
    }

    content
}

/// Return the last `max_chars` characters of `s` (on a char boundary).
fn tail_str(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let byte_start = s.len() - max_chars;
    // floor_char_boundary is stable and available on str.
    let aligned = s.floor_char_boundary(byte_start);
    &s[aligned..]
}

/// Write the full tool output to a file under the session's truncated-outputs
/// directory and return a human-readable path for the truncation marker.
fn save_truncated_output(session_id: &str, tool_name: &str, full_output: &str) -> String {
    let write_result: anyhow::Result<String> = (|| -> anyhow::Result<String> {
        let jcode_dir = crate::storage::jcode_dir()?;
        let dir = jcode_dir
            .join("sessions")
            .join("truncated_outputs");
        std::fs::create_dir_all(&dir)?;

        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let safe_name = tool_name.replace(['/', '\\', ' '], "_");
        let filename = format!("{session_id}_{safe_name}_{timestamp}.txt");
        let path = dir.join(&filename);

        std::fs::write(&path, full_output)?;
        Ok(path.display().to_string())
    })();

    match write_result {
        Ok(path) => path,
        Err(e) => format!("<write failed: {e}>"),
    }
}

/// Build rendered side-pane images from a tool output's attached images.
///
/// This mirrors how `render_messages_and_images` derives images from persisted
/// session history (source = ToolResult), so live-streamed images match what a
/// later History reload would produce. `tool_name` and `tool_input` provide the
/// label fallback (e.g. the `read` tool's `file_path`); `tool_call_id` anchors
/// the image to its tool message in the transcript.
pub(super) fn tool_output_side_pane_images(
    tool_call_id: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    output: &ToolOutput,
) -> Vec<jcode_session_types::RenderedImage> {
    if output.images.is_empty() {
        return Vec::new();
    }
    let fallback_label = tool_input
        .get("file_path")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    output
        .images
        .iter()
        .map(|img| jcode_session_types::RenderedImage {
            media_type: img.media_type.clone(),
            data: img.data.clone(),
            label: img
                .label
                .as_ref()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .or_else(|| fallback_label.clone()),
            source: jcode_session_types::RenderedImageSource::ToolResult {
                tool_name: tool_name.to_string(),
            },
            anchor: Some(jcode_session_types::RenderedImageAnchor::ToolCall {
                id: tool_call_id.to_string(),
            }),
        })
        .collect()
}

pub(super) fn tool_output_to_content_blocks(
    tool_use_id: String,
    output: ToolOutput,
) -> Vec<ContentBlock> {
    let mut blocks = vec![ContentBlock::ToolResult {
        tool_use_id,
        content: output.output,
        is_error: None,
    }];
    for img in output.images {
        blocks.push(ContentBlock::Image {
            media_type: img.media_type,
            data: img.data,
        });
        if let Some(label) = img.label.filter(|label| !label.trim().is_empty()) {
            blocks.push(ContentBlock::Text {
                text: format!(
                    "[Attached image associated with the preceding tool result: {}]",
                    label
                ),
                cache_control: None,
            });
        }
    }
    blocks
}

pub(super) fn print_tool_summary(tool: &ToolCall) {
    match tool.name.as_str() {
        "bash" => {
            if let Some(cmd) = tool.input.get("command").and_then(|v| v.as_str()) {
                let short = if cmd.len() > 60 {
                    format!("{}...", crate::util::truncate_str(cmd, 60))
                } else {
                    cmd.to_string()
                };
                println!("$ {}", short);
            }
        }
        "read" | "write" | "edit" => {
            if let Some(path) = tool.input.get("file_path").and_then(|v| v.as_str()) {
                println!("{}", path);
            }
        }
        "glob" | "grep" => {
            if let Some(pattern) = tool.input.get("pattern").and_then(|v| v.as_str()) {
                println!("'{}'", pattern);
            }
        }
        "ls" => {
            let path = tool
                .input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            println!("{}", path);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_tool_output_leaves_small_output_unchanged() {
        let output = ToolOutput::new("short output");
        let capped = cap_tool_output_for_history("bash", "test-session", output.clone());
        assert_eq!(capped.output, output.output);
    }

    #[test]
    fn cap_tool_output_truncates_with_head_tail_and_file_marker() {
        // Build a string just over the default 60_000 cap
        let chunk = "0123456789".repeat(6_100); // 61_000 chars
        let output = ToolOutput::new(chunk);
        let capped = cap_tool_output_for_history("bash", "test-session", output);

        // Should be shorter than input but larger than the cap (marker adds overhead)
        assert!(capped.output.len() < 61_000);
        assert!(capped.output.contains("[...truncated "));
        assert!(capped.output.contains("full output saved to "));
    }

    #[test]
    fn cap_tool_output_respects_zero_config_disabling() {
        // With the default config cap (60k), a 512K+ output is first
        // truncated to ~60k with the head+tail marker. The hard ceiling
        // at 512K is a backstop only reached when max_tool_result_chars=0.
        let huge = "x".repeat(MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY + 10);
        let output = ToolOutput::new(huge);
        let capped = cap_tool_output_for_history("bash", "test-session", output);
        // The 60k config cap truncates first; verify output is reduced
        assert!(capped.output.len() < 80_000);
        assert!(capped.output.contains("[...truncated "));
        assert!(capped.output.contains("full output saved to "));
    }

    #[test]
    fn cap_sdk_tool_content_truncates_with_head_tail() {
        let chunk = "y".repeat(61_000);
        let capped = cap_sdk_tool_content_for_history("custom", "test-session", chunk);
        assert!(capped.contains("[...truncated "));
        assert!(capped.contains("full output saved to "));
    }

    #[test]
    fn cap_sdk_tool_content_leaves_small_unchanged() {
        let capped = cap_sdk_tool_content_for_history("custom", "test-session", "short".into());
        assert_eq!(capped, "short");
    }

    #[test]
    fn tail_str_preserves_last_chars() {
        let s = "abcdefghij";
        let tail = tail_str(s, 4);
        // The 4 chars should come from the end
        assert!(s.ends_with(tail));
        assert!(tail.chars().count() <= 4);
    }

    #[test]
    fn tail_str_short_string_unchanged() {
        assert_eq!(tail_str("abc", 10), "abc");
    }
}
