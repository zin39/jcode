# Cheap-Routing Subagent Tool-Prune — Implementation Plan (Plan 2 of 5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Let a subagent be spawned with a pruned tool set, so cheap-routing can give each cheap subagent only the tools its subtask needs (cuts ~10k tokens of tool-schema overhead per call — lever C).

**Architecture:** Add an optional `allowed_tools: Vec<String>` parameter to the `subagent` tool's input. When present, the subagent's computed allowed-tool set is intersected with it (a pure helper `prune_allowed_tools`). When absent, behavior is unchanged. Levers B (coach prompt) and E (confidence flag) are intentionally NOT here — they are prompt-content concerns owned by the orchestrator (Plan 3).

**Tech Stack:** Rust edition 2024, `crates/jcode-app-core/src/tool/task.rs`, `std::collections::HashSet`, `cargo test`.

**Run cargo with:** `. "$HOME/.cargo/env" && cargo test -p jcode-app-core ...` (the login shell does not put cargo on PATH).

**Spec:** `docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md` — lever C "Tool pruning".

---

## File Structure

- Modify: `crates/jcode-app-core/src/tool/task.rs`
  - Add pure helper `prune_allowed_tools(HashSet<String>, Option<&[String]>) -> HashSet<String>`.
  - Add field `allowed_tools: Option<Vec<String>>` to `struct SubagentInput` (~line 47).
  - Add `allowed_tools` to `parameters_schema()` JSON (~line 87).
  - Call the helper in `execute()` right after `config().tools.apply_to_allowed_set(&mut allowed)` (~line 160).
  - Add unit tests in the existing `#[cfg(test)] mod tests` block.

Single responsibility preserved: task.rs already owns subagent spawning and its allowed-set logic (see lines 156-162). The prune is a narrowing of that same set.

---

### Task 1: Pure `prune_allowed_tools` helper

**Files:**
- Modify: `crates/jcode-app-core/src/tool/task.rs` (add free function near the other free fns, e.g. after `subagent_title` ~line 282)
- Test: `crates/jcode-app-core/src/tool/task.rs` (in `mod tests`)

- [ ] **Step 1: Write the failing test**

Add inside the existing `#[cfg(test)] mod tests { ... }` block in `task.rs`. Also add `prune_allowed_tools` to the `use super::{...}` import line at the top of `mod tests`:

```rust
    #[test]
    fn prune_allowed_tools_intersects_with_requested() {
        use std::collections::HashSet;
        let allowed: HashSet<String> = ["read", "grep", "bash", "write"]
            .into_iter()
            .map(String::from)
            .collect();

        let requested = vec!["read".to_string(), "grep".to_string()];
        let pruned = super::prune_allowed_tools(allowed, Some(&requested));

        let mut got: Vec<String> = pruned.into_iter().collect();
        got.sort();
        assert_eq!(got, vec!["grep".to_string(), "read".to_string()]);
    }

    #[test]
    fn prune_allowed_tools_none_keeps_everything() {
        use std::collections::HashSet;
        let allowed: HashSet<String> =
            ["read", "grep"].into_iter().map(String::from).collect();

        let pruned = super::prune_allowed_tools(allowed.clone(), None);
        assert_eq!(pruned, allowed);
    }

    #[test]
    fn prune_allowed_tools_ignores_unknown_requested() {
        use std::collections::HashSet;
        let allowed: HashSet<String> =
            ["read", "grep"].into_iter().map(String::from).collect();

        // "bash" is not in the allowed set; intersection just ignores it.
        let requested = vec!["read".to_string(), "bash".to_string()];
        let pruned = super::prune_allowed_tools(allowed, Some(&requested));

        let got: Vec<String> = pruned.into_iter().collect();
        assert_eq!(got, vec!["read".to_string()]);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core prune_allowed_tools`
Expected: FAIL — compile error `cannot find function prune_allowed_tools`.

- [ ] **Step 3: Write minimal implementation**

Add this free function in `task.rs` after `fn subagent_title(...)` (~line 282):

