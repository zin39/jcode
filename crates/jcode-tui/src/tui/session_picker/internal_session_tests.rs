//! The resume picker must list sessions the user opened, and only those.
//!
//! Regression cover for the flood that made `/resume` unusable: every spawned
//! agent wrote a session, and because the picker filtered only `is_debug`,
//! swarm workers, cheap-route subtasks and one-shot runs all listed as if the
//! user had started them. These tests pin the *rule* rather than any one spawn
//! path, so a future spawn site cannot quietly reintroduce the flood.

use super::tests::make_session;
use super::*;
use jcode_session_types::SessionAgentRole;

fn make_agent_session(id: &str, short_name: &str, role: SessionAgentRole) -> SessionInfo {
    let mut session = make_session(id, short_name, false, SessionStatus::Closed);
    session.agent_role = Some(role);
    session
}

#[test]
fn spawned_agent_sessions_are_hidden_from_the_default_picker() {
    let user = make_session("session_user", "user", false, SessionStatus::Closed);
    let worker = make_agent_session("session_worker", "worker", SessionAgentRole::SwarmWorker);
    let subtask = make_agent_session(
        "session_subtask",
        "subtask",
        SessionAgentRole::CheapRouteSubtask,
    );
    let one_shot = make_agent_session("session_oneshot", "oneshot", SessionAgentRole::OneShot);

    let picker = SessionPicker::new(vec![
        user.clone(),
        worker.clone(),
        subtask.clone(),
        one_shot.clone(),
    ]);

    assert_eq!(
        picker.visible_sessions.len(),
        1,
        "only the session the user actually opened belongs in the default list"
    );
    assert_eq!(
        picker.hidden_test_count, 3,
        "the hidden count must report every machine-created session, so the toggle hint is honest"
    );
}

#[test]
fn child_sessions_are_hidden_even_without_an_explicit_role() {
    // Lineage alone is enough. This is the property that makes the fix
    // durable: a new spawn path that forgets to declare a role is still
    // hidden, as long as it records its parent.
    let user = make_session("session_user", "user", false, SessionStatus::Closed);
    let mut child = make_session("session_child", "child", false, SessionStatus::Closed);
    child.parent_id = Some("session_user".to_string());

    let picker = SessionPicker::new(vec![user, child]);

    assert_eq!(
        picker.visible_sessions.len(),
        1,
        "a session spawned from another session is not a session the user opened"
    );
}

#[test]
fn toggling_test_sessions_reveals_spawned_agent_sessions() {
    // Hidden must not mean unreachable: debugging a swarm run requires getting
    // at the worker transcripts.
    let user = make_session("session_user", "user", false, SessionStatus::Closed);
    let worker = make_agent_session("session_worker", "worker", SessionAgentRole::SwarmWorker);

    let mut picker = SessionPicker::new(vec![user, worker]);
    assert_eq!(picker.visible_sessions.len(), 1);

    picker.toggle_test_sessions();
    assert_eq!(
        picker.visible_sessions.len(),
        2,
        "the test-session toggle must still surface machine-created sessions"
    );
    assert_eq!(picker.hidden_test_count, 0);
}

#[test]
fn a_blank_parent_id_does_not_count_as_lineage() {
    // Some persisted sessions carry an empty string rather than null. Treating
    // that as a parent would hide real user sessions, which is far worse than
    // showing an extra worker.
    let mut session = make_session("session_user", "user", false, SessionStatus::Closed);
    session.parent_id = Some("   ".to_string());

    let picker = SessionPicker::new(vec![session]);

    assert_eq!(
        picker.visible_sessions.len(),
        1,
        "a blank parent_id is not lineage and must never hide a user's own session"
    );
}
