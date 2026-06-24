# Cheap-Routing Provider Backend — Implementation Plan (Plan 4 of 5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Provide a real, production `CheapRouteBackend` implementation (`ProviderCheapBackend`) backed by an `Arc<dyn Provider>` + tool `Registry`, so `run_cheap_route` (Plan 3) actually drives a live provider and spawns real subagents — making the cheap-routing engine complete and callable as one unit.

**Architecture:** Add `ProviderCheapBackend` to the existing `crates/jcode-app-core/src/agent/cheap_route.rs`. `ask_parent` delegates to `provider.complete_simple`; `run_subtask` mirrors the proven `SubagentTool::execute` path (create session → set model → build allowed-tool set → `Agent::new_with_session(provider.fork(), ...)` → `run_once_capture`); `routes` delegates to `provider.model_routes()`. A focused unit test verifies the parent-call and routes delegation through a minimal in-crate mock provider (modeled on the existing `DelayedProvider` in `agent_tests.rs`). `run_subtask` is thin glue over the already-working subagent machinery, so it is compile-verified rather than re-tested through a flaky full-agent run.

**Tech Stack:** Rust edition 2024, `async_trait`, `tokio`, `crate::agent::Agent`, `crate::session::Session`, `crate::tool::Registry`, `crate::provider::{Provider, EventStream}`.

**Run cargo with:** `. "$HOME/.cargo/env" && cargo test -p jcode-app-core ...`

**Spec:** `docs/superpowers/specs/2026-06-24-cheap-routing-mode-design.md` — "Subagent spawn" + reuse anchors (`tool/task.rs` spawn path, `provider.complete_simple`).

---

## File Structure

- Modify: `crates/jcode-app-core/src/agent/cheap_route.rs` — append `ProviderCheapBackend` after `run_cheap_route`, and add its test in the existing `#[cfg(test)] mod tests`.

---

### Task 1: `ProviderCheapBackend` real adapter

**Files:**
- Modify: `crates/jcode-app-core/src/agent/cheap_route.rs`

- [ ] **Step 1: Add the adapter struct + impl**

Append this AFTER the `run_cheap_route` function (before the `#[cfg(test)] mod tests` block) in `cheap_route.rs`:

```rust
use std::sync::Arc;

/// Production [`CheapRouteBackend`] backed by a real provider and tool registry.
/// `ask_parent` uses the (expensive) parent provider directly; `run_subtask`
/// spawns a one-shot subagent pinned to the chosen cheap model on an isolated
/// provider fork, mirroring `SubagentTool::execute`.
pub struct ProviderCheapBackend {
    provider: Arc<dyn crate::provider::Provider>,
    registry: crate::tool::Registry,
    parent_system: String,
}

impl ProviderCheapBackend {
    pub fn new(
        provider: Arc<dyn crate::provider::Provider>,
        registry: crate::tool::Registry,
    ) -> Self {
        Self {
            provider,
            registry,
            parent_system:
                "You are a cost-routing coordinator. Decompose, recommend a model, and review \
                 subagent work. Be terse and precise; output exactly what is asked."
                    .to_string(),
        }
    }
}

#[async_trait]
impl CheapRouteBackend for ProviderCheapBackend {
    async fn ask_parent(&self, prompt: &str) -> Result<String> {
        self.provider.complete_simple(prompt, &self.parent_system).await
    }

    async fn run_subtask(&self, subtask: &Subtask, model: &str) -> Result<String> {
        // Mirror SubagentTool::execute: new session pinned to `model`, blocked
        // recursive tools removed, run on an isolated provider fork.
        let mut session = crate::session::Session::create(None, Some(subtask.description.clone()));
        session.model = Some(model.to_string());
        session.save()?;

        let mut allowed: std::collections::HashSet<String> =
            self.registry.tool_names().await.into_iter().collect();
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread"] {
            allowed.remove(blocked);
        }

        let mut agent = crate::agent::Agent::new_with_session(
            self.provider.fork(),
            self.registry.clone(),
            session,
            Some(allowed),
        );
        agent.run_once_capture(&subtask.prompt).await
    }

    fn routes(&self) -> Vec<ModelRoute> {
        self.provider.model_routes()
    }
}
```

- [ ] **Step 2: Add the delegation test**

In the `#[cfg(test)] mod tests` block of `cheap_route.rs`, add the imports and test below. Model the mock on the existing `DelayedProvider` in `crates/jcode-app-core/src/agent_tests.rs` — copy the SAME `use` statements that file uses for `Provider`, `EventStream`, `StreamEvent`, `Message`, `ToolDefinition`, the mpsc channel, and `ReceiverStream`. Then add:

