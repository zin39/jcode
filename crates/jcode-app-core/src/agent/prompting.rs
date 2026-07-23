use super::Agent;
use crate::logging;
use crate::message::{Message, ToolDefinition};

/// Injected into a coordinator's system prompt when `agents.auto_delegate` is on.
/// Pushes execution onto cheap subagents so the expensive coordinator model is
/// spent on planning + review rather than grunt work.
const AUTO_DELEGATION_DIRECTIVE: &str = "\
# Delegation policy (cost control)

You have cheap subagents available via the `subagent` tool. DELEGATE all hands-on \
execution to them and reserve yourself for planning and review:

- Spawn a subagent for every unit of real work — running shell commands, \
  editing/writing files, searching and reading code, investigating behavior, \
  reproducing bugs, and any repetitive or bulk task.
- Do NOT run bash, file edits, grep/search, or file reads yourself when a \
  subagent can do it. Each time you do cheap work directly you waste the \
  expensive model.
- For independent subtasks, spawn multiple subagents in the SAME turn — they run \
  concurrently, which is faster.
- Keep yourself for: understanding the request, decomposing it into delegable \
  subtasks, choosing what to delegate, and reviewing/integrating subagent \
  results before the next step.";

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
            let cleaned = match (text.find("</system-reminder>"), text.contains("<system-reminder")) {
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

        let (mut split, _context_info) = crate::prompt::build_system_prompt_split(
            skill_prompt.as_deref(),
            &available_skills,
            self.session.is_canary,
            memory_prompt,
            working_dir.as_deref(),
        );

        self.append_task_state(&mut split);
        self.append_current_turn_system_reminder(&mut split);
        self.append_auto_delegation_directive(&mut split);
        self.append_gold_mode_directive(&mut split);
        crate::prompt::append_swarm_effort_directive(
            &mut split,
            self.provider.reasoning_effort().as_deref(),
        );

        split
    }


    /// When gold mode is on for this session and this agent can invoke
    /// `cheap_route` (i.e. it is a coordinator, not a spawned subagent — those
    /// have the tool removed, which also blocks recursive debates), instruct it
    /// to auto-route substantive reasoning work through cheap_route so the user
    /// gets gold debates without saying "use cheap_route" each time.
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
        // Only coordinators get this. A spawned subagent has the `subagent` tool
        // removed, so it must not be told to delegate work it cannot delegate.
        if self.validate_tool_allowed("subagent").is_err() {
            return;
        }
        if !split.dynamic_part.is_empty() {
            split.dynamic_part.push_str("\n\n");
        }
        split.dynamic_part.push_str(AUTO_DELEGATION_DIRECTIVE);
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
