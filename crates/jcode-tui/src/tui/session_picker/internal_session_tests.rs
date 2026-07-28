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

#[test]
fn legacy_sessions_without_a_stored_role_are_still_classified_on_load() {
    // End-to-end cover for the pre-existing corpus: sessions written before
    // `agent_role` existed have `None` on disk, and must still be recognised
    // as machine-created when the picker loads them. Without this the fix
    // would only apply to newly created sessions.
    let _env_lock = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let previous_home = std::env::var("JCODE_HOME").ok();
    crate::env::set_var("JCODE_HOME", temp.path());

    let push_user = |session: &mut Session, id: &str, text: &str| {
        session.append_stored_message(crate::session::StoredMessage {
            id: id.to_string(),
            role: crate::message::Role::User,
            content: vec![crate::message::ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        });
    };

    // A one-shot agent run: single user turn, no stored role.
    let mut one_shot = Session::create_with_id("session_legacy_oneshot".to_string(), None, None);
    push_user(
        &mut one_shot,
        "m1",
        "Reply with exactly the two characters: OK",
    );
    assert!(
        one_shot.agent_role.is_none(),
        "fixture must reproduce the legacy on-disk shape: no stored role"
    );
    one_shot.save().expect("save one-shot session");

    // A real conversation: the user replied, so it stays resumable.
    let mut conversation =
        Session::create_with_id("session_legacy_conversation".to_string(), None, None);
    push_user(&mut conversation, "m1", "help me fix the build");
    push_user(&mut conversation, "m2", "still broken, try again");
    conversation.save().expect("save conversation");

    invalidate_session_list_cache();
    let sessions = load_sessions().expect("load sessions");

    let loaded_one_shot = sessions
        .iter()
        .find(|session| session.id == "session_legacy_oneshot")
        .expect("one-shot session should still be loaded, just hidden");
    let loaded_conversation = sessions
        .iter()
        .find(|session| session.id == "session_legacy_conversation")
        .expect("conversation should load");
    let one_shot_hidden = loaded_one_shot.is_internal_agent_session();
    let conversation_visible = !loaded_conversation.is_internal_agent_session();

    match previous_home {
        Some(home) => crate::env::set_var("JCODE_HOME", home),
        None => crate::env::remove_var("JCODE_HOME"),
    }
    invalidate_session_list_cache();

    assert!(
        one_shot_hidden,
        "a legacy single-turn run must be recognised as machine-created without a stored role"
    );
    assert!(
        conversation_visible,
        "a session the user replied in must remain visible in the resume list"
    );
}
