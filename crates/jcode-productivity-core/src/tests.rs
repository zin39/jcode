use crate::model::SessionSummary;
use crate::{render_markdown, render_png, render_svg, report_from_summaries};
use std::collections::BTreeMap;

fn summary(project: &str, user: u32, asst: u32, tools: &[(&str, u32)]) -> SessionSummary {
    let mut t = BTreeMap::new();
    for (k, v) in tools {
        t.insert(k.to_string(), *v);
    }
    SessionSummary {
        project: Some(project.to_string()),
        working_dir: Some(format!("/home/u/{project}")),
        provider_key: Some("openai".to_string()),
        model: Some("gpt-5.5".to_string()),
        user_msgs: user,
        assistant_msgs: asst,
        user_chars: (user as u64) * 50,
        assistant_chars: (asst as u64) * 200,
        tools: t,
        input_tokens: 1000,
        output_tokens: 500,
        cache_read_tokens: 2000,
        active_dates: vec!["2026-06-01".to_string(), "2026-06-02".to_string()],
        ..Default::default()
    }
}

#[test]
fn aggregates_basic_totals() {
    let summaries = vec![
        summary("alpha", 3, 3, &[("read", 5), ("edit", 2), ("bash", 4)]),
        summary("alpha", 2, 2, &[("read", 1), ("apply_patch", 3)]),
        summary("beta", 5, 5, &[("agentgrep", 7), ("browser", 1)]),
    ];
    let r = report_from_summaries(summaries);

    assert_eq!(r.total_sessions, 3);
    assert_eq!(r.user_prompts, 10);
    assert_eq!(r.assistant_messages, 10);
    // read 6 + edit 2 + bash 4 + apply_patch 3 + agentgrep 7 + browser 1 = 23
    assert_eq!(r.total_tool_calls, 23);
    // edit 2 + apply_patch 3 = 5
    assert_eq!(r.code_edits, 5);
    assert_eq!(r.commands_run, 4);
    assert_eq!(r.searches, 7);
    assert_eq!(r.web_actions, 1);
    assert_eq!(r.distinct_projects, 2);
    assert!(r.power_score > 0);
    assert!(!r.archetype.is_empty());
}

#[test]
fn top_lists_sorted_desc() {
    let summaries = vec![
        summary("alpha", 1, 1, &[("read", 10)]),
        summary("alpha", 1, 1, &[("read", 5)]),
        summary("gamma", 1, 1, &[("bash", 1)]),
    ];
    let r = report_from_summaries(summaries);
    assert_eq!(r.top_projects.first().unwrap().name, "alpha");
    assert_eq!(r.top_projects.first().unwrap().count, 2);
    assert_eq!(r.top_tools.first().unwrap().name, "read");
    assert_eq!(r.top_tools.first().unwrap().count, 15);
}

#[test]
fn streaks_and_active_days_dedup() {
    let mut s = summary("alpha", 1, 1, &[("read", 1)]);
    s.active_dates = vec![
        "2026-05-10".to_string(),
        "2026-05-11".to_string(),
        "2026-05-12".to_string(),
        "2026-05-20".to_string(),
    ];
    let r = report_from_summaries(vec![s]);
    assert_eq!(r.active_days, 4);
    assert_eq!(r.longest_streak, 3);
    assert_eq!(r.first_day.as_deref(), Some("2026-05-10"));
    assert_eq!(r.last_day.as_deref(), Some("2026-05-20"));
}

#[test]
fn renders_markdown_and_png() {
    let summaries = vec![summary(
        "alpha",
        4,
        4,
        &[("read", 5), ("edit", 3), ("bash", 2)],
    )];
    let r = report_from_summaries(summaries);

    let md = render_markdown(&r);
    assert!(md.contains("Productivity Report"));
    assert!(md.contains("Power Score"));

    let svg = render_svg(&r);
    assert!(svg.starts_with("<svg"));
    assert!(svg.contains("</text>"));

    // PNG rendering depends on system fonts; ensure it produces a valid PNG.
    let png = render_png(&r).expect("png render");
    assert!(png.len() > 1000, "png too small: {}", png.len());
    assert_eq!(&png[1..4], b"PNG");
}

#[test]
fn empty_report_is_safe() {
    let r = report_from_summaries(vec![]);
    assert_eq!(r.total_sessions, 0);
    let md = render_markdown(&r);
    assert!(md.contains("Productivity Report"));
    let png = render_png(&r).expect("png render");
    assert_eq!(&png[1..4], b"PNG");
}

