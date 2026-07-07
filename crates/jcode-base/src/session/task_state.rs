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
}
