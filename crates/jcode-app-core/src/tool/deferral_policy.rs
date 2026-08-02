//! Which tool schemas are withheld from the default request payload.
//!
//! Split out of `tool/mod.rs`, which is at the oversized-file budget.

/// Tools whose full schema is withheld by default and fetched via `load_tools`
/// on demand, because they are almost never used but are expensive to ship.
///
/// Distinct from the all-or-nothing deferral gated on a small context window:
/// this trims the tail on every window size, so a roomy model still stops
/// paying for tools it will not call.
///
/// The list is measured, not guessed. Across 789 real sessions and 32,159 tool
/// calls, every entry appeared in under 3% of sessions, and together they cost
/// ~5.4k of the ~14.3k token tool payload. Only 6.8% of those sessions ever
/// touched one, so the discovery round-trip they now cost is rare, while the
/// saving applies to every request.
///
/// Entries stay fully callable: `load_tools` expands the schema, and the model
/// still sees each name and one-line summary in the deferred index.
///
/// Removing an entry (making it inline again) is always safe. Adding one trades
/// tokens for a possible round-trip, so it should be backed by the same kind of
/// usage measurement.
pub const RARELY_USED_DEFERRED_TOOLS: &[&str] = &[
    "browser",
    "cheap_route",
    "conversation_search",
    "discover_tools",
    "gmail",
    "initiative",
    "invalid",
    "macos_computer_use",
    "memory",
    "open",
    "patch",
    "schedule",
    "session_search",
    "side_panel",
    "skill_manage",
];

/// List every tool that is missing from `tools` in the `load_tools` description.
///
/// Attached whenever anything is actually withheld, not only in full-deferral
/// mode: the rarely-used trim withholds tools on every window size, and a tool
/// the model cannot see is a lost capability rather than a deferred one.
pub fn advertise_deferred_tools(
    tools: &mut [crate::message::ToolDefinition],
    index: Vec<(String, String)>,
) {
    let inline: std::collections::HashSet<&str> = tools.iter().map(|d| d.name.as_str()).collect();
    let missing: Vec<(String, String)> = index
        .into_iter()
        .filter(|(name, _)| !inline.contains(name.as_str()))
        .collect();
    if missing.is_empty() {
        return;
    }
    let Some(load_tools) = tools.iter_mut().find(|d| d.name == "load_tools") else {
        return;
    };
    let mut desc = load_tools.description.clone();
    desc.push_str("\n\nDeferred tools available to load:\n");
    for (name, summary) in &missing {
        desc.push_str(&format!("- {name} — {summary}\n"));
    }
    load_tools.description = desc;
}
