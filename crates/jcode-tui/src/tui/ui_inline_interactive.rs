use super::*;
use ratatui::widgets::{Block, BorderType, Borders};
use unicode_width::UnicodeWidthStr;

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn truncate_display(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width + 1 > max_width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

fn pad_left_display(text: &str, width: usize) -> String {
    let truncated = truncate_display(text, width);
    let padding = width.saturating_sub(display_width(truncated.as_str()));
    format!("{}{}", truncated, " ".repeat(padding))
}

fn api_method_display(raw: &str) -> String {
    crate::provider::ModelRouteApiMethod::parse(raw).display_label()
}

fn route_provider_display(provider: &str, api_method: &str) -> String {
    if crate::provider::ModelRouteApiMethod::parse(api_method).is_openrouter()
        && provider != "auto"
        && !provider.contains("OpenRouter")
    {
        format!("OpenRouter/{}", provider)
    } else {
        provider.to_string()
    }
}

fn picker_entry_display_name(entry: &crate::tui::PickerEntry) -> String {
    let base = picker_entry_pretty_name(entry);
    let default_marker = if entry.is_default { " default" } else { "" };
    let is_new = entry
        .options
        .iter()
        .any(|option| option.detail.contains("recently added"));
    // Current model gets a visible marker (but not for account picker)
    let current_marker = if entry.is_current
        && matches!(
            entry.action,
            crate::tui::PickerAction::Model | crate::tui::PickerAction::AgentModelChoice { .. }
        ) {
        " ●"
    } else {
        ""
    };
    let suffix = if is_new && !entry.is_current {
        format!(" new{}{}", default_marker, current_marker)
    } else if entry.is_favorite {
        format!(" ♥{}{}", default_marker, current_marker)
    } else if entry.recommended {
        format!(" ★{}{}", default_marker, current_marker)
    } else if entry.old && !entry.is_current {
        if let Some(ref date) = entry.created_date {
            format!(" {}{}{}", date, default_marker, current_marker)
        } else {
            format!(" old{}{}", default_marker, current_marker)
        }
    } else if let Some(ref date) = entry.created_date {
        if !entry.is_current {
            format!(" {}{}{}", date, default_marker, current_marker)
        } else {
            format!("{}{}", default_marker, current_marker)
        }
    } else {
        format!("{}{}", default_marker, current_marker)
    };

    format!("{}{}", base, suffix)
}

/// Human-friendly rendering of a model picker row's model name.
///
/// `/model` rows historically showed the raw provider model id
/// (`claude-opus-4-8`, `gpt-5.5 (high)`), which reads worse than the pretty
/// names every other surface uses (header, status line, info widgets). We
/// prettify only the well-known families so unfamiliar or namespaced ids
/// (OpenRouter `vendor/model`, local profiles) keep their exact spelling and
/// stay copy-pasteable. Effort suffixes such as ` (high)` are preserved.
fn picker_entry_pretty_name(entry: &crate::tui::PickerEntry) -> String {
    if !matches!(
        entry.action,
        crate::tui::PickerAction::Model | crate::tui::PickerAction::AgentModelChoice { .. }
    ) {
        return entry.name.clone();
    }
    let (base, suffix) = match entry.effort.as_deref() {
        Some(_) => match entry.name.rsplit_once(" (") {
            Some((base, rest)) => (base, format!(" ({rest}")),
            None => (entry.name.as_str(), String::new()),
        },
        None => (entry.name.as_str(), String::new()),
    };
    match crate::tui::app::helpers::model_names::pretty_known_model_family(base) {
        Some(pretty) => format!("{pretty}{suffix}"),
        None => entry.name.clone(),
    }
}

fn picker_row_marker(is_row_selected: bool, unavailable: bool, limited: bool) -> &'static str {
    if unavailable {
        "×"
    } else if limited {
        "⚠"
    } else if is_row_selected {
        "▸"
    } else {
        " "
    }
}

fn route_detail_display_text(detail: &str, unavailable: bool) -> Option<String> {
    let trimmed = detail.trim();
    if unavailable {
        if trimmed.is_empty() {
            Some("unavailable".to_string())
        } else {
            Some(format!("unavailable · {}", trimmed))
        }
    } else if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn route_detail_is_limited(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("fallback:")
        || lower.contains("fallback model")
        || lower.contains("no tools")
        || lower.contains("requires an inference profile")
        || lower.contains("catalog still loading")
        || lower.contains("provider will initialize")
}

fn selected_route_notice_text(
    picker: &crate::tui::InlineInteractiveState,
    route: Option<&crate::tui::PickerOption>,
) -> Option<(String, bool)> {
    if picker.kind != crate::tui::PickerKind::Model {
        return None;
    }
    let route = route?;
    let unavailable = !route.available;
    let detail = route_detail_display_text(&route.detail, unavailable)?;
    if unavailable {
        return Some((format!("× {}", detail), true));
    }
    if route_detail_is_limited(&route.detail) {
        return Some((format!("⚠ {}", detail), true));
    }
    if route
        .detail
        .to_ascii_lowercase()
        .contains("inference profile")
    {
        return Some((format!("ⓘ {}", detail), false));
    }
    None
}

/// Format the inline effort ladder for model picker rows.
/// Focused row shows all available efforts: `[none] [low] >[med]< [high]`
/// Unfocused rows show just the selected effort as dim text.
fn format_effort_ladder(
    entry: &crate::tui::PickerEntry,
    is_row_selected: bool,
) -> Option<Vec<Span<'static>>> {
    // Only for model entries with effort support
    if !matches!(
        entry.action,
        crate::tui::PickerAction::Model | crate::tui::PickerAction::AgentModelChoice { .. }
    ) {
        return None;
    }

    if entry.available_efforts.is_empty() {
        return None;
    }

    let mut spans: Vec<Span<'static>> = Vec::new();

    if is_row_selected {
        // Focused row: show only the current effort between chevrons, e.g. `‹ high ›`.
        //
        // Rendering the whole ladder inline needed 30+ columns, so on real
        // terminal widths it was truncated mid-list ("‹none min") and the
        // selected value was often in the clipped region — leaving no way to
        // see what Left/Right had actually selected. Only one value is ever
        // active, and the chevrons already advertise that arrows cycle it.
        let selected_effort = entry
            .effort
            .as_deref()
            .or(entry.available_efforts.first().map(String::as_str))
            .unwrap_or("");

        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled("‹ ", Style::default().fg(dim_color())));
        spans.push(Span::styled(
            short_effort_label(selected_effort),
            Style::default().fg(rgb(255, 200, 100)).bold(),
        ));
        spans.push(Span::styled(" ›", Style::default().fg(dim_color())));
    } else {
        // Unfocused row: just show the selected effort dimly
        if let Some(ref effort) = entry.effort {
            let label = short_effort_label(effort);
            spans.push(Span::styled(
                format!(" {}", label),
                Style::default().fg(rgb(100, 100, 120)),
            ));
        }
    }

    if spans.is_empty() { None } else { Some(spans) }
}

