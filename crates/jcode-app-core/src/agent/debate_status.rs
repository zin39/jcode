//! Optional live-status callback for a gold-mode debate (proposer tiles, phase,
//! gold result). A no-op default keeps the debate engine UI-agnostic + testable.

/// Phase of one proposer in a debate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebatePhase {
    Running,
    Done,
    Failed,
}

/// Sink for live debate status. Implementations route to the TUI (swarm gallery
/// tiles + side panel); the no-op impl ignores everything.
pub trait DebateStatusReporter: Send + Sync {
    /// A proposer changed phase.
    fn proposer(&self, model: &str, phase: DebatePhase);
    /// The debate moved to a named phase: "propose" | "critique" | "merge".
    fn phase(&self, label: &str);
    /// The final gold result (markdown) is ready.
    fn gold(&self, markdown: &str);
}

/// Reporter that ignores everything. Used in non-TUI contexts and tests.
pub struct NoopDebateReporter;
impl DebateStatusReporter for NoopDebateReporter {
    fn proposer(&self, _model: &str, _phase: DebatePhase) {}
    fn phase(&self, _label: &str) {}
    fn gold(&self, _markdown: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn noop_reporter_is_safe() {
        let r = NoopDebateReporter;
        r.proposer("deepseek", DebatePhase::Running);
        r.phase("propose");
        r.gold("# gold");
    }
}