```rust
    // --- minimal provider mock (mirrors agent_tests::DelayedProvider) ---
    struct ParentMock {
        reply: String,
        routes: Vec<ModelRoute>,
    }

    #[async_trait]
    impl crate::provider::Provider for ParentMock {
        async fn complete(
            &self,
            _messages: &[jcode_message_types::Message],
            _tools: &[jcode_message_types::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            let reply = self.reply.clone();
            let (tx, rx) = tokio::sync::mpsc::channel::<Result<jcode_message_types::StreamEvent>>(8);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(jcode_message_types::StreamEvent::TextDelta(reply)))
                    .await;
                let _ = tx
                    .send(Ok(jcode_message_types::StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    }))
                    .await;
            });
            Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
        }

        fn name(&self) -> &str {
            "parentmock"
        }

        fn fork(&self) -> std::sync::Arc<dyn crate::provider::Provider> {
            std::sync::Arc::new(Self {
                reply: self.reply.clone(),
                routes: self.routes.clone(),
            })
        }

        fn model_routes(&self) -> Vec<ModelRoute> {
            self.routes.clone()
        }
    }

    #[tokio::test]
    async fn provider_backend_delegates_ask_parent_and_routes() {
        let provider: std::sync::Arc<dyn crate::provider::Provider> =
            std::sync::Arc::new(ParentMock {
                reply: "PARENT_REPLY".to_string(),
                routes: vec![priced_route("cheapo", 100_000)],
            });
        let registry = crate::tool::Registry::empty();
        let backend = ProviderCheapBackend::new(provider, registry);

        // ask_parent drains the provider stream into text.
        let answer = backend.ask_parent("anything").await.unwrap();
        assert_eq!(answer, "PARENT_REPLY");

        // routes delegates to provider.model_routes().
        let routes = backend.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].model, "cheapo");
    }
```

Note on `Registry::empty()`: confirmed available (used in `crates/jcode-app-core/src/server/provider_control_tests.rs`). If the exact constructor name differs, use whatever the existing tests use to build an empty `Registry` (search `Registry::` in `*_tests.rs`).

- [ ] **Step 3: Run the adapter test**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core provider_backend_delegates`
Expected: PASS — `provider_backend_delegates_ask_parent_and_routes` passes.

- [ ] **Step 4: Run the whole cheap_route + crate**

Run: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core cheap_route`
Expected: PASS — all 8 `cheap_route` tests (7 from Plan 3 + the new adapter test).

Then: `. "$HOME/.cargo/env" && cargo test -p jcode-app-core 2>&1 | tail -20`
Expected: the only failures are the 5 KNOWN pre-existing ones (`tool::bash::tests::test_stdin_forwarding_single_line`, `tool::bash::tests::test_stdin_forwarding_multiple_lines`, `server::queue_tests::queue_soft_interrupt_for_session_persists_when_live_queue_is_unavailable`, `server::reload::reload_tests::persist_reload_recovery_intents_records_running_peer_recovery`, `server::reload_recovery::tests::mark_delivered_is_idempotent_and_matches_exact_continuation`). No NEW failures. If any new test fails, fix it before committing.

- [ ] **Step 5: Commit**

```bash
git add crates/jcode-app-core/src/agent/cheap_route.rs
git commit -m "feat(agent): add ProviderCheapBackend real provider adapter

Cheap-routing Plan 4: production CheapRouteBackend backed by a live provider
and tool registry. ask_parent -> complete_simple; run_subtask mirrors the
subagent spawn path pinned to the chosen cheap model; routes -> model_routes.
Delegation verified via a minimal in-crate mock provider.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:** Spec reuse anchors "subagent spawn (task.rs:211 fork), provider.complete_simple, model_routes" are realized by `ProviderCheapBackend`. With Plan 3's `run_cheap_route`, the engine is now callable end-to-end against a real provider. The user-facing `/cheap` trigger remains for Plan 5.

**2. Placeholder scan:** No TBD/TODO. Adapter code and test are given in full. The mock's `use` statements are specified by reference to the exact existing file (`agent_tests.rs` `DelayedProvider`) plus inline fully-qualified paths (`jcode_message_types::StreamEvent`, `tokio_stream::wrappers::ReceiverStream`, `crate::provider::{Provider, EventStream}`) so they resolve without guessing.

**3. Type consistency:** `ProviderCheapBackend` implements `CheapRouteBackend` (Plan 3) exactly — `ask_parent(&self, &str) -> Result<String>`, `run_subtask(&self, &Subtask, &str) -> Result<String>`, `routes(&self) -> Vec<ModelRoute>`. Calls match verified signatures: `Session::create(Option<String>, Option<String>)`, `session.model: Option<String>`, `Agent::new_with_session(Arc<dyn Provider>, Registry, Session, Option<HashSet<String>>)`, `run_once_capture(&mut self, &str) -> Result<String>`, `provider.complete_simple(&str, &str)`, `provider.model_routes() -> Vec<ModelRoute>`, `provider.fork() -> Arc<dyn Provider>`. The mock implements only the three required trait methods (`complete`, `name`, `fork`) plus an override of the defaulted `model_routes`, matching `DelayedProvider`'s minimal shape.

**4. Ambiguity check:** `run_subtask`'s blocked-tool list matches `SubagentTool::execute` exactly. The test pins both delegations (parent text + routes) so behavior is unambiguous.
