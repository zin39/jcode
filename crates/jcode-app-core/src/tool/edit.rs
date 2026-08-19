use super::{Tool, ToolContext, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};
use std::path::Path;

const FILE_TOUCH_PREVIEW_MAX_LINES: usize = 6;
const FILE_TOUCH_PREVIEW_MAX_BYTES: usize = 240;

pub struct EditTool;

impl EditTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct EditInput {
    #[serde(default)]
    intent: Option<String>,
    file_path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace text in a file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["file_path", "old_string", "new_string"],
            "properties": {
                "intent": super::intent_schema_property(),
                "file_path": {
                    "type": "string",
                    "description": "File path."
                },
                "old_string": {
                    "type": "string",
                    "description": "Text to replace."
                },
                "new_string": {
                    "type": "string",
                    "description": "Replacement text."
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all matches."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: EditInput = serde_json::from_value(input)?;

        if params.old_string == params.new_string {
            return Err(anyhow::anyhow!(
                "old_string and new_string must be different"
            ));
        }

        let path = ctx.resolve_path(Path::new(&params.file_path));

        if !path.exists() {
            return Err(super::read::file_not_found_error(&path, &params.file_path));
        }

        let content = tokio::fs::read_to_string(&path).await?;

        // A miss that is only leading/trailing whitespace is a typo, not an
        // ambiguity: when the trimmed form pins exactly one location we know
        // precisely what to edit, so apply it instead of spending a round trip
        // telling the caller to retype the snippet. "not found exactly, but
        // found after trimming whitespace" was a recurring live failure even
        // though the check that produced it had already located the match.
        let mut old_string = params.old_string.clone();
        let mut new_string = params.new_string.clone();
        let mut occurrences = content.matches(&old_string).count();

        if occurrences == 0
            && let Some(resolved) = resolve_whitespace_only_miss(&content, &old_string, &new_string)
        {
            old_string = resolved.old;
            new_string = resolved.new;
            occurrences = content.matches(&old_string).count();
        }

        if occurrences == 0 {
            // Try flexible matching
            return try_flexible_match(&content, &params.old_string, &params.file_path);
        }

        if occurrences > 1 && !params.replace_all {
            return Err(anyhow::anyhow!(
                "old_string found {} times in the file. Either:\n\
                 1. Provide more context to make it unique, or\n\
                 2. Set replace_all: true to replace all occurrences",
                occurrences
            ));
        }

        // Perform replacement
        let new_content = if params.replace_all {
            content.replace(&old_string, &new_string)
        } else {
            content.replacen(&old_string, &new_string, 1)
        };

        // Find line number where edit starts
        let start_line = find_line_number(&content, &old_string);

        // Write back
        tokio::fs::write(&path, &new_content).await?;

        // Generate a diff with line numbers
        let diff = generate_diff(&old_string, &new_string, start_line);

        // Publish file touch event for swarm coordination
        let end_line = start_line + new_string.lines().count().saturating_sub(1);
        let detail = build_file_touch_preview(&diff);
        Bus::global().publish(BusEvent::FileTouch(FileTouch {
            session_id: ctx.session_id.clone(),
            path: path.to_path_buf(),
            op: FileOp::Edit,
            intent: params
                .intent
                .clone()
                .filter(|value| !value.trim().is_empty()),
            summary: Some(format!(
                "edited lines {}-{} ({} occurrence{})",
                start_line,
                end_line,
                occurrences,
                if occurrences == 1 { "" } else { "s" }
            )),
            detail,
        }));

        // Extract context around the edit to help with consecutive edits
        let end_line = start_line + new_string.lines().count().saturating_sub(1);
        let context = extract_context(&new_content, start_line, end_line, 3);

        let mut body = format!(
            "Edited {}: replaced {} occurrence(s)\n{}\n\nContext after edit (lines {}-{}):\n{}",
            params.file_path, occurrences, diff, context.0, context.1, context.2
        );
        super::config_edit_notice::append_config_edit_notice(
            &mut body,
            &path,
            &content,
            &new_content,
        );

        Ok(ToolOutput::new(body).with_title(params.file_path.clone()))
    }
}

/// Find the 1-based line number where a substring starts
fn find_line_number(content: &str, substring: &str) -> usize {
    if let Some(pos) = content.find(substring) {
        content[..pos].lines().count() + 1
    } else {
        1
    }
}

/// Generate a compact diff: "42- old" / "42+ new"
fn generate_diff(old: &str, new: &str, start_line: usize) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut output = String::new();

