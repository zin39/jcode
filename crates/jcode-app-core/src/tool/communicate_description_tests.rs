// Tests for the swarm tool description contract.
//
// Split out of communicate_tests.rs, which is at the oversized-test budget.

/// The description carries the tool's own contract, but NOT the user's
/// swarm-prompt.md.
///
/// That guidance moved to the system prompt's dynamic part, which is already
/// gated to sessions that can spawn, because embedding it here billed it on
/// every request in every session (1,052 tokens as measured on a real machine)
/// including the majority that never delegate.
#[test]
fn description_carries_the_contract_without_the_user_swarm_prompt() {
    let tool = CommunicateTool::new();
    let description = tool.description();
    assert!(
        !description.contains("Swarm prompt (user-tunable"),
        "user routing guidance must not ride along in the tool schema"
    );
    assert!(
        description.contains("only the root session may spawn agents"),
        "description should advertise the enforced light/ad hoc spawn boundary"
    );
    assert!(
        description.contains(
            "Recursive spawning is enabled only when the root session is running in swarm-deep mode"
        ),
        "description should reserve recursive spawning for deep-swarm roots"
    );
}
