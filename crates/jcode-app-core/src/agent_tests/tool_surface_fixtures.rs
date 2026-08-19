// Provider fixtures for tests that assert on the model-visible tool surface.
//
// Split out of agent_tests.rs, which is already over the oversized-test budget.
/// Same behavior as [`NativeAutoCompactionProvider`] but with a realistic
/// context window.
///
/// `NativeAutoCompactionProvider` reports 1k tokens so the compaction tests can
/// overflow it cheaply. That is at or below `SMALL_CONTEXT_WINDOW_TOKENS`, so
/// any agent built on it silently runs in deferred-tools mode, where `mcp__*`
/// schemas are held behind `load_tools` instead of appearing in the tool list.
/// The MCP snapshot tests are about the locked-snapshot rebuild, not about
/// deferral, so they need a window that does not force that mode.
struct RoomyWindowProvider;

#[async_trait]
impl Provider for RoomyWindowProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        let (_tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(1);
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn context_window(&self) -> usize {
        200_000
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self)
    }

    async fn complete_simple(&self, _prompt: &str, _system: &str) -> Result<String> {
        Ok("roomy window provider".to_string())
    }
}

/// A test provider's context window silently decides whether the agent runs in
/// deferred-tools mode, and deferred mode hides `mcp__*` and other schemas
/// behind `load_tools`.
///
/// Three tests (the two MCP snapshot tests and the gmail exposure test) asserted
/// on the model-visible tool list while running on a provider that reports 1k
/// tokens, so they were really asserting deferred-mode behavior and failed. The
/// failure looked like an MCP bug rather than a fixture problem, which is why it
/// sat unexplained.
///
/// Pin the property so the coupling is visible: a fixture meant for tool-surface
/// assertions must report a window above the deferral threshold.
#[test]
fn roomy_window_provider_does_not_force_deferred_tools() {
    assert!(
        !Agent::context_window_requires_deferred_tools(RoomyWindowProvider.context_window()),
        "RoomyWindowProvider must not trip deferred-tools mode, or tool-surface \
         assertions silently test deferral instead"
    );
    assert!(
        Agent::context_window_requires_deferred_tools(
            NativeAutoCompactionProvider.context_window()
        ),
        "NativeAutoCompactionProvider's tiny window is deliberate for compaction \
         tests; if this changes, those tests need a new way to overflow"
    );
}

#[tokio::test]
async fn gmail_is_exposed_by_default_and_can_be_explicitly_disabled() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("JCODE_HOME");
    let prev_tools = std::env::var_os("JCODE_TOOLS");
    let prev_disabled_tools = std::env::var_os("JCODE_DISABLED_TOOLS");
    let prev_tool_profile = std::env::var_os("JCODE_TOOL_PROFILE");
    let prev_disable_base_tools = std::env::var_os("JCODE_DISABLE_BASE_TOOLS");
    let temp_home = tempfile::TempDir::new().expect("temp home");

    crate::env::set_var("JCODE_HOME", temp_home.path());
    crate::env::remove_var("JCODE_TOOLS");
    crate::env::remove_var("JCODE_DISABLED_TOOLS");
    crate::env::remove_var("JCODE_TOOL_PROFILE");
    crate::env::remove_var("JCODE_DISABLE_BASE_TOOLS");
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(RoomyWindowProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;
    let tool_name = "gmail";

    // Upstream visibility invariants for regular (non-self-dev) sessions.
    assert!(
        tool_names.iter().any(|name| name == "jcode_docs"),
        "jcode_docs must be model-visible in regular sessions"
    );
    assert!(
        !tool_names.iter().any(|name| name == "selfdev"),
        "selfdev must not be model-visible in regular sessions"
    );

    // gmail is in RARELY_USED_DEFERRED_TOOLS (measured: 2 calls across 789
    // sessions), so it is deferred rather than inlined. "Available by default"
    // must still hold: the model has to be able to see it and run it.
    assert!(
        !definitions
            .iter()
            .any(|definition| definition.name == tool_name),
        "{tool_name} is rarely used and should not ship its full schema by default"
    );
    let load_tools = definitions
        .iter()
        .find(|d| d.name == "load_tools")
        .expect("load_tools must be inline so deferred tools can be expanded");
    assert!(
        load_tools.description.contains(tool_name),
        "{tool_name} must stay discoverable via the load_tools index"
    );
    assert!(
        tool_names.iter().any(|name| name == tool_name),
        "{tool_name} must remain registered and callable by default"
    );
    agent
        .validate_tool_allowed(tool_name)
        .expect("gmail must be executable by default");

    crate::env::set_var("JCODE_DISABLED_TOOLS", tool_name);
    crate::config::Config::invalidate_cache();

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    let definitions = agent.tool_definitions().await;
    let tool_names = agent.tool_names().await;

    assert!(
        !definitions
            .iter()
            .any(|definition| definition.name == tool_name),
        "explicitly disabled {tool_name} must not be sent in model-visible tool definitions"
    );
    assert!(
        !tool_names.iter().any(|name| name == tool_name),
        "explicitly disabled {tool_name} must not be listed as model-visible"
    );
    let err = agent
        .validate_tool_allowed(tool_name)
        .expect_err("explicitly disabled gmail must not be executable");
    assert!(err.to_string().contains("disabled"));

    if let Some(previous) = prev_home {
        crate::env::set_var("JCODE_HOME", previous);
    } else {
        crate::env::remove_var("JCODE_HOME");
    }
    if let Some(previous) = prev_tools {
        crate::env::set_var("JCODE_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_TOOLS");
    }
    if let Some(previous) = prev_disabled_tools {
        crate::env::set_var("JCODE_DISABLED_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_DISABLED_TOOLS");
    }
    if let Some(previous) = prev_tool_profile {
        crate::env::set_var("JCODE_TOOL_PROFILE", previous);
    } else {
        crate::env::remove_var("JCODE_TOOL_PROFILE");
    }
    if let Some(previous) = prev_disable_base_tools {
        crate::env::set_var("JCODE_DISABLE_BASE_TOOLS", previous);
    } else {
        crate::env::remove_var("JCODE_DISABLE_BASE_TOOLS");
    }
    crate::config::Config::invalidate_cache();
}

