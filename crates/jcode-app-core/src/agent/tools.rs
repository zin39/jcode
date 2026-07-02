use crate::message::{ContentBlock, ToolCall};
use crate::tool::ToolOutput;
use std::path::PathBuf;

pub(super) const MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY: usize = 512 * 1024;
/// Above this, the full output is spilled to disk and history keeps a
/// head + pointer instead of the whole payload (token + RAM saving).
pub(super) const SPILL_THRESHOLD_CHARS: usize = 50 * 1024;
/// How much of a spilled output stays inline in history.
pub(super) const SPILL_INLINE_HEAD_CHARS: usize = 10 * 1024;

/// Write tool output to disk and return path; returns None on any IO error.
fn spill_output_to_disk(session_id: &str, tool_call_id: &str, full: &str) -> Option<PathBuf> {
    let jcode_home = jcode_storage::jcode_dir().ok()?;
    let tool_outputs_dir = jcode_home.join("tool-outputs").join(session_id);
    std::fs::create_dir_all(&tool_outputs_dir)
        .map_err(|e| {
            crate::logging::warn(&format!(
                "Failed to create tool-outputs dir: {}",
                e
            ));
            e
        })
        .ok()?;
    let file_path = tool_outputs_dir.join(format!("{}.txt", tool_call_id));
    std::fs::write(&file_path, full)
        .map_err(|e| {
            crate::logging::warn(&format!(
                "Failed to write tool output to disk ({}): {}",
                file_path.display(),
                e
            ));
            e
        })
        .ok()?;
    Some(file_path)
}

/// Generate the pointer message for spilled output.
fn spillover_pointer_message(
    tool_name: &str,
    original_chars: usize,
    kept_chars: usize,
    path: &PathBuf,
) -> String {
    format!(
        "\n\n[Tool output truncated by jcode: tool `{}` produced {} chars; kept first {} inline. FULL output saved to {} — use the read tool with start_line/limit for targeted sections.]",
        tool_name, original_chars, kept_chars,
        path.display()
    )
}

pub(super) fn cap_tool_output_for_history(
    tool_name: &str,
    session_id: &str,
    tool_call_id: &str,
    mut output: ToolOutput,
) -> ToolOutput {
    let char_count = output.output.chars().count();
    if char_count <= SPILL_THRESHOLD_CHARS {
        return output;
    }

    let original_chars = char_count;

    // Try to spill to disk
    if let Some(path) = spill_output_to_disk(session_id, tool_call_id, &output.output) {
        let kept = crate::util::truncate_str(&output.output, SPILL_INLINE_HEAD_CHARS);
        output.output = format!("{kept}{}", spillover_pointer_message(tool_name, original_chars, SPILL_INLINE_HEAD_CHARS, &path));
        return output;
    }

    // Spillover failed; fall back to 512KB truncation
    if char_count <= MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY {
        return output;
    }

    let kept = crate::util::truncate_str(&output.output, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY);
    output.output = format!(
        "{}\n\n[Tool output truncated by jcode: tool `{}` produced {} chars; kept first {} chars to protect the remote protocol, session history, and prompt cache. Redirect large logs to a file and read targeted sections.]",
        kept, tool_name, original_chars, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY,
    );
    output
}

