use super::{
    ClientConnectionInfo, FileTouchService, SwarmEvent, SwarmEventType, SwarmMember, SwarmState,
    VersionedPlan, broadcast_swarm_plan, persist_swarm_state_for, record_swarm_event,
};
use crate::agent::Agent;
use crate::protocol::{
    AgentStatusSnapshot, NotificationType, PlanGraphStatus, ServerEvent, SessionActivitySnapshot,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};

type SessionAgents = Arc<RwLock<HashMap<String, Arc<Mutex<Agent>>>>>;

pub(super) struct CommResyncPlanContext<'a> {
    pub(super) client_event_tx: &'a mpsc::UnboundedSender<ServerEvent>,
    pub(super) swarm_members: &'a Arc<RwLock<HashMap<String, SwarmMember>>>,
    pub(super) swarms_by_id: &'a Arc<RwLock<HashMap<String, HashSet<String>>>>,
    pub(super) swarm_plans: &'a Arc<RwLock<HashMap<String, VersionedPlan>>>,
    pub(super) swarm_coordinators: &'a Arc<RwLock<HashMap<String, String>>>,
    pub(super) event_history: &'a Arc<RwLock<std::collections::VecDeque<SwarmEvent>>>,
    pub(super) event_counter: &'a Arc<std::sync::atomic::AtomicU64>,
    pub(super) swarm_event_tx: &'a broadcast::Sender<SwarmEvent>,
}

fn live_activity_snapshot(
    connections: &HashMap<String, ClientConnectionInfo>,
    session_id: &str,
    fallback_processing: bool,
) -> Option<SessionActivitySnapshot> {
    let mut processing_without_tool = false;
    let mut tool_name = None;
    for info in connections.values() {
        if info.session_id != session_id || !info.is_processing {
            continue;
        }
        if let Some(current_tool_name) = info.current_tool_name.clone() {
            tool_name = Some(current_tool_name);
            break;
        }
        processing_without_tool = true;
    }

    tool_name
        .map(|current_tool_name| SessionActivitySnapshot {
            is_processing: true,
            current_tool_name: Some(current_tool_name),
        })
        .or_else(|| {
            processing_without_tool.then_some(SessionActivitySnapshot {
                is_processing: true,
                current_tool_name: None,
            })
        })
        .or_else(|| {
            fallback_processing.then_some(SessionActivitySnapshot {
                is_processing: true,
                current_tool_name: None,
            })
        })
}

/// Recent-token lookback window used when reporting per-agent churn in
/// `swarm list`. Short enough to reflect "what is this agent doing right now".
pub(super) const SWARM_LIST_TOKEN_WINDOW_SECS: u64 = 10;

/// Runtime extras for a swarm member, gathered without holding the agent lock
/// for long. Used to enrich the `swarm list` roster with live activity,
/// provider/model, token churn, turn count, and todo progress.
#[derive(Default)]
pub(super) struct MemberRuntimeExtras {
    pub(super) activity: Option<SessionActivitySnapshot>,
    pub(super) provider_name: Option<String>,
    pub(super) provider_model: Option<String>,
    pub(super) turn_count: Option<u64>,
    pub(super) recent_total_tokens: Option<u64>,
    pub(super) recent_output_tokens: Option<u64>,
    pub(super) recent_window_secs: Option<u64>,
    pub(super) cumulative_total_tokens: Option<u64>,
    pub(super) last_activity_age_secs: Option<u64>,
    pub(super) todos_completed: Option<usize>,
    pub(super) todos_total: Option<usize>,
}

