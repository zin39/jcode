use super::{Tool, ToolContext, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

/// Hard timeout for discovery requests. Discovery is optional by design: if
/// the endpoint is slow or unreachable the tool fails plainly and the agent
/// continues with its normal toolset. No cache, no offline fallback, no retry.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// `discover_tools`: fetch discoverable third-party tools for a category from
/// the hosted sponsored-discovery manifest.
///
/// Disclosure contract: sponsors buy placement (discoverability), never
/// recommendations. Every session that uses this tool renders a
/// `(sponsored discovery)` disclosure line in the UI on first use. The
/// request carries only the category, a short search query, and a reason
/// string, which the discovery service stores for transparency and billing.
/// It must never include session content or private information.
pub struct DiscoverToolsTool {
    client: reqwest::Client,
}

impl DiscoverToolsTool {
    pub fn new() -> Self {
        Self {
            client: crate::provider::shared_http_client(),
        }
    }
}

#[derive(Deserialize)]
struct DiscoverToolsInput {
    category: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    tool: Option<String>,
}

#[async_trait]
impl Tool for DiscoverToolsTool {
    fn name(&self) -> &str {
        "discover_tools"
    }

    fn description(&self) -> &str {
        "Discover third-party developer tools for a category from jcode's sponsored \
         discovery listing. Two phases: call without `tool` to browse a category, then \
         call again with `tool` set to the chosen tool's name to get its setup \
         instructions. Sponsors pay for discoverability, not recommendations: only \
         select a discovered tool when it is genuinely the best option. The category, \
         query, tool, and reason are sent to and stored by the discovery service, so \
         they must never contain private information, secrets, or session content."
    }

    fn parameters_schema(&self) -> Value {
        let categories: Vec<&str> = crate::sponsors::DISCOVERY_CATEGORIES.to_vec();
        json!({
            "type": "object",
            "required": ["category", "reason"],
            "properties": {
                "intent": super::intent_schema_property(),
                "category": {
                    "type": "string",
                    "enum": categories,
                    "description": "Tool category to discover."
                },
                "query": {
                    "type": "string",
                    "description": "Short search query describing the capability needed, e.g. 'virtual card for online checkout'. No private information."
                },
                "reason": {
                    "type": "string",
                    "description": "When browsing: why a tool from this category is needed. When selecting (with `tool`): a detailed, specific reason why this tool was chosen, covering the task that triggered the need, why this tool fits it, and what alternatives were considered (a sentence or two; generic one-liners are rejected). No private information, secrets, file paths, or user-identifying details."
                },
                "tool": {
                    "type": "string",
                    "description": "Name of a tool from a previous browse result. Returns that tool's full setup instructions and records the selection reason."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let config = crate::config::config();
        if !config.sponsors.enabled {
            return Err(anyhow::anyhow!(
                "sponsored discovery is disabled (set [sponsors] enabled = true in config.toml)"
            ));
        }

        let params: DiscoverToolsInput = serde_json::from_value(input)?;
        let category = params.category.trim().to_ascii_lowercase();
        if !crate::sponsors::DISCOVERY_CATEGORIES.contains(&category.as_str()) {
            return Err(anyhow::anyhow!(
                "unknown discovery category '{}'. Available: {}",
                category,
                crate::sponsors::DISCOVERY_CATEGORIES.join(", ")
            ));
        }

        let endpoint = config.sponsors.endpoint.clone();
        let tool_selection = params
            .tool
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_ascii_lowercase);

        // Select phase: return one tool's full setup instructions. The
        // selection (and the agent's reason for it) is recorded server-side.
        if let Some(tool_name) = tool_selection {
            validate_selection_reason(params.reason.as_deref())?;
            let listing = fetch_listing(
                &self.client,
                &endpoint,
                &category,
                params.query.as_deref(),
                params.reason.as_deref(),
                Some(&tool_name),
            )
            .await?;
            let rendered = render_selection(&category, &tool_name, &listing)?;
            crate::sponsors::provenance::record_discovered_setups(extract_mcp_setups_from(
                listing.get("tool").map(std::slice::from_ref).unwrap_or(&[]),
            ));
            return Ok(ToolOutput::new(rendered)
                .with_title(format!(
                    "{tool_name} {}",
                    crate::sponsors::SPONSORED_DISCOVERY_TAG
                ))
                .with_metadata(json!({
                    "sponsored_discovery": true,
                    "category": category,
                    "selected_tool": tool_name,
                    "disclosure_url": crate::sponsors::SPONSORED_DISCOVERY_URL,
                })));
        }

        let listing = fetch_listing(
            &self.client,
            &endpoint,
            &category,
            params.query.as_deref(),
            params.reason.as_deref(),
            None,
        )
        .await?;
        let rendered = render_listing(&category, &listing)?;

        // Remember MCP setups from this listing so a later `mcp connect`
        // matching one of them is tagged with discovery provenance (and
        // metered coarsely; see jcode_base::sponsors::provenance).
        crate::sponsors::provenance::record_discovered_setups(extract_mcp_setups(&listing));

        Ok(ToolOutput::new(rendered)
            .with_title(format!(
                "{} {}",
                category,
                crate::sponsors::SPONSORED_DISCOVERY_TAG
            ))
            .with_metadata(json!({
                "sponsored_discovery": true,
                "category": category,
                "disclosure_url": crate::sponsors::SPONSORED_DISCOVERY_URL,
            })))
    }
}

/// Minimum length for a selection-phase reason. Selection reasons are the
/// highest-value datum in the discovery funnel (why the tool won), so a
/// throwaway string is rejected with guidance instead of being stored.
const MIN_SELECTION_REASON_CHARS: usize = 80;

/// Validate that a selection reason is substantive. Length is a proxy, but
/// the error text does the real work: it tells the model exactly what to
/// cover, and models reliably re-call with a conforming reason.
fn validate_selection_reason(reason: Option<&str>) -> Result<()> {
    let reason = reason.map(str::trim).unwrap_or_default();
    if reason.chars().count() < MIN_SELECTION_REASON_CHARS {
        return Err(anyhow::anyhow!(
            "selection requires a substantive reason (at least {MIN_SELECTION_REASON_CHARS} \
             characters; got {}). Cover: what the current task needs, why this tool fits, \
             and what alternatives you considered. Do not include private information, \
             secrets, or session content.",
            reason.chars().count()
        ));
    }
    Ok(())
}

/// Fetch a category listing (browse) or one tool's entry (select) from the
/// discovery endpoint. Sends the category, an optional capability query, an
/// optional reason string, and the selected tool name only. Hard fails on
/// any error: no cache, no fallback, no retry.
async fn fetch_listing(
    client: &reqwest::Client,
    endpoint: &str,
    category: &str,
    query: Option<&str>,
    reason: Option<&str>,
    tool: Option<&str>,
) -> Result<Value> {
    let endpoint = endpoint.trim_end_matches('/');
    let mut request = client
        .get(endpoint)
        .query(&[("category", category)])
        .header(
            reqwest::header::USER_AGENT,
            format!("jcode/{}", env!("CARGO_PKG_VERSION")),
        )
        .timeout(DISCOVERY_TIMEOUT);
    if let Some(query) = query.filter(|q| !q.trim().is_empty()) {
        request = request.query(&[("q", query.trim())]);
    }
    if let Some(reason) = reason.filter(|r| !r.trim().is_empty()) {
        request = request.query(&[("reason", reason.trim())]);
    }
    if let Some(tool) = tool.filter(|t| !t.trim().is_empty()) {
        request = request.query(&[("tool", tool.trim())]);
    }

    let response = request
        .send()
        .await
        .map_err(|err| anyhow::anyhow!("discovery unavailable: {err}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow::anyhow!("discovery unavailable: HTTP {status}"));
    }
    let body = response
        .text()
        .await
        .map_err(|err| anyhow::anyhow!("discovery unavailable: {err}"))?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(anyhow::anyhow!(
            "discovery response too large ({} bytes)",
            body.len()
        ));
    }
    serde_json::from_str(&body)
        .map_err(|err| anyhow::anyhow!("discovery returned invalid JSON: {err}"))
}

/// Extract structured MCP setups (`mcp: { command, args }`) from a listing
/// for provenance matching. Entries without an `mcp` descriptor are skipped.
fn extract_mcp_setups(listing: &Value) -> Vec<crate::sponsors::provenance::DiscoveredSetup> {
    let Some(tools) = listing.get("tools").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    extract_mcp_setups_from(tools)
}

/// Extract MCP setups from a slice of tool entries.
fn extract_mcp_setups_from(tools: &[Value]) -> Vec<crate::sponsors::provenance::DiscoveredSetup> {
    tools
        .iter()
        .filter_map(|tool| {
            let sponsor = tool.get("name")?.as_str()?.trim().to_ascii_lowercase();
            let mcp = tool.get("mcp")?;
            let command = mcp.get("command")?.as_str()?.to_string();
            let args = mcp
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            Some(crate::sponsors::provenance::DiscoveredSetup {
                sponsor,
                command,
                args,
            })
        })
        .collect()
}

/// Render a discovery listing (browse phase) for the model. Expected shape:
/// `{ "tools": [{ "name": "...", "blurb": "...", "url": "..." }] }`. Setup
/// instructions are not part of browse results: the agent selects a tool
/// (with a reason) to get them.
fn render_listing(category: &str, listing: &Value) -> Result<String> {
    let tools = listing
        .get("tools")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("discovery returned no tool list"))?;
    if tools.is_empty() {
        return Ok(format!(
            "No discoverable tools in category '{category}' right now."
        ));
    }
    let mut out = format!(
        "Discoverable tools in '{category}' (sponsored discovery: placement, not preference; \
         details: {}):\n",
        crate::sponsors::SPONSORED_DISCOVERY_URL
    );
    for tool in tools {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let blurb = tool.get("blurb").and_then(|v| v.as_str()).unwrap_or("");
        out.push_str(&format!("\n- {name}: {blurb}"));
        if let Some(url) = tool.get("url").and_then(|v| v.as_str()) {
            out.push_str(&format!(" ({url})"));
        }
        if let Some(setup) = tool.get("setup").and_then(|v| v.as_str()) {
            out.push_str(&format!("\n  setup: {setup}"));
        }
    }
    out.push_str(
        "\n\nOnly select one of these if it is genuinely the best option for the task. \
         To get a tool's setup instructions, call discover_tools again with `tool` set \
         to its name and `reason` explaining in detail why it was chosen. Consequential \
         actions (signups, spending) must note the sponsorship in the confirmation \
         shown to the user.",
    );
    Ok(out)
}

/// Render a selected tool's full entry (select phase). Expected shape:
/// `{ "tool": { "name": "...", "blurb": "...", "url": "...", "setup": "..." } }`.
fn render_selection(category: &str, tool_name: &str, listing: &Value) -> Result<String> {
    let tool = listing
        .get("tool")
        .ok_or_else(|| anyhow::anyhow!("discovery returned no tool entry for '{tool_name}'"))?;
    let name = tool
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(tool_name);
    let blurb = tool.get("blurb").and_then(|v| v.as_str()).unwrap_or("");
    let mut out = format!(
        "Selected '{name}' from '{category}' (sponsored discovery: placement, not \
         preference; details: {}):\n\n{name}: {blurb}",
        crate::sponsors::SPONSORED_DISCOVERY_URL
    );
    if let Some(url) = tool.get("url").and_then(|v| v.as_str()) {
        out.push_str(&format!(" ({url})"));
    }
    if let Some(setup) = tool.get("setup").and_then(|v| v.as_str()) {
        out.push_str(&format!("\n\nSetup: {setup}"));
    }
    out.push_str(
        "\n\nConsequential actions (signups, spending) must note the sponsorship in \
         the confirmation shown to the user.",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_listing_includes_disclosure_and_tools() {
        let listing = json!({
            "tools": [
                {"name": "agentcard", "blurb": "virtual payment cards", "url": "https://agentcard.example"},
            ]
        });
        let out = render_listing("payments", &listing).unwrap();
        assert!(out.contains("agentcard"));
        assert!(out.contains("virtual payment cards"));
        assert!(out.contains("sponsored discovery"));
        assert!(out.contains("placement, not preference"));
    }

    #[test]
    fn render_listing_rejects_missing_tools() {
        assert!(render_listing("payments", &json!({})).is_err());
    }

    #[test]
    fn render_listing_handles_empty_category() {
        let out = render_listing("payments", &json!({"tools": []})).unwrap();
        assert!(out.contains("No discoverable tools"));
    }

    #[test]
    fn render_listing_instructs_selection_phase() {
        let listing = json!({
            "tools": [{"name": "agentcard", "blurb": "virtual cards", "url": "https://a.example"}]
        });
        let out = render_listing("payments", &listing).unwrap();
        assert!(out.contains("call discover_tools again with `tool`"));
    }

    #[test]
    fn selection_reason_validation_rejects_short_reasons_with_guidance() {
        let err = validate_selection_reason(Some("it fits")).unwrap_err();
        assert!(err.to_string().contains("what the current task needs"));
        assert!(validate_selection_reason(None).is_err());
        let good = "The task needs a capped single-use card for an online checkout; agentcard \
                    fits because cards are amount-capped and expire in 7 days; no other listed \
                    payments tool issues cards.";
        assert!(validate_selection_reason(Some(good)).is_ok());
    }

    #[test]
    fn render_selection_includes_setup_and_disclosure() {
        let listing = json!({
            "tool": {
                "name": "agentcard",
                "blurb": "virtual cards",
                "url": "https://a.example",
                "setup": "npm install -g agentcard"
            }
        });
        let out = render_selection("payments", "agentcard", &listing).unwrap();
        assert!(out.contains("Selected 'agentcard'"));
        assert!(out.contains("Setup: npm install -g agentcard"));
        assert!(out.contains("sponsored discovery"));
        assert!(render_selection("payments", "ghost", &json!({})).is_err());
    }

    /// Minimal one-shot HTTP server that answers a single request with the
    /// given body, returning the request line + headers it received.
    async fn one_shot_server(
        status_line: &'static str,
        body: String,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let response = format!(
                "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.ok();
            request
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn fetch_listing_round_trips_and_sends_only_expected_params() {
        let body = json!({"tools": [{"name": "agentcard", "blurb": "virtual cards", "url": "https://a.example"}]}).to_string();
        let (endpoint, server) = one_shot_server("HTTP/1.1 200 OK", body).await;
        let client = reqwest::Client::new();
        let listing = fetch_listing(
            &client,
            &endpoint,
            "payments",
            Some("virtual card for checkout"),
            Some("task needs an online payment"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(listing["tools"][0]["name"], "agentcard");

        let request = server.await.unwrap();
        let request_line = request.lines().next().unwrap();
        // Exactly the three disclosed parameters, nothing else.
        assert!(request_line.contains("category=payments"), "{request_line}");
        assert!(request_line.contains("q=virtual"), "{request_line}");
        assert!(request_line.contains("reason=task"), "{request_line}");
    }

    #[tokio::test]
    async fn fetch_listing_hard_fails_on_http_error() {
        let (endpoint, _server) =
            one_shot_server("HTTP/1.1 500 Internal Server Error", "{}".to_string()).await;
        let client = reqwest::Client::new();
        let err = fetch_listing(&client, &endpoint, "payments", None, None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("discovery unavailable"));
    }

    #[tokio::test]
    async fn fetch_listing_hard_fails_when_endpoint_unreachable() {
        // Reserved port with no listener: connection refused, no fallback.
        let client = reqwest::Client::new();
        let err = fetch_listing(&client, "http://127.0.0.1:9", "payments", None, None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("discovery unavailable"));
    }

    fn test_ctx() -> crate::tool::ToolContext {
        crate::tool::ToolContext {
            session_id: "test".into(),
            message_id: "test".into(),
            tool_call_id: "test".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    #[tokio::test]
    async fn execute_end_to_end_with_enabled_config_and_local_server() {
        let _guard = crate::storage::lock_test_env();
        let prev_home = std::env::var_os("JCODE_HOME");
        let temp = tempfile::tempdir().unwrap();
        crate::env::set_var("JCODE_HOME", temp.path());

        let body = json!({"tools": [{"name": "agentcard", "blurb": "single-use virtual visa cards", "url": "https://agentcard.example", "setup": "MCP server: npx agentcard-mcp"}]}).to_string();
        let (endpoint, _server) = one_shot_server("HTTP/1.1 200 OK", body).await;
        std::fs::write(
            temp.path().join("config.toml"),
            format!("[sponsors]\nenabled = true\nendpoint = \"{endpoint}\"\n"),
        )
        .unwrap();
        crate::config::Config::invalidate_cache();

        let tool = DiscoverToolsTool::new();
        let output = tool
            .execute(
                json!({
                    "category": "payments",
                    "query": "virtual card for checkout",
                    "reason": "task requires an online card payment"
                }),
                test_ctx(),
            )
            .await
            .unwrap();

        assert!(output.output.contains("agentcard"));
        assert!(output.output.contains("sponsored discovery"));
        assert!(output.output.contains("placement, not preference"));
        let title = output.title.unwrap();
        assert!(title.contains("(sponsored discovery)"), "{title}");
        let meta = output.metadata.unwrap();
        assert_eq!(meta["sponsored_discovery"], true);

        // Opted-out config: execute refuses without any network call.
        std::fs::write(
            temp.path().join("config.toml"),
            "[sponsors]\nenabled = false\n",
        )
        .unwrap();
        crate::config::Config::invalidate_cache();
        let err = tool
            .execute(json!({"category": "payments", "reason": "x"}), test_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));

        if let Some(prev) = prev_home {
            crate::env::set_var("JCODE_HOME", prev);
        } else {
            crate::env::remove_var("JCODE_HOME");
        }
        crate::config::Config::invalidate_cache();
    }
}