    let mut old_line = start_line;
    let mut new_line = start_line;

    for change in diff.iter_all_changes() {
        let content = change.value().trim();
        let (prefix, line_num) = match change.tag() {
            ChangeTag::Delete => {
                let num = old_line;
                old_line += 1;
                if content.is_empty() {
                    continue;
                }
                ("-", num)
            }
            ChangeTag::Insert => {
                let num = new_line;
                new_line += 1;
                if content.is_empty() {
                    continue;
                }
                ("+", num)
            }
            ChangeTag::Equal => {
                old_line += 1;
                new_line += 1;
                continue;
            }
        };

        // Compact format: "42- content" (no spaces)
        output.push_str(&format!("{}{} {}\n", line_num, prefix, content));
    }

    if output.is_empty() {
        String::new()
    } else {
        output.trim_end().to_string()
    }
}

fn build_file_touch_preview(diff: &str) -> Option<String> {
    let trimmed = diff.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut lines = trimmed.lines();
    let mut preview = lines
        .by_ref()
        .take(FILE_TOUCH_PREVIEW_MAX_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let mut truncated = lines.next().is_some();

    if preview.len() > FILE_TOUCH_PREVIEW_MAX_BYTES {
        preview = crate::util::truncate_str(&preview, FILE_TOUCH_PREVIEW_MAX_BYTES)
            .trim_end()
            .to_string();
        truncated = true;
    }

    if truncated {
        preview.push_str("\n…");
    }

    Some(preview)
}

/// Extract lines around the edited region, returns (start_line, end_line, content)
fn extract_context(
    content: &str,
    edit_start: usize,
    edit_end: usize,
    padding: usize,
) -> (usize, usize, String) {
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    // Calculate range with padding (1-indexed to 0-indexed)
    let start = edit_start.saturating_sub(padding + 1);
    let end = (edit_end + padding).min(total_lines);

    let context_lines: Vec<String> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>4}│ {}", start + i + 1, line))
        .collect();

    (start + 1, end, context_lines.join("\n"))
}

/// An edit whose only error was surrounding whitespace, rewritten to the text
/// actually present in the file.
struct ResolvedMatch {
    old: String,
    new: String,
}

/// Resolve a miss caused solely by leading/trailing whitespace.
///
/// Returns `None` unless trimming pins exactly one location, so an ambiguous
/// or genuinely absent snippet still surfaces as an error rather than being
/// applied somewhere the caller did not mean. Both sides are trimmed together,
/// which leaves the file's own surrounding whitespace untouched.
fn resolve_whitespace_only_miss(
    content: &str,
    old_string: &str,
    new_string: &str,
) -> Option<ResolvedMatch> {
    let trimmed_old = old_string.trim();
    // An all-whitespace old_string trims to "", which every file "contains";
    // resolving that would target an arbitrary position.
    if trimmed_old.is_empty() || trimmed_old == old_string {
        return None;
    }
    if content.matches(trimmed_old).count() != 1 {
        return None;
    }
    let trimmed_new = new_string.trim();
    // Trimming can collapse a real edit into a no-op; that is a caller mistake
    // worth reporting rather than silently "succeeding" with no change.
    if trimmed_new == trimmed_old {
        return None;
    }
    Some(ResolvedMatch {
        old: trimmed_old.to_string(),
        new: trimmed_new.to_string(),
    })
}