```rust
/// Narrow an allowed-tool set to only the tools the caller requested. When
/// `requested` is `None`, the set is returned unchanged. Requested names that
/// are not already allowed are ignored (set intersection), so this can only ever
/// remove tools, never grant new ones.
fn prune_allowed_tools(
    mut allowed: std::collections::HashSet<String>,
    requested: Option<&[String]>,
) -> std::collections::HashSet<String> {
    if let Some(requested) = requested {
        let keep: std::collections::HashSet<&str> =
            requested.iter().map(String::as_str).collect();
        allowed.retain(|tool| keep.contains(tool.as_str()));
    }
    allowed
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core prune_allowed_tools`
Expected: PASS — all three `prune_allowed_tools_*` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core/src/tool/task.rs
git commit -m "feat(subagent): add prune_allowed_tools helper

Cheap-routing Plan 2 (lever C): pure set-intersection helper to narrow a
subagent's allowed tools to a requested subset. Can only remove, never grant.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: Wire `allowed_tools` param into `SubagentInput` and `execute`

**Files:**
- Modify: `crates/jcode-app-core/src/tool/task.rs` (struct ~line 47, schema ~line 87, execute ~line 160)

- [ ] **Step 1: Add the field to `SubagentInput`**

In `struct SubagentInput` (after the `output_mode` field, before `_command`), add:

```rust
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
```

The struct already derives `Deserialize` and uses `#[serde(default)]` on optional fields, so an absent `allowed_tools` deserializes to `None`.

- [ ] **Step 2: Add the field to the JSON schema**

In `parameters_schema()`, inside the `"properties"` object (after the `"output_mode"` property, before `"command"`), add:

```rust
                "allowed_tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional subset of tool names this subagent may use. When set, the subagent's tools are intersected with this list (can only remove tools, never grant new ones)."
                },
```

- [ ] **Step 3: Apply the prune in `execute`**

In `execute()`, find this existing block (~line 156-162):

```rust
        let mut allowed: HashSet<String> = self.registry.tool_names().await.into_iter().collect();
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread"] {
            allowed.remove(blocked);
        }
        crate::config::config()
            .tools
            .apply_to_allowed_set(&mut allowed);
```

Immediately AFTER that block, add:

```rust
        let allowed = prune_allowed_tools(allowed, params.allowed_tools.as_deref());
```

(`params.allowed_tools` is `Option<Vec<String>>`; `.as_deref()` yields `Option<&[String]>`, matching the helper. The rebind shadows the prior `allowed` and is used by `Agent::new_with_session(..., Some(allowed))` further down.)

- [ ] **Step 4: Fix the test constructor for the new field**

The existing test `subagent_display_title_includes_type_and_model` builds a `SubagentInput { ... }` literal (~line 378) and will now fail to compile (missing field). Add the field to that literal, after `output_mode: SubagentOutputMode::Answer,`:

```rust
            allowed_tools: None,
```

- [ ] **Step 5: Run the crate tests**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core`
Expected: PASS — full crate compiles and all tests pass (the three new `prune_allowed_tools_*`, the updated `subagent_display_title_*`, and all pre-existing tests). No warnings about an unused `allowed_tools` field (it is read via `.as_deref()` in `execute`).

- [ ] **Step 6: Commit**

```bash
git add crates/jcode-app-core/src/tool/task.rs
git commit -m "feat(subagent): accept allowed_tools to prune subagent tool set

Cheap-routing Plan 2 (lever C): subagent input gains an optional allowed_tools
list; when present the subagent's tools are intersected with it, cutting tool-
schema token overhead for cheap subtasks.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:** Lever C "Tool pruning — pass each cheap subagent only the tools its subtask needs via the allowed_tools filter" is implemented by Tasks 1+2. Levers B/E are explicitly deferred to Plan 3 (documented in Architecture). No spec requirement for Plan 2 is left unimplemented.

**2. Placeholder scan:** No TBD/TODO. All code shown in full, including the exact existing block to anchor the `execute` edit and the test-literal fix.

**3. Type consistency:** `prune_allowed_tools(HashSet<String>, Option<&[String]>) -> HashSet<String>` is defined in Task 1 and called in Task 2 Step 3 with `params.allowed_tools.as_deref()` (`Option<Vec<String>>` → `Option<&[String]>`) — types line up. `allowed_tools: Option<Vec<String>>` field matches the `#[serde(default)]` convention used by the other optional `SubagentInput` fields (`model`, `session_id`).

**4. Ambiguity check:** Intersection semantics ("can only remove, never grant") are pinned by `prune_allowed_tools_ignores_unknown_requested`. Absent param = unchanged behavior, pinned by `prune_allowed_tools_none_keeps_everything`.