/// Short effort label for the inline ladder (none, low, med, high, xhigh, max, minimal).
/// Returns a static string for known efforts, or formats unknown ones.
fn short_effort_label(effort: &str) -> String {
    match effort {
        "none" => "none".to_string(),
        "minimal" => "min".to_string(),
        "low" => "low".to_string(),
        "medium" => "med".to_string(),
        "high" => "high".to_string(),
        "xhigh" => "xhigh".to_string(),
        "max" => "max".to_string(),
        other => other.to_string(), // fallback to the raw string
    }
}

fn dim_color() -> Color {
    rgb(100, 100, 120)
}

fn model_picker_top_hint(picker: &crate::tui::InlineInteractiveState) -> Option<&'static str> {
    let is_swarm_agent_model_picker = picker.kind == crate::tui::PickerKind::Model
        && picker.entries.iter().any(|entry| {
            matches!(
                entry.action,
                crate::tui::PickerAction::AgentModelChoice {
                    target: crate::tui::AgentModelTarget::Swarm,
                    ..
                }
            )
        });
    if is_swarm_agent_model_picker {
        return Some(
            " swarm routing is configured by a prompt · /swarm-prompt to edit the active file",
        );
    }

    // The favorite/default hotkeys now work in both the focused picker and the
    // as-you-type preview, so the hint is shown whenever this is a runtime model
    // picker (i.e. it has selectable model rows).
    let is_runtime_model_picker = picker.kind == crate::tui::PickerKind::Model
        && picker
            .entries
            .iter()
            .any(|entry| matches!(entry.action, crate::tui::PickerAction::Model));
    if is_runtime_model_picker {
        Some(
            " keys: Ctrl+O set default · Ctrl+N favorite · Shift+Tab switch active model to next favorite",
        )
    } else {
        None
    }
}

fn account_picker_shows_provider_badge(picker: &crate::tui::InlineInteractiveState) -> bool {
    let mut providers: Vec<&str> = Vec::new();
    for &fi in &picker.filtered {
        let entry = &picker.entries[fi];
        if let Some(route) = entry.options.get(entry.selected_option) {
            let provider = route.provider.trim();
            if !provider.is_empty()
                && !providers
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(provider))
            {
                providers.push(provider);
                if providers.len() > 1 {
                    return true;
                }
            }
        }
    }
    false
}

fn account_picker_entry_title(
    entry: &crate::tui::PickerEntry,
    show_provider_badge: bool,
) -> (String, usize) {
    let display_name = picker_entry_display_name(entry);
    let provider_prefix = if show_provider_badge {
        entry
            .options
            .get(entry.selected_option)
            .map(|route| format!("{} · ", route.provider))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let prefix_chars = provider_prefix.chars().count();
    (format!("{}{}", provider_prefix, display_name), prefix_chars)
}

fn account_inline_interactive_state_label(entry: &crate::tui::PickerEntry) -> &'static str {
    entry.account_state_label().unwrap_or("-")
}

fn picker_render_width(picker: &crate::tui::InlineInteractiveState, max_width: usize) -> usize {
    let marker_width = 3usize;
    let is_preview = picker.preview;
    const WIDTH_SCAN_LIMIT: usize = 200;

    if picker.uses_compact_navigation() {
        let show_provider_badge = account_picker_shows_provider_badge(picker);
        let mut max_title_len = display_width("ACCOUNT");
        let mut max_state_len = display_width("STATE");

        for &fi in &picker.filtered {
            let entry = &picker.entries[fi];
            let (title, _) = account_picker_entry_title(entry, show_provider_badge);
            max_title_len = max_title_len.max(display_width(title.as_str()));
            max_state_len =
                max_state_len.max(display_width(account_inline_interactive_state_label(entry)));
        }

        let state_width = (max_state_len + 1).clamp(7, 10);
        let min_title_width = max_title_len.clamp(8, 10);
        let title_cap = if show_provider_badge { 42 } else { 34 };
        let budget = max_width.saturating_sub(marker_width + state_width);
        let title_width = max_title_len
            .min(title_cap)
            .min(budget.max(min_title_width.min(budget)));

        return marker_width + title_width + state_width;
    }

    let mut max_model_len = display_width(picker.primary_label());
    let mut max_provider_len = display_width(picker.secondary_label(is_preview));
    let mut max_via_len = display_width(picker.tertiary_label());
    // The inline effort ladder is emitted between the model and provider
    // columns at render time but was never counted here, so every column after
    // it was pushed right and the last one (METHOD) fell off the edge. On a
    // real catalog that truncated "api key" to "ap", which reads as though the
    // row *is* an api-key route and hides whether OAuth is being used at all.
    let mut max_effort_len = 0usize;

    for &fi in picker.filtered.iter().take(WIDTH_SCAN_LIMIT) {
        let entry = &picker.entries[fi];
        max_model_len = max_model_len.max(display_width(picker_entry_display_name(entry).as_str()));
        max_effort_len = max_effort_len.max(effort_ladder_display_width(entry));
        if let Some(route) = entry.active_option() {
            let provider_label = route_provider_display(&route.provider, &route.api_method);
            let provider_label = if entry.option_count() > 1 {
                format!("{} ({})", provider_label, entry.option_count())
            } else {
                provider_label
            };
            max_provider_len = max_provider_len.max(display_width(provider_label.as_str()));
            max_via_len = max_via_len.max(display_width(&api_method_display(&route.api_method)));
        }
    }

    let mut provider_width = max_provider_len + 1;
    let mut via_width = max_via_len + 1;
    let model_cap = if picker.kind == crate::tui::PickerKind::Model {
        max_width
    } else if is_preview {
        42
    } else {
        56
    };
    let min_model_width = max_model_len.clamp(6, 8);

    // Reserve ladder columns only while the row can still afford them. The
    // provider/method floors (8 + 4) plus the marker already claim most of a
    // very narrow window, so on a ~24-column terminal a 10-column ladder cannot
    // fit at all. Drop the reservation rather than return a row wider than the
    // window: a clipped ladder is recoverable (arrow keys still cycle effort,
    // and the value is echoed in the status line), whereas an oversized row
    // corrupts the whole picker frame.
    let floors = 8usize + 4;
    if max_effort_len + floors + marker_width > max_width {
        max_effort_len = 0;
    }

    let budget = max_width.saturating_sub(marker_width + max_effort_len);
    if provider_width + via_width + min_model_width > budget {
        let provider_floor = 8usize.min(provider_width);
        let via_floor = 4usize.min(via_width);

        let provider_reduction = (provider_width + via_width + min_model_width)
            .saturating_sub(budget)
            .min(provider_width.saturating_sub(provider_floor));
        provider_width = provider_width.saturating_sub(provider_reduction);

        let via_reduction = (provider_width + via_width + min_model_width)
            .saturating_sub(budget)
            .min(via_width.saturating_sub(via_floor));
        via_width = via_width.saturating_sub(via_reduction);
    }

    let model_budget = budget.saturating_sub(provider_width + via_width);
    // Clamp to what is left rather than to `min_model_width`: on a very narrow
    // terminal the provider/method floors can already consume the whole budget,
    // and honouring a model minimum on top of them would return a row wider
    // than the window. Reserving ladder columns made that reachable, so the
    // final width is capped at the budget it was computed from.
    let model_width = max_model_len
        .min(model_cap)
        .min(model_budget.max(min_model_width))
        .min(model_budget);

    marker_width + max_effort_len + provider_width + via_width + model_width
}

