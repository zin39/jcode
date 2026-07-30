//! Mirror cheap-route progress onto the main tool-progress bus.
//!
//! `SidePanelDebateReporter` already streams a rich view, but it renders into
//! the side panel, which is closed by default. A `cheap_route` call therefore
//! looked like a total freeze: no output at all until the run finished, which
//! for an 11-candidate fallback chain can be many minutes. Users cannot tell a
//! working run from a hung one, which is the single worst property a
//! long-running tool can have.
//!
//! The `batch` tool already solved this: it publishes `BatchProgress` on the
//! global bus and the TUI renders a live per-subcall list inline, where the
//! user is already looking. Reusing that channel means cheap-route runs become
//! visible without inventing a second progress mechanism or touching the
//! renderer.

use std::sync::Mutex;

use crate::bus::{BatchSubcallProgress, BatchSubcallState};
use jcode_message_types::ToolCall;

use crate::agent::debate_status::{DebatePhase, DebateStatusReporter};

/// One cheap-route subtask as the progress bus sees it.
struct SubtaskRow {
    description: String,
    state: BatchSubcallState,
    /// Model currently attempting this subtask, or the failure reason.
    detail: String,
}

/// Publishes cheap-route subtask state to the global bus as `BatchProgress`,
/// so the inline UI shows live per-subtask rows during a run.
pub struct BusProgressReporter {
    session_id: String,
    tool_call_id: String,
    rows: Mutex<Vec<SubtaskRow>>,
}

impl BusProgressReporter {
    pub fn new(session_id: String, tool_call_id: String) -> Self {
        Self {
            session_id,
            tool_call_id,
            rows: Mutex::new(Vec::new()),
        }
    }

    /// Render the current rows and publish them.
    ///
    /// Called on every state change. Publishing is cheap (a bus send) and the
    /// UI throttles its own redraws, so there is no need to rate-limit here.
    fn publish(&self) {
        let rows = self.rows.lock().unwrap_or_else(|p| p.into_inner());
        if rows.is_empty() {
            return;
        }

        let subcalls: Vec<BatchSubcallProgress> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| BatchSubcallProgress {
                index,
                tool_call: ToolCall {
                    id: format!("cheap-route-{index}"),
                    name: Self::row_label(row),
                    input: serde_json::Value::Null,
                    intent: Some(row.description.clone()),
                    thought_signature: None,
                },
                state: row.state,
            })
            .collect();

        let completed = rows
            .iter()
            .filter(|row| row.state != BatchSubcallState::Running)
            .count();
        let running: Vec<ToolCall> = subcalls
            .iter()
            .filter(|sub| sub.state == BatchSubcallState::Running)
            .map(|sub| sub.tool_call.clone())
            .collect();

        crate::bus::Bus::global().publish(crate::bus::BusEvent::BatchProgress(
            crate::bus::BatchProgress {
                session_id: self.session_id.clone(),
                tool_call_id: self.tool_call_id.clone(),
                total: rows.len(),
                completed,
                last_completed: None,
                running,
                subcalls,
            },
        ));
    }

    /// Label shown on a row: the model actually doing the work, because "which
    /// model is this running on" is the question users ask during a slow run.
    fn row_label(row: &SubtaskRow) -> String {
        if row.detail.is_empty() {
            "cheap_route".to_string()
        } else {
            row.detail.clone()
        }
    }
}

impl DebateStatusReporter for BusProgressReporter {
    fn proposer(&self, _model: &str, _phase: DebatePhase) {}

    fn phase(&self, _label: &str) {}

    fn gold(&self, _markdown: &str) {}

    fn plan(&self, subtasks: &[(String, u8)]) {
        {
            let mut rows = self.rows.lock().unwrap_or_else(|p| p.into_inner());
            *rows = subtasks
                .iter()
                .map(|(description, _difficulty)| SubtaskRow {
                    description: description.clone(),
                    state: BatchSubcallState::Running,
                    detail: String::new(),
                })
                .collect();
        }
        self.publish();
    }

    fn subtask(&self, index: usize, phase: DebatePhase, detail: &str) {
        {
            let mut rows = self.rows.lock().unwrap_or_else(|p| p.into_inner());
            let Some(row) = rows.get_mut(index) else {
                return;
            };
            row.state = match phase {
                DebatePhase::Running => BatchSubcallState::Running,
                DebatePhase::Done => BatchSubcallState::Succeeded,
                DebatePhase::Failed => BatchSubcallState::Failed,
            };
            row.detail = detail.to_string();
        }
        self.publish();
    }
}

/// Fan out to several reporters so a run can drive the side panel and the
/// inline progress bus at once.
///
/// Without this, adding inline progress would mean *replacing* the side-panel
/// view and losing the live output tail, which is the more detailed surface.
pub struct MultiReporter {
    reporters: Vec<std::sync::Arc<dyn DebateStatusReporter>>,
}

