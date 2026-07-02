# Live Subagent Visibility (local TUI)

**Date:** 2026-07-02
**Status:** approved scope A (local TUI only)

## Problem

When the agent spawns a subagent via the `subagent` tool, the user sees only a
spinner until the child finishes. The child *does* publish live
`BusEvent::SubagentStatus` events ("calling API", "running grep", "streaming" —
`agent/turn_loops.rs:81,141,952`), and the TUI *does* render such events in the
status line (`tui/app/turn.rs:1347`, rendered via `info_widget.rs`), but the
child publishes under its **own** session id while the TUI filters for the
**parent** session id. The events drop silently. The code comment "Publish
status for TUI to show during Task execution" indicates this visibility was
intended and regressed when subagents moved to isolated sessions.

## Design (approach A — rebroadcast under parent id)

In `SubagentTool::execute` (`crates/jcode-app-core/src/tool/task.rs`), the
existing bus listener already subscribes and filters events by the child
session id to collect tool summaries. Extend it:

- On `BusEvent::SubagentStatus` matching the child session id, republish the
  event as `SubagentStatus { session_id: <parent>, status: "<desc>: <status>",
  model }` via `Bus::global()`.
- Label = the subagent's `description` param, truncated to 40 chars.
- Formatting lives in a pure helper `forward_subagent_status(parent_id, desc,
  status) -> SubagentStatus` so it is unit-testable.

No TUI changes: the existing status line starts receiving events again.
No protocol/server changes: local TUI only (remote/desktop forwarding would
touch files dirty on the current branch; deferred).

## Loop safety

The republished event carries the parent session id; the listener only reacts
to events carrying the child session id, so no feedback loop. Nested subagents
cannot occur (the `subagent` tool is removed from the child's allowed set).

## Lifecycle

- While child runs: parent status line shows `desc: running grep · <model>`.
- After child completes: the parent's own next `SubagentStatus` ("calling
  API") naturally overwrites the line; TUI clears it at turn end
  (`tui/app/local.rs:345`).

## Testing

- Unit tests for `forward_subagent_status`: label prefix, truncation, model
  passthrough, parent session id.
- Existing task tool tests must stay green: `cargo test -p jcode-app-core task`.