fn try_flexible_match(content: &str, old_string: &str, file_path: &str) -> Result<ToolOutput> {
    // Trimming located the snippet but could not pin it to one place; naming
    // the count tells the caller how much more context to add.
    let trimmed = old_string.trim();
    if !trimmed.is_empty() && trimmed != old_string {
        let matches = content.matches(trimmed).count();
        if matches > 0 {
            return Err(anyhow::anyhow!(
                "old_string matched {} places once surrounding whitespace was ignored, \
                 so it is ambiguous.\n\
                 Include more surrounding lines to identify a single location.",
                matches
            ));
        }
    }

    // Try line-by-line matching with normalized whitespace
    let old_lines: Vec<&str> = old_string.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();

    for (i, window) in content_lines.windows(old_lines.len()).enumerate() {
        let matches = window
            .iter()
            .zip(old_lines.iter())
            .all(|(a, b)| a.trim() == b.trim());

        if matches {
            return Err(anyhow::anyhow!(
                "old_string not found exactly, but found with different indentation around line {}.\n\
                 Make sure to preserve the exact whitespace from the file.",
                i + 1
            ));
        }
    }

    Err(anyhow::anyhow!(
        "old_string not found in {}.\n\
         Use the read tool to see the current file contents.",
        file_path
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The logged live failure: a snippet that differs from the file only by
    /// surrounding whitespace was rejected even though the check that produced
    /// the error had already located it. One unambiguous location means we know
    /// exactly what to edit.
    #[test]
    fn whitespace_only_miss_is_resolved_when_unambiguous() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        let resolved = resolve_whitespace_only_miss(content, "  let x = 1;  ", "  let x = 2;  ")
            .expect("an unambiguous whitespace-only miss should resolve");

        assert_eq!(resolved.old, "let x = 1;");
        assert_eq!(resolved.new, "let x = 2;");
        assert_eq!(content.matches(&resolved.old).count(), 1);
    }

    /// Resolving must never guess: if trimming matches several places, the
    /// caller has to disambiguate rather than have us edit an arbitrary one.
    #[test]
    fn whitespace_only_miss_is_not_resolved_when_ambiguous() {
        let content = "let x = 1;\nlet x = 1;\n";
        assert!(resolve_whitespace_only_miss(content, " let x = 1; ", " let x = 2; ").is_none());
    }

    /// An exact match needs no rescue, so resolution must stay out of the way.
    #[test]
    fn exact_match_is_left_alone() {
        let content = "let x = 1;\n";
        assert!(resolve_whitespace_only_miss(content, "let x = 1;", "let x = 2;").is_none());
    }

    /// An all-whitespace old_string trims to "", which every file contains;
    /// resolving it would target an arbitrary position.
    #[test]
    fn whitespace_only_old_string_is_not_resolved() {
        let content = "let x = 1;\n";
        assert!(resolve_whitespace_only_miss(content, "   ", "something").is_none());
    }

    /// If trimming makes both sides identical the edit is a no-op, which is a
    /// caller mistake worth reporting rather than a silent success.
    #[test]
    fn miss_that_trims_to_a_no_op_is_not_resolved() {
        let content = "let x = 1;\n";
        assert!(resolve_whitespace_only_miss(content, "  let x = 1;", "let x = 1;  ").is_none());
    }

    /// A snippet that is genuinely absent must still fail.
    #[test]
    fn absent_snippet_is_not_resolved() {
        let content = "let x = 1;\n";
        assert!(
            resolve_whitespace_only_miss(content, "  let y = 9;  ", "  let y = 8;  ").is_none()
        );
    }

    /// When trimming finds the snippet in several places, the error should say
    /// how many, so the caller knows to add context instead of retyping.
    #[test]
    fn ambiguous_whitespace_miss_reports_the_count() {
        let content = "let x = 1;\nlet x = 1;\n";
        let err = try_flexible_match(content, " let x = 1; ", "f.rs")
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 places"), "should report the count: {err}");
        assert!(err.contains("ambiguous"), "should name the problem: {err}");
    }

    #[test]
    fn test_generate_diff_single_line_change() {
        let old = "hello world";
        let new = "hello rust";
        let diff = generate_diff(old, new, 10);

        // Compact format: "10- content" / "10+ content"
        assert!(diff.contains("10- hello world"), "Should show deleted line");
        assert!(diff.contains("10+ hello rust"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_multi_line() {
        let old = "line one\nline two\nline three";
        let new = "line one\nmodified two\nline three";
        let diff = generate_diff(old, new, 5);

        // Line 6 should be the changed line (5 + 1 for "line two")
        assert!(diff.contains("6- line two"), "Should show deleted line");
        assert!(diff.contains("6+ modified two"), "Should show added line");
        // Equal lines should not appear
        assert!(
            !diff.contains("line one"),
            "Should not show unchanged lines"
        );
        assert!(
            !diff.contains("line three"),
            "Should not show unchanged lines"
        );
    }

    #[test]
    fn test_generate_diff_addition_only() {
        let old = "first\nthird";
        let new = "first\nsecond\nthird";
        let diff = generate_diff(old, new, 1);

        assert!(diff.contains("+ second"), "Should show added line");
    }

    #[test]
    fn test_generate_diff_deletion_only() {
        let old = "first\nsecond\nthird";
        let new = "first\nthird";
        let diff = generate_diff(old, new, 1);

        assert!(diff.contains("- second"), "Should show deleted line");
    }

    #[test]
    fn test_generate_diff_no_changes() {
        let old = "same content";
        let new = "same content";
        let diff = generate_diff(old, new, 1);

        assert!(diff.is_empty(), "No changes should produce empty diff");
    }

    #[test]
    fn test_generate_diff_line_number_format() {
        let old = "old";
        let new = "new";
        let diff = generate_diff(old, new, 42);

        // Compact format: no padding
        assert!(
            diff.contains("42- old"),
            "Should have line number directly before minus"
        );
        assert!(
            diff.contains("42+ new"),
            "Should have line number directly before plus"
        );
    }

    #[test]
    fn test_find_line_number() {
        let content = "line 1\nline 2\nline 3\nline 4";

        assert_eq!(find_line_number(content, "line 1"), 1);
        assert_eq!(find_line_number(content, "line 2"), 2);
        assert_eq!(find_line_number(content, "line 3"), 3);
        assert_eq!(find_line_number(content, "line 4"), 4);
        assert_eq!(find_line_number(content, "not found"), 1);
    }

    #[test]
    fn test_extract_context() {
        let content =
            "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10";

        // Edit at line 5, with 2 lines padding
        let (start, end, ctx) = extract_context(content, 5, 5, 2);

        assert_eq!(start, 3, "Should start at line 3 (5 - 2)");
        assert_eq!(end, 7, "Should end at line 7 (5 + 2)");
        assert!(ctx.contains("line 3"), "Should include line 3");
        assert!(ctx.contains("line 5"), "Should include edited line 5");
        assert!(ctx.contains("line 7"), "Should include line 7");
        assert!(!ctx.contains("line 2"), "Should not include line 2");
        assert!(!ctx.contains("line 8"), "Should not include line 8");
    }

    #[test]
    fn test_extract_context_at_start() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        // Edit at line 1, with 2 lines padding - shouldn't go negative
        let (start, _end, ctx) = extract_context(content, 1, 1, 2);

        assert_eq!(start, 1, "Should start at line 1 (can't go before)");
        assert!(ctx.contains("line 1"), "Should include line 1");
        assert!(ctx.contains("line 3"), "Should include line 3");
    }

    #[test]
    fn test_extract_context_at_end() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        // Edit at line 5, with 2 lines padding - shouldn't go past end
        let (_start, end, ctx) = extract_context(content, 5, 5, 2);

        assert_eq!(end, 5, "Should end at line 5 (can't go past)");
        assert!(ctx.contains("line 5"), "Should include line 5");
        assert!(ctx.contains("line 3"), "Should include line 3");
    }

    #[test]
    fn test_extract_context_range_past_end() {
        let content = "line 1\nline 2\nline 3\nline 4\nline 5";

        // Edit range extends past the end of the file.
        let (start, end, ctx) = extract_context(content, 4, 10, 1);

        assert_eq!(start, 3, "Should start at line 3 (4 - 1)");
        assert_eq!(end, 5, "Should clamp to last line");
        assert!(ctx.contains("line 3"), "Should include line 3");
        assert!(ctx.contains("line 5"), "Should include line 5");
    }
}
