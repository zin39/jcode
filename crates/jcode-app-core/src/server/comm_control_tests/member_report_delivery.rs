/// Regression tests for swarm member completion-report delivery.
///
/// Bug: worker completion reports were only fanned out to attached UI clients
/// as a transient `scope:"swarm"` notification card, which the TUI
/// intentionally does not persist into the transcript, so the report body
/// never reached the coordinating agent's context. The fix publishes
/// `BusEvent::SwarmMemberReportReady` at the same site and delivers it via
/// wake/soft-interrupt like background-task completions.

#[tokio::test]
async fn member_completion_publishes_report_ready_bus_event() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-report-ready";
    let coordinator = "coord-report-ready";
    let worker = "worker-report-ready";

    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (
            coordinator.to_string(),
            member(coordinator, swarm_id, "ready"),
        ),
        (
            worker.to_string(),
            owned_member(worker, swarm_id, "running", coordinator),
        ),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([coordinator.to_string(), worker.to_string()]),
    )])));

    let mut bus_rx = crate::bus::Bus::global().subscribe();

    crate::server::swarm::update_member_status_with_report_tldr(
        worker,
        "completed",
        Some("done".to_string()),
        Some("REPORT-BODY: built the thing, tests pass".to_string()),
        Some("built the thing".to_string()),
        &swarm_members,
        &swarms_by_id,
        None,
        None,
        None,
    )
    .await;

    let event = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match bus_rx.recv().await {
                Ok(crate::bus::BusEvent::SwarmMemberReportReady(event))
                    if event.member_session_id == worker =>
                {
                    return event;
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("bus closed before SwarmMemberReportReady arrived")
                }
            }
        }
    })
    .await
    .expect("member completion with a report should publish SwarmMemberReportReady");

    assert_eq!(event.recipient_session_id, coordinator);
    assert_eq!(event.status, "completed");
    assert!(
        event.notification.contains("REPORT-BODY: built the thing"),
        "notification should carry the report body, got: {}",
        event.notification
    );
}

#[tokio::test]
async fn dispatch_member_report_queues_soft_interrupt_for_busy_recipient() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-report-dispatch";
    let coordinator = "coord-report-dispatch";
    let worker = "worker-report-dispatch";

    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        member(coordinator, swarm_id, "running"),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([coordinator.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(AtomicU64::new(0));

    // No live agent for the coordinator (busy/detached), but a registered
    // interrupt queue: delivery must fall back to the soft-interrupt path.
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let queue: jcode_agent_runtime::SoftInterruptQueue =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        queue.clone(),
    )])));

    let event = crate::bus::SwarmMemberReportReady {
        recipient_session_id: coordinator.to_string(),
        member_session_id: worker.to_string(),
        member_name: Some("fox".to_string()),
        status: "completed".to_string(),
        notification: "fox completed.\n\nReport:\nREPORT-BODY: all done".to_string(),
    };

    crate::server::background_tasks::dispatch_swarm_member_report(
        &event,
        &sessions,
        &soft_interrupt_queues,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    )
    .await;

    let queued = queue.lock().expect("queue lock");
    assert_eq!(
        queued.len(),
        1,
        "report should be queued as a soft interrupt for the busy recipient"
    );
    assert!(
        queued[0].content.contains("REPORT-BODY: all done"),
        "queued interrupt should carry the report body, got: {}",
        queued[0].content
    );
}

#[tokio::test]
async fn dispatch_member_report_skips_when_pending_await_covers_member() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-report-await-dedupe";
    let coordinator = "coord-report-await-dedupe";
    let worker = "worker-report-await-dedupe";

    // Pending background await watching this member for "completed": the
    // await's own completion delivery carries the report, so direct dispatch
    // must skip to avoid duplicating it in the recipient's context.
    let key = crate::server::await_members_state::request_key(
        coordinator,
        swarm_id,
        &[worker.to_string()],
        &["completed".to_string()],
        None,
    );
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    crate::server::await_members_state::save_state(
        &crate::server::await_members_state::PersistedAwaitMembersState {
            key,
            session_id: coordinator.to_string(),
            swarm_id: swarm_id.to_string(),
            target_status: vec!["completed".to_string()],
            requested_ids: vec![worker.to_string()],
            mode: None,
            created_at_unix_ms: now_ms,
            deadline_unix_ms: now_ms + 60_000,
            background: true,
            notify: true,
            wake: true,
            final_response: None,
        },
    );

    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        member(coordinator, swarm_id, "running"),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([coordinator.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(AtomicU64::new(0));
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let queue: jcode_agent_runtime::SoftInterruptQueue =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        queue.clone(),
    )])));

    let event = crate::bus::SwarmMemberReportReady {
        recipient_session_id: coordinator.to_string(),
        member_session_id: worker.to_string(),
        member_name: Some("fox".to_string()),
        status: "completed".to_string(),
        notification: "fox completed.\n\nReport:\nREPORT-BODY: covered by await".to_string(),
    };

    crate::server::background_tasks::dispatch_swarm_member_report(
        &event,
        &sessions,
        &soft_interrupt_queues,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    )
    .await;

    assert!(
        queue.lock().expect("queue lock").is_empty(),
        "report covered by a pending await must not be double-delivered"
    );
}