impl MultiReporter {
    pub fn new(reporters: Vec<std::sync::Arc<dyn DebateStatusReporter>>) -> Self {
        Self { reporters }
    }
}

impl DebateStatusReporter for MultiReporter {
    fn proposer(&self, model: &str, phase: DebatePhase) {
        for reporter in &self.reporters {
            reporter.proposer(model, phase);
        }
    }

    fn phase(&self, label: &str) {
        for reporter in &self.reporters {
            reporter.phase(label);
        }
    }

    fn gold(&self, markdown: &str) {
        for reporter in &self.reporters {
            reporter.gold(markdown);
        }
    }

    fn plan(&self, subtasks: &[(String, u8)]) {
        for reporter in &self.reporters {
            reporter.plan(subtasks);
        }
    }

    fn subtask(&self, index: usize, phase: DebatePhase, detail: &str) {
        for reporter in &self.reporters {
            reporter.subtask(index, phase, detail);
        }
    }

    fn subtask_live(&self, index: usize, tail: &str) {
        for reporter in &self.reporters {
            reporter.subtask_live(index, tail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_then_subtask_updates_track_state_per_row() {
        let reporter = BusProgressReporter::new("session_x".to_string(), "call_1".to_string());
        reporter.plan(&[("first".to_string(), 1), ("second".to_string(), 3)]);

        {
            let rows = reporter.rows.lock().unwrap();
            assert_eq!(rows.len(), 2, "both subtasks must be tracked");
            assert!(
                rows.iter()
                    .all(|row| row.state == BatchSubcallState::Running),
                "a freshly announced plan starts every row running"
            );
        }

        reporter.subtask(0, DebatePhase::Done, "deepseek-v4-pro");
        reporter.subtask(1, DebatePhase::Failed, "all candidates failed");

        let rows = reporter.rows.lock().unwrap();
        assert_eq!(rows[0].state, BatchSubcallState::Succeeded);
        assert_eq!(
            rows[0].detail, "deepseek-v4-pro",
            "the row must name the model that ran it, so a slow run is diagnosable"
        );
        assert_eq!(rows[1].state, BatchSubcallState::Failed);
    }

    /// The reporter must actually reach the BUS, not just mutate its own rows.
    ///
    /// Both existing tests inspect `reporter.rows` directly, so they would pass
    /// even if `publish` were never called and nothing ever appeared in the UI.
    /// The whole point of this type is that a cheap_route run is visible live
    /// (the user asked whether the side panel shows progress in real time), and
    /// that claim was resting on reading the code rather than on a test.
    #[tokio::test]
    async fn progress_reaches_the_bus_so_the_ui_can_render_it_live() {
        let mut rx = crate::bus::Bus::global().subscribe();
        while rx.try_recv().is_ok() {}

        let reporter = BusProgressReporter::new("session_bus".to_string(), "call_bus".to_string());
        reporter.plan(&[("alpha".to_string(), 1), ("beta".to_string(), 2)]);

        let progress = loop {
            match rx.try_recv() {
                Ok(crate::bus::BusEvent::BatchProgress(progress))
                    if progress.tool_call_id == "call_bus" =>
                {
                    break progress;
                }
                Ok(_) => continue,
                Err(err) => panic!("plan() published no BatchProgress on the bus: {err:?}"),
            }
        };

        assert_eq!(progress.session_id, "session_bus");
        assert_eq!(progress.total, 2, "both subtasks must be reported");
        assert_eq!(
            progress.completed, 0,
            "nothing has finished yet when the plan is announced"
        );
        assert_eq!(
            progress.running.len(),
            2,
            "both subtasks start running, which is what the UI renders as active rows"
        );
        assert!(
            progress
                .subcalls
                .iter()
                .any(|sub| sub.tool_call.intent.as_deref() == Some("alpha")),
            "each row must carry its subtask description so the panel is readable: {:?}",
            progress.subcalls
        );

        // A completion must be published too, otherwise the panel would show
        // work as perpetually running.
        reporter.subtask(0, DebatePhase::Done, "deepseek-v4-pro");
        let done = loop {
            match rx.try_recv() {
                Ok(crate::bus::BusEvent::BatchProgress(progress))
                    if progress.tool_call_id == "call_bus" =>
                {
                    break progress;
                }
                Ok(_) => continue,
                Err(err) => panic!("subtask() published no BatchProgress: {err:?}"),
            }
        };
        assert_eq!(done.completed, 1, "the finished subtask must be counted");
        assert_eq!(done.running.len(), 1, "one subtask is still running");
    }

    #[test]
    fn subtask_update_for_unknown_index_is_ignored() {
        let reporter = BusProgressReporter::new("session_x".to_string(), "call_1".to_string());
        reporter.plan(&[("only".to_string(), 1)]);

        // A reporter must never panic on an out-of-range index: it would take
        // down a real run for a cosmetic update.
        reporter.subtask(7, DebatePhase::Done, "ghost");

        let rows = reporter.rows.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, BatchSubcallState::Running);
    }
}
