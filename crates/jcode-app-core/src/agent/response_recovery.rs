use super::*;

impl Agent {
    fn parse_text_wrapped_tool_call(
        text: &str,
    ) -> Option<(String, String, serde_json::Value, String)> {
        let marker = "to=functions.";
        let marker_idx = text.find(marker)?;
        let after_marker = &text[marker_idx + marker.len()..];

        let mut tool_name_end = 0usize;
        for (idx, ch) in after_marker.char_indices() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                tool_name_end = idx + ch.len_utf8();
            } else {
                break;
            }
        }
        if tool_name_end == 0 {
            return None;
        }

        let tool_name = after_marker[..tool_name_end].to_string();
        let remaining = &after_marker[tool_name_end..];
        let mut fallback: Option<(String, String, serde_json::Value, String)> = None;

        for (brace_idx, ch) in remaining.char_indices() {
            if ch != '{' {
                continue;
            }
            let slice = &remaining[brace_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            let parsed = match stream.next() {
                Some(Ok(value)) => value,
                Some(Err(_)) | None => continue,
            };
            let consumed = stream.byte_offset();
            if !parsed.is_object() {
                continue;
            }

            let prefix = text[..marker_idx].trim_end().to_string();
            let suffix = remaining[brace_idx + consumed..].trim().to_string();
            if suffix.is_empty() {
                return Some((prefix, tool_name.clone(), parsed, suffix));
            }
            if fallback.is_none() {
                fallback = Some((prefix, tool_name.clone(), parsed, suffix));
            }
        }

        fallback
    }

    pub(super) fn recover_text_wrapped_tool_call(
        &self,
        text_content: &mut String,
        tool_calls: &mut Vec<ToolCall>,
    ) -> bool {
        if !tool_calls.is_empty() || text_content.trim().is_empty() {
            return false;
        }

        let Some((prefix, tool_name, arguments, suffix)) =
            Self::parse_text_wrapped_tool_call(text_content)
        else {
            return false;
        };

        let mut sanitized = String::new();
        if !prefix.is_empty() {
            sanitized.push_str(&prefix);
        }
        if !suffix.is_empty() {
            if !sanitized.is_empty() {
                sanitized.push('\n');
            }
            sanitized.push_str(&suffix);
        }
        *text_content = sanitized;

        let call_id = format!("fallback_text_call_{}", id::new_id("call"));
        let recovered_total = RECOVERED_TEXT_WRAPPED_TOOL_CALLS
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        logging::warn(&format!(
            "[agent] Recovered text-wrapped tool call for '{}' ({}, total={})",
            tool_name, call_id, recovered_total
        ));
        let intent = ToolCall::intent_from_input(&arguments);
        tool_calls.push(ToolCall {
            id: call_id,
            name: tool_name,
            input: arguments,
            intent,
            thought_signature: None,
        });

        true
    }

    pub(crate) fn should_continue_after_stop_reason(stop_reason: &str) -> bool {
        let reason = stop_reason.trim().to_ascii_lowercase();
        if reason.is_empty() {
            return false;
        }

        if matches!(reason.as_str(), "stop" | "end_turn" | "tool_use") {
            return false;
        }

        reason.contains("incomplete")
            || reason.contains("max_output_tokens")
            || reason.contains("max_tokens")
            || reason.contains("length")
            || reason.contains("trunc")
            || reason.contains("commentary")
    }

    /// True when the provider's stop reason indicates a model-side
    /// guardrail/safety stop (e.g. Anthropic `refusal`), as opposed to a
    /// normal end-of-turn or truncation.
    ///
    /// Anthropic refusals arrive as `refusal:<category>` when the API names the
    /// policy category, so the reason is matched on the part before the colon.
    pub(crate) fn is_guardrail_stop_reason(stop_reason: Option<&str>) -> bool {
        let Some(reason) = stop_reason else {
            return false;
        };
        let reason = reason.trim().to_ascii_lowercase();
        let base = reason.split(':').next().unwrap_or(&reason);
        matches!(base, "refusal" | "content_filter" | "safety")
            || reason.contains("guardrail")
            || reason.contains("policy_violation")
    }

    /// Split an Anthropic-style `refusal:<category>` stop reason into its base
    /// reason and the policy category, when one was reported.
    pub(crate) fn split_guardrail_category(stop_reason: &str) -> (&str, Option<&str>) {
        match stop_reason.trim().split_once(':') {
            Some((base, category)) if !category.trim().is_empty() => {
                (base.trim(), Some(category.trim()))
            }
            _ => (stop_reason.trim(), None),
        }
    }

    /// Plain-language guidance for a documented Anthropic refusal category.
    ///
    /// The classifier scores the whole request, tool definitions and injected
    /// project files included, not just what the user typed. That is why a
    /// refusal can land on a message as innocuous as "hi", and why the advice
    /// here points at the surrounding context rather than at the user's words.
    fn guardrail_category_hint(category: &str) -> Option<&'static str> {
        match category.trim().to_ascii_lowercase().as_str() {
            "cyber" => Some(
                "Category `cyber`: security-adjacent wording somewhere in the request tripped a safeguard. Note that the classifier reads the whole request, including tool definitions and any project files loaded into context, so this can fire on benign defensive work.",
            ),
            "bio" => Some(
                "Category `bio`: the request looked life-sciences sensitive. Benign biology work can also trigger this.",
            ),
            "frontier_llm" => Some(
                "Category `frontier_llm`: something in the request looked like work on competing AI models. Comparisons of rival models or benchmark tables in your prompt files or tool descriptions are a common cause, since they are sent on every request.",
            ),
            "reasoning_extraction" => Some(
                "Category `reasoning_extraction`: the request asked the model to reproduce its internal reasoning as text. Use structured thinking output instead of asking for the chain of thought.",
            ),
            "general_harms" => Some(
                "Category `general_harms`: the request touched an area flagged as harmful. Benign work can also trigger this.",
            ),
            _ => None,
        }
    }

    /// Builds the user-facing notice for a turn that ended with no visible
    /// assistant output (no text, no tool calls). Returns `None` when the turn
    /// looks normal and no notice should be surfaced.
    pub(crate) fn provider_guardrail_notice(
        stop_reason: Option<&str>,
        visible_text_empty: bool,
        had_reasoning: bool,
    ) -> Option<String> {
        let guardrail = Self::is_guardrail_stop_reason(stop_reason);
        if !guardrail && !visible_text_empty {
            return None;
        }
        let reason_label = stop_reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or("unknown");
        if guardrail {
            let (_, category) = Self::split_guardrail_category(reason_label);
            let category_hint = match category.and_then(Self::guardrail_category_hint) {
                Some(hint) => format!(" {hint}"),
                None => String::new(),
            };
            return Some(format!(
                "Provider guardrail stopped the response (stop_reason: {}). The model declined to answer this request.{} Rephrasing, narrowing the request, or providing more context may help.",
                reason_label, category_hint
            ));
        }
        // Empty visible output with a non-guardrail stop reason: still surface,
        // since the user otherwise sees nothing at all.
        let reasoning_hint = if had_reasoning {
            " after producing only internal reasoning"
        } else {
            ""
        };
        Some(format!(
            "The model ended its turn without any visible output{} (stop_reason: {}). This is usually a provider-side guardrail or filter silently dropping the response. Rephrasing the request may help.",
            reasoning_hint, reason_label
        ))
    }
    fn continuation_prompt_for_stop_reason(stop_reason: &str) -> String {
        format!(
            "[System reminder: your previous response ended before completion (stop_reason: {}). Continue exactly where you left off, do not repeat completed content, and if the next step is a tool call, emit the tool call now.]",
            stop_reason.trim()
        )
    }

    pub(crate) fn maybe_continue_incomplete_response(
        &mut self,
        stop_reason: Option<&str>,
        attempts: &mut u32,
    ) -> Result<bool> {
        let Some(stop_reason) = stop_reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        else {
            return Ok(false);
        };

        if !Self::should_continue_after_stop_reason(stop_reason) {
            return Ok(false);
        }

        if *attempts >= Self::MAX_INCOMPLETE_CONTINUATION_ATTEMPTS {
            logging::warn(&format!(
                "Response ended with stop_reason='{}' after {} continuation attempts; returning partial output",
                stop_reason, attempts
            ));
            return Ok(false);
        }

        *attempts += 1;
        logging::warn(&format!(
            "Response ended with stop_reason='{}'; requesting continuation (attempt {}/{})",
            stop_reason,
            attempts,
            Self::MAX_INCOMPLETE_CONTINUATION_ATTEMPTS
        ));

        self.add_message(
            Role::User,
            vec![ContentBlock::Text {
                text: Self::continuation_prompt_for_stop_reason(stop_reason),
                cache_control: None,
            }],
        );
        self.session.save()?;
        Ok(true)
    }

    pub(super) fn filter_truncated_tool_calls(
        &mut self,
        stop_reason: Option<&str>,
        tool_calls: &mut Vec<ToolCall>,
        assistant_message_id: Option<&String>,
    ) {
        let stop_reason = stop_reason.unwrap_or("");
        if !Self::should_continue_after_stop_reason(stop_reason) {
            return;
        }

        let before = tool_calls.len();
        tool_calls.retain(|tc| !tc.input.is_null());
        let discarded = before - tool_calls.len();
        if discarded > 0 && tool_calls.is_empty() {
            logging::warn(&format!(
                "Discarded {} tool call(s) with null input (truncated by {}); requesting continuation",
                discarded,
                if stop_reason.is_empty() {
                    "unknown"
                } else {
                    stop_reason
                }
            ));
            if let Some(msg_id) = assistant_message_id {
                self.session.remove_tool_use_blocks(msg_id);
                self.persist_session_best_effort("truncated tool-call repair");
            }
        }
    }

    /// Explain a turn that ended with nothing visible.
    ///
    /// Returns `None` when the turn produced real text, or when no
    /// empty-response continuation was ever attempted (an ordinary quiet turn
    /// is not a fault and must not be annotated). Otherwise returns the notice
    /// to show the user, so a silent dead end becomes a recoverable one.
    pub(crate) fn empty_turn_notice(text_content: &str, attempts: u32) -> Option<String> {
        if !text_content.trim().is_empty() || attempts == 0 {
            return None;
        }
        Some(format!(
            "[no response] The model returned an empty reply {} times in a row. \
             This is usually a transient provider fault; retry, or switch models with /model.",
            attempts + 1
        ))
    }

    pub(crate) fn messages_end_with_tool_result(messages: &[Message]) -> bool {
        // Walk backwards looking for a real tool result. A trailing
        // `<system-reminder>` (injected memory, file-activity notices, the
        // per-session context header) is only *continuation* context when it
        // follows an actual tool result, so it is skipped rather than treated
        // as proof of one.
        //
        // Treating a bare reminder as a tool result made every session look
        // like a tool continuation, because the session context header is
        // itself a `<system-reminder>`. That mislabelling fired the
        // empty-response continuation retry on ordinary prompts that had never
        // called a tool, which then re-sent the whole context up to five times
        // and left the user staring at silence.
        for message in messages.iter().rev() {
            if !matches!(message.role, Role::User) {
                return false;
            }
            if message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
            {
                return true;
            }
            let only_system_reminders = !message.content.is_empty()
                && message.content.iter().all(|block| match block {
                    ContentBlock::Text { text, .. } => {
                        text.trim().is_empty() || text.trim().starts_with("<system-reminder>")
                    }
                    _ => false,
                });
            if only_system_reminders {
                continue;
            }
            return false;
        }
        false
    }
}