/// Restores `JCODE_HOME` on drop. Used by the durable-fallback test, which
/// needs a real on-disk jcode home so `session_exists` and the pending
/// soft-interrupt store resolve into a temp dir. Deliberately does NOT take the
/// storage test lock: callers already hold it via `RuntimeEnvGuard`.
struct HomeEnvGuard {
    prev_home: Option<std::ffi::OsString>,
}

impl HomeEnvGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev_home = std::env::var_os("JCODE_HOME");
        crate::env::set_var("JCODE_HOME", path);
        Self { prev_home }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("JCODE_HOME", prev_home);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
    }
}

#[tokio::test]
async fn dispatch_member_report_wakes_idle_recipient_instead_of_queueing() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-report-wake";
    let coordinator = "coord-report-wake";
    let worker = "worker-report-wake";

    // Live, idle recipient: an agent exists in `sessions` and the member has an
    // open event channel, so `run_live_turn_if_idle` must drive the report as a
    // real turn rather than parking it in the interrupt queue.
    let coordinator_agent = test_agent().await;
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        Arc::clone(&coordinator_agent),
    )])));

    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let swarm_members = Arc::new(RwLock::new(HashMap::from([(coordinator.to_string(), {
        let mut m = member(coordinator, swarm_id, "ready");
        // Keep the receiver alive above so this sender stays open: that is what
        // marks the session as having a live attachment.
        m.event_tx = event_tx;
        m.role = "coordinator".to_string();
        m
    })])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([coordinator.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(AtomicU64::new(0));
    let queue: jcode_agent_runtime::SoftInterruptQueue =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        queue.clone(),
    )])));

    let event = crate::bus::SwarmMemberReportReady {
        recipient_session_id: coordinator.to_string(),
        member_session_id: worker.to_string(),
        member_name: Some("fox".to_string()),
        status: "completed".to_string(),
        notification: "fox completed.\n\nReport:\nREPORT-BODY: woke the coordinator".to_string(),
    };

    crate::server::background_tasks::dispatch_swarm_member_report(
        &event,
        &sessions,
        &soft_interrupt_queues,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    )
    .await;

    // `spawn_tracked_live_turn` flips the member to `running` synchronously
    // before spawning the turn, so this is deterministic without sleeping.
    assert_eq!(
        swarm_members.read().await[coordinator].status, "running",
        "an idle recipient must be woken into a live turn carrying the report"
    );
    assert!(
        queue.lock().expect("queue lock").is_empty(),
        "a woken recipient must not also get the report queued as a soft interrupt"
    );
}

