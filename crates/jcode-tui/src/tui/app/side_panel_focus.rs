//! Which side-panel page is focused: whether an incoming snapshot may reveal
//! a hidden panel, and which page reopening restores.
//!
//! Split out of navigation.rs and state_ui.rs, which are both already over
//! the code-size ratchet, and kept pure so both decisions are testable
//! without a live TUI.

/// Which side-panel page to focus when the user reopens the panel.
///
/// Prefers the page updated most recently over the one the user last looked at.
/// Restoring the remembered page meant that opening the panel after a
/// cheap_route or web-search run showed a STALE page while the live one sat
/// unfocused, which reads as "the panel is broken". Pages without a timestamp
/// (0) cannot be compared, so they fall through to the remembered id and then
/// to the first page.
pub(crate) fn side_panel_page_to_restore(
    pages: &[crate::side_panel::SidePanelPage],
    remembered_id: Option<&str>,
) -> Option<String> {
    let freshest = pages
        .iter()
        .filter(|page| page.updated_at_ms > 0)
        .max_by_key(|page| page.updated_at_ms)
        .map(|page| page.id.clone());
    let remembered = remembered_id
        .filter(|id| pages.iter().any(|page| page.id == *id))
        .map(str::to_owned);
    freshest
        .or(remembered)
        .or_else(|| pages.first().map(|page| page.id.clone()))
}

/// Whether an incoming snapshot focuses a page the UI has never shown.
///
/// A user-hidden panel must stay hidden for routine refreshes, but a brand new
/// page is information the user has not dismissed yet. Closing the panel once
/// otherwise disabled auto-open for the rest of the session, so a cheap_route
/// run's live view never appeared.
pub(crate) fn side_panel_focus_is_new_page(
    focused_id: Option<&str>,
    known_pages: &[crate::side_panel::SidePanelPage],
) -> bool {
    focused_id.is_some_and(|id| !known_pages.iter().any(|page| page.id == id))
}

#[cfg(test)]
#[path = "side_panel_focus_tests.rs"]
mod side_panel_focus_tests;
