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
    /// A cheap-route run announced its plan: subtask descriptions in order.
    /// Default no-op keeps existing reporters/tests source-compatible.
    fn plan(&self, _subtasks: &[(String, u8)]) {}
    /// A cheap-route subtask changed state. `detail` carries the model that is
    /// running it (or the error). Default no-op.
    fn subtask(&self, _index: usize, _phase: DebatePhase, _detail: &str) {}
}

/// Reporter that ignores everything. Used in non-TUI contexts and tests.
pub struct NoopDebateReporter;
impl DebateStatusReporter for NoopDebateReporter {
    fn proposer(&self, _model: &str, _phase: DebatePhase) {}
    fn phase(&self, _label: &str) {}
    fn gold(&self, _markdown: &str) {}
}

// ---------------------------------------------------------------------------
// SidePanelDebateReporter — live updates to the session side panel.
// ---------------------------------------------------------------------------

struct DebateState {
    phase_label: String,
    proposers: Vec<(String, DebatePhase)>,
    gold: Option<String>,
    /// Cheap-route plan: (description, difficulty, phase, detail) per subtask.
    subtasks: Vec<(String, u8, DebatePhase, String)>,
}

impl DebateState {
    fn render(&self) -> String {
        let mut buf = String::from("# Cheap Route\n\n");
        if !self.phase_label.is_empty() {
            buf.push_str(&format!("**Phase:** {}\n\n", self.phase_label));
        }
        if !self.subtasks.is_empty() {
            buf.push_str("## Subtasks\n\n");
            for (desc, difficulty, phase, detail) in &self.subtasks {
                let icon = match phase {
                    DebatePhase::Running => "⏳",
                    DebatePhase::Done => "✅",
                    DebatePhase::Failed => "❌",
                };
                if detail.is_empty() {
                    buf.push_str(&format!("- {} (d{}) {}\n", icon, difficulty, desc));
                } else {
                    buf.push_str(&format!("- {} (d{}) {} — `{}`\n", icon, difficulty, desc, detail));
                }
            }
            buf.push('\n');
        }
        if !self.proposers.is_empty() {
            buf.push_str("## Proposers\n\n");
            for (model, phase) in &self.proposers {
                let icon = match phase {
                    DebatePhase::Running => "⏳",
                    DebatePhase::Done => "✅",
                    DebatePhase::Failed => "❌",
                };
                buf.push_str(&format!("- {} {}\n", icon, model));
            }
            buf.push('\n');
        }
        if let Some(gold) = &self.gold {
            buf.push_str("## Gold Result\n\n");
            buf.push_str(gold);
            if !gold.ends_with('\n') {
                buf.push('\n');
            }
        }
        buf
    }
}

/// Reporter that writes debate progress live to the session's side panel page
/// `"debate"` and publishes a `BusEvent::SidePanelUpdated` so the client
/// receives the update immediately (the same path used by `/goals`).
pub struct SidePanelDebateReporter {
    session_id: String,
    state: std::sync::Mutex<DebateState>,
}

impl SidePanelDebateReporter {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            state: std::sync::Mutex::new(DebateState {
                phase_label: String::new(),
                proposers: Vec::new(),
                gold: None,
                subtasks: Vec::new(),
            }),
        }
    }

    fn flush(&self, state: &DebateState) {
        let content = state.render();
        match crate::side_panel::write_markdown_page(
            &self.session_id,
            "debate",
            Some("Gold Debate"),
            &content,
            true,
        ) {
            Ok(snapshot) => {
                crate::bus::Bus::global().publish(crate::bus::BusEvent::SidePanelUpdated(
                    crate::bus::SidePanelUpdated {
                        session_id: self.session_id.clone(),
                        snapshot,
                    },
                ));
            }
            Err(err) => {
                crate::logging::warn(&format!(
                    "[debate_reporter] failed to write side panel page: {err}"
                ));
            }
        }
    }
}

impl DebateStatusReporter for SidePanelDebateReporter {
    fn proposer(&self, model: &str, phase: DebatePhase) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = state.proposers.iter_mut().find(|(m, _)| m == model) {
            entry.1 = phase;
        } else {
            state.proposers.push((model.to_string(), phase));
        }
        self.flush(&state);
    }

    fn phase(&self, label: &str) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.phase_label = label.to_string();
        self.flush(&state);
    }

    fn gold(&self, markdown: &str) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.gold = Some(markdown.to_string());
        self.flush(&state);
    }

    fn plan(&self, subtasks: &[(String, u8)]) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.subtasks = subtasks
            .iter()
            .map(|(desc, difficulty)| {
                (desc.clone(), *difficulty, DebatePhase::Running, String::new())
            })
            .collect();
        state.phase_label = "running subtasks".to_string();
        self.flush(&state);
    }

    fn subtask(&self, index: usize, phase: DebatePhase, detail: &str) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = state.subtasks.get_mut(index) {
            entry.2 = phase;
            entry.3 = detail.to_string();
        }
        self.flush(&state);
    }
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
