use crate::message::{ContentBlock, Role};
use jcode_session_types::StoredMessage;

/// Classify a session by use-case based on its title and messages.
///
/// Returns `None` when no strong signal is found.
pub(super) fn classify_session(title: Option<&str>, messages: &[StoredMessage]) -> Option<String> {
    let first_user_text = first_visible_user_text(messages);
    let combined = match (title, first_user_text.as_deref()) {
        (Some(t), Some(u)) => format!("{} {}", t, u),
        (Some(t), None) => t.to_string(),
        (None, Some(u)) => u.to_string(),
        (None, None) => return None,
    };
    let lower = combined.to_lowercase();

    // ── Selfdev ──────────────────────────────────────────────────────
    if lower.contains("selfdev")
        || lower.contains("self-dev")
        || lower.contains("jcode codebase")
        || lower.contains("crates/jcode")
        || lower.contains("working on the jcode")
        || lower.contains("modify jcode")
        || lower.contains("jcode itself")
    {
        return Some("selfdev".into());
    }

    // ── Testing ─────────────────────────────────────────────────────
    if lower.contains("cargo test")
        || lower.contains("unit test")
        || lower.contains("add tests")
        || lower.contains("write tests")
        || lower.contains("test coverage")
        || lower.contains("test suite")
        || lower.contains("failing test")
    {
        return Some("testing".into());
    }

    // ── Automation ──────────────────────────────────────────────────
    if lower.contains("you are a log distiller")
        || lower.contains("reply with exactly")
        || lower.contains("no preamble")
        || lower.contains("no additional text")
        || lower.contains("no commentary")
        || lower.contains("output only the")
    {
        return Some("automation".into());
    }

    // swarm-worker phrasing
    {
        if lower.contains("swarm worker")
            || lower.contains("swarm agent")
            || (lower.contains("worker") && lower.contains("subagent"))
        {
            return Some("automation".into());
        }
    }

    // ── Troubleshooting ─────────────────────────────────────────────
    if lower.contains("debug")
        || lower.contains("doesn't work")
        || lower.contains("not working")
        || lower.contains("is broken")
        || lower.contains("error:")
        || lower.contains("stack trace")
        || lower.contains("crash")
        || lower.contains("traceback")
        || lower.contains("panic")
        || lower.contains("segfault")
        || lower.contains("failed to build")
        || lower.contains("build failure")
    {
        return Some("troubleshooting".into());
    }

    // ── Research ────────────────────────────────────────────────────
    if lower.contains("research")
        || lower.contains("investigate")
        || lower.contains("explore")
        || lower.contains("how does")
        || lower.contains("explain")
        || lower.contains("compare")
        || lower.contains("vs.")
        || lower.contains("versus")
        || lower.contains("trade-off")
        || lower.contains("tradeoff")
        || lower.contains("best practice")
        || lower.contains("pros and cons")
    {
        return Some("research".into());
    }

    // ── Ops ─────────────────────────────────────────────────────────
    if lower.contains("deploy")
        || lower.contains("ci/cd")
        || lower.contains("pipeline")
        || lower.contains("docker")
        || lower.contains("kubernetes")
        || lower.contains("k8s")
        || lower.contains("terraform")
        || lower.contains("infra")
        || lower.contains("monitoring")
        || lower.contains("alert")
        || lower.contains("downtime")
        || lower.contains("incident")
    {
        return Some("ops".into());
    }

    // ── Writing ─────────────────────────────────────────────────────
    if lower.contains("write a blog")
        || lower.contains("write an article")
        || lower.contains("write documentation")
        || lower.contains("write docs")
        || lower.contains("draft an email")
        || lower.contains("compose")
        || lower.contains("proofread")
        || lower.contains("edit this text")
        || lower.contains("rewrite")
    {
        return Some("writing".into());
    }

    // ── Learning ────────────────────────────────────────────────────
    if lower.contains("teach me")
        || lower.contains("learn")
        || lower.contains("tutorial")
        || lower.contains("how do i")
        || lower.contains("how should i")
        || lower.contains("what are")
        || lower.contains("why is")
        || lower.contains("introduction")
        || lower.contains("beginners")
    {
        return Some("learning".into());
    }

    // ── Chitchat ────────────────────────────────────────────────────
    if lower.contains("hello")
        || lower.contains("hi ")
        || lower.contains("hey ")
        || lower.contains("how are you")
        || lower.contains("thank")
        || lower.contains("good morning")
        || lower.contains("good afternoon")
        || lower.contains("good evening")
        || lower.contains("joke")
        || lower.contains("tell me a story")
        || lower.contains("fun fact")
    {
        return Some("chitchat".into());
    }

    // ── Coding (catch-all for code-related signals) ─────────────────
    if lower.contains("code")
        || lower.contains("implement")
        || lower.contains("refactor")
        || lower.contains("optimize")
        || lower.contains("patch")
        || lower.contains("merge")
        || lower.contains("commit")
        || lower.contains("pr ")
        || lower.contains("pull request")
        || lower.contains("review")
        || lower.contains("function")
        || lower.contains("struct")
        || lower.contains("class ")
        || lower.contains("api ")
        || lower.contains("endpoint")
        || lower.contains("database")
        || lower.contains("query")
        || lower.contains("migration")
        || lower.contains("schema")
        || lower.contains("build")
        || lower.contains("compile")
        || lower.contains("cargo ")
        || lower.contains("npm ")
        || lower.contains("pip ")
        || lower.contains("git ")
    {
        return Some("coding".into());
    }

    None
}