#[cfg(test)]
mod tool_continuation_tests {
    use super::*;

    fn user_text(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: content.to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: Some(1),
        }
    }

    #[test]
    fn messages_end_with_tool_result_detects_tool_continuation_context() {
        let messages = vec![
            user_text("tell me about the desktop application"),
            tool_result("functions.read:0", "desktop architecture docs"),
            tool_result("functions.agentgrep:4", "desktop source summary"),
        ];

        assert!(Agent::messages_end_with_tool_result(&messages));
    }

    #[test]
    fn messages_end_with_tool_result_allows_memory_after_tool_results() {
        let messages = vec![
            user_text("tell me about the desktop application"),
            tool_result("functions.read:0", "desktop architecture docs"),
            user_text("<system-reminder>Relevant memory</system-reminder>"),
        ];

        assert!(Agent::messages_end_with_tool_result(&messages));
    }

    #[test]
    fn messages_end_with_tool_result_ignores_plain_user_prompt() {
        let messages = vec![user_text("hello")];

        assert!(!Agent::messages_end_with_tool_result(&messages));
    }

    /// Every session opens with a `<system-reminder>` context header, so
    /// treating a bare reminder as tool-continuation context marked *all*
    /// sessions as continuations. Measured over 7 days of real sessions, 950
    /// sessions fired the empty-response retry and 100% of them had never
    /// produced a single tool result.
    #[test]
    fn session_context_header_alone_is_not_a_tool_continuation() {
        let messages = vec![
            user_text("<system-reminder>\n# Session Context\nDate: 2026-07-31\n</system-reminder>"),
            user_text("You are a log distiller. Summarize the excerpt."),
        ];

        assert!(
            !Agent::messages_end_with_tool_result(&messages),
            "a plain prompt preceded by the session context header must not look \
             like a tool continuation"
        );
    }

    /// The reminder header is the *first* message, so the backwards walk has to
    /// keep rejecting even when the reminder is not adjacent to the tail.
    #[test]
    fn system_reminder_before_plain_prompt_is_not_a_tool_continuation() {
        let messages = vec![
            user_text("<system-reminder>Session Context</system-reminder>"),
            user_text("just give me a basic html page"),
        ];

        assert!(!Agent::messages_end_with_tool_result(&messages));
    }

    /// A reminder that genuinely trails a tool result still counts, so the fix
    /// does not regress legitimate post-tool continuations.
    #[test]
    fn system_reminder_after_real_tool_result_still_counts() {
        let messages = vec![
            user_text("<system-reminder>Session Context</system-reminder>"),
            user_text("read the file"),
            tool_result("functions.read:0", "file contents"),
            user_text("<system-reminder>Relevant memory</system-reminder>"),
        ];

        assert!(Agent::messages_end_with_tool_result(&messages));
    }

    /// A normal quiet turn must not be annotated: no continuation was ever
    /// attempted, so there is no fault to explain.
    #[test]
    fn empty_turn_without_continuations_is_not_annotated() {
        assert_eq!(Agent::empty_turn_notice("", 0), None);
        assert_eq!(Agent::empty_turn_notice("   \n ", 0), None);
    }

    /// Real text is never overwritten, even after continuations were attempted.
    #[test]
    fn empty_turn_notice_never_replaces_real_text() {
        assert_eq!(Agent::empty_turn_notice("here is the answer", 3), None);
    }

    /// The measured failure: continuations ran and nothing visible came back.
    /// 949 of 950 affected sessions ended exactly here with an empty bubble.
    #[test]
    fn exhausted_continuations_explain_the_silence() {
        let notice = Agent::empty_turn_notice("", 5).expect("silence must be explained");
        assert!(notice.contains("empty reply"), "notice: {notice}");
        assert!(
            notice.contains("6 times"),
            "counts the original reply too: {notice}"
        );
        assert!(
            notice.contains("/model"),
            "offers a recovery path: {notice}"
        );
    }

    #[test]
    fn whitespace_only_turn_after_continuations_is_explained() {
        assert!(Agent::empty_turn_notice("  \n\t ", 1).is_some());
    }
}