/// Gather live runtime extras for a single member session.
///
/// `member_is_running` is used as a fallback "processing" hint when no live
/// client connection is reporting activity (e.g. headless sessions).
pub(super) async fn member_runtime_extras(
    session_id: &str,
    member_is_running: bool,
    sessions: &SessionAgents,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
) -> MemberRuntimeExtras {
    let activity = {
        let connections = client_connections.read().await;
        live_activity_snapshot(&connections, session_id, member_is_running)
    };

    let (provider_name, provider_model) = {
        let agent_sessions = sessions.read().await;
        if let Some(agent) = agent_sessions.get(session_id) {
            // Never block on a busy agent: token churn and turns come from the
            // lock-free metrics registry, so a missing provider name here just
            // means the agent is mid-turn.
            if let Ok(agent) = agent.try_lock() {
                (Some(agent.provider_name()), Some(agent.provider_model()))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        }
    };

    let metrics = crate::session_metrics::snapshot(
        session_id,
        std::time::Duration::from_secs(SWARM_LIST_TOKEN_WINDOW_SECS),
    );

    let (todos_completed, todos_total) = match crate::todo::load_todos(session_id) {
        Ok(todos) if !todos.is_empty() => {
            let completed = todos.iter().filter(|t| t.status == "completed").count();
            (Some(completed), Some(todos.len()))
        }
        _ => (None, None),
    };

    MemberRuntimeExtras {
        activity,
        provider_name,
        provider_model,
        turn_count: metrics.map(|m| m.turns),
        recent_total_tokens: metrics.map(|m| m.recent_total_tokens),
        recent_output_tokens: metrics.map(|m| m.recent_output_tokens),
        recent_window_secs: metrics.map(|_| SWARM_LIST_TOKEN_WINDOW_SECS),
        cumulative_total_tokens: metrics.map(|m| m.cumulative_total_tokens),
        last_activity_age_secs: metrics.and_then(|m| m.last_activity_age_secs),
        todos_completed,
        todos_total,
    }
}

async fn ensure_same_swarm_access(
    id: u64,
    req_session_id: &str,
    target_session: &str,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) -> bool {
    let (req_swarm, target_swarm) = {
        let members = swarm_members.read().await;
        (
            members
                .get(req_session_id)
                .and_then(|member| member.swarm_id.clone()),
            members
                .get(target_session)
                .and_then(|member| member.swarm_id.clone()),
        )
    };

    if req_swarm.is_some() && req_swarm == target_swarm {
        true
    } else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!(
                "Session '{}' is not in the same swarm as requester '{}'",
                target_session, req_session_id
            ),
            retry_after_secs: None,
        });
        false
    }
}

async fn can_read_full_context(
    req_session_id: &str,
    target_session: &str,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
) -> bool {
    if req_session_id == target_session {
        return true;
    }

    let members = swarm_members.read().await;
    members
        .get(req_session_id)
        .map(|member| member.role == "coordinator")
        .unwrap_or(false)
}

/// How long a read-only swarm inspection waits for a busy agent's lock.
///
/// An agent holds its lock for the duration of a turn, so `try_lock` fails
/// whenever the target is mid-work, which is exactly when a coordinator wants
/// to look at it. Rejecting immediately turned a normal contention window into
/// a tool failure the caller had to notice and retry by hand: "is busy; try
/// summary again shortly" is the single most common swarm tool error in
/// ~/.jcode/logs (41 occurrences across the retained logs).
///
/// A short bounded wait absorbs the common case where the lock frees in
/// milliseconds. It stays short because these handlers are awaited inline in
/// the per-client request loop, so this delays only the requesting client, and
/// a genuinely long-running turn still reports busy rather than hanging.
const BUSY_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(750);

/// Acquire a lock, waiting up to [`BUSY_LOCK_WAIT`] rather than failing on
/// contention. `None` means the holder is still working, which callers turn
/// into the "busy, retry shortly" response.
///
/// Generic over the guarded type so the waiting behavior is testable without
/// constructing a whole [`Agent`].
async fn lock_or_busy<T>(lock: &Mutex<T>) -> Option<tokio::sync::MutexGuard<'_, T>> {
    match tokio::time::timeout(BUSY_LOCK_WAIT, lock.lock()).await {
        Ok(guard) => Some(guard),
        // Elapsing is the expected outcome for a target that is mid-turn, not
        // an error to report.
        Err(_elapsed) => None,
    }
}

