use super::*;

pub(super) use jcode_tui_messages::{centered_wrap_width, left_pad_lines_for_centered_mode};

pub(crate) fn get_cached_message_lines<F>(
    msg: &DisplayMessage,
    width: u16,
    diff_mode: crate::config::DiffDisplayMode,
    render: F,
) -> Vec<Line<'static>>
where
    F: FnOnce(&DisplayMessage, u16, crate::config::DiffDisplayMode) -> Vec<Line<'static>>,
{
    // An in-flight tool row renders an animated spinner, but the message cache
    // is keyed on message content, which does not change while the tool runs.
    // Caching it would pin one spinner frame forever and the row would look
    // frozen. There is at most a handful of running rows at a time, and the row
    // is a single line, so rendering it uncached is cheap. Once the result
    // lands, `content` changes and the row caches normally again.
    if super::messages::tool_message_is_running(msg) {
        return render(msg, width, diff_mode);
    }

    jcode_tui_messages::get_cached_message_lines(
        msg,
        width,
        diff_mode,
        jcode_tui_messages::MessageCacheContext {
            diagram_mode: crate::config::config().display.diagram_mode,
            centered: markdown::center_code_blocks(),
            mermaid_epoch: crate::tui::mermaid::deferred_render_epoch(),
            mermaid_aspect_bucket: crate::tui::mermaid::current_preferred_aspect_ratio_bucket(),
            show_agentgrep_output: crate::config::config().display.show_agentgrep_output,
            show_bash_output: crate::config::config().display.show_bash_output,
            tool_call_details: crate::config::config().display.tool_call_details,
        },
        render,
    )
}
