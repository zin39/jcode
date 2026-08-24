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
