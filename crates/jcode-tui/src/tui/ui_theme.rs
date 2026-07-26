pub(super) use jcode_tui_style::theme::{
    accent_color, ai_color, ai_text, asap_color, blend_color, dim_color, file_link_color,
    header_icon_color, header_name_color, header_session_color, pending_color,
    prompt_entry_bg_color, prompt_entry_color, prompt_entry_shimmer_color, queued_color,
    system_message_color, tool_color, user_bg, user_color, user_text,
};
use ratatui::prelude::*;

pub(super) fn activity_indicator_frame_index(elapsed: f32, fps: f32) -> usize {
    let policy = crate::perf::tui_policy();
    jcode_tui_style::theme::activity_indicator_frame_index(
        elapsed,
        fps,
        policy.enable_decorative_animations,
        policy.no_animation,
    )
}

pub(super) fn activity_indicator(elapsed: f32, fps: f32) -> &'static str {
    let policy = crate::perf::tui_policy();
    jcode_tui_style::theme::activity_indicator(
        elapsed,
        fps,
        policy.enable_decorative_animations,
        policy.no_animation,
    )
}
