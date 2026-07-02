# P0: Context/Loop Reliability Floor — Design

## Context
Deep research (run `wf_6f33f07d-7c4`, 22 sources, 24 adversarially-verified claims) into SOTA 2024–2026 context + loop engineering produced a roadmap (`~/.claude/plans/okay-this-is-our-concurrent-quokka.md`). The **measurement floor** (E1) plus the **strongest single quality lever** (C1, error excision) are the P0 slice: small, high-confidence, and a prerequisite for measuring every later adaptation.

**Why now:** SWE-Bench Pro trajectory analysis (arXiv 2509.16941) names the dominant agent failure modes concretely — for Sonnet 4: context-overflow 35.6%, endless-file-read 17%, stuck-in-loop 33.9%. jcode cannot currently *count* any of these. Separately, the "self-conditioning" result (arXiv 2509.09677) shows keeping a model's own errors in context degrades subsequent steps, and scale does not fix it — yet jcode's compaction ignores the `is_error` flag that already exists on every tool result.

## Scope (3 items, one slice)
- **E1** — first-class failure-mode metrics.
- **C1** — error-aware compaction (drop failed-attempt payloads from summarization input only).
- **C4** — flip cache-violation tracking always-on + expose count (shrunk: append-only is already proven by tests).

Explicitly **out of scope** (later P1/P2): reversible compaction (C2), reflection buffer (L1), semantic retention ranking (C3), verify→evolve loop (L2), fan-out gating (O2).

## Current-state findings (from repo scout)
- `ContentBlock::ToolResult { is_error: Option<bool> }` exists (`jcode-message-types/src/lib.rs:165`) and is **ignored by all compaction code** (zero refs in `compaction.rs`).
- Compaction summarization text is built in `jcode-compaction-core/src/lib.rs:154` `build_compaction_conversation_text` — the `ToolResult` arm (~180) emits `[Result: …]` with no error check.
- Cutoff/selection: `jcode-base/src/compaction.rs:871` (recency keep-last-10 / semantic); `messages_to_summarize` cloned at `compaction.rs:887`.
- Metrics infra already exists: `jcode-base/src/session_metrics.rs` (`SessionMetrics`, `record_token_usage`, `record_turn`, `snapshot`); `jcode-telemetry-core/src/lib.rs:615` already infers `tool_error_loop` / `agent_got_stuck` stop reasons.
- Context-overflow detection point: `agent/compaction.rs:110` `try_auto_compact_after_context_limit` (+ `is_context_limit_error` at :90).
- Tool-call history available in loop: `Agent.tool_call_ids` (`agent.rs:201`), `session.messages` with `ContentBlock::ToolUse { id, name, input }`.
- Cache append-only: proven by `cache_prefix_invariant_tests` in `jcode-provider-anthropic/src/lib.rs` (memory appends after breakpoints, byte-identical prefix). `CacheTracker` (`jcode-base/src/cache_tracker.rs`) detects violations but is gated OFF by `JCODE_TRACK_CLIENT_CACHE` (`agent.rs:255`), warning-level only.

## Design

### E1 — failure-mode metrics
Extend `SessionMetrics` with three monotonic counters and a recent-window for loop detection:
- `context_overflow_count` — incremented in `try_auto_compact_after_context_limit` whenever `is_context_limit_error` fires (the real overflow signal, post-compaction-attempt).
- `repeated_read_count` — detected in the turn loop: scan recent `ToolUse` blocks; if the same `(tool_name, hash(input))` appears ≥ N times (N=3) within the last K turns, increment once per detected streak.
- `stuck_loop_count` — reuse the existing `tool_error_loop` heuristic (≥3 executed tool failures, 0 successes in a window); increment when the streak crosses threshold.

New public fns mirror existing style: `record_context_overflow(session_id)`, `record_repeated_read(session_id, tool_name)`, `record_stuck_loop(session_id)`. Add the three counts to `SessionMetricsSnapshot`. These are **observational in P0** — not yet stop-conditions (that's a fast-follow once baselines exist).

### C1 — error-aware compaction (summary-only)
In `build_compaction_conversation_text` (`jcode-compaction-core/src/lib.rs`), change the `ToolResult` arm: when `is_error == Some(true)`, emit a compact marker `[tool failed: <first ~80 chars of content>]` instead of the full `[Result: …]` payload. Net effect: failed-attempt payloads never enter the summary the model carries forward, but the *fact* of the failure survives (prevents re-trying dead approaches).

Constraints:
- **Only affects summarization input** — the retained verbatim suffix (last `RECENT_TURNS_TO_KEEP=10`) is untouched, so the agent's active error-recovery context is preserved.
- **No change to `safe_compaction_cutoff`** tool-pair integrity logic.
- Marker length capped; multi-line error content collapsed to one line.

### C4 — always-on cache-violation counter
Make lightweight prefix-hash tracking run unconditionally (drop the `JCODE_TRACK_CLIENT_CACHE` gate for the *counter*, keep verbose logging gated). On a detected violation, increment a `cache_violation_count` in `SessionMetrics` (alongside E1). The existing append-only proof means this should read ~0 in healthy runs — making any nonzero value an actionable regression signal.

## Testing
- **C1:** unit test in `jcode-compaction-core` — feed a message vector containing a `ToolResult{is_error:Some(true)}` to `build_compaction_conversation_text`; assert the full error payload is absent and the `[tool failed: …]` marker present; assert a successful result is unchanged. TDD: write failing test first.
- **E1:** unit tests in `session_metrics.rs` for each counter (record → snapshot reflects count); a turn-loop-level test for repeated-read detection (3 identical reads → one increment).
- **C4:** extend `cache_tracker` tests — a violation increments the metric even with the env var unset.
- **Build/regression:** `source ~/.cargo/env && cargo test -p jcode-compaction-core -p jcode-base` (+ app-core for loop hook).
- **Manual smoke:** long session that triggers a real compaction — confirm summary excludes a known error payload and `snapshot()` shows nonzero overflow/loop counts when provoked.

## Verification of impact
E1 counters ARE the verification substrate for the whole roadmap: capture a baseline over a fixed task set after this lands, then measure every later adaptation (C2/L1/C3) against context-overflow rate, stuck-loop %, and repeated-read count.

## Open questions
- Repeated-read window params (N=3, K=? turns) — start N=3/K=8, tune against baseline.
- Should `stuck_loop_count` crossing a threshold *interrupt* the turn in P0, or stay observational? Default: observational; revisit after baseline.
