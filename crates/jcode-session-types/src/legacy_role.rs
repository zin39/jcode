//! Classify sessions that predate the `agent_role` field.
//!
//! The role is recorded at creation from now on, but thousands of sessions
//! already on disk have no classification, so without this the fix would only
//! apply to newly created sessions and the resume list would stay buried under
//! old machine-created work.
//!
//! This runs at *read* time, deriving the role from fields the session picker
//! already parses. The obvious alternative, a one-shot migration that rewrote
//! every snapshot, was measurably worse: on a real ~350MB session directory it
//! stalled the first `/resume` for over ten seconds and rewrote thousands of
//! files, trading one bad experience for another.
//!
//! Classification is deliberately conservative. Wrongly hiding a session the
//! user actually had is far worse than leaving a worker visible, so it keys on
//! structural evidence jcode itself produced:
//!
//! * lineage (`parent_id`) - already an internal marker;
//! * the swarm worker title format `run_swarm_task` writes;
//! * having no second user turn, which is what separates a one-shot agent call
//!   from a conversation worth resuming.
//!
//! A stored role always wins over anything inferred here, and a session the
//! user explicitly saved is never reclassified at all.

use crate::SessionAgentRole;

/// Title suffix written by `run_swarm_task` for spawned workers.
const SWARM_TITLE_SUFFIX: &str = " swarm)";

/// Decide the role for a legacy session, or `None` to leave it untouched.
///
/// Split out from the filesystem walk so the classification rules are testable
/// without writing thousands of session files.
pub fn classify_legacy_session(
    parent_id: Option<&str>,
    title: Option<&str>,
    user_message_count: usize,
    saved: bool,
) -> Option<SessionAgentRole> {
    // An explicit bookmark is the user stating this session matters. Never
    // infer it away, whatever its shape looks like.
    if saved {
        return None;
    }

    if parent_id.is_some_and(|parent| !parent.trim().is_empty()) {
        return Some(SessionAgentRole::Internal);
    }

    if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
        // "<description> (@<type> swarm)" is written only by run_swarm_task.
        if title.ends_with(SWARM_TITLE_SUFFIX) && title.contains("(@") {
            return Some(SessionAgentRole::SwarmWorker);
        }
    }

    // A conversation is what makes a session worth resuming, and a
    // conversation needs at least a second user turn. Everything jcode spawns
    // (cheap-route subtasks, subagents, health probes, `run -p` calls) is
    // one prompt and one answer, then done.
    //
    // Measured against the real corpus that motivated this: of ~4500 stored
    // sessions, only ~600 ever reached a third user turn, and those are
    // unmistakably the user's own work (hundreds of turns each), while the
    // single-turn population is dominated by machine traffic like
    // "Reply with exactly the two characters: OK".
    //
    // Requiring *two or more* turns to stay visible keeps a genuine short
    // session that the user replied in even once, while a session they opened
    // and abandoned after one prompt is not something they resume anyway.
    if user_message_count < 2 {
        return Some(SessionAgentRole::Subagent);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_worker_titles_are_recognised() {
        assert_eq!(
            classify_legacy_session(
                None,
                Some("Investigate the picker (@explorer swarm)"),
                1,
                false
            ),
            Some(SessionAgentRole::SwarmWorker)
        );
    }

    #[test]
    fn lineage_alone_is_enough() {
        assert_eq!(
            classify_legacy_session(Some("session_parent"), None, 9, false),
            Some(SessionAgentRole::Internal),
            "a child session is internal regardless of how long it ran"
        );
    }

    #[test]
    fn single_turn_titled_runs_are_treated_as_subagents() {
        assert_eq!(
            classify_legacy_session(
                None,
                Some("Report value of CHEAP_ROUTE_COOLDOWN_SECS"),
                1,
                false
            ),
            Some(SessionAgentRole::Subagent)
        );
    }

    #[test]
    fn multi_turn_sessions_are_left_visible() {
        // The conservative half of the rule: a conversation the user kept
        // going is theirs, whatever its title looks like.
        assert_eq!(
            classify_legacy_session(None, Some("Fix the login bug"), 4, false),
            None,
            "hiding a real session is worse than leaving a worker visible"
        );
    }

    #[test]
    fn untitled_single_turn_runs_are_hidden() {
        // The dominant machine population: untitled, one prompt, no reply.
        // Titles are not required, because health probes and `run -p` calls
        // create untitled sessions too.
        assert_eq!(
            classify_legacy_session(None, None, 1, false),
            Some(SessionAgentRole::Subagent)
        );
        assert_eq!(
            classify_legacy_session(None, Some("   "), 0, false),
            Some(SessionAgentRole::Subagent)
        );
    }

    #[test]
    fn a_session_the_user_replied_in_stays_visible() {
        // Two user turns means someone was talking to it. That is the line
        // between a conversation and a one-shot call.
        assert_eq!(classify_legacy_session(None, None, 2, false), None);
        assert_eq!(
            classify_legacy_session(None, Some("Untitled work"), 2, false),
            None
        );
    }

    #[test]
    fn a_saved_session_is_never_reclassified() {
        // Bookmarking is an explicit statement that this session matters, so
        // it outranks every structural signal, including single-turn shape.
        assert_eq!(
            classify_legacy_session(None, Some("Reply with OK"), 1, true),
            None
        );
        assert_eq!(
            classify_legacy_session(Some("session_parent"), None, 0, true),
            None
        );
    }
}
