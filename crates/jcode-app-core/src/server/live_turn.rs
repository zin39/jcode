//! Server-initiated ("wake") turns for live sessions.
//!
//! Several server paths start a full conversation turn in a session without
//! that session's client sending a message: swarm DM/broadcast wake delivery,
//! background-task completion wakes, scheduled-task delivery, and post-reload
//! resume. Those turns must keep the same bookkeeping as client-initiated
//! turns, otherwise the swarm member status stays "ready/idle" while the agent
//! is actually streaming and attached TUIs never learn the turn finished.
//!
//! This module is the single shared implementation: it marks the member
//! `running` while the turn streams, flips it back to `ready` (with a
//! completion report) or `failed` at the end, and fans out a terminal
//! `Done`/`Error` event (id 0) so attached clients can settle the externally
//! started turn in their UI.

use super::client_lifecycle::process_message_streaming_mpsc;
use super::{
    SwarmEvent, SwarmMember, session_event_fanout_sender, truncate_detail, update_member_status,
    update_member_status_with_report,
};
use crate::agent::Agent;
use crate::protocol::ServerEvent;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock, broadcast, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;

/// Swarm bookkeeping handles needed to keep member status accurate around a
/// server-initiated turn.
#[derive(Clone)]
pub(super) struct LiveTurnSwarmContext {
    pub members: Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub swarms_by_id: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub event_history: Arc<RwLock<VecDeque<SwarmEvent>>>,
    pub event_counter: Arc<AtomicU64>,
    pub event_tx: broadcast::Sender<SwarmEvent>,
}

impl LiveTurnSwarmContext {
    pub(super) fn new(
        members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
        swarms_by_id: &Arc<RwLock<HashMap<String, HashSet<String>>>>,
        event_history: &Arc<RwLock<VecDeque<SwarmEvent>>>,
        event_counter: &Arc<AtomicU64>,
        event_tx: &broadcast::Sender<SwarmEvent>,
    ) -> Self {
        Self {
            members: Arc::clone(members),
            swarms_by_id: Arc::clone(swarms_by_id),
            event_history: Arc::clone(event_history),
            event_counter: Arc::clone(event_counter),
            event_tx: event_tx.clone(),
        }
    }
}

/// Return the live agent for `session_id`, together with its lock already
/// held, when the session has at least one live client attachment and its
/// agent is currently idle (lock not held).
///
/// The returned [`OwnedMutexGuard`] must be held by the caller until the turn
/// task has committed to running (see [`run_live_turn_if_idle`]). Dropping it
/// immediately after the idle check (as a prior implementation did, via
/// `agent.try_lock().is_ok()` on a throwaway temporary) reopens the classic
/// TOCTOU window: a second concurrent caller can observe the same "idle"
/// state before the first caller's turn actually starts, and both end up
/// spawning duplicate concurrent turns against the same session.
pub(super) async fn idle_live_agent(
    session_id: &str,
    sessions: &SessionAgents,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> Option<(Arc<Mutex<Agent>>, OwnedMutexGuard<Agent>)> {
    let agent = {
        let guard = sessions.read().await;
        guard.get(session_id).cloned()
    }?;

    let has_live_attachments = {
        let members = swarm_members.read().await;
        members
            .get(session_id)
            .map(|member| !member.event_txs.is_empty() || !member.event_tx.is_closed())
            .unwrap_or(false)
    };
    if !has_live_attachments {
        return None;
    }

    let agent_guard = Arc::clone(&agent).try_lock_owned().ok()?;
    Some((agent, agent_guard))
}

/// Run the streaming turn and report completion/failure back into swarm
/// bookkeeping. Shared by both `spawn_tracked_live_turn` entry points below;
/// the only difference between them is how `start_message_index` is obtained
/// (a fresh lock vs. an already-held guard handed off by the idle check).
async fn drive_tracked_live_turn(
    agent: Arc<Mutex<Agent>>,
    session_id: String,
    message: String,
    system_reminder: Option<String>,
    start_message_index: usize,
    event_tx: mpsc::UnboundedSender<ServerEvent>,
    swarm: LiveTurnSwarmContext,
) {
    let result = process_message_streaming_mpsc(
        Arc::clone(&agent),
        &message,
        vec![],
        system_reminder,
        event_tx.clone(),
    )
    .await;
    match result {
        Ok(()) => {
            let completion_report = {
                let agent_guard = agent.lock().await;
                agent_guard.latest_assistant_text_after(start_message_index)
            };
            update_member_status_with_report(
                &session_id,
                "ready",
                None,
                completion_report,
                &swarm.members,
                &swarm.swarms_by_id,
                Some(&swarm.event_history),
                Some(&swarm.event_counter),
                Some(&swarm.event_tx),
            )
            .await;
            let _ = event_tx.send(ServerEvent::Done { id: 0 });
        }
        Err(error) => {
            crate::logging::error(&format!(
                "Server-initiated turn failed for live session {}: {}",
                session_id, error
            ));
            update_member_status(
                &session_id,
                "failed",
                Some(truncate_detail(&error.to_string(), 120)),
                &swarm.members,
                &swarm.swarms_by_id,
                Some(&swarm.event_history),
                Some(&swarm.event_counter),
                Some(&swarm.event_tx),
            )
            .await;
            let _ = event_tx.send(ServerEvent::Error {
                id: 0,
                message: crate::util::format_error_chain(&error),
                retry_after_secs: None,
            });
        }
    }
}

/// Spawn `message` as a full tracked turn in a live session.
///
/// Mirrors the client-initiated turn lifecycle: the swarm member is marked
/// `running` before the turn starts and `ready` (with a completion report) or
/// `failed` when it finishes. A synthetic terminal `Done { id: 0 }` (or
/// `Error { id: 0, .. }`) is fanned out to attached clients so their UI can
/// finish rendering the externally started turn.
///
/// Callers that already hold the agent's lock from an idle check (see
/// [`idle_live_agent`] / [`run_live_turn_if_idle`]) should use
/// [`spawn_tracked_live_turn_with_lock`] instead so that lock is held
/// continuously through the hand-off, closing the TOCTOU window where a
/// second caller could also observe "idle" and start a duplicate turn.
pub(super) async fn spawn_tracked_live_turn(
    session_id: &str,
    agent: Arc<Mutex<Agent>>,
    message: String,
    system_reminder: Option<String>,
    status_detail: Option<String>,
    swarm: LiveTurnSwarmContext,
) {
    update_member_status(
        session_id,
        "running",
        status_detail,
        &swarm.members,
        &swarm.swarms_by_id,
        Some(&swarm.event_history),
        Some(&swarm.event_counter),
        Some(&swarm.event_tx),
    )
    .await;

    let event_tx = session_event_fanout_sender(session_id.to_string(), Arc::clone(&swarm.members));
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        let start_message_index = {
            let agent_guard = agent.lock().await;
            agent_guard.message_count()
        };
        drive_tracked_live_turn(
            agent,
            session_id,
            message,
            system_reminder,
            start_message_index,
            event_tx,
            swarm,
        )
        .await;
    });
}