pub(super) async fn handle_comm_summary(
    id: u64,
    req_session_id: String,
    target_session: String,
    limit: Option<usize>,
    sessions: &SessionAgents,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if !ensure_same_swarm_access(
        id,
        &req_session_id,
        &target_session,
        swarm_members,
        client_event_tx,
    )
    .await
    {
        return;
    }

    let limit = limit.unwrap_or(10);
    let agent_sessions = sessions.read().await;
    if let Some(agent) = agent_sessions.get(&target_session) {
        let tool_calls = if let Some(agent) = lock_or_busy(agent).await {
            agent.get_tool_call_summaries(limit)
        } else {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!(
                    "Session '{}' is busy; try summary again shortly",
                    target_session
                ),
                retry_after_secs: Some(1),
            });
            return;
        };
        let _ = client_event_tx.send(ServerEvent::CommSummaryResponse {
            id,
            session_id: target_session,
            tool_calls,
        });
    } else {
        let _ = client_event_tx.send(ServerEvent::CommSummaryResponse {
            id,
            session_id: target_session,
            tool_calls: Vec::new(),
        });
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "status snapshots combine live connection state, session metadata, files touched, and optional provider/model hints"
)]
pub(super) async fn handle_comm_status(
    id: u64,
    req_session_id: String,
    target_session: String,
    sessions: &SessionAgents,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_connections: &Arc<RwLock<HashMap<String, ClientConnectionInfo>>>,
    file_touch: &FileTouchService,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if !ensure_same_swarm_access(
        id,
        &req_session_id,
        &target_session,
        swarm_members,
        client_event_tx,
    )
    .await
    {
        return;
    }

    let snapshot = {
        let members = swarm_members.read().await;
        let Some(member) = members.get(&target_session) else {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!("Unknown session '{target_session}'"),
                retry_after_secs: None,
            });
            return;
        };

        let files_touched = file_touch
            .sorted_file_strings_for_session(&target_session)
            .await;

        let activity = {
            let connections = client_connections.read().await;
            live_activity_snapshot(&connections, &target_session, member.status == "running")
        };

        let (provider_name, provider_model) = {
            let agent_sessions = sessions.read().await;
            if let Some(agent) = agent_sessions.get(&target_session) {
                if let Ok(agent) = agent.try_lock() {
                    (Some(agent.provider_name()), Some(agent.provider_model()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        };

        AgentStatusSnapshot {
            session_id: member.session_id.clone(),
            friendly_name: member.friendly_name.clone(),
            swarm_id: member.swarm_id.clone(),
            status: Some(member.status.clone()),
            detail: member.detail.clone(),
            role: Some(member.role.clone()),
            is_headless: Some(member.is_headless),
            live_attachments: Some(member.event_txs.len()),
            status_age_secs: Some(member.last_status_change.elapsed().as_secs()),
            last_activity_age_secs: crate::session_metrics::last_activity_age_secs(&target_session),
            joined_age_secs: Some(member.joined_at.elapsed().as_secs()),
            files_touched,
            activity,
            provider_name,
            provider_model,
        }
    };

    let _ = client_event_tx.send(ServerEvent::CommStatusResponse { id, snapshot });
}

pub(super) async fn handle_comm_read_context(
    id: u64,
    req_session_id: String,
    target_session: String,
    sessions: &SessionAgents,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    if !ensure_same_swarm_access(
        id,
        &req_session_id,
        &target_session,
        swarm_members,
        client_event_tx,
    )
    .await
    {
        return;
    }

    if !can_read_full_context(&req_session_id, &target_session, swarm_members).await {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Only the coordinator, worktree manager, or the target session may read full context. Use summary for lightweight access.".to_string(),
            retry_after_secs: None,
        });
        return;
    }

    let agent_sessions = sessions.read().await;
    if let Some(agent) = agent_sessions.get(&target_session) {
        let messages = if let Some(agent) = lock_or_busy(agent).await {
            agent.get_history()
        } else {
            let _ = client_event_tx.send(ServerEvent::Error {
                id,
                message: format!(
                    "Session '{}' is busy; try read_context again shortly",
                    target_session
                ),
                retry_after_secs: Some(1),
            });
            return;
        };
        let _ = client_event_tx.send(ServerEvent::CommContextHistory {
            id,
            session_id: target_session,
            messages,
        });
    } else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: format!("Unknown session '{target_session}'"),
            retry_after_secs: None,
        });
    }
}

pub(super) async fn handle_comm_plan_status(
    id: u64,
    req_session_id: String,
    swarm_members: &Arc<RwLock<HashMap<String, SwarmMember>>>,
    swarm_plans: &Arc<RwLock<HashMap<String, VersionedPlan>>>,
    client_event_tx: &mpsc::UnboundedSender<ServerEvent>,
) {
    let swarm_id = {
        let members = swarm_members.read().await;
        members
            .get(&req_session_id)
            .and_then(|member| member.swarm_id.clone())
    };

    let Some(swarm_id) = swarm_id else {
        let _ = client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm.".to_string(),
            retry_after_secs: None,
        });
        return;
    };

    let summary = {
        let plans = swarm_plans.read().await;
        let plan = plans.get(&swarm_id);
        if let Some(plan) = plan {
            PlanGraphStatus::from_versioned_plan(swarm_id.clone(), plan, Some(8), Vec::new())
        } else {
            PlanGraphStatus::empty_for_swarm(swarm_id.clone())
        }
    };

    let _ = client_event_tx.send(ServerEvent::CommPlanStatusResponse { id, summary });
}