/// Reproduction for #206: MCP tools that register on the registry *after* the
/// first turn locks the tool snapshot never reach the provider, because
/// `tool_definitions()` returns the frozen `locked_tools` snapshot and the only
/// unlock path (`unlock_tools_if_needed`) fires solely when the LLM invokes the
/// `"mcp"` management tool — which it never does, since it cannot see the
/// `mcp__*` tools it would need to trigger that unlock.
#[tokio::test]
async fn mcp_tools_registered_after_lock_are_visible_to_agent() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(RoomyWindowProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    // First turn locks the snapshot (this is what happens before the async MCP
    // registration spawn completes).
    let before = agent.tool_definitions().await;
    let before_len = before.len();
    assert!(
        !before.iter().any(|t| t.name.starts_with("mcp__")),
        "precondition: no mcp tools before async registration completes"
    );

    // Simulate the spawned MCP registration task finishing: a new mcp__* tool
    // lands on the shared registry.
    agent
        .registry
        .register(
            "mcp__test__write_memory".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__write_memory".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;

    // The next turn should now advertise the MCP tool to the provider.
    let after = agent.tool_definitions().await;
    assert!(
        after.iter().any(|t| t.name == "mcp__test__write_memory"),
        "regression #206: MCP tool registered after the first turn never reaches \
         the agent's tool surface (locked snapshot of {} tools is reused forever)",
        before_len
    );

    // Once MCP tools are present in the locked snapshot, subsequent turns must
    // return the *same* stable snapshot so provider prompt-cache hits stay warm
    // (the whole point of locked_tools). The #206 fix must not flap.
    let names =
        |defs: &[ToolDefinition]| -> Vec<String> { defs.iter().map(|t| t.name.clone()).collect() };
    let stable_a = agent.tool_definitions().await;
    let stable_b = agent.tool_definitions().await;
    assert_eq!(
        names(&stable_a),
        names(&stable_b),
        "tool snapshot must be stable across turns once MCP tools are present"
    );
    assert_eq!(
        names(&stable_a),
        names(&after),
        "snapshot must not change after MCP tools are already included"
    );
}

/// The intentional, MCP-driven prompt-cache miss must happen at most ONCE per
/// locked snapshot. After the first late-registered `mcp__*` tool is picked up
/// (the one accepted miss), a *second* MCP tool that registers even later must
/// NOT trigger another rebuild — otherwise a server that connects in waves would
/// thrash the provider prompt cache. Guards the `mcp_late_register_resolved`
/// one-shot flag (#206 follow-up).
#[tokio::test]
async fn mcp_late_registration_rebuild_happens_at_most_once() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(RoomyWindowProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    // First turn locks the snapshot with no MCP tools yet.
    let _ = agent.tool_definitions().await;

    // First MCP tool arrives -> one accepted rebuild exposes it.
    agent
        .registry
        .register(
            "mcp__test__first".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__first".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after_first = agent.tool_definitions().await;
    assert!(
        after_first.iter().any(|t| t.name == "mcp__test__first"),
        "first late MCP tool must be picked up by the one accepted rebuild"
    );
    assert!(
        agent.mcp_late_register_resolved,
        "one-shot guard must latch after the accepted rebuild"
    );

    // A SECOND MCP tool registers even later (server connected in a second
    // wave). The one-shot guard means we do NOT rebuild again, so the snapshot
    // stays cache-stable and this tool is intentionally not surfaced until the
    // tool list is explicitly unlocked.
    agent
        .registry
        .register(
            "mcp__test__second".to_string(),
            Arc::new(FakeMcpTool {
                name: "mcp__test__second".to_string(),
            }) as Arc<dyn crate::tool::Tool>,
        )
        .await;
    let after_second = agent.tool_definitions().await;
    let names: Vec<String> = after_second.iter().map(|t| t.name.clone()).collect();
    assert!(
        names.iter().any(|n| n == "mcp__test__first"),
        "previously surfaced MCP tool must remain"
    );
    assert!(
        !names.iter().any(|n| n == "mcp__test__second"),
        "second-wave MCP tool must NOT trigger a second cache-busting rebuild"
    );

    // An explicit unlock (e.g. the `mcp` reload tool) re-arms the one-shot guard
    // and lets the next snapshot pick up everything currently registered.
    agent.unlock_tools();
    assert!(
        !agent.mcp_late_register_resolved,
        "explicit unlock must re-arm the one-shot guard"
    );
    let after_unlock = agent.tool_definitions().await;
    let unlocked_names: Vec<String> = after_unlock.iter().map(|t| t.name.clone()).collect();
    assert!(
        unlocked_names.iter().any(|n| n == "mcp__test__second"),
        "after explicit unlock, the second-wave MCP tool must finally surface"
    );
}

/// The user's `swarm-prompt.md` must still reach a coordinator after moving out
/// of the `swarm` tool description, and must NOT reach a spawned worker.
///
/// It used to ride in the tool schema, so it was billed on every request in
/// every session (1,052 tokens as measured on a real machine) including the
/// majority that never spawn an agent. Moving it must not silently drop the
/// guidance for the sessions that actually route models with it.
#[tokio::test]
async fn swarm_prompt_reaches_coordinators_only() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("home");
    let _home = crate::storage::scoped_test_home(home.path());
    std::fs::write(
        home.path().join("config.toml"),
        "[agents]\nauto_delegate = true\n",
    )
    .expect("write config");
    std::fs::write(
        home.path().join("swarm-prompt.md"),
        "ROUTING MARKER: prefer the cheap model",
    )
    .expect("write swarm prompt");
    crate::config::invalidate_config_cache();

    let provider: Arc<dyn Provider> = Arc::new(RoomyWindowProvider);
    let registry = Registry::new(provider.clone()).await;

    let mut coordinator = Agent::new(provider.clone(), registry.clone());
    coordinator.session.agent_role = None;
    let block = coordinator.delegation_block_for_test();
    assert!(
        block.contains("ROUTING MARKER"),
        "a coordinator must still receive the swarm prompt, got: {block}"
    );

    let mut worker = Agent::new(provider, registry);
    worker.session.agent_role = Some(jcode_session_types::SessionAgentRole::SwarmWorker);
    assert!(
        !worker.delegation_block_for_test().contains("ROUTING MARKER"),
        "a spawned worker must not be billed for coordinator routing guidance"
    );
}

/// The rarely-used trim must DEFER capability, never delete it.
///
/// Each withheld tool has to stay (a) discoverable, by appearing in the
/// `load_tools` index the model can read, and (b) recoverable, by returning to
/// the inline schema set once the session expands it. Otherwise the token
/// saving is really a capability regression.
#[tokio::test]
async fn rarely_used_tools_are_deferred_not_deleted() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(RoomyWindowProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);
    agent.session.id = format!("trim-test-{}", std::process::id());

    let tools = agent.tool_definitions().await;
    let names: std::collections::HashSet<&str> = tools.iter().map(|t| t.name.as_str()).collect();

    // A representative withheld tool: measured at 0% of sessions, 1 call ever.
    // (`browser` is always registered; upstream's `integration_tools` is
    // sponsor-gated so it cannot serve as the fixture here.)
    assert!(
        !names.contains("browser"),
        "a rarely-used tool should not ship its full schema by default"
    );

    // Discoverable: the model can see it exists via load_tools.
    let load_tools = tools
        .iter()
        .find(|t| t.name == "load_tools")
        .expect("load_tools must always be inline, or nothing can be expanded");
    assert!(
        load_tools.description.contains("browser"),
        "a withheld tool must be listed in the load_tools index, or it is invisible \
         rather than deferred: {}",
        load_tools.description
    );

    // Frequently-used tools must NOT be withheld.
    for keep in ["bash", "read", "edit", "swarm", "todo", "batch"] {
        assert!(
            names.contains(keep),
            "{keep} is used in a large share of sessions and must stay inline"
        );
    }

    // Recoverable: expanding restores the full schema.
    crate::tool::expand_session_tools(&agent.session.id, &["browser".to_string()]);
    agent.unlock_tools();
    let after = agent.tool_definitions().await;
    assert!(
        after.iter().any(|t| t.name == "browser"),
        "load_tools must restore a deferred tool's full schema"
    );
}
