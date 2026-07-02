# Verification-Before-Done Loop (opt-in)

**Date:** 2026-07-02
**Status:** approved (smartness roadmap #1)

## Problem

The agent can claim "done" with broken code. Research: test-based patch
filtering ~doubles patch precision (NeurIPS SWT-Bench); verification as a
first-class loop stage is core to the current SWE-bench SOTA harness. jcode's
`turn_end` hook is observer-only — nothing harness-side runs checks or feeds
failures back.

## Design

Opt-in config:

```toml
[verify]
enabled = false          # master switch
commands = []            # e.g. ["cargo check --quiet", "cargo fmt --check"]
max_attempts = 2         # verify->fix cycles per turn before giving up
timeout_secs = 300       # per command
```

Project override: if `<working_dir>/.jcode/verify.toml` exists, its `[verify]`
section replaces the global one (same pattern as project `.jcode/mcp.json`).

Flow (both agent loops):

1. Track modifications: set a `turn_made_edits` flag when an executed tool is
   one of `write, edit, multiedit, patch, apply_patch` (bash excluded — cannot
   know; documented).
2. At each loop-exit point where the model produced no tool calls
   (`turn_loops.rs:768/801` blocking; the streaming no-tool-calls path), if
   `enabled && turn_made_edits && attempts < max_attempts`: run the commands
   sequentially in the session working dir (tokio::process, per-command
   timeout, capture stdout+stderr).
3. All pass → clear flag, log, break as today.
4. Any fail → append a user-role message:
   `[jcode verification] command \`<cmd>\` failed (exit <code>). Fix before
   finishing:\n<last 8KB of output>` — then `continue` the loop (same
   mechanism as the existing continuation prompt at turn_loops.rs:780-788).
   attempts += 1; flag reset so another edit re-arms verification.
5. Attempts exhausted → append a final note to the turn output that
   verification is still failing (so the user sees it), break.

New module `crates/jcode-app-core/src/agent/verify.rs`:
- `VerifyConfig` resolution (global + project override)
- `async fn run_verification(cmds, cwd, timeout) -> VerifyOutcome { passed, report }`
- pure, unit-testable; loop wiring stays thin.

## Non-goals

Auto-detecting project check commands (follow-up); running verification on
bash-only turns; hook-based implementation (observer hooks can't feed back).

## Files

- `crates/jcode-base/src/config.rs` — `VerifyConfig` section.
- `crates/jcode-app-core/src/agent/verify.rs` — new.
- `crates/jcode-app-core/src/agent/turn_loops.rs` + `turn_streaming_mpsc.rs`
  — flag tracking + exit-point wiring.
- `crates/jcode-app-core/src/agent.rs` — per-turn state fields.

## Testing

- Unit: config resolution (global/project/disabled); run_verification with a
  passing + failing shell command (use `true`/`false`/`echo`); report
  truncation.
- Loop wiring: unit-test the decision helper (should_verify(flags, attempts))
  rather than the full loop.
- Manual: enable in config with `["false"]` command → agent turn with an edit
  must retry then surface failure.
