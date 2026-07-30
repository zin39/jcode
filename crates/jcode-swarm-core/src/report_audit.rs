//! Cross-checking a worker's completion report against what it actually did.
//!
//! A worker's `validation` field is free text, so a model can describe commands
//! it never ran. This is not hypothetical: measured across real sessions,
//! reports carry confident strings like "All 7 steps executed with real command
//! output" from sessions whose transcript contains zero tool calls.
//!
//! Activity is tallied as tools execute rather than read back off the agent at
//! report time. A worker calls `report` from inside its own turn, so its agent
//! `Mutex` is already held and re-locking it to inspect the transcript
//! self-deadlocks (observed as "Failed to record report: deadline has elapsed").
//! A process-global per-session tally keeps the audit lock-free.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// Per-session tool tallies, recorded at dispatch time.
///
/// `Mutex` rather than `RwLock` so a panic while holding it poisons the lock and
/// the recovery below is explicit: the tally is advisory bookkeeping, and a
/// poisoned lock must not take down a worker's report. Recovering the inner
/// value keeps counting rather than silently reporting "no activity", which
/// would turn a lock error into a false accusation of fabrication.
static SESSION_TOOL_ACTIVITY: LazyLock<Mutex<HashMap<String, ToolActivity>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Access the tally, recovering from a poisoned lock.
fn with_activity<T>(action: impl FnOnce(&mut HashMap<String, ToolActivity>) -> T) -> T {
    let mut guard = SESSION_TOOL_ACTIVITY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    action(&mut guard)
}

/// Record that `session_id` invoked `tool_name`.
///
/// Called from the tool dispatch path, where both are already in scope.
pub fn record_session_tool_call(session_id: &str, tool_name: &str) {
    with_activity(|activity| {
        activity
            .entry(session_id.to_string())
            .or_default()
            .record(tool_name)
    });
}

/// Tool activity recorded so far for `session_id`.
pub fn session_tool_activity(session_id: &str) -> ToolActivity {
    // A session with no recorded calls has no entry yet, which is a
    // genuine zero rather than a discarded error.
    with_activity(|activity| match activity.get(session_id) {
        Some(activity) => *activity,
        None => ToolActivity::default(),
    })
}

/// Drop a finished session's tally so long-lived servers do not grow unbounded.
pub fn forget_session_tool_activity(session_id: &str) {
    with_activity(|activity| activity.remove(session_id));
}

/// What a worker's transcript shows it actually did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolActivity {
    /// Commands actually executed (`bash` tool uses).
    pub commands_run: usize,
    /// Files actually read or searched (`read`/`agentgrep` tool uses).
    pub files_inspected: usize,
    /// Files actually modified (`edit`/`write`/`patch` tool uses).
    pub files_edited: usize,
}

impl ToolActivity {
    /// Count one tool invocation.
    pub fn record(&mut self, tool_name: &str) {
        match tool_name {
            "bash" => self.commands_run += 1,
            "read" | "agentgrep" => self.files_inspected += 1,
            "edit" | "write" | "multiedit" | "patch" | "apply_patch" => self.files_edited += 1,
            _ => {}
        }
    }

    /// Tally activity from the tool names a session invoked, in order.
    pub fn from_tool_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut activity = Self::default();
        for name in names {
            activity.record(name.as_ref());
        }
        activity
    }

    /// True when the worker did nothing observable at all.
    pub fn is_silent(self) -> bool {
        self.commands_run == 0 && self.files_inspected == 0 && self.files_edited == 0
    }
}

/// Phrases that assert work was performed. Deliberately narrow: each one claims
/// an *executed action*, not an opinion, so matching text is a checkable claim
/// rather than a hedge like "this looks correct".
const EXECUTION_CLAIMS: &[&str] = &[
    "ran ",
    "i ran",
    "executed",
    "command output",
    "cargo test",
    "cargo build",
    "cargo check",
    "cargo clippy",
    "test suite",
    "tests pass",
    "all tests",
    "verified by running",
    "confirmed by running",
    "output shows",
    "grep confirmed",
];

