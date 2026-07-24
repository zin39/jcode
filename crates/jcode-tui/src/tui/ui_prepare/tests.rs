use super::*;

#[test]
fn centered_mode_centers_unstructured_messages_and_preserves_structured_left_blocks() {
    for role in ["user", "assistant", "meta", "usage", "error", "memory"] {
        assert_eq!(
            default_message_alignment(role, true),
            ratatui::layout::Alignment::Center,
            "role {role} should default to centered alignment"
        );
    }
    for role in ["tool", "system", "swarm", "background_task"] {
        assert_eq!(
            default_message_alignment(role, true),
            ratatui::layout::Alignment::Left,
            "role {role} should keep left/default alignment"
        );
    }
}

#[test]
fn prepare_body_preserves_multiline_user_prompt_lines() {
    let mut lines = Vec::new();
    let mut raw_plain_lines = Vec::new();
    let mut line_raw_overrides = Vec::new();
    let mut line_copy_offsets = Vec::new();
    let mut user_line_indices = Vec::new();

    push_user_prompt_lines(
        &mut lines,
        &mut raw_plain_lines,
        &mut line_raw_overrides,
        &mut line_copy_offsets,
        &mut user_line_indices,
        1,
        "first line\nsecond line\n\nthird line",
        ratatui::layout::Alignment::Left,
        Tier::Rich,
        "testuser",
    );

    let plain: Vec<String> = lines.iter().map(ui::line_plain_text).collect();

    // New format: header row + body lines with gutter.
    // Line 0: " ▌1 › testuser"  (gutter + header)
    // Line 1: " │ first line"    (gutter + body)
    // Line 2: " │ second line"   (gutter + body)
    // Line 3: " │ "              (gutter + blank)
    // Line 4: " │ third line"    (gutter + body)
    assert_eq!(plain.len(), 5, "expected 5 lines: header + 4 body lines");
    assert!(plain[0].contains("1 › testuser"), "header line: {:?}", plain[0]);
    assert!(plain[1].contains("first line"), "body line 1: {:?}", plain[1]);
    assert!(plain[2].contains("second line"), "body line 2: {:?}", plain[2]);
    assert!(plain[4].contains("third line"), "body line 4: {:?}", plain[4]);

    assert_eq!(
        raw_plain_lines,
        vec!["1 › testuser", "first line", "second line", "", "third line"]
    );
    // user_line_indices now points to the line immediately after the header.
    assert_eq!(user_line_indices, vec![1]);
    // copy offsets: header=0, body lines skip gutter "│ " (2 chars)
    assert_eq!(line_copy_offsets, vec![0, 2, 2, 2, 2]);
}

/// Regression coverage for issue #344: loading older compacted history above
/// an unchanged tail must be detected as a suffix match so scrolling to the
/// start of a long session reuses the prepared tail instead of re-rendering
/// the whole transcript per chunk.
#[test]
fn matching_suffix_len_detects_prepended_history() {
    use jcode_tui_messages::MessageBoundary;

    let old: Vec<DisplayMessage> = (0..4)
        .map(|i| DisplayMessage::system(format!("msg {i}")))
        .collect();

    // Base prepared from the old transcript: boundaries in transcript order.
    let base = PreparedMessages {
        wrapped_lines: Vec::new(),
        wrapped_plain_lines: Arc::new(Vec::new()),
        wrapped_copy_offsets: Arc::new(Vec::new()),
        raw_plain_lines: Arc::new(Vec::new()),
        wrapped_line_map: Arc::new(Vec::new()),
        wrapped_user_indices: Vec::new(),
        wrapped_user_prompt_starts: Vec::new(),
        wrapped_user_prompt_ends: Vec::new(),
        user_prompt_texts: Vec::new(),
        image_regions: Vec::new(),
        edit_tool_ranges: Vec::new(),
        copy_targets: Vec::new(),
        message_boundaries: old
            .iter()
            .map(|m| MessageBoundary {
                msg_hash: m.stable_cache_hash(),
                wrapped_len: 0,
                raw_len: 0,
                user_prompt_len: 0,
            })
            .collect(),
        mermaid_pending_epoch: None,
    };

    // New transcript: two older-history messages prepended, tail unchanged.
    let mut new_msgs: Vec<DisplayMessage> = vec![
        DisplayMessage::system("older history a"),
        DisplayMessage::system("older history b"),
    ];
    new_msgs.extend(old.iter().cloned());
    assert_eq!(matching_suffix_len(&base, &new_msgs), 4);

    // Changed tail: no suffix reuse.
    let mut changed = new_msgs.clone();
    changed.last_mut().unwrap().content = "edited".to_string();
    assert_eq!(matching_suffix_len(&base, &changed), 0);

    // Identical transcript: full suffix match.
    assert_eq!(matching_suffix_len(&base, &old), 4);
}