pub(super) fn cap_sdk_tool_content_for_history(
    tool_name: &str,
    session_id: &str,
    tool_call_id: &str,
    content: String,
) -> String {
    let char_count = content.chars().count();
    if char_count <= SPILL_THRESHOLD_CHARS {
        return content;
    }

    let original_chars = char_count;

    // Try to spill to disk
    if let Some(path) = spill_output_to_disk(session_id, tool_call_id, &content) {
        let kept = crate::util::truncate_str(&content, SPILL_INLINE_HEAD_CHARS);
        return format!("{kept}{}", spillover_pointer_message(tool_name, original_chars, SPILL_INLINE_HEAD_CHARS, &path));
    }

    // Spillover failed; fall back to 512KB truncation
    if char_count <= MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY {
        return content;
    }

    let kept = crate::util::truncate_str(&content, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY);
    format!(
        "{}\n\n[Tool output truncated by jcode: tool `{}` produced {} chars; kept first {} chars to protect the remote protocol, session history, and prompt cache. Redirect large logs to a file and read targeted sections.]",
        kept, tool_name, original_chars, MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY,
    )
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
    use std::fs;

    struct EnvGuard {
        key: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let original = std::env::var_os(key);
            crate::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.take() {
                crate::env::set_var(self.key, value);
            } else {
                crate::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn cap_tool_output_leaves_small_output_unchanged() {
        let output = ToolOutput::new("short output");
        let capped = cap_tool_output_for_history("bash", "sess123", "tool456", output.clone());
        assert_eq!(capped.output, output.output);
    }

    #[test]
    fn cap_tool_output_below_spill_threshold_unchanged() {
        let content = "x".repeat(SPILL_THRESHOLD_CHARS - 100);
        let output = ToolOutput::new(&content);
        let capped = cap_tool_output_for_history("bash", "sess123", "tool456", output.clone());
        assert_eq!(capped.output, output.output);
    }

    #[test]
    fn cap_tool_output_spills_above_threshold() {
        let _lock = crate::storage::lock_test_env();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _home = EnvGuard::set("JCODE_HOME", temp_dir.path());
        let session_id = "test_session";
        let tool_call_id = "test_tool_call";

        let content = "x".repeat(SPILL_THRESHOLD_CHARS + 1000);
        let output = ToolOutput::new(&content);
        let capped = cap_tool_output_for_history("bash", session_id, tool_call_id, output);

        // Verify spillover happened
        assert!(capped.output.contains("Tool output truncated by jcode"));
        assert!(capped.output.contains(&format!("produced {} chars", SPILL_THRESHOLD_CHARS + 1000)));
        assert!(capped.output.contains("FULL output saved to"));
        assert!(capped.output.contains("tool-outputs"));

        // Verify file was created
        let spill_path = temp_dir
            .path()
            .join("tool-outputs")
            .join(session_id)
            .join(format!("{}.txt", tool_call_id));
        assert!(spill_path.exists());
        let written_content = fs::read_to_string(&spill_path).expect("Failed to read spilled file");
        assert_eq!(written_content, content);
    }

    #[test]
    fn cap_tool_output_fallback_on_spill_failure() {
        let _lock = crate::storage::lock_test_env();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        // Place a regular file where the tool-outputs dir would go so
        // create_dir_all fails deterministically.
        fs::write(temp_dir.path().join("tool-outputs"), "blocker").expect("write blocker");
        let _home = EnvGuard::set("JCODE_HOME", temp_dir.path());

        let output = ToolOutput::new(&"x".repeat(MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY + 10));
        let capped = cap_tool_output_for_history("bash", "sess123", "tool456", output);

        // On spill failure, should fall back to truncation at MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY
        assert!(capped.output.len() < MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY + 5000);
        assert!(capped.output.contains("Tool output truncated by jcode"));
        assert!(capped.output.contains("Redirect large logs to a file"));
    }

    #[test]
    fn cap_sdk_tool_content_below_threshold_unchanged() {
        let content = "y".repeat(SPILL_THRESHOLD_CHARS - 100);
        let capped = cap_sdk_tool_content_for_history("custom", "sess123", "tool456", content.clone());
        assert_eq!(capped, content);
    }

    #[test]
    fn cap_sdk_tool_content_spills_above_threshold() {
        let _lock = crate::storage::lock_test_env();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        let _home = EnvGuard::set("JCODE_HOME", temp_dir.path());
        let session_id = "test_session_sdk";
        let tool_call_id = "test_tool_call_sdk";

        let content = "y".repeat(SPILL_THRESHOLD_CHARS + 2000);
        let capped = cap_sdk_tool_content_for_history("custom", session_id, tool_call_id, content.clone());

        // Verify spillover happened
        assert!(capped.contains("Tool output truncated by jcode"));
        assert!(capped.contains("produced"));
        assert!(capped.contains("FULL output saved to"));

        // Verify file was created
        let spill_path = temp_dir
            .path()
            .join("tool-outputs")
            .join(session_id)
            .join(format!("{}.txt", tool_call_id));
        assert!(spill_path.exists());
        let written_content = fs::read_to_string(&spill_path).expect("Failed to read spilled file");
        assert_eq!(written_content, content);
    }

    #[test]
    fn cap_sdk_tool_content_fallback_on_spill_failure() {
        let _lock = crate::storage::lock_test_env();
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        fs::write(temp_dir.path().join("tool-outputs"), "blocker").expect("write blocker");
        let _home = EnvGuard::set("JCODE_HOME", temp_dir.path());

        let content = "y".repeat(MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY + 10);
        let capped = cap_sdk_tool_content_for_history("custom", "sess123", "tool456", content);

        // On spill failure, should fall back to truncation at MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY
        assert!(capped.len() < MAX_TOOL_OUTPUT_CHARS_FOR_HISTORY + 5000);
        assert!(capped.contains("Tool output truncated by jcode"));
    }
}