/// Columns the inline effort ladder occupies for `entry`.
///
/// Must track [`format_effort_ladder`]: the focused row renders
/// `"  \u{2039} <label> \u{203a}"` and unfocused rows render `" <label>"`. Width is
/// computed for the *focused* shape because any row can become focused without
/// the layout being recomputed, and a column that shifts as you arrow down is
/// worse than one that is uniformly a little wider.
fn effort_ladder_display_width(entry: &crate::tui::PickerEntry) -> usize {
    if !matches!(
        entry.action,
        crate::tui::PickerAction::Model | crate::tui::PickerAction::AgentModelChoice { .. }
    ) {
        return 0;
    }
    if entry.available_efforts.is_empty() {
        return 0;
    }

    let selected = entry
        .effort
        .as_deref()
        .or(entry.available_efforts.first().map(String::as_str))
        .unwrap_or("");
    // "  " + "\u{2039} " + label + " \u{203a}"
    display_width("  \u{2039}  \u{203a}") + display_width(short_effort_label(selected).as_ref())
}

pub(super) fn format_elapsed(secs: f32) -> String {
    if secs >= 3600.0 {
        let hours = (secs / 3600.0) as u32;
        let mins = ((secs % 3600.0) / 60.0) as u32;
        format!("{}h {}m", hours, mins)
    } else if secs >= 60.0 {
        let mins = (secs / 60.0) as u32;
        let s = (secs % 60.0) as u32;
        format!("{}m {}s", mins, s)
    } else {
        format!("{}s", secs as u32)
    }
}

fn fuzzy_match_positions(pattern: &str, text: &str) -> Vec<usize> {
    jcode_fuzzy::fuzzy_match_positions(pattern, text)
}