/// WP3 snapshot test: verify user + assistant message rendering format across
/// multiple terminal widths, including Plain tier fallback.
#[test]
fn snapshot_user_and_assistant_format_at_varying_widths() {
    // Build a rendered conversation fixture:
    //  - 2 user messages (multi-line)
    //  - 2 assistant messages (short + markdown)
    let widths = [60u16, 100, 140];
    let tiers = [Tier::Rich, Tier::Plain];

    for &width in &widths {
        for &tier in &tiers {
            let mut lines: Vec<Line<'static>> = Vec::new();
            let mut raw = Vec::new();
            let mut overrides = Vec::new();
            let mut copy_offsets = Vec::new();
            let mut user_indices = Vec::new();

            // User message 1
            push_user_prompt_lines(
                &mut lines, &mut raw, &mut overrides, &mut copy_offsets,
                &mut user_indices, 1,
                "Explain the retry backoff logic in src/app.rs",
                ratatui::layout::Alignment::Left,
                tier, "testuser",
            );

            // Assistant message 1 (body only, model tag is added by render_message_into)
            let assistant1 = render_assistant_message(
                &DisplayMessage::assistant("I'll trace through the reconnect path.\n\nThe machine has 4 states: Idle, Connecting, Streaming, Blocked."),
                width.saturating_sub(4), crate::config::DiffDisplayMode::Off,
            );
            // Prepend model tag (mimicking what render_message_into does)
            let agent_color = role_color(Role::Agent, tier);
            lines.push(
                Line::from(Span::styled(
                    format!("jcode · mock-model"),
                    Style::default().fg(agent_color),
                ))
                .alignment(ratatui::layout::Alignment::Left),
            );
            raw.push("jcode · mock-model".to_string());
            overrides.push(Some(WrappedLineMap { raw_line: raw.len() - 1, start_col: 0, end_col: 0 }));
            copy_offsets.push(0);
            // blank line between blocks
            lines.push(Line::from(""));
            raw.push(String::new());
            overrides.push(Some(WrappedLineMap { raw_line: raw.len() - 1, start_col: 0, end_col: 0 }));
            copy_offsets.push(0);
            for line in assistant1 {
                lines.push(line);
                raw.push(String::new());
                overrides.push(Some(WrappedLineMap { raw_line: raw.len() - 1, start_col: 0, end_col: 0 }));
                copy_offsets.push(0);
            }

            // Verify structure
            let plain: Vec<String> = lines.iter().map(ui::line_plain_text).collect();
            let full: String = plain.join("\n");

            // Gutter/header for each tier
            let expected_gutter = if tier == Tier::Plain { "|" } else { "▌" };
            let expected_body_gutter = if tier == Tier::Plain { "|" } else { "│" };

            // Check user message 1 format
            assert!(
                full.contains(&format!("{expected_gutter} 1 › testuser")),
                "width {width}, tier {tier:?}: header missing gutter+number+name\n{full}"
            );
            assert!(
                full.contains(&format!("{expected_body_gutter} Explain")),
                "width {width}, tier {tier:?}: body missing gutter\n{full}"
            );

            // Check assistant model tag
            if tier != Tier::Plain {
                assert!(
                    full.contains("jcode · mock-model"),
                    "width {width}, tier {tier:?}: missing model tag\n{full}"
                );
            }
            assert!(
                full.contains("The machine has 4 states"),
                "width {width}, tier {tier:?}: assistant body lost content\n{full}"
            );

            // ---- Render to buffer and check for full-width backgrounds ----
            let total_height = lines.len().max(1) as u16;
            let mut buffer = ratatui::buffer::Buffer::empty(
                ratatui::layout::Rect::new(0, 0, width, total_height),
            );
            for (y, line) in lines.iter().enumerate() {
                let x_offset = match line.alignment {
                    Some(ratatui::layout::Alignment::Center) => {
                        (width as usize).saturating_sub(line.width()) / 2
                    }
                    _ => 0,
                } as u16;
                let mut x = x_offset;
                for span in &line.spans {
                    let span_width = span.width() as u16;
                    if x < width {
                        buffer.set_span(x, y as u16, span, span_width);
                    }
                    x = x.saturating_add(span_width);
                }
            }

            // Check: no full-width background rows. A full-width bg row would
            // have every cell in the row with the same non-Reset bg color.
            let has_full_width_bg = (0..total_height).any(|y| {
                let first_bg = buffer[(0, y)].bg;
                if first_bg == ratatui::style::Color::Reset {
                    return false;
                }
                (1..width).all(|x| buffer[(x, y)].bg == first_bg)
            });
            assert!(
                !has_full_width_bg,
                "width {width}, tier {tier:?}: found full-width background rows\n{full}"
            );
        }
    }
}
