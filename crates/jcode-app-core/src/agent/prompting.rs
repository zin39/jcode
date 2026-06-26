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

        let pending = if crate::message::ends_with_fresh_user_turn(&messages) {
            crate::memory::take_pending_memory(session_id)
        } else {
            None
        };

        // Use the persistent memory-agent pipeline as the single source of truth.
        // Running both this and the legacy MemoryManager background retrieval path
        // can prepare overlapping pending prompts for the same turn, which makes
        // memory injection feel overly aggressive.
        crate::memory_agent::update_context_sync_with_dir(
            session_id,
            messages,
            self.session.working_dir.clone(),
        );

        pending
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

        self.append_current_turn_system_reminder(&mut split);
        self.append_auto_delegation_directive(&mut split);

        split
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