/// Whether `validation` claims commands were executed.
fn claims_execution(validation: &str) -> bool {
    let lowered = validation.to_ascii_lowercase();
    EXECUTION_CLAIMS.iter().any(|claim| lowered.contains(claim))
}

/// Check a report's `validation` claim against observed tool activity.
///
/// Returns a note to append to the report when the claim is contradicted by the
/// transcript, or `None` when the claim is consistent (or absent).
///
/// This never rejects the report. A worker that has finished cannot un-finish,
/// and rejecting at that point only costs another round trip; the coordinator
/// is the one who needs to know the claim is unbacked, so the finding is
/// surfaced to the reader rather than thrown back at the writer.
pub fn audit_validation_claim(validation: Option<&str>, activity: ToolActivity) -> Option<String> {
    let validation = validation
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if !claims_execution(validation) {
        return None;
    }
    if activity.commands_run > 0 {
        return None;
    }

    Some(if activity.is_silent() {
        "⚠ Unverified: this report claims commands were run, but the worker's \
         transcript records no tool calls at all. Treat the validation claim as \
         unsupported."
            .to_string()
    } else {
        format!(
            "⚠ Unverified: this report claims commands were run, but the worker's \
             transcript records no command executions (it inspected {} file(s) and \
             edited {}). Treat the validation claim as unsupported.",
            activity.files_inspected, activity.files_edited
        )
    })
}

/// Append an output contract to a spawned worker's prompt.
///
/// Recovered from `7a1c15833` (`feat(subagent): optional structured JSON output
/// via output_schema`), which was dropped when the `subagent` tool it patched
/// was deleted upstream.
///
/// Prompt-only rather than provider-native, because the primary worker model
/// rejects strict schema mode outright: `deepseek-v4-pro` answers
/// `HTTP 400 "This response_format type is unavailable now"`. The contract must
/// therefore be stated in the prompt and checked on the way back.
pub fn append_output_contract(prompt: &str, schema: &serde_json::Value) -> String {
    let schema = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
    format!(
        "{prompt}\n\n## Output contract\nYour FINAL message must be exactly one \
         JSON object, with no prose and no code fences, conforming to this JSON \
         Schema:\n{schema}"
    )
}

