use super::Agent;
use crate::logging;
use crate::message::{Message, ToolDefinition};

/// Injected into a coordinator's system prompt when `agents.auto_delegate` is on.
/// Pushes execution onto cheap subagents so the expensive coordinator model is
/// spent on planning + review rather than grunt work.
///
/// The safe/unsafe split is not arbitrary. Cheap models track frontier models
/// closely on retrieval and single-file edits, but fall off a cliff on
/// multi-file refactors and long-horizon debugging (the SWE-bench
/// Verified -> Pro gap), and they are materially worse at tool-call
/// round-tripping. Several cheap families also default to premature "task
/// complete" claims, so delegated work needs an explicit verification step
/// rather than a trusted self-report. See
/// `cheap-models-coding-agents-research.md`.
pub(crate) const AUTO_DELEGATION_DIRECTIVE: &str = "\
# Delegation policy (cost control)

You have two ways to offload work, and you should reach for one of them on \
EVERY task rather than doing the work yourself:

1. `cheap_route` — hand it a whole multi-step task. It decomposes the task, \
   rates each subtask's difficulty, and runs each one on the cheapest model \
   strong enough for it. Prefer this for anything with more than one step.
2. `swarm` with `action: spawn` — spawn a worker for a single unit of work, \
   or several in one turn when the units are independent. Always pass a \
   `label` and a `prompt`.

DELEGATE all hands-on execution and reserve yourself for planning and review:

- Delegate every unit of real work — running shell commands, \
  editing/writing files, searching and reading code, investigating behavior, \
  reproducing bugs, and any repetitive or bulk task.
- Do NOT run bash, file edits, grep/search, or file reads yourself when a \
  spawned worker can do it. Each time you do cheap work directly you waste the \
  expensive model.
- For independent subtasks, spawn multiple workers in the SAME turn — they run \
  concurrently, which is faster.
- Keep yourself for: understanding the request, decomposing it into delegable \
  subtasks, choosing what to delegate, and reviewing/integrating subagent \
  results before the next step.

Delegation is for gathering and grunt work, not for judgment. Keep these on \
yourself, because cheap models measurably collapse on them:

- Multi-file refactors (roughly 4+ files), architecture and design decisions, \
  root-cause debugging, and security-sensitive changes. Delegate the reading \
  and reproduction around them, then make the call and write the fix yourself.

Never trust a subagent's word that it finished. Cheap models frequently report \
success on unfinished work, so:

- Ask workers to return evidence (diffs, command output, test results), not a \
  claim of completion.
- Confirm anything load-bearing yourself — check the diff, or have a worker \
  run the tests and show you the output — before you build on it or report done.";

/// Injected into a coordinator's system prompt when gold mode is on for the
/// session (`session.gold_mode_enabled` AND `agents.cheap_route_gold_mode`).
/// Makes the coordinator auto-route substantive reasoning work through the
/// `cheap_route` tool, which runs the multi-model debate and folds the
/// proposals into one "gold" answer — no explicit "use cheap_route" needed.
const GOLD_MODE_DIRECTIVE: &str = "\
# Gold mode (multi-model debate) is ON

For any substantive reasoning task — design, architecture, analysis, research, \
comparison, planning, debugging strategy, or any open-ended question with a \
single best answer — offload it to the `cheap_route` tool instead of answering \
it yourself. cheap_route runs several models in parallel as proposers and folds \
their answers into one high-quality \"gold\" result.

- Pass the user's full task text as the `task` argument.
- Do this automatically; the user does NOT need to say \"use cheap_route\". Gold \
  mode being on IS the instruction to route through it.
- You keep light coordination and presenting the gold result back to the user.
- Skip cheap_route ONLY for trivial chat, simple factual replies, or pure \
  mechanical edits where a debate adds no value.";

impl Agent {
    pub(super) fn log_prompt_prefix_accounting(
        &self,
        split: &crate::prompt::SplitSystemPrompt,
        tools: &[ToolDefinition],
    ) {
        let system_tokens = split.estimated_tokens();
        let tool_tokens = ToolDefinition::aggregate_prompt_token_estimate(tools);
        let prefix_tokens = system_tokens + tool_tokens;
        logging::info(&format!(
            "Prompt prefix estimate: total={} tokens (system={} tools={})",
            prefix_tokens, system_tokens, tool_tokens
        ));
    }

    pub(super) fn build_memory_prompt_nonblocking_shared(
        &self,
        messages: std::sync::Arc<[Message]>,
        _memory_event_tx: Option<crate::memory::MemoryEventSink>,
    ) -> Option<crate::memory::PendingMemory> {
        if !self.memory_enabled {
            return None;
        }

        let session_id = &self.session.id;

        let fresh_user_turn = crate::message::ends_with_fresh_user_turn(&messages);
        let pending = if fresh_user_turn {
            crate::memory::take_pending_memory(session_id)
        } else {
            None
        };

        // Use the persistent memory-agent pipeline as the single source of truth.
        // Running both this and the legacy MemoryManager background retrieval path
        // can prepare overlapping pending prompts for the same turn, which makes
        // memory injection feel overly aggressive.
        // Relevance results are consumed only at the start of a fresh user turn.
        // Enqueuing again after every tool result runs the local embedding model
        // for each provider continuation without creating an additional injection
        // opportunity. One update per user turn keeps memory current while avoiding
        // redundant 512-token inference during tool-heavy agent loops.
        if fresh_user_turn {
            crate::memory_agent::update_context_sync_with_dir(
                session_id,
                messages,
                self.session.working_dir.clone(),
            );
        }

        pending
    }