pub(super) fn draw_inline_interactive(frame: &mut Frame, app: &dyn TuiState, area: Rect) {
    let picker = match app.inline_interactive_state() {
        Some(p) => p,
        None => return,
    };

    let height = area.height as usize;
    let width = area.width as usize;
    if height <= 2 || width <= 2 {
        return;
    }

    let selected = picker.selected;
    let total = picker.entries.len();
    let filtered_count = picker.filtered.len();
    let col = picker.column;
    let is_preview = picker.preview;
    let is_account_picker = picker.uses_compact_navigation();
    let is_usage_picker = picker.kind == crate::tui::PickerKind::Usage;

    let col_focus_style = Style::default().fg(accent_color()).bold();
    let col_dim_style = Style::default().fg(dim_color());
    let marker_width = 3usize;
    const WIDTH_SCAN_LIMIT: usize = 200;

    let show_account_provider_badge =
        is_account_picker && account_picker_shows_provider_badge(picker);
    let mut max_provider_len = display_width(picker.secondary_label(is_preview));
    let mut max_via_len = display_width(picker.tertiary_label());
    let mut max_account_title_len = display_width("ACCOUNT");
    let mut max_account_state_len = display_width("STATE");
    for &fi in picker.filtered.iter().take(WIDTH_SCAN_LIMIT) {
        let entry = &picker.entries[fi];
        let route = entry.active_option();
        if let Some(r) = route {
            max_provider_len = max_provider_len.max(display_width(r.provider.as_str()));
            max_via_len = max_via_len.max(display_width(&api_method_display(&r.api_method)));
        }
        if is_account_picker {
            let (title, _) = account_picker_entry_title(entry, show_account_provider_badge);
            max_account_title_len = max_account_title_len.max(display_width(title.as_str()));
            max_account_state_len = max_account_state_len
                .max(display_width(account_inline_interactive_state_label(entry)));
        }
    }
    max_provider_len = max_provider_len.max(8);
    max_via_len = max_via_len.max(3);

    let content_width = picker_render_width(picker, width.saturating_sub(2)).max(1);
    let outer_width = content_width.saturating_add(2).min(width);
    let horizontal_offset = if app.centered_mode() {
        area.width.saturating_sub(outer_width as u16) / 2
    } else {
        0
    };

    // Hotkey hint sits ABOVE the picker box (outside its border) so the
    // shortcuts are always visible without competing with the column headers.
    let keybind_hint = model_picker_top_hint(picker);
    let hint_rows: u16 = if keybind_hint.is_some() && area.height > 3 {
        1
    } else {
        0
    };
    if let Some(hint) = keybind_hint.filter(|_| hint_rows == 1) {
        // The hint lives outside the box, so it may use the full available
        // width rather than the (often narrow) intrinsic box width. This keeps
        // the favorites/default shortcuts visible even for short model lists.
        let hint_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_display(hint, area.width.saturating_sub(1) as usize),
                Style::default().fg(rgb(120, 120, 150)).italic(),
            ))),
            hint_area,
        );
    }

    let render_area = Rect {
        x: area.x + horizontal_offset,
        y: area.y + hint_rows,
        width: outer_width as u16,
        height: area.height.saturating_sub(hint_rows),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(85, 85, 110)))
        .style(Style::default().bg(rgb(18, 18, 26)));
    frame.render_widget(block.clone(), render_area);

    let inner = block.inner(render_area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let height = inner.height as usize;
    let width = inner.width as usize;

    let mut provider_width = (max_provider_len + 1).max(8);
    let mut via_width = (max_via_len + 1).max(6);
    if !is_account_picker {
        let min_model_width = 8usize;
        let needed = marker_width + provider_width + via_width + min_model_width;
        if needed > width {
            let provider_floor = 8usize.min(provider_width);
            let via_floor = 6usize.min(via_width);
            let provider_reduction = needed
                .saturating_sub(width)
                .min(provider_width.saturating_sub(provider_floor));
            provider_width = provider_width.saturating_sub(provider_reduction);
            let still_needed = marker_width + provider_width + via_width + min_model_width;
            let via_reduction = still_needed
                .saturating_sub(width)
                .min(via_width.saturating_sub(via_floor));
            via_width = via_width.saturating_sub(via_reduction);
        }
    }
    let account_state_width = (max_account_state_len + 1).clamp(7, 10);
    let account_title_width = width.saturating_sub(marker_width + account_state_width);
    let model_width = width.saturating_sub(marker_width + provider_width + via_width);

    let (col_labels, col_logical) = picker.header_layout(is_preview);
    let col_widths: [usize; 3] = if is_account_picker {
        [account_title_width, account_state_width, 0]
    } else if is_preview {
        [provider_width, model_width, via_width]
    } else {
        [model_width, provider_width, via_width]
    };

    let mut header_spans: Vec<Span> = Vec::new();

    let first_label = col_labels[0];
    let first_w = marker_width + col_widths[0];
    let first_style = if col_logical[0] == col {
        col_focus_style
    } else {
        col_dim_style
    };
    header_spans.push(Span::styled(
        format!(" {:<w$}", first_label, w = first_w.saturating_sub(1)),
        first_style,
    ));

    let second_label = col_labels[1];
    let second_w = col_widths[1];
    let second_style = if col_logical[1] == col {
        col_focus_style
    } else {
        col_dim_style
    };
    header_spans.push(Span::styled(
        if is_preview {
            format!(" {:<w$}", second_label, w = second_w.saturating_sub(1))
        } else {
            format!("{:<w$}", second_label, w = second_w)
        },
        second_style,
    ));

    if !is_account_picker {
        let third_label = col_labels[2];
        let third_style = if col_logical[2] == col {
            col_focus_style
        } else {
            col_dim_style
        };
        header_spans.push(Span::styled(format!(" {}", third_label), third_style));
    }

    let mut meta_parts = String::new();
    if !picker.filter.is_empty() {
        meta_parts.push_str(&format!("  \"{}\"", picker.filter));
    }
    let count_str = if filtered_count == total {
        format!(" ({})", total)
    } else {
        format!(" ({}/{})", filtered_count, total)
    };
    meta_parts.push_str(&count_str);
    header_spans.push(Span::styled(meta_parts, Style::default().fg(dim_color())));

    if is_preview {
        header_spans.push(Span::styled(
            picker.preview_submit_hint(),
            Style::default().fg(rgb(60, 60, 80)).italic(),
        ));
    } else {
        header_spans.push(Span::styled(
            picker.active_submit_hint(),
            Style::default().fg(rgb(60, 60, 80)),
        ));
        if picker.shows_default_shortcut_hint() {
            header_spans.push(Span::styled(
                "  Ctrl-O=set default",
                Style::default().fg(rgb(60, 60, 80)).italic(),
            ));
        }
    }

    let row_base_width = if is_account_picker {
        marker_width + account_title_width + account_state_width
    } else {
        marker_width + provider_width + via_width + model_width
    };
    let detail_width = width.saturating_sub(row_base_width).saturating_sub(2);

    let selected_route_notice = picker
        .filtered
        .get(selected)
        .and_then(|entry_idx| picker.entries.get(*entry_idx))
        .and_then(|entry| selected_route_notice_text(picker, entry.active_option()));

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(header_spans));
    if let Some((notice, warning)) = selected_route_notice.as_ref() {
        let notice_width = width.saturating_sub(1);
        lines.push(Line::from(Span::styled(
            format!(" {}", truncate_display(notice.as_str(), notice_width)),
            if *warning {
                Style::default().fg(rgb(210, 150, 110)).italic()
            } else {
                Style::default().fg(dim_color()).italic()
            },
        )));
    }

    if picker.filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "   no matches",
            Style::default().fg(dim_color()).italic(),
        )));
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    let list_header_lines = 1 + usize::from(selected_route_notice.is_some());
    let list_height = height.saturating_sub(list_header_lines);
    if list_height == 0 {
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }

    // Use display_rows for rendering (includes section headers)
    let display_count = picker.display_rows.len();
    let half = list_height / 2;
    let start = if selected <= half {
        0
    } else if selected + list_height - half > display_count {
        display_count.saturating_sub(list_height)
    } else {
        selected - half
    };
    let end = (start + list_height).min(display_count);

    for vi in start..end {
        let row = &picker.display_rows[vi];
        let is_row_selected = vi == selected;

        // Handle section headers
        match row {
            crate::tui::PickerDisplayRow::RecentHeader { count } => {
                let header_text = format!("★ Recent ({})", count);
                lines.push(Line::from(Span::styled(
                    format!("   {}", header_text),
                    Style::default().fg(rgb(255, 220, 120)).bold(),
                )));
                continue;
            }
            crate::tui::PickerDisplayRow::ProviderHeader { provider, count } => {
                let collapsed = picker.collapse_state.is_collapsed(provider);
                let expand_marker = if collapsed { "▸" } else { "▾" };
                let header_text = format!("{} {} ({})", expand_marker, provider, count);
                let style = if is_row_selected {
                    Style::default().fg(rgb(200, 200, 220)).bg(rgb(50, 50, 70))
                } else {
                    Style::default().fg(rgb(140, 140, 160))
                };
                lines.push(Line::from(Span::styled(
                    format!("   {}", header_text),
                    style,
                )));
                continue;
            }
            crate::tui::PickerDisplayRow::Entry { entry_index } => {
                let model_idx = *entry_index;
                let entry = &picker.entries[model_idx];
                let route = entry.active_option();
                let unavailable = route.map(|r| !r.available).unwrap_or(true);

                let limited = route
                    .map(|r| r.available && route_detail_is_limited(&r.detail))
                    .unwrap_or(false);
                let marker = picker_row_marker(is_row_selected, unavailable, limited);

                let mut spans: Vec<Span> = Vec::new();
                spans.push(Span::styled(
                    format!(" {} ", marker),
                    if unavailable {
                        Style::default().fg(rgb(180, 120, 120)).bold()
                    } else if is_row_selected {
                        Style::default().fg(Color::White).bold()
                    } else {
                        Style::default().fg(dim_color())
                    },
                ));
                let display_name = picker_entry_display_name(entry);
                let account_action_color = match &entry.action {
                    crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Add {
                        ..
                    }) => Some(rgb(140, 220, 170)),
                    crate::tui::PickerAction::Account(
                        crate::tui::AccountPickerAction::Replace { .. },
                    ) => Some(rgb(240, 200, 120)),
                    crate::tui::PickerAction::Account(
                        crate::tui::AccountPickerAction::OpenCenter { .. },
                    ) => Some(rgb(150, 190, 255)),
                    _ => None,
                };
                let primary_style = if unavailable {
                    Style::default().fg(rgb(80, 80, 80))
                } else if is_row_selected && col == 0 {
                    Style::default().fg(Color::White).bg(rgb(60, 60, 80)).bold()
                } else if let Some(color) = account_action_color {
                    Style::default().fg(color).bold()
                } else if entry.is_current {
                    Style::default().fg(accent_color())
                } else if entry.is_favorite {
                    Style::default().fg(rgb(255, 160, 210)).bold()
                } else if entry.recommended {
                    Style::default().fg(rgb(255, 220, 120))
                } else if entry.old {
                    Style::default().fg(rgb(120, 120, 130))
                } else {
                    Style::default().fg(rgb(200, 200, 220))
                };

                if is_account_picker {
                    let (title_text, title_prefix_chars) =
                        account_picker_entry_title(entry, show_account_provider_badge);
                    let padded_title = pad_left_display(title_text.as_str(), account_title_width);
                    let state_label = account_inline_interactive_state_label(entry);
                    let state_display = format!(
                        " {}",
                        pad_left_display(state_label, account_state_width.saturating_sub(1))
                    );
                    let match_positions = if !picker.filter.is_empty() {
                        fuzzy_match_positions(&picker.filter, &entry.name)
                            .into_iter()
                            .map(|p| p + title_prefix_chars)
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    let title_spans: Vec<Span> = if match_positions.is_empty() || unavailable {
                        vec![Span::styled(padded_title, primary_style)]
                    } else {
                        let title_chars: Vec<char> = padded_title.chars().collect();
                        let highlight_style = primary_style.underlined();
                        let mut result = Vec::new();
                        let mut run_start = 0;
                        let mut is_match_run =
                            !title_chars.is_empty() && match_positions.contains(&0);
                        for ci in 1..=title_chars.len() {
                            let cur_is_match =
                                ci < title_chars.len() && match_positions.contains(&ci);
                            if cur_is_match != is_match_run || ci == title_chars.len() {
                                let chunk: String = title_chars[run_start..ci].iter().collect();
                                result.push(Span::styled(
                                    chunk,
                                    if is_match_run {
                                        highlight_style
                                    } else {
                                        primary_style
                                    },
                                ));
                                run_start = ci;
                                is_match_run = cur_is_match;
                            }
                        }
                        result
                    };

                    let state_style = if unavailable {
                        Style::default().fg(rgb(80, 80, 80))
                    } else if is_row_selected {
                        Style::default().fg(Color::White).bg(rgb(60, 60, 80)).bold()
                    } else if entry.is_current {
                        Style::default().fg(accent_color()).bold()
                    } else if let Some(color) = account_action_color {
                        Style::default().fg(color)
                    } else {
                        Style::default().fg(dim_color())
                    };

                    spans.extend(title_spans);
                    spans.push(Span::styled(state_display, state_style));
                    if let Some(route) = route
                        && let Some(detail_text) =
                            route_detail_display_text(&route.detail, unavailable)
                        && detail_width > 0
                    {
                        spans.push(Span::styled(
                            format!("  {}", truncate_display(detail_text.as_str(), detail_width)),
                            if unavailable {
                                Style::default().fg(rgb(180, 120, 120)).italic()
                            } else {
                                Style::default().fg(dim_color())
                            },
                        ));
                    }

                    lines.push(Line::from(spans));
                    continue;
                }

                let padded_model = if is_preview {
                    format!(
                        " {}",
                        pad_left_display(display_name.as_str(), model_width.saturating_sub(1))
                    )
                } else {
                    pad_left_display(display_name.as_str(), model_width)
                };

                let match_positions = if !picker.filter.is_empty() {
                    let raw = fuzzy_match_positions(&picker.filter, display_name.as_str());
                    if is_preview && !raw.is_empty() {
                        // Account for the single leading space in the preview model column.
                        raw.into_iter().map(|p| p + 1).collect()
                    } else {
                        raw
                    }
                } else {
                    Vec::new()
                };
                let model_spans: Vec<Span> = if match_positions.is_empty() || unavailable {
                    vec![Span::styled(padded_model, primary_style)]
                } else {
                    let model_chars: Vec<char> = padded_model.chars().collect();
                    let highlight_style = primary_style.underlined();
                    let mut result = Vec::new();
                    let mut run_start = 0;
                    let mut is_match_run = !model_chars.is_empty() && match_positions.contains(&0);
                    for ci in 1..=model_chars.len() {
                        let cur_is_match = ci < model_chars.len() && match_positions.contains(&ci);
                        if cur_is_match != is_match_run || ci == model_chars.len() {
                            let chunk: String = model_chars[run_start..ci].iter().collect();
                            result.push(Span::styled(
                                chunk,
                                if is_match_run {
                                    highlight_style
                                } else {
                                    primary_style
                                },
                            ));
                            run_start = ci;
                            is_match_run = cur_is_match;
                        }
                    }
                    result
                };

                let route_count = entry.option_count();
                let provider_raw = route
                    .map(|r| route_provider_display(&r.provider, &r.api_method))
                    .unwrap_or_else(|| "-".to_string());
                let provider_label = if col == 0 && route_count > 1 {
                    format!("{} ({})", provider_raw, route_count)
                } else {
                    provider_raw
                };
                let pw = provider_width.saturating_sub(1);
                let provider_display =
                    format!(" {}", pad_left_display(provider_label.as_str(), pw));
                let provider_style = if unavailable {
                    Style::default().fg(rgb(80, 80, 80))
                } else if is_row_selected && col == 1 {
                    Style::default().fg(Color::White).bg(rgb(60, 60, 80)).bold()
                } else {
                    Style::default().fg(rgb(140, 180, 255))
                };

                let via_raw = route
                    .map(|r| api_method_display(&r.api_method))
                    .unwrap_or_else(|| "-".to_string());
                let vw = via_width.saturating_sub(1);
                let via_display = format!(" {}", pad_left_display(via_raw.as_str(), vw));
                let via_style = if unavailable {
                    Style::default().fg(rgb(80, 80, 80))
                } else if is_row_selected && col == 2 {
                    Style::default().fg(Color::White).bg(rgb(60, 60, 80)).bold()
                } else if is_usage_picker {
                    Style::default().fg(rgb(196, 170, 255))
                } else {
                    Style::default().fg(rgb(220, 190, 120))
                };

                if is_preview && !is_account_picker {
                    spans.push(Span::styled(provider_display, provider_style));
                    spans.extend(model_spans);
                    // Add inline effort ladder for model rows
                    if let Some(effort_ladder) = format_effort_ladder(entry, is_row_selected) {
                        spans.extend(effort_ladder);
                    }
                    spans.push(Span::styled(via_display, via_style));
                } else {
                    spans.extend(model_spans);
                    // Add inline effort ladder for model rows
                    if let Some(effort_ladder) = format_effort_ladder(entry, is_row_selected) {
                        spans.extend(effort_ladder);
                    }
                    spans.push(Span::styled(provider_display, provider_style));
                    spans.push(Span::styled(via_display, via_style));
                }

                if let Some(route) = route
                    && let Some(detail_text) = route_detail_display_text(&route.detail, unavailable)
                    && detail_width > 0
                {
                    spans.push(Span::styled(
                        format!("  {}", truncate_display(detail_text.as_str(), detail_width)),
                        if unavailable {
                            Style::default().fg(rgb(180, 120, 120)).italic()
                        } else {
                            Style::default().fg(dim_color())
                        },
                    ));
                }

                lines.push(Line::from(spans));
            }
        }
    }

    // Add footer hint line for model picker (not account picker)
    if !is_account_picker && picker.kind == crate::tui::PickerKind::Model && !lines.is_empty() {
        let footer_hint = " ←/→ effort · header ←/→ expand/collapse · Tab route · Enter select ";
        lines.push(Line::from(Span::styled(
            footer_hint,
            Style::default().fg(rgb(80, 80, 100)).italic(),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_elapsed_uses_whole_seconds_below_one_minute() {
        assert_eq!(format_elapsed(0.0), "0s");
        assert_eq!(format_elapsed(1.2), "1s");
        assert_eq!(format_elapsed(59.9), "59s");
        assert_eq!(format_elapsed(61.2), "1m 1s");
    }

    #[test]
    fn effort_ladder_shows_only_the_selected_effort_and_stays_narrow() {
        let mut picker = sample_picker();
        picker.entries[0].available_efforts = vec![
            "none".to_string(),
            "minimal".to_string(),
            "medium".to_string(),
            "high".to_string(),
            "xhigh".to_string(),
        ];
        picker.entries[0].effort = Some("medium".to_string());

        // The focused row shows the selected value and nothing else: rendering
        // every option overflowed the column and clipped the actual selection.
        let focused = effort_ladder_text(&picker.entries[0], true);
        assert!(focused.contains("med"), "focused row: {focused}");
        for other in ["none", "min", "high", "xhigh"] {
            assert!(
                !focused.contains(other),
                "focused row should not list `{other}`, got: {focused}"
            );
        }
        assert!(
            focused.chars().count() <= 12,
            "ladder must stay narrow enough to never clip, got {} chars: {focused}",
            focused.chars().count()
        );

        // Cycling to another effort updates the visible value.
        picker.entries[0].effort = Some("xhigh".to_string());
        let cycled = effort_ladder_text(&picker.entries[0], true);
        assert!(cycled.contains("xhigh"), "after cycling: {cycled}");
    }

    #[test]
    fn effort_ladder_shows_ladder_with_chevrons_on_focused_row() {
        let mut picker = sample_picker();
        picker.entries[0].available_efforts =
            vec!["none".to_string(), "low".to_string(), "medium".to_string()];
        picker.entries[0].effort = Some("low".to_string());

        let text = effort_ladder_text(&picker.entries[0], true);
        assert!(text.contains('‹'), "should have ‹ chevron, got: {text}");
        assert!(text.contains('›'), "should have › chevron, got: {text}");
    }

    /// Rendered text of an entry's effort ladder, for assertion convenience.
    fn effort_ladder_text(entry: &crate::tui::PickerEntry, focused: bool) -> String {
        format_effort_ladder(entry, focused)
            .expect("should render effort ladder")
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn fallback_route_details_are_warning_limited() {
        assert!(route_detail_is_limited(
            "https://mkp-api.fptcloud.com; fallback: static provider model list"
        ));
        assert_eq!(picker_row_marker(true, false, true), "⚠");
        assert_eq!(picker_row_marker(false, false, true), "⚠");
    }

    #[test]
    fn selected_fallback_model_shows_warning_notice() {
        let mut picker = sample_picker();
        picker.entries[0].options[0].detail =
            "https://mkp-api.fptcloud.com; fallback: static provider model list".to_string();

        let (notice, warning) =
            selected_route_notice_text(&picker, picker.entries[0].active_option())
                .expect("fallback model should show a warning notice");

        assert!(warning);
        assert!(notice.starts_with("⚠ "));
        assert!(notice.contains("fallback: static provider model list"));
    }

    fn sample_picker() -> crate::tui::InlineInteractiveState {
        crate::tui::InlineInteractiveState {
            kind: crate::tui::PickerKind::Model,
            filtered: vec![0],
            selected: 0,
            column: 0,
            filter: String::new(),
            preview: false,
            display_rows: vec![crate::tui::PickerDisplayRow::Entry { entry_index: 0 }],
            collapse_state: crate::tui::CollapseState::default(),
            entries: vec![crate::tui::PickerEntry {
                name: "gpt-5.4".to_string(),
                options: vec![crate::tui::PickerOption {
                    provider: "openai".to_string(),
                    api_method: "oauth".to_string(),
                    available: true,
                    detail: String::new(),
                    estimated_reference_cost_micros: None,
                }],
                action: crate::tui::PickerAction::Model,
                selected_option: 0,
                is_current: true,
                is_default: false,
                is_favorite: false,
                recommended: true,
                recommendation_rank: 0,
                usage_score: 0,
                old: false,
                created_date: None,
                effort: None,
                available_efforts: Vec::new(),
                provider_group: None,
                is_recent: false,
            }],
        }
    }

    fn sample_account_picker(mixed_providers: bool) -> crate::tui::InlineInteractiveState {
        let mut models = vec![crate::tui::PickerEntry {
            name: "work".to_string(),
            options: vec![crate::tui::PickerOption {
                provider: "Claude".to_string(),
                api_method: "active".to_string(),
                available: true,
                detail: String::new(),
                estimated_reference_cost_micros: None,
            }],
            action: crate::tui::PickerAction::Account(crate::tui::AccountPickerAction::Switch {
                provider_id: "claude".to_string(),
                label: "work".to_string(),
            }),
            selected_option: 0,
            is_current: true,
            is_default: false,
            is_favorite: false,
            recommended: false,
            recommendation_rank: usize::MAX,
            usage_score: 0,
            old: false,
            created_date: None,
            effort: None,
            available_efforts: Vec::new(),
            provider_group: None,
            is_recent: false,
        }];

        if mixed_providers {
            models.push(crate::tui::PickerEntry {
                name: "personal".to_string(),
                options: vec![crate::tui::PickerOption {
                    provider: "OpenAI".to_string(),
                    api_method: "saved".to_string(),
                    available: true,
                    detail: String::new(),
                    estimated_reference_cost_micros: None,
                }],
                action: crate::tui::PickerAction::Account(
                    crate::tui::AccountPickerAction::Switch {
                        provider_id: "openai".to_string(),
                        label: "personal".to_string(),
                    },
                ),
                selected_option: 0,
                is_current: false,
                is_default: false,
                is_favorite: false,
                recommended: false,
                recommendation_rank: usize::MAX,
                usage_score: 0,
                old: false,
                created_date: None,
                effort: None,
                available_efforts: Vec::new(),
                provider_group: None,
                is_recent: false,
            });
        }

        crate::tui::InlineInteractiveState {
            kind: crate::tui::PickerKind::Account,
            filtered: (0..models.len()).collect(),
            selected: 0,
            column: 0,
            filter: String::new(),
            preview: false,
            display_rows: Vec::new(),
            collapse_state: crate::tui::CollapseState::default(),
            entries: models,
        }
    }

    fn sample_agent_target_picker() -> crate::tui::InlineInteractiveState {
        crate::tui::InlineInteractiveState {
            kind: crate::tui::PickerKind::Model,
            filtered: vec![0],
            selected: 0,
            column: 0,
            filter: String::new(),
            preview: false,
            display_rows: vec![crate::tui::PickerDisplayRow::Entry { entry_index: 0 }],
            collapse_state: crate::tui::CollapseState::default(),
            entries: vec![crate::tui::PickerEntry {
                name: "Swarm / subagent".to_string(),
                options: vec![crate::tui::PickerOption {
                    provider: "gpt-5 default".to_string(),
                    api_method: "agents.swarm_model".to_string(),
                    available: true,
                    detail: "/agents swarm".to_string(),
                    estimated_reference_cost_micros: None,
                }],
                action: crate::tui::PickerAction::AgentTarget(crate::tui::AgentModelTarget::Swarm),
                selected_option: 0,
                is_current: false,
                is_default: false,
                is_favorite: false,
                recommended: false,
                recommendation_rank: usize::MAX,
                usage_score: 0,
                old: false,
                created_date: None,
                effort: None,
                available_efforts: Vec::new(),
                provider_group: None,
                is_recent: false,
            }],
        }
    }

    #[test]
    fn picker_row_marker_uses_explicit_unavailable_marker() {
        assert_eq!(picker_row_marker(true, true, false), "×");
        assert_eq!(picker_row_marker(false, true, false), "×");
        // Limited routes keep their warning marker even when selected, so the
        // fallback/limited signal never disappears while navigating.
        assert_eq!(picker_row_marker(true, false, true), "⚠");
        assert_eq!(picker_row_marker(false, false, true), "⚠");
        assert_eq!(picker_row_marker(true, false, false), "▸");
        assert_eq!(picker_row_marker(false, false, false), " ");
    }

    #[test]
    fn route_detail_display_text_prefixes_unavailable_reason() {
        assert_eq!(
            route_detail_display_text("credentials expired", true).as_deref(),
            Some("unavailable · credentials expired")
        );
        assert_eq!(
            route_detail_display_text("no matching configured provider route", true).as_deref(),
            Some("unavailable · no matching configured provider route")
        );
        assert_eq!(
            route_detail_display_text("", true).as_deref(),
            Some("unavailable")
        );
        assert_eq!(route_detail_display_text("", false), None);
        assert_eq!(
            route_detail_display_text("catalog still loading", false).as_deref(),
            Some("catalog still loading")
        );
    }

    #[test]
    fn selected_model_route_notice_explains_unavailable_and_limited_routes() {
        let mut picker = sample_picker();
        picker.entries[0].options[0].available = false;
        picker.entries[0].options[0].detail = "legacy Bedrock model".to_string();
        let notice = selected_route_notice_text(&picker, picker.entries[0].active_option());
        assert_eq!(
            notice
                .as_ref()
                .map(|(text, warning)| (text.as_str(), *warning)),
            Some(("× unavailable · legacy Bedrock model", true))
        );

        picker.entries[0].options[0].available = true;
        picker.entries[0].options[0].detail = "ConverseStream · no tools".to_string();
        let notice = selected_route_notice_text(&picker, picker.entries[0].active_option());
        assert_eq!(
            notice
                .as_ref()
                .map(|(text, warning)| (text.as_str(), *warning)),
            Some(("⚠ ConverseStream · no tools", true))
        );
    }

    /// The METHOD column must survive the inline effort ladder.
    ///
    /// The ladder is rendered between the model and provider columns but was
    /// not counted in the width budget, so it pushed every later column right
    /// and clipped the last one. With a real catalog that rendered "api key"
    /// as "ap", which reads like the row *is* an api-key route: the exact
    /// symptom that made a working OAuth login look absent from `/model`.
    #[test]
    fn effort_ladder_does_not_squeeze_out_the_method_column() {
        let mut picker = sample_picker();
        let without_ladder = picker_render_width(&picker, 200);

        picker.entries[0].effort = Some("high".to_string());
        picker.entries[0].available_efforts =
            vec!["low".to_string(), "high".to_string(), "max".to_string()];
        let with_ladder = picker_render_width(&picker, 200);

        let ladder = effort_ladder_display_width(&picker.entries[0]);
        assert!(
            ladder > 0,
            "a model with efforts must reserve ladder columns"
        );
        assert_eq!(
            with_ladder,
            without_ladder + ladder,
            "the ladder must widen the row rather than steal from METHOD"
        );
    }

    /// Rows without effort support must not pay for a ladder they never draw.
    #[test]
    fn models_without_efforts_reserve_no_ladder_width() {
        let picker = sample_picker();
        assert_eq!(effort_ladder_display_width(&picker.entries[0]), 0);
    }

    /// Reserving ladder columns must not let the row overflow a narrow
    /// terminal. The budget shrinks the provider/method columns to their
    /// floors instead, so a small window still renders a usable picker rather
    /// than a row wider than the screen.
    #[test]
    fn ladder_reservation_still_fits_a_narrow_terminal() {
        let mut picker = sample_picker();
        picker.entries[0].effort = Some("high".to_string());
        picker.entries[0].available_efforts = vec!["high".to_string()];

        for max_width in [24usize, 32, 40, 60, 80, 120] {
            let w = picker_render_width(&picker, max_width);
            assert!(
                w <= max_width,
                "picker width {w} exceeds the {max_width}-column budget"
            );
        }
    }

    #[test]
    fn picker_render_width_uses_intrinsic_content_width() {
        let picker = sample_picker();
        let width = picker_render_width(&picker, 120);
        assert!(
            width < 120,
            "model picker should fit content, not fill the window"
        );
        // Content-fit: marker + the widths of the model/provider/via columns.
        // Each column is at least as wide as its header label, then grows to
        // fit the widest row (the sample entry: "gpt-5.4 ★" / "openai" / OAuth).
        let model = display_width(picker_entry_display_name(&picker.entries[0]).as_str())
            .max(display_width(picker.primary_label()));
        let provider =
            display_width("openai").max(display_width(picker.secondary_label(false))) + 1;
        let via = display_width(api_method_display("oauth").as_str())
            .max(display_width(picker.tertiary_label()))
            + 1;
        assert_eq!(
            width,
            3 + model + provider + via,
            "model picker width should equal its content-fit column widths"
        );
    }

    #[test]
    fn picker_render_area_centers_in_centered_mode() {
        let picker = sample_picker();
        let width = picker_render_width(&picker, 80) as u16;
        let area = Rect::new(5, 3, 80, 2);
        let horizontal_offset = area.width.saturating_sub(width) / 2;
        let render_area = Rect {
            x: area.x + horizontal_offset,
            y: area.y,
            width,
            height: area.height,
        };

        assert!(
            render_area.x > area.x,
            "content-fit picker should center when possible"
        );
        assert_eq!(render_area.width, width);
    }

    #[test]
    fn model_picker_method_display_uses_user_friendly_labels() {
        assert_eq!(api_method_display("openai-oauth"), "oauth");
        assert_eq!(api_method_display("openai-api-key"), "api key");
        assert_eq!(api_method_display("openai-compatible:comtegra"), "api key");
    }

    #[test]
    fn picker_entry_display_name_labels_recently_added_models_as_new() {
        let mut picker = sample_picker();
        let entry = &mut picker.entries[0];
        entry.is_current = false;
        entry.options[0].detail = "recently added · https://llm.comtegra.cloud/v1".to_string();

        assert!(picker_entry_display_name(entry).contains(" new"));
    }

    #[test]
    fn picker_entry_display_name_labels_recommended_even_when_current() {
        let mut picker = sample_picker();
        let entry = &mut picker.entries[0];
        entry.is_current = true;
        entry.recommended = true;

        assert!(picker_entry_display_name(entry).contains("★"));
    }

    #[test]
    fn picker_entry_display_name_labels_default_models_explicitly() {
        let mut picker = sample_picker();
        let entry = &mut picker.entries[0];
        entry.is_default = true;

        assert!(picker_entry_display_name(entry).contains(" default"));
    }

    #[test]
    fn model_picker_shows_default_shortcut_hint() {
        let picker = sample_picker();

        assert!(picker.shows_default_shortcut_hint());
    }

    #[test]
    fn model_picker_keybind_hint_mentions_default_and_favorites() {
        let picker = sample_picker();
        let hint = model_picker_top_hint(&picker).expect("active model picker should show hint");

        assert!(hint.contains("Ctrl+O set default"));
        assert!(hint.contains("Ctrl+N favorite"));
    }

    #[test]
    fn swarm_agent_model_picker_permanently_links_to_swarm_prompt_command() {
        let mut picker = sample_picker();
        for entry in &mut picker.entries {
            entry.action = crate::tui::PickerAction::AgentModelChoice {
                target: crate::tui::AgentModelTarget::Swarm,
                clear_override: false,
            };
        }

        let hint = model_picker_top_hint(&picker).expect("swarm picker should show prompt hint");
        assert!(hint.contains("/swarm-prompt"));
        assert!(hint.contains("configured by a prompt"));
    }

    #[test]
    fn picker_entry_display_name_prettifies_known_model_families() {
        let mut picker = sample_picker();
        let entry = &mut picker.entries[0];
        entry.recommended = false;
        entry.is_current = false;
        entry.name = "claude-opus-4-8".to_string();
        assert_eq!(picker_entry_display_name(entry), "Claude Opus 4.8");

        entry.name = "gpt-5.5 (high)".to_string();
        entry.effort = Some("high".to_string());
        assert_eq!(picker_entry_display_name(entry), "GPT-5.5 (high)");
    }

    #[test]
    fn picker_entry_display_name_keeps_unknown_and_namespaced_ids_verbatim() {
        let mut picker = sample_picker();
        let entry = &mut picker.entries[0];
        entry.recommended = false;
        entry.is_current = false;
        for raw in [
            "deepseek-ai/DeepSeek-V3",
            "qwen3-coder-plus",
            "openai/gpt-5.5",
            "gpt-oss-120b",
            "GLM-5.1",
        ] {
            entry.name = raw.to_string();
            assert_eq!(picker_entry_display_name(entry), raw);
        }
    }

    #[test]
    fn picker_entry_display_name_labels_favorites() {
        let mut picker = sample_picker();
        let entry = &mut picker.entries[0];
        entry.is_favorite = true;
        entry.recommended = true;

        assert!(picker_entry_display_name(entry).contains("♥"));
    }

    #[test]
    fn account_picker_width_uses_compact_two_column_layout() {
        let picker = sample_account_picker(true);
        let width = picker_render_width(&picker, 120);
        assert!(width < 60, "account picker should stay compact");
        assert!(
            width >= 18,
            "account picker should still fit title and state"
        );
    }

    #[test]
    fn account_picker_only_shows_provider_badges_when_needed() {
        let mixed = sample_account_picker(true);
        let single = sample_account_picker(false);

        assert!(account_picker_shows_provider_badge(&mixed));
        assert!(!account_picker_shows_provider_badge(&single));

        let (mixed_title, _) = account_picker_entry_title(&mixed.entries[0], true);
        let (single_title, _) = account_picker_entry_title(&single.entries[0], false);
        assert!(mixed_title.starts_with("Claude · "));
        assert_eq!(single_title, "work");
    }

    #[test]
    fn agent_target_picker_uses_specific_column_labels() {
        let picker = sample_agent_target_picker();

        assert!(picker.is_agent_target_picker());
        assert_eq!(picker.primary_label(), "TARGET");
        assert_eq!(picker.secondary_label(false), "MODEL");
        assert_eq!(picker.tertiary_label(), "CONFIG");
        assert!(!picker.shows_default_shortcut_hint());
    }
}
