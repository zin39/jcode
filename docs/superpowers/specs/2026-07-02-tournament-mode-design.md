# Tournament Mode (best-of-N attempts with judge)

**Date:** 2026-07-02
**Status:** approved (smartness roadmap #2)

## Problem

One attempt per hard task leaves measurable quality on the table: best-of-3
with selection lifted SWE-bench Verified 76.1 → 81.2 pass@3 (Verdent);
Cursor 3 ships up to 8 parallel worktree agents. jcode has every ingredient
(subagents with per-session working_dir, FuturesUnordered batch pattern,
structured output, git diff helpers) but no orchestration for
attempt-in-isolation + judge.

## Design — new `tournament` tool

`crates/jcode-app-core/src/tool/tournament.rs`, registered like SubagentTool
(needs provider + registry). Deferred-eligible (not in CORE set).

Input schema:
- `prompt` (required) — the task.
- `attempts` (int, default 3, clamped 2..=4)
- `judge_criteria` (optional string, folded into the judge prompt)
- `model` (optional — per-attempt child model override)
- `cleanup` (bool, default true — remove losing worktrees after judging;
  winner's worktree always kept and its path returned)

Flow:
1. Parent must be in a git repo (`git rev-parse --git-dir` in ctx.working_dir;
   error out otherwise). Repo root via `git rev-parse --show-toplevel`.
2. Per attempt i: `git worktree add --detach <jcode home>/tournaments/
   <parent-session>/attempt-<i> HEAD` (run via tokio::process, same pattern as
   agent/verify.rs commands).
3. Run N child agents concurrently (FuturesUnordered, like batch.rs): each a
   fresh Session whose `working_dir` = its worktree; child prompt = task +
   note that it must work only inside its directory. Same tool-blocklist as
   SubagentTool (no subagent/tournament recursion — add "tournament" to the
   blocked list in BOTH SubagentTool and this tool).
4. Per attempt capture: `git -C <wt> add -A --intent-to-add && git -C <wt>
   diff` (captures untracked as new-file diffs), truncated to 30k chars per
   attempt (tail-safe truncation, reuse the pattern from agent/verify.rs).
5. Judge: child agent (structured output via the existing output-contract
   pattern in task.rs — schema `{winner: integer, reasoning: string,
   scores: [{attempt: integer, score: number, notes: string}]}`) fed the task,
   criteria, and each attempt's final answer + diff. Parse with the same
   fence-tolerant JSON enforcement as task.rs.
6. Output text: winner index, judge reasoning + scores, the winner's diff,
   winner worktree path ("apply with: git apply <path>.diff or continue in the
   worktree"). Metadata: attempts, winner, worktree path, per-attempt session
   ids. Losing worktrees removed with `git worktree remove --force` when
   cleanup=true; on judge parse failure keep ALL worktrees, return all
   summaries with an explicit "judge failed, pick manually" note.

## Non-goals

Auto-applying the winning diff to the parent tree; swarm-plan integration;
attempts >4 (judge context limits); non-git projects.

## Files

- `crates/jcode-app-core/src/tool/tournament.rs` — new.
- `crates/jcode-app-core/src/tool/mod.rs` — mod + registration.
- `crates/jcode-app-core/src/tool/task.rs` — add "tournament" to the child
  blocked-tool list.

## Testing

- Unit (pure parts): input clamping, judge schema shape, judge-output parsing
  (valid/fenced/garbage), diff truncation, worktree path construction.
- Manual e2e: real git repo, `tournament {prompt:"add a comment to README",
  attempts:2}` with a live provider.
