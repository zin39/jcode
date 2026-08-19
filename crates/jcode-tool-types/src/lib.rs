#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub output: String,
    pub title: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub images: Vec<ToolImage>,
}

#[derive(Debug, Clone)]
pub struct ToolImage {
    pub media_type: String,
    pub data: String,
    pub label: Option<String>,
}

impl ToolOutput {
    pub fn new(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            title: None,
            metadata: None,
            images: Vec::new(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_image(mut self, media_type: impl Into<String>, data: impl Into<String>) -> Self {
        self.images.push(ToolImage {
            media_type: media_type.into(),
            data: data.into(),
            label: None,
        });
        self
    }

    pub fn with_labeled_image(
        mut self,
        media_type: impl Into<String>,
        data: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        self.images.push(ToolImage {
            media_type: media_type.into(),
            data: data.into(),
            label: Some(label.into()),
        });
        self
    }
}

/// Resolve tool name aliases to their canonical internal names.
///
/// Providers can present tools with Claude Code aliases (e.g. `file_grep`,
/// `shell_exec`) or API namespace prefixes (e.g. `functions.bash`). Models can
/// repeat those names in sub-tool calls such as `batch`, while our registry
/// uses canonical internal names (`agentgrep`, `bash`). This mapping ensures
/// all of those forms resolve correctly.
///
/// This lives in `jcode-tool-types` (rather than the tool `Registry`) so that
/// low-level crates such as config can normalize tool names without depending
/// on the full tool subsystem.
pub fn resolve_tool_name(name: &str) -> &str {
    // Some function-calling APIs expose a recipient such as `functions.bash`.
    // Models occasionally preserve that transport namespace when constructing
    // a nested tool call, especially inside `batch`.
    let name = name.strip_prefix("functions.").unwrap_or(name);

    match name {
        "communicate" => "swarm",
        // `subagent` was deleted from the registry; `swarm` is the spawn path.
        // Leaving these pointed at the old name turned a recoverable alias into
        // an "Unknown tool: subagent" error, observed in 9 real sessions.
        "task" | "task_runner" | "subagent" => "swarm",
        "launch" => "open",
        "shell" => "bash",
        "shell_exec" => "bash",
        "read_file" => "read",
        "file_read" => "read",
        "write_file" => "write",
        "file_write" => "write",
        "edit_file" => "edit",
        "file_edit" => "edit",
        // The native grep tool was removed in favor of agentgrep, but models
        // still frequently call `grep` (and OAuth's `file_grep`). agentgrep's
        // grep mode accepts `pattern` as an alias for `query`, so these calls
        // work as-is.
        "grep" | "file_grep" => "agentgrep",
        // Models reach for a `glob` tool that this registry never had. agentgrep's
        // `find` mode is the file-name search they want, so route it there rather
        // than returning "Unknown tool: glob" (observed in a real session).
        "glob" | "Glob" => "agentgrep",
        "skill" | "Skill" => "skill_manage",
        // The integration catalog tool was renamed from `discover_tools`;
        // models trained on or resuming from the old vocabulary still emit it.
        "discover_tools" => "integration_tools",
        "todoread" | "todowrite" | "todo_read" | "todo_write" | "todos" => "todo",
        // The Anthropic OAuth surface advertises PascalCase tool names and
        // reverse-maps them provider-side for top-level calls, but nested
        // `batch` subcall names bypass that mapping and resolve here (issue
        // #486). Keep these in sync with anthropic_map_tool_name_from_oauth.
        "Bash" => "bash",
        "Read" => "read",
        "Write" => "write",
        "Edit" => "edit",
        "Grep" => "agentgrep",
        "Agent" => "swarm",
        "ScheduleWakeup" => "schedule",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_tool_name;

    #[test]
    fn resolve_tool_name_strips_function_namespace_before_alias_resolution() {
        assert_eq!(resolve_tool_name("functions.bash"), "bash");
        assert_eq!(resolve_tool_name("functions.shell_exec"), "bash");
        assert_eq!(resolve_tool_name("functions.file_grep"), "agentgrep");
    }

    #[test]
    fn resolve_tool_name_does_not_strip_unrecognized_namespaces() {
        assert_eq!(
            resolve_tool_name("mcp.functions.bash"),
            "mcp.functions.bash"
        );
    }

    #[test]
    fn resolve_tool_name_maps_pascalcase_oauth_aliases() {
        // Anthropic OAuth advertises PascalCase names; batch subcalls resolve
        // through here rather than the provider-side reverse map (issue #486).
        assert_eq!(resolve_tool_name("Read"), "read");
        assert_eq!(resolve_tool_name("Bash"), "bash");
        assert_eq!(resolve_tool_name("Write"), "write");
        assert_eq!(resolve_tool_name("Edit"), "edit");
        assert_eq!(resolve_tool_name("Grep"), "agentgrep");
        assert_eq!(resolve_tool_name("Agent"), "swarm");
        assert_eq!(resolve_tool_name("ScheduleWakeup"), "schedule");
        assert_eq!(resolve_tool_name("Skill"), "skill_manage");
        assert_eq!(resolve_tool_name("functions.Read"), "read");
    }

    /// Every alias must resolve to a tool that still exists.
    ///
    /// `subagent` was deleted from the registry, but four aliases (`task`,
    /// `task_runner`, `subagent`, `Agent`) still pointed at it, so a model
    /// reaching for the familiar name got `Unknown tool: subagent` instead of
    /// being routed to `swarm`. That happened in 9 real sessions before this
    /// was noticed. `glob` had no alias at all and failed the same way.
    ///
    /// The registry lives in a downstream crate, so this asserts against the
    /// known-deleted names rather than a live Registry; the app-core test
    /// `delegation_directive_only_names_tools_that_are_registered` covers the
    /// live side.
    #[test]
    fn no_alias_resolves_to_a_tool_that_was_deleted() {
        const DELETED: &[&str] = &["subagent", "grep", "glob", "task", "communicate"];
        const ALIASES: &[&str] = &[
            "task",
            "task_runner",
            "subagent",
            "Agent",
            "communicate",
            "glob",
            "Glob",
            "grep",
            "file_grep",
            "Grep",
        ];
        for alias in ALIASES {
            let resolved = resolve_tool_name(alias);
            assert!(
                !DELETED.contains(&resolved),
                "alias `{alias}` resolves to `{resolved}`, which is not a \
                 registered tool; models calling it get `Unknown tool`"
            );
        }
    }
}
