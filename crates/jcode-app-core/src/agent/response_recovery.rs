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

    pub(super) fn should_continue_after_stop_reason(stop_reason: &str) -> bool {
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
    pub(crate) fn is_guardrail_stop_reason(stop_reason: Option<&str>) -> bool {
        let Some(reason) = stop_reason else {
            return false;
        };
        let reason = reason.trim().to_ascii_lowercase();
        matches!(reason.as_str(), "refusal" | "content_filter" | "safety")
            || reason.contains("guardrail")
            || reason.contains("policy_violation")
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
            return Some(format!(
                "Provider guardrail stopped the response (stop_reason: {}). The model declined to answer this request. Rephrasing, narrowing the request, or providing more context may help.",
                reason_label
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

    /// Number of consecutive collapsed turns (tiny visible output, no tool
    /// calls, non-guardrail stop) before we treat the collapse as a silent
    /// guardrail and surface a notice. A single terse answer ("Yes.", "Done.")
    /// is legitimate; a repeated collapse to a few tokens is the signal that the
    /// provider is silently truncating under a safety filter (observed live on
    /// `claude-opus-4-8` for credential-scanning content: three turns in a row
    /// returned 2-9 output tokens with a normal `end_turn` stop and no answer
    /// the user could see).
    pub(crate) const TINY_OUTPUT_COLLAPSE_THRESHOLD: u32 = 2;

    /// Upper bound (in provider-reported output tokens) below which a no-tool
    /// turn is considered "collapsed". Chosen well under any real answer: even
    /// a one-sentence reply runs 15-40 tokens, while the silent-guardrail
    /// collapses we observed were 2-9 tokens.
    pub(crate) const TINY_OUTPUT_TOKEN_CEILING: u64 = 12;

    /// Upper bound (in trimmed visible characters) for a turn to still count as
    /// collapsed. A genuinely useful terse answer the user can act on ("Yes,
    /// the file exists.") stays under the token ceiling but carries real text;
    /// requiring near-empty visible output keeps legitimate short replies from
    /// tripping the streak. The silent collapses we observed rendered nothing
    /// the user could see.
    pub(crate) const TINY_OUTPUT_CHAR_CEILING: usize = 8;

    /// True when a completed no-tool-call turn produced a suspiciously tiny
    /// visible answer: the provider reported an output-token count at or below
    /// [`Self::TINY_OUTPUT_TOKEN_CEILING`], the visible text is near-empty
    /// (at most [`Self::TINY_OUTPUT_CHAR_CEILING`] chars after trimming), and
    /// the stop reason was a normal end-of-turn (not truncation, not an
    /// explicit guardrail; those are handled elsewhere). `output_tokens == 0`
    /// is excluded because a truly empty answer is already caught by
    /// [`Self::provider_guardrail_notice`].
    pub(crate) fn is_suspicious_tiny_output(
        stop_reason: Option<&str>,
        output_tokens: u64,
        visible_chars: usize,
        has_tool_calls: bool,
        has_reasoning: bool,
    ) -> bool {
        if has_tool_calls {
            return false;
        }
        // Reasoning-only turns (thinking but no answer) are a different failure
        // mode already surfaced by the empty-output notice; don't double-count.
        if has_reasoning {
            return false;
        }
        if output_tokens == 0 || output_tokens > Self::TINY_OUTPUT_TOKEN_CEILING {
            return false;
        }
        // A readable answer, however short, is not a collapse.
        if visible_chars > Self::TINY_OUTPUT_CHAR_CEILING {
            return false;
        }
        // Explicit guardrails and truncation/length stops have their own
        // handling and messaging; only a "normal" stop collapses silently.
        if Self::is_guardrail_stop_reason(stop_reason) {
            return false;
        }
        let reason = stop_reason.map(str::trim).unwrap_or("").to_ascii_lowercase();
        if Self::should_continue_after_stop_reason(&reason) {
            return false;
        }
        // Empty / end_turn / stop are the silent-collapse carriers.
        reason.is_empty() || matches!(reason.as_str(), "end_turn" | "stop" | "tool_use")
    }

    /// User-facing notice for a run of consecutive collapsed turns, once the
    /// streak reaches [`Self::TINY_OUTPUT_COLLAPSE_THRESHOLD`]. Distinct wording
    /// from [`Self::provider_guardrail_notice`] because the stop reason looked
    /// normal, so we frame it as a likely silent guardrail rather than an
    /// explicit refusal.
    pub(crate) fn silent_collapse_notice(streak: u32, output_tokens: u64) -> String {
        format!(
            "The model returned an unusually short response ({output_tokens} tokens) with no visible answer {streak} times in a row. This is usually a provider-side safety guardrail silently truncating the reply rather than returning an explicit refusal. Rephrasing or narrowing the request, or switching to a stronger model, often resolves it."
        )
    }

    // ── Stream-open failure backoff ────────────────────────────────────────

    /// Record a stream-open timeout: increment the consecutive-failure counter,
    /// sleep with exponential backoff (0s → 5s → 15s cap), and return the
    /// user-facing error. When the counter reaches 2 the message is augmented
    /// to call out that identical retries keep failing and suggest switching
    /// models, so the caller (or a swarm coordinator) can route around the
    /// problem instead of looping forever.
    pub(crate) async fn note_stream_open_failure(
        &mut self,
        timeout_secs: u64,
    ) -> anyhow::Error {
        self.consecutive_stream_open_failures =
            self.consecutive_stream_open_failures.saturating_add(1);
        let n = self.consecutive_stream_open_failures;

        // Exponential-ish backoff: 0s, 5s, 15s (capped).
        let backoff = match n {
            1 => std::time::Duration::from_secs(0),
            2 => std::time::Duration::from_secs(5),
            _ => std::time::Duration::from_secs(15),
        };
        if !backoff.is_zero() {
            tokio::time::sleep(backoff).await;
        }

        let base = format!(
            "Provider did not respond within {timeout_secs}s — it may be rate-limited or unreachable. Try again or switch models with /model."
        );
        if n >= 2 {
            anyhow::anyhow!(
                "{base} (Stream-open has failed {n} times in a row — repeated identical retries are unlikely to succeed. Consider switching to a different model.)"
            )
        } else {
            anyhow::anyhow!("{base}")
        }
    }

    /// Reset the consecutive stream-open-failure counter after a successful
    /// stream open.
    pub(crate) fn note_stream_open_success(&mut self) {
        self.consecutive_stream_open_failures = 0;
    }

    /// Update the consecutive-tiny-output streak for a completed no-tool-call
    /// turn and, once the streak reaches [`Self::TINY_OUTPUT_COLLAPSE_THRESHOLD`],
    /// return the user-facing notice to surface. Turns that are NOT a suspicious
    /// collapse reset the streak to 0. Returns `None` while the streak is still
    /// below the threshold (or on a healthy turn).
    ///
    /// This complements [`Self::provider_guardrail_notice`], which only covers
    /// *empty* output and explicit guardrail stops. The gap this closes is a
    /// tiny-but-non-empty reply (a few output tokens the user cannot act on)
    /// with a normal `end_turn` stop, which is how some providers surface a
    /// silent safety truncation.
    pub(crate) fn note_turn_output_collapse(
        &mut self,
        stop_reason: Option<&str>,
        output_tokens: u64,
        visible_chars: usize,
        has_tool_calls: bool,
        has_reasoning: bool,
    ) -> Option<String> {
        let collapsed = Self::is_suspicious_tiny_output(
            stop_reason,
            output_tokens,
            visible_chars,
            has_tool_calls,
            has_reasoning,
        );
        if !collapsed {
            self.consecutive_tiny_outputs = 0;
            return None;
        }
        self.consecutive_tiny_outputs = self.consecutive_tiny_outputs.saturating_add(1);
        logging::warn(&format!(
            "SILENT_COLLAPSE: no-tool turn with tiny output (output_tokens={}, stop_reason={:?}, streak={})",
            output_tokens, stop_reason, self.consecutive_tiny_outputs
        ));
        if self.consecutive_tiny_outputs >= Self::TINY_OUTPUT_COLLAPSE_THRESHOLD {
            Some(Self::silent_collapse_notice(
                self.consecutive_tiny_outputs,
                output_tokens,
            ))
        } else {
            None
        }
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

        // Collect ids of the specific truncated (null-input) tool calls before
        // dropping them, so we can strip only their orphaned ToolUse blocks
        // from history - a mix of one valid + one truncated call must keep
        // the valid call's block intact.
        let discarded_ids: Vec<String> = tool_calls
            .iter()
            .filter(|tc| tc.input.is_null())
            .map(|tc| tc.id.clone())
            .collect();
        if discarded_ids.is_empty() {
            return;
        }
        tool_calls.retain(|tc| !tc.input.is_null());
        let discarded = discarded_ids.len();
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
            // No per-id removal API is exposed on Session; rebuild the
            // message vector locally and hand it back via replace_messages
            // (which handles cache/dirty-state invalidation) instead of
            // nuking every ToolUse block in the message.
            let mut messages = std::mem::take(&mut self.session.messages);
            if let Some(msg) = messages.iter_mut().find(|m| &m.id == msg_id) {
                msg.content.retain(|block| match block {
                    ContentBlock::ToolUse { id, .. } => !discarded_ids.contains(id),
                    _ => true,
                });
            }
            self.session.replace_messages(messages);
            self.persist_session_best_effort("truncated tool-call repair");
        }
    }
}
