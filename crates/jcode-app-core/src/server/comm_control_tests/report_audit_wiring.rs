/// The live report path must actually stitch the tally to the audit.
///
/// Guards the integration, not just the pure predicate: the pure check and the
/// tally were both individually tested and green while a live worker's
/// fabricated report still came back unannotated, because nothing exercised
/// `build_audited_completion_report` itself.
#[tokio::test]
async fn the_live_report_builder_appends_the_audit_note_for_a_silent_worker() {
    let session_id = format!("report-audit-wiring-{}", std::process::id());
    jcode_swarm_core::forget_session_tool_activity(&session_id);

    // A worker that called no tools, claiming it ran the test suite.
    let report = super::build_audited_completion_report(
        &session_id,
        "Checked the build.",
        Some("Ran cargo test -p jcode-base and all 1213 tests passed."),
        None,
    );
    assert!(
        report.contains("Checked the build."),
        "the worker's own message must survive: {report}"
    );
    assert!(
        report.contains("Unverified"),
        "a fabricated validation claim must be annotated: {report}"
    );

    // Same claim, but the worker actually ran a command: no annotation.
    jcode_swarm_core::record_session_tool_call(&session_id, "bash");
    let honest = super::build_audited_completion_report(
        &session_id,
        "Checked the build.",
        Some("Ran cargo test -p jcode-base and all 1213 tests passed."),
        None,
    );
    assert!(
        !honest.contains("Unverified"),
        "a backed claim must not be annotated: {honest}"
    );

    jcode_swarm_core::forget_session_tool_activity(&session_id);
}