// ---------------------------------------------------------------------------
// Delegated work
//
// `/n` renders a *shareable* report. Before this, every swarm worker and
// cheap-route subtask counted as the user's own activity, so the more work
// you delegated the more the report overstated what you personally did.
// These pin the split: delegated work is reported, never folded in.
// ---------------------------------------------------------------------------

fn delegated_summary(project: &str, user: u32, asst: u32, tools: &[(&str, u32)]) -> SessionSummary {
    SessionSummary {
        delegated: true,
        ..summary(project, user, asst, tools)
    }
}

#[test]
fn delegated_sessions_do_not_inflate_the_users_own_totals() {
    let own = vec![summary("alpha", 3, 3, &[("read", 5)])];
    let mixed = vec![
        summary("alpha", 3, 3, &[("read", 5)]),
        delegated_summary("alpha", 40, 40, &[("read", 900), ("bash", 900)]),
    ];

    let own_report = report_from_summaries(own);
    let mixed_report = report_from_summaries(mixed);

    // Every headline figure must be identical whether or not agents ran.
    assert_eq!(mixed_report.total_sessions, own_report.total_sessions);
    assert_eq!(mixed_report.user_prompts, own_report.user_prompts);
    assert_eq!(mixed_report.total_messages, own_report.total_messages);
    assert_eq!(mixed_report.total_tool_calls, own_report.total_tool_calls);
    assert_eq!(mixed_report.user_chars, own_report.user_chars);
    assert_eq!(mixed_report.input_tokens, own_report.input_tokens);
    assert_eq!(
        mixed_report.avg_session_msgs, own_report.avg_session_msgs,
        "averaging over agent sessions would distort the user's typical session"
    );
}

#[test]
fn delegated_work_is_reported_rather_than_dropped() {
    // The other half of the contract: hiding delegated work entirely would
    // understate throughput just as badly as merging it overstates effort.
    let r = report_from_summaries(vec![
        summary("alpha", 3, 3, &[("read", 5)]),
        delegated_summary("alpha", 2, 2, &[("read", 10), ("bash", 4)]),
        delegated_summary("beta", 1, 1, &[("edit", 1)]),
    ]);

    assert_eq!(r.delegated_sessions, 2);
    assert_eq!(r.delegated_messages, 6);
    assert_eq!(r.delegated_tool_calls, 15);
    assert!(
        r.delegated_input_tokens > 0,
        "agent token spend is real spend and must be visible"
    );

    let md = render_markdown(&r);
    assert!(md.contains("Delegated to agents"));
    assert!(
        md.contains("67%"),
        "2 of 3 sessions were delegated; got:\n{md}"
    );
}

#[test]
fn a_report_with_no_delegated_work_omits_the_section() {
    // Users who never delegate should not see an empty table of zeroes.
    let r = report_from_summaries(vec![summary("alpha", 3, 3, &[("read", 5)])]);

    assert_eq!(r.delegated_sessions, 0);
    let md = render_markdown(&r);
    assert!(!md.contains("Delegated to agents"), "got:\n{md}");
}

#[test]
fn an_all_delegated_corpus_reports_zero_own_activity_without_dividing_by_zero() {
    // A machine-only corpus (fresh checkout, CI, heavy swarm use) must not
    // panic or produce NaN in the average.
    let r = report_from_summaries(vec![
        delegated_summary("alpha", 1, 1, &[("read", 1)]),
        delegated_summary("beta", 1, 1, &[("bash", 1)]),
    ]);

    assert_eq!(r.total_sessions, 0);
    assert_eq!(r.total_messages, 0);
    assert_eq!(r.avg_session_msgs, 0.0);
    assert!(r.avg_session_msgs.is_finite());
    assert_eq!(r.delegated_sessions, 2);

    let md = render_markdown(&r);
    assert!(md.contains("100%"), "got:\n{md}");
}

// ---------------------------------------------------------------------------
// Scan-level classification
//
// The aggregate tests above assume `delegated` is set correctly. These pin the
// step that actually sets it, against real transcript JSON, because that is
// where the original bug lived: the scanner had no concept of an agent session
// at all and never looked at `agent_role`, `parent_id` or `is_debug`.
// ---------------------------------------------------------------------------

