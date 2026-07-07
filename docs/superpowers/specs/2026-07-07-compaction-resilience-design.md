# Compaction Resilience: Task-State File + Tool-Result Clearing

**Date:** 2026-07-07
**Problem:** Long sessions compact and the agent forgets in-flight work (plan, progress, decisions). Users report "difficult to work with" after compaction.
**Provenance:** last30days research 2026-07-07 (`~/Documents/Last30Days/coding-agent-context-compaction-and-long-session-memory-raw-v3.md`). Field consensus: (a) disk-persisted state re-injected after compaction survives, in-window state dies; (b) tool-result clearing is the safest first-stage compaction (Anthropic first-party strategy); (c) compaction should be reversible — keep originals retrievable. Continues the context-loop roadmap: this is the C2 slice (reversible/externalized compaction).

## Feature 1: Task-state file that survives compaction

- New file per session: `~/.jcode/sessions/<session_id>.task-state.md` (sibling of session snapshot).
- New core tool `update_task_state` (always-available schema, like `load_tools`): full-replace write of the file, capped at 8 KB (truncated with marker beyond).
- Injection: every turn, if the file is non-empty, its content is appended to the **dynamic part** of the split system prompt (`build_system_prompt_split`) under a `# Task State` header. Dynamic part is already uncached, so no cache-invalidation cost.
- Because the file lives on disk and is re-read each turn, it survives compaction, hard compaction, and session resume by construction.
- Model guidance lives in the tool description: maintain plan/progress/decisions for multi-step tasks; update when a sub-task completes or a decision is made.

## Feature 2: Tool-result clearing before summarization (stage-1 compaction)

- At the 80% soft threshold (`COMPACTION_THRESHOLD`), before spawning summary generation, first **clear old tool-result payloads** from the provider-visible view: every `ContentBlock::ToolResult` in messages older than the last `RECENT_TURNS_TO_KEEP` (10) messages is replaced with a short elision marker. If the original content contains a spill pointer line ("FULL output saved to …"), that line is preserved in the marker so the agent can still retrieve the full output.
- If clearing brings estimated usage back under the threshold, summarization is skipped this round (new `CompactionAction::ToolResultsCleared { cleared }`).
- **Reversible:** stored session history is never mutated. Clearing is applied at API-view assembly time (`messages_for_api_with`) using a watermark `tool_cleared_up_to` (absolute message index) held in `CompactionManager` and persisted in `StoredCompactionState` (new optional field, serde-default for backward compat). TUI rendering and session files keep full content; spilled outputs remain on disk.
- Cache note: raising the watermark rewrites mid-history content and invalidates the provider cache from that point — same cost class as compaction itself, and it only happens when compaction would otherwise fire. Not a violation regression.
- Kill switch: `JCODE_DISABLE_TOOL_RESULT_CLEARING=1` env var disables stage-1 (falls straight through to today's behavior).

## Out of scope (later slices)

- Model-decided compaction timing (SelfCompact rubric) — after this lands.
- Anthropic server-side compaction beta (`compact-2026-01-12`).
- Retrieval tool over compacted-away history.

## Verification

- Unit: compaction-core clearing fn (marker format, spill-pointer preservation, keep-window respected, idempotence); task-state store cap + round-trip.
- Integration: manager test — usage above threshold, clearing applied, summarization skipped when under threshold after clearing; watermark persists across save/load.
- Manual smoke: long session, force compaction, confirm task-state re-injected and old tool payloads elided in provider view only.