/// Same as [`spawn_tracked_live_turn`], but takes ownership of an
/// [`OwnedMutexGuard`] the caller already holds from its idle check (see
/// [`idle_live_agent`]) instead of re-acquiring the lock after the fact.
///
/// The guard is kept alive across the `update_member_status("running")` call
/// and moved into the spawned task, where it is used to read
/// `start_message_index` and then dropped *before* `drive_tracked_live_turn`
/// starts its own locking — holding it any longer would self-deadlock against
/// `process_message_streaming_mpsc`'s internal `agent.lock().await` calls.
/// This keeps the agent continuously locked from the original idle check all
/// the way through to the point this task takes over the turn, so no second
/// concurrent caller can observe "idle" and spawn a duplicate turn in between.
async fn spawn_tracked_live_turn_with_lock(
    session_id: &str,
    agent: Arc<Mutex<Agent>>,
    agent_guard: OwnedMutexGuard<Agent>,
    message: String,
    system_reminder: Option<String>,
    status_detail: Option<String>,
    swarm: LiveTurnSwarmContext,
) {
    update_member_status(
        session_id,
        "running",
        status_detail,
        &swarm.members,
        &swarm.swarms_by_id,
        Some(&swarm.event_history),
        Some(&swarm.event_counter),
        Some(&swarm.event_tx),
    )
    .await;

    let event_tx = session_event_fanout_sender(session_id.to_string(), Arc::clone(&swarm.members));
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        let start_message_index = agent_guard.message_count();
        drop(agent_guard);
        drive_tracked_live_turn(
            agent,
            session_id,
            message,
            system_reminder,
            start_message_index,
            event_tx,
            swarm,
        )
        .await;
    });
}

/// Run `message` immediately as a tracked turn if the session is live and
/// idle. Returns `true` when the turn was started.
pub(super) async fn run_live_turn_if_idle(
    session_id: &str,
    message: &str,
    system_reminder: Option<String>,
    sessions: &SessionAgents,
    swarm: LiveTurnSwarmContext,
) -> bool {
    let Some((agent, agent_guard)) = idle_live_agent(session_id, sessions, &swarm.members).await
    else {
        return false;
    };
    let detail = Some(truncate_detail(message, 120)).filter(|detail| !detail.is_empty());
    spawn_tracked_live_turn_with_lock(
        session_id,
        agent,
        agent_guard,
        message.to_string(),
        system_reminder,
        detail,
        swarm,
    )
    .await;
    true
}
