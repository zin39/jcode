//! Per-session task-state file: small model-maintained doc (plan / progress /
//! decisions) that survives compaction because it lives on disk and is
//! re-injected into the dynamic system prompt every turn.

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Hard cap on stored task-state size. Injected every turn, so keep it small.
pub const MAX_TASK_STATE_CHARS: usize = 8_192;
pub const TRUNCATION_MARKER: &str = "\n[task state truncated by jcode at 8KB cap]";

pub fn task_state_path_in_dir(base: &Path, session_id: &str) -> PathBuf {
    base.join("sessions")
        .join(format!("{}.task-state.md", session_id))
}

pub fn task_state_path(session_id: &str) -> Result<PathBuf> {
    let base = crate::storage::jcode_dir()?;
    Ok(task_state_path_in_dir(&base, session_id))
}

pub fn read_task_state_in_dir(base: &Path, session_id: &str) -> Option<String> {
    let content = std::fs::read_to_string(task_state_path_in_dir(base, session_id)).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read the task state for a session, or `None` if absent/empty.
pub fn read_task_state(session_id: &str) -> Option<String> {
    let base = crate::storage::jcode_dir().ok()?;
    read_task_state_in_dir(&base, session_id)
}

pub fn write_task_state_in_dir(base: &Path, session_id: &str, content: &str) -> Result<()> {
    let path = task_state_path_in_dir(base, session_id);
    if content.trim().is_empty() {
        // Empty write clears the state (file removed so injection stops).
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let capped: String = if content.chars().count() > MAX_TASK_STATE_CHARS {
        let mut s: String = content.chars().take(MAX_TASK_STATE_CHARS).collect();
        s.push_str(TRUNCATION_MARKER);
        s
    } else {
        content.to_string()
    };
    std::fs::write(&path, capped)?;
    Ok(())
}

/// Write (full replace) the task state for a session. Empty content clears it.
pub fn write_task_state(session_id: &str, content: &str) -> Result<()> {
    let base = crate::storage::jcode_dir()?;
    write_task_state_in_dir(&base, session_id, content)
}

/// Auto-seed a task state from the first user message if none exists yet.
///
/// This implements the "recitation" pattern: the original user goal is captured
/// to disk so it survives compaction even when the agent never calls
/// `update_task_state` explicitly.
///
/// Messages under 20 chars (e.g. "hi") are skipped. Messages over 2000 chars
/// are truncated at a char boundary with a "... [truncated]" suffix.
pub fn seed_task_state_if_empty_in_dir(
    base: &Path,
    session_id: &str,
    first_user_message: &str,
) {
    if read_task_state_in_dir(base, session_id).is_some() {
        return; // Already has state – no-op
    }
    let trimmed = first_user_message.trim();
    if trimmed.chars().count() < 20 {
        return; // Greeting / trivial message – skip
    }
    let truncated: String = if trimmed.chars().count() > 2000 {
        let mut s: String = trimmed.chars().take(2000).collect();
        s.push_str("... [truncated]");
        s
    } else {
        trimmed.to_string()
    };
    let state = format!(
        "# Original Task (auto-captured)\n\n{truncated}\n\n## Working State\n\n(not yet updated - use update_task_state as you work)"
    );
    let _ = write_task_state_in_dir(base, session_id, &state);
}

/// Convenience wrapper that resolves the jcode directory internally.
pub fn seed_task_state_if_empty(session_id: &str, first_user_message: &str) {
    let base = match crate::storage::jcode_dir() {
        Ok(b) => b,
        Err(_) => return,
    };
    seed_task_state_if_empty_in_dir(&base, session_id, first_user_message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_content() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state_in_dir(dir.path(), "s1", "## Plan\n- step").unwrap();
        assert_eq!(
            read_task_state_in_dir(dir.path(), "s1").as_deref(),
            Some("## Plan\n- step")
        );
    }

    #[test]
    fn missing_file_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_task_state_in_dir(dir.path(), "nope"), None);
    }

    #[test]
    fn caps_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_TASK_STATE_CHARS + 100);
        write_task_state_in_dir(dir.path(), "s2", &big).unwrap();
        let read = read_task_state_in_dir(dir.path(), "s2").unwrap();
        assert_eq!(read.chars().count(), MAX_TASK_STATE_CHARS + TRUNCATION_MARKER.chars().count());
        assert!(read.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn empty_write_clears_state() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state_in_dir(dir.path(), "s3", "content").unwrap();
        write_task_state_in_dir(dir.path(), "s3", "").unwrap();
        assert_eq!(read_task_state_in_dir(dir.path(), "s3"), None);
    }

    #[test]
    fn seed_creates_state_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        seed_task_state_if_empty_in_dir(dir.path(), "s1", "I need to build a CRUD API for my todo app");
        let state = read_task_state_in_dir(dir.path(), "s1").unwrap();
        assert!(state.starts_with("# Original Task (auto-captured)"));
        assert!(state.contains("build a CRUD API"));
        assert!(state.contains("## Working State"));
        assert!(state.contains("use update_task_state as you work"));
    }

    #[test]
    fn seed_no_ops_when_state_exists() {
        let dir = tempfile::tempdir().unwrap();
        write_task_state_in_dir(dir.path(), "s1", "## Existing Plan").unwrap();
        // Seed with a different message – should NOT overwrite
        seed_task_state_if_empty_in_dir(dir.path(), "s1", "I need to build a CRUD API");
        let state = read_task_state_in_dir(dir.path(), "s1").unwrap();
        assert_eq!(state, "## Existing Plan");
    }

    #[test]
    fn seed_skips_short_messages() {
        let dir = tempfile::tempdir().unwrap();
        seed_task_state_if_empty_in_dir(dir.path(), "s1", "hi");
        assert_eq!(read_task_state_in_dir(dir.path(), "s1"), None);
        seed_task_state_if_empty_in_dir(dir.path(), "s2", "hello there");
        assert_eq!(read_task_state_in_dir(dir.path(), "s2"), None);
        // Exactly 19 chars should be skipped
        seed_task_state_if_empty_in_dir(dir.path(), "s3", "1234567890123456789");
        assert_eq!(read_task_state_in_dir(dir.path(), "s3"), None);
        // 20 chars should be accepted
        seed_task_state_if_empty_in_dir(dir.path(), "s4", "12345678901234567890");
        assert!(read_task_state_in_dir(dir.path(), "s4").is_some());
    }

    #[test]
    fn seed_truncates_long_messages() {
        let dir = tempfile::tempdir().unwrap();
        let long = "a".repeat(2500);
        seed_task_state_if_empty_in_dir(dir.path(), "s1", &long);
        let state = read_task_state_in_dir(dir.path(), "s1").unwrap();
        // Truncated at 2000 chars + "... [truncated]" (16 chars) = 2016
        assert!(state.contains("... [truncated]"));
        assert!(!state.contains(&"a".repeat(2001)));
        // The 2000 'a's should be there, plus the truncated marker
        let content_start = "# Original Task (auto-captured)\n\n";
        let content_part = &state[content_start.len()..];
        let end_of_content = content_part.find("\n\n## Working State").unwrap();
        let content = &content_part[..end_of_content];
        assert_eq!(content, &format!("{}... [truncated]", "a".repeat(2000)));
    }
}