fn transcript(extra_fields: &str, user_turns: usize) -> Vec<u8> {
    let msgs: Vec<String> = (0..user_turns)
        .flat_map(|i| {
            [
                format!(r#"{{"role":"user","content":[{{"type":"text","text":"prompt {i}"}}]}}"#),
                format!(
                    r#"{{"role":"assistant","content":[{{"type":"text","text":"reply {i}"}}]}}"#
                ),
            ]
        })
        .collect();
    format!(
        r#"{{"working_dir":"/home/u/proj",{extra_fields}"messages":[{}]}}"#,
        msgs.join(",")
    )
    .into_bytes()
}

#[test]
fn the_scanner_marks_sessions_that_declare_an_agent_role() {
    for role in [
        "swarm_worker",
        "cheap_route_subtask",
        "subagent",
        "one_shot",
        "internal",
    ] {
        let s = crate::scan::summarize_json(&transcript(&format!(r#""agent_role":"{role}","#), 5))
            .expect("parse");
        assert!(s.delegated, "role {role} must count as delegated work");
    }
}

#[test]
fn the_scanner_marks_spawned_and_debug_sessions_without_a_role() {
    // Lineage and the debug flag are the two signals that predate `agent_role`,
    // and a long-running child is still a child.
    let child = crate::scan::summarize_json(&transcript(r#""parent_id":"session_parent","#, 20))
        .expect("parse");
    assert!(child.delegated, "a spawned child is not the user's session");

    let debug = crate::scan::summarize_json(&transcript(r#""is_debug":true,"#, 20)).expect("parse");
    assert!(debug.delegated);
}

#[test]
fn the_scanner_leaves_the_users_own_conversations_alone() {
    // The conservative half: over-counting a worker is a cosmetic error,
    // but erasing the user's real work from their own report is not.
    let mine = crate::scan::summarize_json(&transcript("", 6)).expect("parse");
    assert!(!mine.delegated);
    assert_eq!(mine.user_msgs, 6);

    // An explicit bookmark outranks every structural signal, including the
    // single-turn shape that would otherwise be inferred as machine traffic.
    let saved = crate::scan::summarize_json(&transcript(r#""saved":true,"#, 1)).expect("parse");
    assert!(!saved.delegated, "a session the user saved is theirs");
}

#[test]
fn the_scanner_applies_the_legacy_rule_to_sessions_written_before_agent_role() {
    // The dominant machine population on a real corpus: untitled, one prompt,
    // one answer, no stored role. This is what flooded the report.
    let one_shot = crate::scan::summarize_json(&transcript("", 1)).expect("parse");
    assert!(
        one_shot.delegated,
        "a single-turn run with no role is machine traffic"
    );

    // Two user turns means someone was talking to it.
    let conversation = crate::scan::summarize_json(&transcript("", 2)).expect("parse");
    assert!(!conversation.delegated);
}

#[test]
fn tool_results_are_not_counted_as_things_the_user_said() {
    // Tool results are stored as user-role messages. On a real transcript they
    // outnumber genuine prompts roughly 8:1, so counting them did two kinds of
    // damage: "prompts sent" was inflated by ~5x, and every agent run looked
    // like a long conversation, which defeated the delegation rule entirely.
    // Measured on the live corpus, fixing this moved 3480 sessions from
    // "yours" to "delegated" and prompts from 25.5K to 5.2K.
    let json = br#"{
        "working_dir":"/home/u/proj",
        "messages":[
            {"role":"user","content":[{"type":"text","text":"do the thing"}]},
            {"role":"assistant","content":[{"type":"tool_use","name":"bash","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","content":"output"}]},
            {"role":"user","content":[{"type":"tool_result","content":"more output"}]},
            {"role":"user","content":[{"type":"text","text":"<system-reminder>ignore me</system-reminder>"}]},
            {"role":"user","content":[{"type":"text","text":"notice"}],"display_role":"system"}
        ]
    }"#;

    let s = crate::scan::summarize_json(json).expect("parse");
    assert_eq!(
        s.user_msgs, 1,
        "only the one real prompt counts, not tool results, reminders or system notices"
    );
    assert_eq!(
        s.user_chars,
        "do the thing".len() as u64,
        "the human-effort proxy must exclude everything the user did not type"
    );
    assert!(
        s.delegated,
        "one real prompt and no reply is a one-shot run, not a conversation"
    );
}
