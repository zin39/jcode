//! Tests for which side-panel page is focused: when a snapshot may reveal a
//! hidden panel, and which page reopening restores.
//!
//! Kept beside the code rather than inside it because both host files are
//! already over the size ratchet.

#[cfg(test)]
mod side_panel_restore_tests {
    use super::super::side_panel_page_to_restore;
    use crate::side_panel::SidePanelPage;

    fn page(id: &str, updated_at_ms: u64) -> SidePanelPage {
        SidePanelPage {
            id: id.to_string(),
            title: id.to_string(),
            file_path: String::new(),
            format: Default::default(),
            source: Default::default(),
            content: String::new(),
            updated_at_ms,
        }
    }

    /// Reopening the panel after a cheap_route run showed the STALE page: the
    /// remembered id won even when a different page had just been written. The
    /// freshest page must win, or the live view looks broken.
    #[test]
    fn reopening_focuses_the_freshest_page_not_the_remembered_one() {
        let pages = vec![page("websearch", 100), page("debate", 900)];

        assert_eq!(
            side_panel_page_to_restore(&pages, Some("websearch")).as_deref(),
            Some("debate"),
            "a page written after the remembered one must win"
        );

        // Order in the vec must not matter; only the timestamp.
        let reversed = vec![page("debate", 900), page("websearch", 100)];
        assert_eq!(
            side_panel_page_to_restore(&reversed, Some("websearch")).as_deref(),
            Some("debate")
        );
    }

    /// Without timestamps there is nothing to compare, so the previous
    /// behaviour (remembered page, else first) must still hold.
    #[test]
    fn falls_back_to_remembered_then_first_when_untimestamped() {
        let pages = vec![page("a", 0), page("b", 0)];
        assert_eq!(
            side_panel_page_to_restore(&pages, Some("b")).as_deref(),
            Some("b"),
            "untimestamped pages should honour the remembered id"
        );
        assert_eq!(
            side_panel_page_to_restore(&pages, None).as_deref(),
            Some("a"),
            "with no memory and no timestamps, fall back to the first page"
        );
        // A remembered id that no longer exists must not be resurrected.
        assert_eq!(
            side_panel_page_to_restore(&pages, Some("deleted")).as_deref(),
            Some("a")
        );
        // No pages at all: nothing to focus.
        assert_eq!(side_panel_page_to_restore(&[], Some("x")), None);
    }
}

#[cfg(test)]
mod side_panel_autoopen_tests {
    use super::super::side_panel_focus_is_new_page;
    use crate::side_panel::SidePanelPage;

    fn page(id: &str) -> SidePanelPage {
        SidePanelPage {
            id: id.to_string(),
            title: id.to_string(),
            file_path: String::new(),
            format: Default::default(),
            source: Default::default(),
            content: String::new(),
            updated_at_ms: 1,
        }
    }

    /// Closing the panel once used to disable auto-open for the whole session,
    /// so a cheap_route run's live view never appeared again. A page the UI has
    /// never shown is new information the user has not dismissed, and must be
    /// allowed to reveal the panel.
    #[test]
    fn a_never_seen_page_may_reveal_a_hidden_panel() {
        let known = vec![page("websearch")];
        assert!(
            side_panel_focus_is_new_page(Some("debate"), &known),
            "a cheap_route page the UI has never shown must be able to open the panel"
        );
    }

    /// The sticky hide still has to work, or a panel the user deliberately
    /// closed would pop back on every routine refresh.
    #[test]
    fn refreshing_an_already_known_page_does_not_reveal() {
        let known = vec![page("websearch"), page("debate")];
        assert!(
            !side_panel_focus_is_new_page(Some("debate"), &known),
            "re-focusing a page the user already dismissed must NOT reopen the panel"
        );
        assert!(
            !side_panel_focus_is_new_page(None, &known),
            "a snapshot with no focus must never count as a reveal"
        );
    }
}
