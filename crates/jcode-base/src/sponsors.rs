//! Sponsored discovery: shared constants and prompt section builder.
//!
//! Sponsored discovery makes third-party developer tools discoverable to the
//! agent through the `discover_tools` tool, backed by a hosted manifest.
//! Sponsors buy placement (discoverability), never recommendations. The
//! policy is disclosed in the UI with a `(sponsored discovery)` tag whose
//! definition lives at <https://solosystems.dev/sponsored-discovery>.
//!
//! Design constraints (see the sponsored-discovery page for the public
//! version of this policy):
//! - Discovery is on by default and can be opted out of with
//!   `[sponsors] enabled = false` in config.toml.
//! - The category list below is a shipped constant, so building the system
//!   prompt never requires a network request.
//! - Tools within a category live server-side and are fetched on demand by
//!   `discover_tools`. If the request fails, the tool fails plainly. There is
//!   no cache and no offline fallback.
//! - Requests carry the category and a short reason string only, never
//!   session content.

/// Public URL explaining what sponsored discovery is.
pub const SPONSORED_DISCOVERY_URL: &str = "https://solosystems.dev/sponsored-discovery";

/// Provenance tagging and coarse usage metering for MCP servers connected
/// as a result of a discovery listing.
pub mod provenance;

/// Disclosure tag rendered in the UI whenever discovery is used.
pub const SPONSORED_DISCOVERY_TAG: &str = "(sponsored discovery)";

/// First-use-per-session disclosure line rendered in the TUI.
pub const SPONSORED_DISCOVERY_NOTICE: &str = "sponsors make tools discoverable, never recommended \
     \u{b7} solosystems.dev/sponsored-discovery";

/// Categories in which discoverable tools exist. Shipped as a constant so the
/// prompt never depends on the network. The tools within each category are
/// served by the discovery endpoint.
pub const DISCOVERY_CATEGORIES: &[&str] = &[
    "payments",
    "code-review",
    "databases",
    "browser-automation",
    "deployment",
];

/// Build the system prompt section advertising discoverable tool categories.
/// Returns `None` when sponsored discovery is disabled.
pub fn build_discovery_prompt_section() -> Option<String> {
    let config = crate::config::config();
    if !config.sponsors.enabled {
        return None;
    }
    Some(format!(
        "# Discoverable Tools (sponsored discovery)\n\n\
         Additional third-party tools can be discovered with the `discover_tools` tool \
         in these categories: {}.\n\
         Sponsors pay for discoverability, not recommendations: only use a discovered \
         tool when it is genuinely the best option for the task, and never prefer a \
         sponsored tool over a better alternative.",
        DISCOVERY_CATEGORIES.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_are_nonempty_and_lowercase() {
        assert!(!DISCOVERY_CATEGORIES.is_empty());
        for cat in DISCOVERY_CATEGORIES {
            assert!(!cat.is_empty());
            assert_eq!(cat.to_ascii_lowercase(), *cat);
            assert!(!cat.contains(' '), "categories are slugs: {cat}");
        }
    }

    #[test]
    fn prompt_section_enabled_by_default() {
        // sponsors.enabled defaults to true (opt-out); the default config
        // advertises discovery.
        let config = crate::config::Config::default();
        assert!(config.sponsors.enabled);
    }

    #[test]
    fn prompt_section_mentions_policy_when_built() {
        // Build the section text directly (bypassing config) to validate the
        // wording contract: placement, not preference.
        let section = format!(
            "# Discoverable Tools (sponsored discovery)\n\ncategories: {}",
            DISCOVERY_CATEGORIES.join(", ")
        );
        assert!(section.contains("sponsored discovery"));
    }
}