    fn append_task_state(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        // Seed from the first user message if no task state exists yet.
        // This implements the "recitation" pattern: the original goal is
        // captured to disk so it survives compaction even when the agent
        // never explicitly calls update_task_state.
        self.seed_task_state_from_first_message();

        let Some(state) = jcode_base::session::task_state::read_task_state(&self.session.id) else {
            return;
        };

        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str(
            "# Task State\n\nYour saved working state (maintained via the `update_task_state` tool; survives compaction). Keep it current:\n\n",
        );
        split.dynamic_part.push_str(&state);
    }

    /// Extract the first user message text from the session and seed the task
    /// state file if it is empty. No-op when state already exists or no user
    /// message is found.
    fn seed_task_state_from_first_message(&self) {
        let first_user_text = self
            .session
            .messages
            .iter()
            .filter(|m| m.role == crate::message::Role::User)
            .flat_map(|m| {
                m.content.iter().filter_map(|block| {
                    if let crate::message::ContentBlock::Text { text, .. } = block {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
            })
            // Session-context reminders and tool results are injected as User
            // messages; the real request is the first text that isn't one.
            .find(|text| {
                let t = text.trim_start();
                !t.starts_with("<system-reminder") && !t.starts_with("[Recovered orphaned")
            });
        if let Some(text) = first_user_text {
            // Strip a leading inline system-reminder block when the real
            // request shares one text block with it.
            let cleaned = match (
                text.find("</system-reminder>"),
                text.contains("<system-reminder"),
            ) {
                (Some(end), true) => text[end + "</system-reminder>".len()..].trim(),
                _ => text.trim(),
            };
            jcode_base::session::task_state::seed_task_state_if_empty(&self.session.id, cleaned);
        }
    }

    fn append_current_turn_system_reminder(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        let Some(reminder) = self
            .current_turn_system_reminder
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        else {
            return;
        };

        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str("# System Reminder\n\n");
        split.dynamic_part.push_str(reminder);
    }

    /// Build split system prompt for better caching
    /// Returns static (cacheable) and dynamic (not cached) parts separately
    pub(super) fn build_system_prompt_split(
        &self,
        memory_prompt: Option<&str>,
    ) -> crate::prompt::SplitSystemPrompt {
        if let Some(ref override_prompt) = self.system_prompt_override {
            return crate::prompt::SplitSystemPrompt {
                static_part: override_prompt.clone(),
                dynamic_part: String::new(),
            };
        }

        let skills = self.current_skills_snapshot();
        let skill_prompt = self
            .active_skill
            .as_ref()
            .and_then(|name| skills.get(name).map(|skill| skill.get_prompt().to_string()));

        let available_skills: Vec<crate::prompt::SkillInfo> = self
            .current_skills_snapshot()
            .list()
            .iter()
            .map(|skill| crate::prompt::SkillInfo {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect();

        let working_dir = self
            .session
            .working_dir
            .as_ref()
            .map(std::path::PathBuf::from);

        let (mut split, _context_info) = crate::prompt::build_system_prompt_split_with_agents_md(
            skill_prompt.as_deref(),
            &available_skills,
            self.session.is_canary,
            memory_prompt,
            working_dir.as_deref(),
            self.agents_md_snapshot.clone(),
        );

        self.append_task_state(&mut split);
        self.append_current_turn_system_reminder(&mut split);
        self.append_auto_delegation_directive(&mut split);
        self.append_gold_mode_directive(&mut split);
        crate::prompt::append_swarm_effort_directive(
            &mut split,
            self.provider.reasoning_effort().as_deref(),
        );
        crate::prompt::append_web_grounding_directive(
            &mut split,
            crate::config::config().features.web_grounding,
        );

        split
    }

    /// When gold mode is on for this session and this agent can invoke
    /// `cheap_route` (i.e. it is a coordinator, not a spawned subagent — those
    /// have the tool removed, which also blocks recursive debates), instruct it
    /// to auto-route substantive reasoning work through cheap_route so the user
    /// gets gold debates without saying "use cheap_route" each time.
    /// Test hook: run only the auto-delegation append and report whether the
    /// directive was emitted. Keeps the guard testable without constructing a
    /// full system prompt (which needs skills, memory and a working dir).
    #[cfg(test)]
    pub(crate) fn delegation_directive_emitted_for_test(&self) -> bool {
        self.delegation_block_for_test()
            .contains("Delegation policy")
    }

    /// Test hook: the full dynamic block this session would receive from the
    /// auto-delegation append, including the user's swarm prompt.
    #[cfg(test)]
    pub(crate) fn delegation_block_for_test(&self) -> String {
        let mut split = crate::prompt::SplitSystemPrompt::default();
        self.append_auto_delegation_directive(&mut split);
        split.dynamic_part
    }

    fn append_gold_mode_directive(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        let gold_on = self.session.gold_mode_enabled.unwrap_or(false)
            && crate::config::config().agents.cheap_route_gold_mode;
        if !gold_on {
            return;
        }
        if self.validate_tool_allowed("cheap_route").is_err() {
            return;
        }
        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str(GOLD_MODE_DIRECTIVE);
    }

    /// When `agents.auto_delegate` is on and this agent can spawn subagents (i.e.
    /// it is a coordinator, not a spawned subagent), instruct it to offload all
    /// hands-on execution to cheap subagents and keep itself for planning/review.
    fn append_auto_delegation_directive(&self, split: &mut crate::prompt::SplitSystemPrompt) {
        if !crate::config::config().agents.auto_delegate {
            return;
        }
        // This gate must name the tool the directive actually tells the model to
        // call. It used to check `subagent`, which no longer exists in the
        // registry: the check therefore always passed (it only consults
        // allow/deny lists, not registration) and every coordinator was told to
        // call a deleted tool, producing "Unknown tool: subagent" at runtime.
        if self.validate_tool_allowed("swarm").is_err() {
            return;
        }

        // Tool availability is NOT the same as spawn capability. A spawned
        // worker keeps `swarm` because it needs `report` to hand results back,
        // so the check above passes for workers too and they received the full
        // "delegate everything" directive. Recursive spawning is disabled for
        // light and ad hoc swarms, so those workers then tried to spawn and got
        // "Recursive swarm spawning is disabled" back. Measured across 800
        // sessions: 31 failed spawn calls, and 17 of the 19 affected sessions
        // had agent_role = swarm_worker.
        //
        // Any session with an agent_role is itself delegated work. Telling it to
        // delegate again is either rejected outright or, in deep mode, an
        // invitation to fan out where the coordinator wanted focused execution.
        if self.session.agent_role.is_some() {
            return;
        }
        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str(AUTO_DELEGATION_DIRECTIVE);

        // The user's swarm-prompt.md is model-routing guidance that only a
        // session which can spawn will ever act on. It used to be appended to
        // the `swarm` tool description, so it shipped on every request in every
        // session, including the 62% that never spawn an agent. Measured on
        // this machine it is 1,052 tokens per request.
        //
        // Emitting it here instead keeps it byte-identical for coordinators
        // while removing it from workers and from sessions that never delegate.
        let swarm_prompt = crate::prompt::load_swarm_prompt(
            self.session
                .working_dir
                .as_deref()
                .map(std::path::Path::new),
        );
        if !swarm_prompt.is_empty() {
            split
                .dynamic_part
                .push_str("\n\nSwarm prompt (user-tunable via ~/.jcode/swarm-prompt.md):\n");
            split.dynamic_part.push_str(&swarm_prompt);
        }
    }

    /// Non-blocking memory prompt - takes pending result and spawns check for next turn
    #[cfg(test)]
    pub(super) fn build_memory_prompt_nonblocking(
        &self,
        messages: &[Message],
        _memory_event_tx: Option<crate::memory::MemoryEventSink>,
    ) -> Option<crate::memory::PendingMemory> {
        self.build_memory_prompt_nonblocking_shared(messages.to_vec().into(), _memory_event_tx)
    }
}

#[cfg(test)]
mod delegation_directive_tests {
    use super::AUTO_DELEGATION_DIRECTIVE;

    /// The directive is the entire mechanism behind "delegate automatically":
    /// it is what tells a coordinator to offload work instead of doing it. It
    /// previously named only `subagent`, so a coordinator never learned that
    /// `cheap_route` exists — and cheap_route is the better tool for a
    /// multi-step request, because it rates each subtask's difficulty and runs
    /// each on the cheapest model strong enough for it.
    #[test]
    fn directive_offers_both_delegation_tools() {
        assert!(
            AUTO_DELEGATION_DIRECTIVE.contains("cheap_route"),
            "coordinator must be told cheap_route exists, or it will never use it"
        );
        assert!(
            AUTO_DELEGATION_DIRECTIVE.contains("swarm"),
            "single-unit delegation must still be offered"
        );
        // The directive must state the routing behaviour that makes cheap_route
        // the right default, not just name the tool.
        assert!(
            AUTO_DELEGATION_DIRECTIVE.contains("difficulty"),
            "directive should explain that cheap_route routes by subtask difficulty"
        );
    }

    /// Cheap models measurably collapse on long-horizon, multi-file work, so
    /// the directive must keep naming what NOT to delegate. A live run of this
    /// config had a cheap worker search the wrong repository and then report a
    /// function "does not exist" rather than reading the file it was given.
    #[test]
    fn directive_still_reserves_judgment_work_and_demands_evidence() {
        for expected in ["Multi-file refactors", "root-cause debugging", "evidence"] {
            assert!(
                AUTO_DELEGATION_DIRECTIVE.contains(expected),
                "directive must retain the {expected:?} guardrail"
            );
        }
    }
}