/// Parse a worker's final message as the JSON object its contract required.
///
/// Tolerates fences and surrounding prose, which models add even when told not
/// to ("Sure! Here you go:" ahead of a ```json block is the common shape).
/// Being lenient here costs nothing and saves a whole round trip, since the
/// object the caller wanted is present either way.
pub fn enforce_structured_output(final_text: &str) -> Result<String, String> {
    let trimmed = final_text.trim();

    // Prefer a fenced block anywhere in the message, then the whole message,
    // then the outermost brace-delimited span. First one that parses wins.
    let mut candidates = Vec::new();
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        let body = match after.find("```") {
            Some(end) => &after[..end],
            None => after,
        };
        candidates.push(body.trim());
    }
    candidates.push(trimmed);
    if let (Some(open), Some(close)) = (trimmed.find('{'), trimmed.rfind('}'))
        && open < close
    {
        candidates.push(&trimmed[open..=close]);
    }

    let mut first_error = None;
    for candidate in candidates {
        match serde_json::from_str::<serde_json::Value>(candidate) {
            Ok(value) => {
                return serde_json::to_string_pretty(&value)
                    .map_err(|err| format!("could not re-serialize: {err}"));
            }
            Err(err) => first_error.get_or_insert_with(|| err.to_string()),
        };
    }

    Err(format!(
        "invalid JSON: {}",
        first_error.unwrap_or_else(|| "no JSON object found".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_tally_into_activity_by_kind() {
        let activity = ToolActivity::from_tool_names([
            "bash",
            "read",
            "edit",
            "bash",
            "agentgrep",
            "swarm",
            "todo",
        ]);
        assert_eq!(activity.commands_run, 2);
        assert_eq!(activity.files_inspected, 2);
        assert_eq!(activity.files_edited, 1);
        assert!(!activity.is_silent());
        assert!(ToolActivity::from_tool_names(["swarm", "todo"]).is_silent());
    }

    /// The measured failure: a real deepseek-v4-pro report whose validation read
    /// "All 7 steps executed with real command output. grep confirmed ..." from
    /// a session with zero bash calls.
    #[test]
    fn a_claim_of_executed_commands_from_a_silent_worker_is_flagged() {
        let note = audit_validation_claim(
            Some("All 7 steps executed with real command output. grep confirmed no scp/rsync."),
            ToolActivity::default(),
        )
        .expect("an execution claim with no tool calls must be flagged");
        assert!(note.contains("no tool calls at all"), "note was: {note}");
    }

    #[test]
    fn the_same_claim_is_not_flagged_when_the_worker_actually_ran_commands() {
        assert_eq!(
            audit_validation_claim(
                Some("Ran cargo test -p jcode-base: 1213 passed."),
                ToolActivity::from_tool_names(["bash", "bash"]),
            ),
            None,
        );
    }

    /// A worker that read code but ran nothing still gets flagged, and the note
    /// reports what it *did* do so the coordinator can judge.
    #[test]
    fn a_reader_that_claims_execution_is_flagged_with_what_it_actually_did() {
        let note = audit_validation_claim(
            Some("Verified by running the test suite."),
            ToolActivity::from_tool_names(["read", "read", "agentgrep", "edit"]),
        )
        .expect("execution claim without commands must be flagged");
        assert!(note.contains("inspected 3 file(s)"), "note was: {note}");
        assert!(note.contains("edited 1"), "note was: {note}");
        assert!(!note.contains("no tool calls at all"), "note was: {note}");
    }

    /// Honest reports must stay clean, or the warning becomes noise people
    /// learn to ignore.
    #[test]
    fn reports_that_make_no_execution_claim_are_left_alone() {
        let silent = ToolActivity::default();
        assert_eq!(audit_validation_claim(None, silent), None);
        assert_eq!(audit_validation_claim(Some("   "), silent), None);
        assert_eq!(
            audit_validation_claim(Some("Design only; no code written."), silent),
            None,
        );
        assert_eq!(
            audit_validation_claim(Some("This approach looks correct to me."), silent),
            None,
        );
    }

    #[test]
    fn the_output_contract_names_the_schema_and_forbids_fences() {
        let schema = serde_json::json!({"type": "object"});
        let prompt = append_output_contract("Do the thing.", &schema);
        assert!(prompt.starts_with("Do the thing."), "prompt was: {prompt}");
        assert!(prompt.contains("no code fences"), "prompt was: {prompt}");
        assert!(
            prompt.contains("\"type\""),
            "schema must be inlined: {prompt}"
        );
    }

    /// Models add fences even when told not to, so the parser tolerates them.
    #[test]
    fn structured_output_accepts_json_bare_and_fenced() {
        let want = "{\n  \"key\": \"value\"\n}";
        for input in [
            r#"{"key": "value"}"#,
            "```json\n{\"key\": \"value\"}\n```",
            "```\n{\"key\": \"value\"}\n```",
            "  \n{\"key\": \"value\"}\n  ",
            // Models routinely prepend a friendly sentence despite the
            // contract saying no prose, and sometimes trail one too.
            "Sure! Here you go:\n```json\n{\"key\": \"value\"}\n```",
            "```json\n{\"key\": \"value\"}\n```\n\nLet me know if you need more.",
            "Here is the result: {\"key\": \"value\"}",
        ] {
            let got = enforce_structured_output(input)
                .unwrap_or_else(|err| panic!("should parse {input:?}: {err}"));
            assert_eq!(got, want, "input was {input:?}");
        }
    }

    #[test]
    fn structured_output_reports_why_prose_is_not_usable() {
        let err = enforce_structured_output("Sure! Here is the result: all good.")
            .expect_err("prose must not parse as the contracted object");
        assert!(err.contains("invalid JSON"), "err was: {err}");
    }
}