#[tokio::test]
async fn dispatch_member_report_persists_for_detached_recipient_session() {
    let (_env, runtime_dir) = RuntimeEnvGuard::new();
    let _home = HomeEnvGuard::set(runtime_dir.path());

    let swarm_id = "swarm-report-durable";
    let coordinator = "coord-report-durable";
    let worker = "worker-report-durable";

    // The recipient session exists on disk but has no live agent and no
    // registered interrupt queue (detached, e.g. the TUI was closed). Delivery
    // must fall through to the durable pending-soft-interrupt store so the
    // report survives until the session is reopened.
    let sessions_dir = runtime_dir.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
    std::fs::write(sessions_dir.join(format!("{coordinator}.json")), "{}")
        .expect("write session snapshot");
    assert!(
        crate::session::session_exists(coordinator),
        "fixture must look like a real on-disk session"
    );

    let swarm_members = Arc::new(RwLock::new(HashMap::from([(
        coordinator.to_string(),
        member(coordinator, swarm_id, "ready"),
    )])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([coordinator.to_string()]),
    )])));
    let (swarm_event_tx, _swarm_event_rx) = broadcast::channel(32);
    let event_history = Arc::new(RwLock::new(VecDeque::new()));
    let event_counter = Arc::new(AtomicU64::new(0));
    let sessions: crate::server::SessionAgents = Arc::new(RwLock::new(HashMap::new()));
    let soft_interrupt_queues = Arc::new(RwLock::new(HashMap::new()));

    let event = crate::bus::SwarmMemberReportReady {
        recipient_session_id: coordinator.to_string(),
        member_session_id: worker.to_string(),
        member_name: Some("fox".to_string()),
        status: "completed".to_string(),
        notification: "fox completed.\n\nReport:\nREPORT-BODY: survived detach".to_string(),
    };

    crate::server::background_tasks::dispatch_swarm_member_report(
        &event,
        &sessions,
        &soft_interrupt_queues,
        &swarm_members,
        &swarms_by_id,
        &event_history,
        &event_counter,
        &swarm_event_tx,
    )
    .await;

    let persisted =
        crate::soft_interrupt_store::load(coordinator).expect("load pending soft interrupts");
    assert_eq!(
        persisted.len(),
        1,
        "report for a detached session must be persisted, not dropped"
    );
    assert!(
        persisted[0].content.contains("REPORT-BODY: survived detach"),
        "persisted interrupt should carry the report body, got: {}",
        persisted[0].content
    );
}

#[tokio::test]
async fn failed_and_crashed_members_publish_report_ready_for_their_owner() {
    let (_env, _runtime_dir) = RuntimeEnvGuard::new();
    let swarm_id = "swarm-report-terminal";
    let coordinator = "coord-report-terminal";
    let failing = "worker-failing";
    let crashing = "worker-crashing";

    let swarm_members = Arc::new(RwLock::new(HashMap::from([
        (
            coordinator.to_string(),
            member(coordinator, swarm_id, "ready"),
        ),
        (
            failing.to_string(),
            owned_member(failing, swarm_id, "running", coordinator),
        ),
        (
            crashing.to_string(),
            owned_member(crashing, swarm_id, "running", coordinator),
        ),
    ])));
    let swarms_by_id = Arc::new(RwLock::new(HashMap::from([(
        swarm_id.to_string(),
        HashSet::from([
            coordinator.to_string(),
            failing.to_string(),
            crashing.to_string(),
        ]),
    )])));

    let mut bus_rx = crate::bus::Bus::global().subscribe();

    // A worker that failed while holding work, with a report.
    crate::server::swarm::update_member_status_with_report_tldr(
        failing,
        "failed",
        Some("boom".to_string()),
        Some("REPORT-BODY: failed halfway, left the branch dirty".to_string()),
        None,
        &swarm_members,
        &swarms_by_id,
        None,
        None,
        None,
    )
    .await;

    // A worker that died mid-task and produced no report at all: the owner
    // must still be told, otherwise worker deaths pass silently.
    crate::server::swarm::update_member_status_with_report_tldr(
        crashing,
        "crashed",
        None,
        None,
        None,
        &swarm_members,
        &swarms_by_id,
        None,
        None,
        None,
    )
    .await;

    let mut seen: HashMap<String, crate::bus::SwarmMemberReportReady> = HashMap::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while seen.len() < 2 {
            match bus_rx.recv().await {
                Ok(crate::bus::BusEvent::SwarmMemberReportReady(event))
                    if event.member_session_id == failing
                        || event.member_session_id == crashing =>
                {
                    seen.insert(event.member_session_id.clone(), event);
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("bus closed before both terminal reports arrived")
                }
            }
        }
    })
    .await
    .expect("failed and crashed members should both publish SwarmMemberReportReady");

    let failed_event = &seen[failing];
    assert_eq!(failed_event.recipient_session_id, coordinator);
    assert_eq!(failed_event.status, "failed");
    assert!(
        failed_event
            .notification
            .contains("REPORT-BODY: failed halfway"),
        "failure notification should carry the report body, got: {}",
        failed_event.notification
    );

    let crashed_event = &seen[crashing];
    assert_eq!(crashed_event.recipient_session_id, coordinator);
    assert_eq!(crashed_event.status, "crashed");
    assert!(
        crashed_event.notification.contains("No final textual report"),
        "a reportless crash must still be delivered, got: {}",
        crashed_event.notification
    );
}