pub(super) async fn handle_comm_resync_plan(
    id: u64,
    req_session_id: String,
    ctx: &CommResyncPlanContext<'_>,
) {
    let swarm_id = {
        let members = ctx.swarm_members.read().await;
        members
            .get(&req_session_id)
            .and_then(|member| member.swarm_id.clone())
    };

    if let Some(swarm_id) = swarm_id {
        let plan_state = {
            let mut plans = ctx.swarm_plans.write().await;
            plans.get_mut(&swarm_id).map(|plan| {
                plan.participants.insert(req_session_id.clone());
                (plan.version, plan.items.len())
            })
        };
        if let Some((version, item_count)) = plan_state {
            let swarm_state = SwarmState {
                members: Arc::clone(ctx.swarm_members),
                swarms_by_id: Arc::clone(ctx.swarms_by_id),
                plans: Arc::clone(ctx.swarm_plans),
                coordinators: Arc::clone(ctx.swarm_coordinators),
            };
            persist_swarm_state_for(&swarm_id, &swarm_state).await;
            if let Some(member) = ctx.swarm_members.read().await.get(&req_session_id) {
                let _ = member.event_tx.send(ServerEvent::Notification {
                    from_session: req_session_id.clone(),
                    from_name: member.friendly_name.clone(),
                    notification_type: NotificationType::Message {
                        scope: Some("plan".to_string()),
                        channel: None,
                        tldr: None,
                    },
                    message: format!(
                        "Plan attached to this session (v{}, {} items).",
                        version, item_count
                    ),
                });
            }
            broadcast_swarm_plan(
                &swarm_id,
                Some("resync".to_string()),
                ctx.swarm_plans,
                ctx.swarm_members,
                ctx.swarms_by_id,
            )
            .await;
            record_swarm_event(
                ctx.event_history,
                ctx.event_counter,
                ctx.swarm_event_tx,
                req_session_id.clone(),
                None,
                Some(swarm_id.clone()),
                SwarmEventType::PlanUpdate {
                    swarm_id: swarm_id.clone(),
                    item_count,
                },
            )
            .await;
            let _ = ctx.client_event_tx.send(ServerEvent::Done { id });
        } else {
            let _ = ctx.client_event_tx.send(ServerEvent::Error {
                id,
                message: "No swarm plan exists for this swarm.".to_string(),
                retry_after_secs: None,
            });
        }
    } else {
        let _ = ctx.client_event_tx.send(ServerEvent::Error {
            id,
            message: "Not in a swarm.".to_string(),
            retry_after_secs: None,
        });
    }
}

#[cfg(test)]
mod busy_lock_tests {
    use super::*;

    /// An uncontended lock must be returned immediately, so the bounded wait
    /// adds no latency to the normal path.
    #[tokio::test]
    async fn uncontended_lock_is_acquired_immediately() {
        let agent: Arc<Mutex<u32>> = Arc::new(Mutex::new(7));
        let start = std::time::Instant::now();
        let guard = lock_or_busy(&agent).await;
        assert!(guard.is_some(), "an uncontended lock must be acquired");
        assert!(
            start.elapsed() < BUSY_LOCK_WAIT / 2,
            "should not wait when free, took {:?}",
            start.elapsed()
        );
    }

    /// The failure this fixes: a lock held briefly by an in-flight turn used to
    /// reject the caller outright. Waiting absorbs that contention window.
    #[tokio::test]
    async fn briefly_held_lock_is_waited_for_rather_than_rejected() {
        let agent: Arc<Mutex<u32>> = Arc::new(Mutex::new(7));
        let holder = agent.clone();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();

        // Hold the lock on another task so the guard's lifetime is owned there,
        // mirroring an agent that holds its lock across an in-flight turn.
        tokio::spawn(async move {
            let guard = holder.lock().await;
            let _ = held_tx.send(());
            let _ = release_rx.await;
            drop(guard);
        });
        held_rx.await.expect("holder should take the lock");

        // try_lock is what the old code did, and it fails here.
        assert!(
            agent.try_lock().is_err(),
            "precondition: the lock is contended"
        );

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = release_tx.send(());
        });

        let acquired = lock_or_busy(&agent).await;
        assert!(
            acquired.is_some(),
            "a briefly held lock should be waited for, not rejected"
        );
    }

    /// A genuinely long-running turn must still report busy rather than hang
    /// the requesting client.
    #[tokio::test]
    async fn long_held_lock_still_reports_busy() {
        let agent: Arc<Mutex<u32>> = Arc::new(Mutex::new(7));
        let _held = agent.lock().await;

        let start = std::time::Instant::now();
        let acquired = lock_or_busy(&agent).await;
        assert!(acquired.is_none(), "a held lock must time out");
        assert!(
            start.elapsed() < BUSY_LOCK_WAIT * 3,
            "must give up promptly, took {:?}",
            start.elapsed()
        );
    }
}