/// Extract the text content from the first visible user message,
/// stripping `<system-reminder>` blocks.
fn first_visible_user_text(messages: &[StoredMessage]) -> Option<String> {
    for msg in messages {
        if msg.role != Role::User {
            continue;
        }
        // Skip internal messages shown only in the UI
        if msg.display_role.is_some() {
            continue;
        }
        let text = extract_message_text(msg);
        let cleaned = strip_system_reminders(&text);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

/// Concatenate all text blocks from a message.
fn extract_message_text(msg: &StoredMessage) -> String {
    msg.content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove `<system-reminder>...</system-reminder>` blocks from text.
fn strip_system_reminders(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        match rest.find("<system-reminder>") {
            None => {
                result.push_str(rest);
                break;
            }
            Some(start) => {
                result.push_str(&rest[..start]);
                let after_tag = &rest[start + "<system-reminder>".len()..];
                match after_tag.find("</system-reminder>") {
                    None => {
                        // Malformed: no closing tag, keep rest as-is
                        result.push_str(after_tag);
                        break;
                    }
                    Some(end) => {
                        rest = &after_tag[end + "</system-reminder>".len()..];
                    }
                }
            }
        }
    }
    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentBlock, Role};
    use jcode_session_types::{StoredDisplayRole, StoredMessage};
    use chrono::Utc;

    fn msg(role: Role, text: &str) -> StoredMessage {
        StoredMessage {
            id: format!("msg-{}", text.len()),
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            display_role: None,
            timestamp: Some(Utc::now()),
            tool_duration_ms: None,
            token_usage: None,
        }
    }

    fn user(text: &str) -> StoredMessage {
        msg(Role::User, text)
    }

    fn assistant(text: &str) -> StoredMessage {
        msg(Role::Assistant, text)
    }

    // ── Category tests ──────────────────────────────────────────────

    #[test]
    fn selfdev_by_title() {
        let msgs = vec![user("what does git status do")];
        assert_eq!(
            classify_session(Some("selfdev build for tui"), &msgs),
            Some("selfdev".into())
        );
    }

    #[test]
    fn selfdev_by_content() {
        let msgs = vec![user("modify jcode to add a new feature in crates/jcode-base")];
        assert_eq!(
            classify_session(Some("some task"), &msgs),
            Some("selfdev".into())
        );
    }

    #[test]
    fn testing_by_content() {
        let msgs = vec![user("add unit tests for the session classifier")];
        assert_eq!(
            classify_session(Some("testing task"), &msgs),
            Some("testing".into())
        );
    }

    #[test]
    fn testing_cargo_test() {
        let msgs = vec![user("cargo test -p jcode-base session")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("testing".into())
        );
    }

    #[test]
    fn automation_log_distiller() {
        let msgs = vec![user("You are a log distiller. Parse this log file and output only the errors.")];
        assert_eq!(
            classify_session(Some("log parsing"), &msgs),
            Some("automation".into())
        );
    }

    #[test]
    fn automation_reply_with_exactly() {
        let msgs = vec![user("Reply with exactly: hello world, no preamble")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("automation".into())
        );
    }

    #[test]
    fn automation_swarm_worker() {
        let msgs = vec![user("swarm agent subagent worker: run these tests")];
        assert_eq!(
            classify_session(Some("worker task"), &msgs),
            Some("automation".into())
        );
    }

    #[test]
    fn troubleshooting_by_bug() {
        let msgs = vec![user("my code doesn't work, there is a crash when I call this function")];
        assert_eq!(
            classify_session(Some("fix bug"), &msgs),
            Some("troubleshooting".into())
        );
    }

    #[test]
    fn troubleshooting_by_error() {
        let msgs = vec![user("error: segmentation fault in the runtime")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("troubleshooting".into())
        );
    }

    #[test]
    fn research_by_explore() {
        let msgs = vec![user("explore the codebase architecture")];
        assert_eq!(
            classify_session(Some("research"), &msgs),
            Some("research".into())
        );
    }

    #[test]
    fn research_by_vs() {
        let msgs = vec![user("compare rust vs go for web servers")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("research".into())
        );
    }

    #[test]
    fn ops_by_deploy() {
        let msgs = vec![user("deploy the new version to kubernetes")];
        assert_eq!(
            classify_session(Some("ops task"), &msgs),
            Some("ops".into())
        );
    }

    #[test]
    fn ops_by_docker() {
        let msgs = vec![user("docker compose up for the ci/cd pipeline")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("ops".into())
        );
    }

    #[test]
    fn writing_by_blog() {
        let msgs = vec![user("write a blog post about rust async")];
        assert_eq!(
            classify_session(Some("writing task"), &msgs),
            Some("writing".into())
        );
    }

    #[test]
    fn learning_by_teach() {
        let msgs = vec![user("teach me how to use generics in rust")];
        assert_eq!(
            classify_session(Some("learning"), &msgs),
            Some("learning".into())
        );
    }

    #[test]
    fn chitchat_by_hello() {
        let msgs = vec![user("hello how are you today")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("chitchat".into())
        );
    }

    #[test]
    fn chitchat_by_thanks() {
        let msgs = vec![user("thank you for all the help")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("chitchat".into())
        );
    }

    #[test]
    fn coding_by_implement() {
        let msgs = vec![user("implement a new struct with serde support")];
        assert_eq!(
            classify_session(Some("coding task"), &msgs),
            Some("coding".into())
        );
    }

    #[test]
    fn coding_by_refactor() {
        let msgs = vec![user("refactor the authentication module")];
        assert_eq!(
            classify_session(None, &msgs),
            Some("coding".into())
        );
    }

    #[test]
    fn unsure_no_signals() {
        let msgs = vec![user("today the sky is blue with fluffy clouds")];
        assert_eq!(classify_session(None, &msgs), None);
    }

    #[test]
    fn unsure_empty() {
        assert_eq!(classify_session(None, &[]), None);
    }

    // ── Reminder stripping ──────────────────────────────────────────

    #[test]
    fn strips_system_reminders() {
        let msgs = vec![user(
            "<system-reminder>\nSystem context\n</system-reminder>\nactual user question"
        )];
        let text = first_visible_user_text(&msgs);
        assert_eq!(text, Some("actual user question".into()));
    }

    #[test]
    fn strips_multiple_reminders() {
        let msgs = vec![user(
            "<system-reminder>A</system-reminder>\nhello\n<system-reminder>B</system-reminder>\nworld"
        )];
        let text = first_visible_user_text(&msgs);
        assert_eq!(text, Some("hello\n\nworld".into()));
    }

    #[test]
    fn reminder_only_messages_skipped() {
        let msgs = vec![user("<system-reminder>internal</system-reminder>")];
        let text = first_visible_user_text(&msgs);
        assert_eq!(text, None);
    }

    #[test]
    fn skips_internal_messages() {
        let internal = StoredMessage {
            id: "sys-1".into(),
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello world".into(),
                cache_control: None,
            }],
            display_role: Some(StoredDisplayRole::System),
            timestamp: None,
            tool_duration_ms: None,
            token_usage: None,
        };
        let msgs = vec![internal, user("real question")];
        let text = first_visible_user_text(&msgs);
        assert_eq!(text, Some("real question".into()));
    }

    #[test]
    fn skips_assistant_messages() {
        let msgs = vec![
            assistant("I'll help you with that"),
            user("real question"),
        ];
        let text = first_visible_user_text(&msgs);
        assert_eq!(text, Some("real question".into()));
    }

    // ── Serde roundtrip via Session ─────────────────────────────────
    // (done in session_tests to avoid circular deps)
}
