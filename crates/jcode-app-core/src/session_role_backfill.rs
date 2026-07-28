//! One-time backfill of `agent_role` onto sessions written before it existed.
//!
//! The role is recorded at creation from now on, but ~11k sessions already on
//! disk predate that field. Without a backfill the fix only helps sessions
//! created after the upgrade, and the user's resume list stays buried under
//! years of machine-created work, which is the whole complaint.
//!
//! The backfill is deliberately conservative. Wrongly hiding a session the user
//! actually had is far worse than leaving a worker visible, so a session is
//! only reclassified on *structural* evidence that jcode itself produced:
//!
//! * lineage (`parent_id`) - already an internal marker;
//! * the swarm worker title format this codebase writes;
//! * being a single-turn run whose title is a verbatim subtask description,
//!   which is exactly the shape `cheap_route` and `subagent` create.
//!
//! Anything ambiguous is left alone. This never deletes a session: it only
//! annotates, so a mistake is recoverable by toggling test sessions on.

use crate::session::{Session, SessionAgentRole};

/// Marker recording that the backfill already ran, so startup does not rescan
/// thousands of files on every launch.
const MARKER: &str = "session-role-backfill-v1.done";

/// Title suffix written by `run_swarm_task` for spawned workers.
const SWARM_TITLE_SUFFIX: &str = " swarm)";

/// Decide the role for a legacy session, or `None` to leave it untouched.
///
/// Split out from the filesystem walk so the classification rules are testable
/// without writing thousands of session files.
pub(crate) fn classify_legacy_session(
    parent_id: Option<&str>,
    title: Option<&str>,
    user_message_count: usize,
) -> Option<SessionAgentRole> {
    if parent_id.is_some_and(|parent| !parent.trim().is_empty()) {
        return Some(SessionAgentRole::Internal);
    }

    let title = title.map(str::trim).filter(|title| !title.is_empty())?;

    // "<description> (@<type> swarm)" is written only by run_swarm_task.
    if title.ends_with(SWARM_TITLE_SUFFIX) && title.contains("(@") {
        return Some(SessionAgentRole::SwarmWorker);
    }

    // Cheap-route and subagent runs title the session with the subtask
    // description and complete in a single user turn. Interactive sessions
    // reach a second turn almost immediately, so requiring exactly one turn
    // keeps real short sessions visible.
    if user_message_count <= 1 {
        return Some(SessionAgentRole::Subagent);
    }

    None
}

/// Whether the backfill has already been performed.
fn already_done() -> bool {
    jcode_storage::jcode_dir()
        .map(|dir| dir.join(MARKER).exists())
        .unwrap_or(true)
}

fn mark_done(updated: usize) {
    let Ok(dir) = jcode_storage::jcode_dir() else {
        return;
    };
    // If the marker cannot be written the backfill simply runs again next
    // launch. That is wasteful but harmless (it is idempotent), so report it
    // instead of failing startup.
    if let Err(err) = std::fs::write(dir.join(MARKER), format!("{updated}\n")) {
        crate::logging::warn(&format!(
            "session role backfill completed but its marker could not be written; \
             it will run again next launch: {err}"
        ));
    }
}

/// Annotate legacy sessions with an agent role. Returns how many were updated.
///
/// Safe to call on every startup: it no-ops once the marker exists.
pub fn backfill_session_roles() -> usize {
    if already_done() {
        return 0;
    }

    let Ok(sessions_dir) = jcode_storage::jcode_dir().map(|dir| dir.join("sessions")) else {
        return 0;
    };
    let Ok(entries) = std::fs::read_dir(&sessions_dir) else {
        // No sessions directory yet: nothing to migrate, and marking it done
        // keeps a fresh install from rescanning forever.
        mark_done(0);
        return 0;
    };

    let mut updated = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        // Snapshots only. Journals are rebuilt from their snapshot, so writing
        // both would risk the two disagreeing.
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let mut session = match Session::load(stem) {
            Ok(session) => session,
            Err(err) => {
                // One unreadable session must not abort the migration for the
                // rest; it stays unclassified and simply remains visible.
                crate::logging::warn(&format!(
                    "session role backfill skipped unreadable session {stem}: {err}"
                ));
                continue;
            }
        };
        if session.agent_role.is_some() {
            continue;
        }

        let user_turns = session
            .messages
            .iter()
            .filter(|message| {
                matches!(message.role, crate::message::Role::User) && message.display_role.is_none()
            })
            .count();

        let Some(role) = classify_legacy_session(
            session.parent_id.as_deref(),
            session.title.as_deref(),
            user_turns,
        ) else {
            continue;
        };

        session.agent_role = Some(role);
        if session.save().is_ok() {
            updated += 1;
        }
    }

    mark_done(updated);
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_worker_titles_are_recognised() {
        assert_eq!(
            classify_legacy_session(None, Some("Investigate the picker (@explorer swarm)"), 1),
            Some(SessionAgentRole::SwarmWorker)
        );
    }

    #[test]
    fn lineage_alone_is_enough() {
        assert_eq!(
            classify_legacy_session(Some("session_parent"), None, 9),
            Some(SessionAgentRole::Internal),
            "a child session is internal regardless of how long it ran"
        );
    }

    #[test]
    fn single_turn_titled_runs_are_treated_as_subagents() {
        assert_eq!(
            classify_legacy_session(None, Some("Report value of CHEAP_ROUTE_COOLDOWN_SECS"), 1),
            Some(SessionAgentRole::Subagent)
        );
    }

    #[test]
    fn multi_turn_sessions_are_left_visible() {
        // The conservative half of the rule: a conversation the user kept
        // going is theirs, whatever its title looks like.
        assert_eq!(
            classify_legacy_session(None, Some("Fix the login bug"), 4),
            None,
            "hiding a real session is worse than leaving a worker visible"
        );
    }

    #[test]
    fn untitled_root_sessions_are_left_visible() {
        // Interactive sessions are usually untitled, so an untitled root
        // session must never be reclassified on turn count alone.
        assert_eq!(classify_legacy_session(None, None, 1), None);
        assert_eq!(classify_legacy_session(None, Some("   "), 0), None);
    }
}
