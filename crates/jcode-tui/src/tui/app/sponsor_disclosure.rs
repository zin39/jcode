//! Sponsored discovery disclosure line.
//!
//! Whenever a session first uses the `discover_tools` tool, we render a
//! persistent `(sponsored discovery)` system line in the transcript. This is
//! a disclosure, not a teaching hint: unlike `swarm_hint` it has no lifetime
//! cap and fires once per session, every session, as long as sponsored
//! discovery is used. The line links to the public policy page so users can
//! read what sponsored discovery means.

use super::{App, DisplayMessage};

/// Tool name that triggers the disclosure.
pub(super) const DISCOVERY_TOOL_NAME: &str = "discover_tools";

/// The disclosure line shown in the transcript.
pub(super) fn disclosure_message() -> String {
    format!(
        "{} {}",
        crate::sponsors::SPONSORED_DISCOVERY_TAG,
        crate::sponsors::SPONSORED_DISCOVERY_NOTICE
    )
}

/// Pure decision: disclose on first sponsored-discovery use in a session.
pub(super) fn should_disclose(shown_this_session: bool) -> bool {
    !shown_this_session
}

impl App {
    /// Surface the sponsored-discovery disclosure the first time this session
    /// uses `discover_tools`. Persistent system message, once per session,
    /// no lifetime cap: disclosure must fire every session that uses
    /// discovery.
    pub(in crate::tui::app) fn maybe_surface_sponsor_disclosure(&mut self, tool_name: &str) {
        if tool_name != DISCOVERY_TOOL_NAME {
            return;
        }
        if !should_disclose(self.sponsor_disclosure_shown_this_session) {
            return;
        }
        self.sponsor_disclosure_shown_this_session = true;
        self.push_display_message(DisplayMessage {
            role: "system".to_string(),
            content: disclosure_message(),
            tool_calls: vec![],
            duration_secs: None,
            title: None,
            tool_data: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discloses_once_per_session() {
        assert!(should_disclose(false));
        assert!(!should_disclose(true));
    }

    #[test]
    fn disclosure_names_the_policy_and_links_it() {
        let message = disclosure_message();
        assert!(message.contains("(sponsored discovery)"));
        assert!(message.contains("never recommended"));
        assert!(message.contains("solosystems.dev/sponsored-discovery"));
    }
}
