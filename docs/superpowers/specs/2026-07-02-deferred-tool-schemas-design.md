# Deferred Tool Schemas (opt-in)

**Date:** 2026-07-02
**Status:** approved (efficiency roadmap item #1)

## Problem

Every request serializes full schemas for all registered tools (30-55 tools ≈
20-50KB JSON ≈ ~10k tokens, the bulk of SYSTEM_OVERHEAD_TOKENS=18k).
`Registry.definitions()` → `build_filtered_tool_definitions()`
(`agent/turn_execution.rs:357`) → `locked_tools` snapshot (`agent.rs:217`) →
provider. Industry pattern (Anthropic Tool Search): send names only, fetch
schemas on demand — ~85-95% tool-context reduction and measured accuracy
gains from less schema noise.

## Design

Opt-in via `[tools] deferred = true` (config.rs `ToolConfig`), default false.

1. **Core set always full-schema** — the high-frequency tools:
   `bash, read, write, edit, multiedit, ls, glob, grep, todo, subagent,
   load_tools`. Constant `CORE_FULL_SCHEMA_TOOLS` in `tool/mod.rs`.
2. **New meta-tool `load_tools`** (`tool/load_tools.rs`):
   - schema: `{ names: [string] }` (required).
   - Its *description* embeds the deferred-tool index: one line per deferred
     tool, `name — first sentence of description` (~1k tokens for 45 tools,
     replacing ~9k of schemas).
   - `execute`: validates names against the registry, records them in the
     session's expanded set, unlocks the tool snapshot, returns
     "loaded: <names> — schemas available from the next model call".
3. **Session expanded set** — extend the existing `SessionToolPolicy`
   (`tool/mod.rs:61`) with `expanded_tools: HashSet<String>` + helper
   `expand_session_tools(session_id, names)`.
4. **Filtering** — in `build_filtered_tool_definitions()`: when deferred mode
   is on, drop definitions not in `CORE_FULL_SCHEMA_TOOLS ∪ expanded ∪
   {mcp tools already expanded}`. MCP tools are deferred like the rest (their
   schemas are the largest).
5. **Cache interplay** — expansion invalidates the `locked_tools` snapshot
   (reuse the `mcp_late_register_resolved` unlock pattern, `agent.rs:228`):
   one prompt-cache re-write per expansion, then stable again. Net win as long
   as expansions are rare relative to turns, which matches real usage.
6. **Subagents** inherit the parent's mode; their allowed-set filtering is
   unchanged.

## Non-goals

Provider-side "tool search" APIs; changing OAuth mode's hardcoded 9-tool set;
auto-eviction of expanded tools.

## Files

- `crates/jcode-base/src/config.rs` — `deferred: bool` on ToolConfig.
- `crates/jcode-app-core/src/tool/mod.rs` — CORE set, policy extension,
  registry index helper.
- `crates/jcode-app-core/src/tool/load_tools.rs` — new meta-tool + tests.
- `crates/jcode-app-core/src/agent/turn_execution.rs` — deferred filter.
- `crates/jcode-app-core/src/agent.rs` — snapshot unlock on expansion.

## Testing

- Unit: filter drops non-core defs when deferred on; keeps all when off;
  load_tools execute expands policy + unlocks; index lists exactly the
  deferred tools; unknown name → error listing valid names.
- Token proof: test asserting serialized tools JSON with deferred on is
  <30% of the full serialization for the base registry.
