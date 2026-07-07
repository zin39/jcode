use super::{Tool, ToolContext, ToolOutput};
use crate::config::WebSearchEngine;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

/// Web search using DuckDuckGo or Bing (HTML scraping, with optional Bing API)
pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: crate::provider::shared_http_client(),
        }
    }
}

#[derive(Deserialize)]
struct WebSearchInput {
    query: String,
    #[serde(default)]
    num_results: Option<usize>,
    #[serde(default)]
    engine: Option<WebSearchEngine>,
    #[serde(default)]
    bing_market: Option<String>,
}

#[derive(Debug, Clone)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    /// Per-engine quality/relevance score in roughly 0..1 (0 when the engine
    /// only provides an implicit rank ordering, e.g. DuckDuckGo/Bing HTML).
    score: f32,
    /// Which engine(s) surfaced this result. Used by the hybrid engine to
    /// reward cross-engine agreement ("verified") in the final ranking.
    engines: Vec<String>,
}

impl SearchResult {
    /// Construct a rank-ordered result (no explicit relevance score).
    fn basic(title: String, url: String, snippet: String) -> Self {
        Self {
            title,
            url,
            snippet,
            score: 0.0,
            engines: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct BingSearchOptions<'a> {
    market: &'a str,
    configured_api_key: Option<&'a str>,
    api_key_env: &'a str,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "websearch"
    }

    fn description(&self) -> &str {
        "Search the web."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "intent": super::intent_schema_property(),
                "query": {
                    "type": "string",
                    "description": "Search query."
                },
                "num_results": {
                    "type": "integer",
                    "description": "Max results."
                },
                "engine": {
                    "type": "string",
                    "enum": ["hybrid", "tavily", "last30days", "duckduckgo", "bing", "searxng"],
                    "description": "Search engine. Defaults to hybrid, which runs Tavily (fast, keyed) and the last30days skill (deep social: Reddit/HN/GitHub/etc.) concurrently, cross-verifies their results, and returns one best-first ranking (results found by both engines are marked verified). Use tavily for a fast keyed search, last30days for deep engagement-scored social research, duckduckgo/bing for keyless HTML search, or searxng for a self-hosted instance (JCODE_SEARXNG_URL)."
                },
                "bing_market": {
                    "type": "string",
                    "description": "Optional Bing market, e.g. en-US or zh-CN. Defaults to JCODE_BING_MARKET or en-US."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: WebSearchInput = serde_json::from_value(input)?;
        let num_results = params.num_results.unwrap_or(8).min(20);

        let config = crate::config::config();
        let mut engines = Vec::new();
        engines.push(params.engine.unwrap_or(config.websearch.engine));
        engines.extend(config.websearch.fallback_engines.iter().copied());
        engines.dedup();

        let market = params
            .bing_market
            .as_deref()
            .unwrap_or(&config.websearch.bing_market);
        let mut last_error = None;
        let mut results = Vec::new();
        for (index, engine) in engines.into_iter().enumerate() {
            let allow_bing_api = index == 0;
            match self
                .search_with_engine(
                    engine,
                    &params.query,
                    num_results,
                    BingSearchOptions {
                        market,
                        configured_api_key: config.websearch.bing_api_key.as_deref(),
                        api_key_env: &config.websearch.bing_api_key_env,
                    },
                    allow_bing_api,
                )
                .await
            {
                Ok(found) => {
                    if !found.is_empty() {
                        results = found;
                        break;
                    }
                }
                Err(err) => last_error = Some(err),
            }
        }

        if results.is_empty()
            && let Some(err) = last_error
        {
            return Err(err);
        }

        if results.is_empty() {
            return Ok(ToolOutput::new(format!(
                "No results found for: {}\n\n\
                 If results are consistently empty on this machine, the default \
                 DuckDuckGo/Bing engines may be blocked here by TLS fingerprinting \
                 or IP reputation (common on Linux/servers). Workarounds:\n\
                 - Point at a SearXNG instance: set `websearch.searxng_url` (or \
                 JCODE_SEARXNG_URL) and use engine \"searxng\".\n\
                 - Or provide a Bing Search API key via JCODE_BING_API_KEY.",
                params.query
            )));
        }

        let mut output = format!("Search results for: {}\n\n", params.query);

        for (i, result) in results.iter().enumerate() {
            let badge = render_result_badge(result);
            output.push_str(&format!(
                "{}. **{}**{}\n   {}\n   {}\n\n",
                i + 1,
                result.title,
                badge,
                result.url,
                result.snippet
            ));
        }

        Ok(ToolOutput::new(output))
    }
}

/// Render a short provenance/quality badge for a merged result: which engines
/// surfaced it and whether independent engines agreed ("verified"). Returns an
/// empty string when there is no attribution to show (single-engine searches).
fn render_result_badge(result: &SearchResult) -> String {
    if result.engines.is_empty() {
        return String::new();
    }
    let verified = result.engines.len() > 1;
    let engines = result.engines.join("+");
    if verified {
        format!("  _[✓ verified · {engines}]_")
    } else {
        format!("  _[{engines}]_")
    }
}

impl WebSearchTool {
    async fn search_with_engine(
        &self,
        engine: WebSearchEngine,
        query: &str,
        num_results: usize,
        bing: BingSearchOptions<'_>,
        allow_bing_api: bool,
    ) -> Result<Vec<SearchResult>> {
        match engine {
            WebSearchEngine::Duckduckgo => self.search_duckduckgo(query, num_results).await,
            WebSearchEngine::Bing => {
                self.search_bing(query, num_results, bing, allow_bing_api)
                    .await
            }
            WebSearchEngine::Searxng => self.search_searxng(query, num_results).await,
            WebSearchEngine::Tavily => self.search_tavily(query, num_results).await,
            WebSearchEngine::Last30days => self.search_last30days(query, num_results).await,
            WebSearchEngine::Hybrid => self.search_hybrid(query, num_results).await,
        }
    }

    async fn search_duckduckgo(
        &self,
        query: &str,
        num_results: usize,
    ) -> Result<Vec<SearchResult>> {
        // DuckDuckGo's HTML endpoint now serves an anti-bot "anomaly" challenge
        // (HTTP 202, no results) for plain GET requests. Submitting the query as
        // a POST form, the same way the real HTML page does, still returns the
        // standard results markup with a 200.
        let response = self
            .client
            .post("https://html.duckduckgo.com/html/")
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .form(&[("q", query), ("kl", "us-en")])
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Search failed with status: {}",
                response.status()
            ));
        }

        let body = response.text().await?;
        let results = parse_ddg_results(&body, num_results);
        if results.is_empty()
            && let Some(reason) = detect_anti_bot_page(&body)
        {
            return Err(anyhow::anyhow!(
                "DuckDuckGo served an anti-bot challenge page ({reason}) instead of \
                 results. This is commonly caused by TLS fingerprinting or IP \
                 reputation on Linux. Falling back to another engine if configured."
            ));
        }

        Ok(results)
    }

    async fn search_bing(
        &self,
        query: &str,
        num_results: usize,
        options: BingSearchOptions<'_>,
        allow_api: bool,
    ) -> Result<Vec<SearchResult>> {
        if allow_api {
            if let Some(api_key) = options
                .configured_api_key
                .filter(|key| !key.trim().is_empty())
            {
                return self
                    .search_bing_api(query, num_results, options.market, api_key)
                    .await;
            }
            if let Ok(api_key) = std::env::var(options.api_key_env)
                && !api_key.trim().is_empty()
            {
                return self
                    .search_bing_api(query, num_results, options.market, &api_key)
                    .await;
            }
        }

        self.search_bing_html(query, num_results, options.market)
            .await
    }

    async fn search_bing_api(
        &self,
        query: &str,
        num_results: usize,
        market: &str,
        api_key: &str,
    ) -> Result<Vec<SearchResult>> {
        let response = self
            .client
            .get("https://api.bing.microsoft.com/v7.0/search")
            .query(&[
                ("q", query),
                ("count", &num_results.to_string()),
                ("mkt", market),
            ])
            .header("Ocp-Apim-Subscription-Key", api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Bing API search failed with status: {}",
                response.status()
            ));
        }

        Ok(parse_bing_api_results(response.json().await?, num_results))
    }

    async fn search_bing_html(
        &self,
        query: &str,
        num_results: usize,
        market: &str,
    ) -> Result<Vec<SearchResult>> {
        let url = format!(
            "https://www.bing.com/search?q={}&mkt={}",
            urlencoding::encode(query),
            urlencoding::encode(market)
        );

        let response = self
            .client
            .get(&url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
            )
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Bing search failed with status: {}",
                response.status()
            ));
        }

        let body = response.text().await?;
        let results = parse_bing_html_results(&body, num_results);
        if results.is_empty()
            && let Some(reason) = detect_anti_bot_page(&body)
        {
            return Err(anyhow::anyhow!(
                "Bing served an anti-bot challenge page ({reason}) instead of results."
            ));
        }

        Ok(results)
    }

    /// Query a user-configured SearXNG instance via its JSON API. SearXNG is a
    /// self-hostable metasearch engine; because the request goes to an instance
    /// the user controls (or a public one they trust), it sidesteps the TLS
    /// fingerprinting / IP-reputation blocks that DuckDuckGo and Bing apply to
    /// scraped requests on some hosts (see issue #270).
    async fn search_searxng(&self, query: &str, num_results: usize) -> Result<Vec<SearchResult>> {
        let config = crate::config::config();
        let base = config
            .websearch
            .searxng_url
            .as_deref()
            .filter(|u| !u.trim().is_empty())
            .map(|u| u.to_string())
            .or_else(|| {
                std::env::var(&config.websearch.searxng_url_env)
                    .ok()
                    .filter(|u| !u.trim().is_empty())
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "SearXNG engine selected but no instance URL configured. Set \
                     `websearch.searxng_url` in your config or the {} environment \
                     variable to a SearXNG base URL (e.g. https://searx.example.org).",
                    config.websearch.searxng_url_env
                )
            })?;

        let endpoint = format!("{}/search", base.trim_end_matches('/'));
        let response = self
            .client
            .get(&endpoint)
            .query(&[("q", query), ("format", "json")])
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "SearXNG search failed with status {} (endpoint: {endpoint}). \
                 Ensure the instance has the JSON format enabled in its settings.",
                response.status()
            ));
        }

        let parsed: SearxngResponse = response.json().await.map_err(|err| {
            anyhow::anyhow!(
                "SearXNG returned a non-JSON response ({err}). The instance may have \
                 the JSON format disabled; enable `formats: [html, json]` in its settings."
            )
        })?;

        Ok(parse_searxng_results(parsed, num_results))
    }

    /// Query the Tavily Search API. Fast, keyed engine. Tavily returns a
    /// per-result relevance `score` in 0..1 which we carry through for ranking.
    ///
    /// Supports multiple API keys (comma-separated `TAVILY_API_KEYS`). Keys are
    /// tried in a rotating order so load spreads round-robin, and a key that is
    /// out of quota or invalid (HTTP 401/402/403/429/432) fails over to the next
    /// key instead of failing the search. Only when every key is exhausted does
    /// the search return an error.
    async fn search_tavily(&self, query: &str, num_results: usize) -> Result<Vec<SearchResult>> {
        let config = crate::config::config();
        let keys = resolve_tavily_keys(&config.websearch);
        if keys.is_empty() {
            return Err(anyhow::anyhow!(
                "Tavily engine selected but no API key found. Set \
                 `websearch.tavily_api_key`, the {} environment variable, or add \
                 a `TAVILY_API_KEYS=...` line to ~/.jcode/tavily.env.",
                config.websearch.tavily_api_key_env
            ));
        }

        let depth = if config.websearch.tavily_search_depth.trim() == "advanced" {
            "advanced"
        } else {
            "basic"
        };

        let ordered = tavily_keys_for_call(&keys);
        let total = ordered.len();
        let mut last_error: Option<anyhow::Error> = None;
        for (idx, api_key) in ordered.into_iter().enumerate() {
            match self.search_tavily_once(query, num_results, depth, &api_key).await {
                Ok(results) => return Ok(results),
                Err(err) => {
                    // Only fail over across keys for auth/quota-class failures;
                    // for those, a different key may still have capacity.
                    if err.is_key_exhausted && idx + 1 < total {
                        crate::logging::info(&format!(
                            "Tavily key {}/{} exhausted or rejected (status {:?}); failing over to next key",
                            idx + 1,
                            total,
                            err.status
                        ));
                        last_error = Some(err.into_error());
                        continue;
                    }
                    return Err(err.into_error());
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("Tavily search failed: all {} key(s) exhausted", total)
        }))
    }

    /// Single Tavily request with one key. Classifies auth/quota failures so the
    /// caller can decide whether to fail over to another key.
    async fn search_tavily_once(
        &self,
        query: &str,
        num_results: usize,
        depth: &str,
        api_key: &str,
    ) -> std::result::Result<Vec<SearchResult>, TavilyKeyError> {
        let response = self
            .client
            .post("https://api.tavily.com/search")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", api_key),
            )
            .json(&json!({
                "query": query,
                "search_depth": depth,
                "max_results": num_results,
                "include_answer": false,
            }))
            .send()
            .await
            .map_err(|e| TavilyKeyError {
                status: None,
                // Network errors are not key-specific; do not burn through every
                // key on a transient outage.
                is_key_exhausted: false,
                message: format!("Tavily request failed: {}", e),
            })?;

        let status = response.status();
        if !status.is_success() {
            // 401 invalid key, 402 payment required, 403 forbidden, 429 rate
            // limited, 432 Tavily's plan-limit-exceeded code: another key may
            // still work, so mark these as "exhausted" to trigger failover.
            let is_key_exhausted = matches!(status.as_u16(), 401 | 402 | 403 | 429 | 432);
            return Err(TavilyKeyError {
                status: Some(status.as_u16()),
                is_key_exhausted,
                message: format!("Tavily API search failed with status: {}", status),
            });
        }

        let parsed: TavilyResponse = response.json().await.map_err(|e| TavilyKeyError {
            status: Some(status.as_u16()),
            is_key_exhausted: false,
            message: format!("Tavily response parse failed: {}", e),
        })?;
        Ok(parse_tavily_results(parsed, num_results))
    }

    /// Run the installed last30days skill for a deep, engagement-scored search
    /// across Reddit/HN/GitHub/etc. Slow (tens of seconds), so callers should
    /// bound it with a timeout (the hybrid engine does). Shells out to the
    /// bundled Python engine and parses its `--emit=json` output.
    async fn search_last30days(
        &self,
        query: &str,
        num_results: usize,
    ) -> Result<Vec<SearchResult>> {
        let config = crate::config::config();
        if !config.websearch.last30days_enabled {
            return Err(anyhow::anyhow!(
                "last30days engine is disabled (websearch.last30days_enabled = false)."
            ));
        }
        let script = locate_last30days_script(&config.websearch).ok_or_else(|| {
            anyhow::anyhow!(
                "last30days engine selected but the skill is not installed. Install it \
                 under ~/.jcode/skills/last30days/ or set `websearch.last30days_script` \
                 to the path of last30days.py."
            )
        })?;

        let out_dir = std::env::temp_dir().join(format!("jcode-l30d-{}", std::process::id()));
        let _ = tokio::fs::create_dir_all(&out_dir).await;
        let out_file = out_dir.join(format!(
            "{}.json",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));

        let mut cmd = tokio::process::Command::new("python3");
        cmd.arg(&script)
            .arg(query)
            .arg("--quick")
            .arg("--emit=json")
            .arg("--search")
            .arg(&config.websearch.last30days_sources)
            .arg("--output")
            .arg(&out_file)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Keep the deep engine to keyless sources unless the operator opted in
        // via config; never let it read browser cookies implicitly.
        cmd.env("LAST30DAYS_NO_BROWSER_COOKIES", "1");

        let status = cmd
            .status()
            .await
            .map_err(|e| anyhow::anyhow!("failed to launch last30days engine: {e}"))?;
        if !status.success() {
            let _ = tokio::fs::remove_file(&out_file).await;
            return Err(anyhow::anyhow!(
                "last30days engine exited with status {status}"
            ));
        }

        let body = tokio::fs::read_to_string(&out_file)
            .await
            .map_err(|e| anyhow::anyhow!("last30days produced no output file: {e}"))?;
        let _ = tokio::fs::remove_file(&out_file).await;

        let parsed: Last30daysResponse = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("failed to parse last30days output: {e}"))?;
        Ok(parse_last30days_results(parsed, num_results))
    }

    /// Hybrid engine: run the fast (Tavily) and deep (last30days) engines
    /// concurrently, then merge, cross-verify, and rank. Results surfaced by
    /// both independent engines are treated as higher-confidence ("verified")
    /// and float to the top. The deep engine is bounded by a timeout so the
    /// search always returns promptly with whatever the fast engine found.
    async fn search_hybrid(&self, query: &str, num_results: usize) -> Result<Vec<SearchResult>> {
        let config = crate::config::config();
        // Ask each leg for extra results so the merged/verified set is rich.
        let leg_results = num_results.saturating_mul(2).max(num_results);

        let fast = self.search_tavily(query, leg_results);

        let deep_enabled =
            config.websearch.last30days_enabled && locate_last30days_script(&config.websearch).is_some();
        let timeout = std::time::Duration::from_secs(config.websearch.last30days_timeout_secs.max(1));

        let (fast_res, deep_res) = if deep_enabled {
            let deep = tokio::time::timeout(timeout, self.search_last30days(query, leg_results));
            let (fast_res, deep_timed) = tokio::join!(fast, deep);
            let deep_res = match deep_timed {
                Ok(inner) => inner,
                Err(_) => Err(anyhow::anyhow!(
                    "last30days engine timed out after {}s",
                    timeout.as_secs()
                )),
            };
            (fast_res, deep_res)
        } else {
            (fast.await, Err(anyhow::anyhow!("last30days engine unavailable")))
        };

        let mut legs: Vec<(String, Vec<SearchResult>)> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        match fast_res {
            Ok(r) if !r.is_empty() => legs.push(("tavily".to_string(), r)),
            Ok(_) => {}
            Err(e) => errors.push(format!("tavily: {e}")),
        }
        match deep_res {
            Ok(r) if !r.is_empty() => legs.push(("last30days".to_string(), r)),
            Ok(_) => {}
            Err(e) => errors.push(format!("last30days: {e}")),
        }

        if legs.is_empty() {
            // Both hybrid legs failed; let the caller fall through to keyless
            // fallback engines (DuckDuckGo/Bing) configured in fallback_engines.
            return Err(anyhow::anyhow!(
                "hybrid search found no results ({})",
                if errors.is_empty() {
                    "no engines available".to_string()
                } else {
                    errors.join("; ")
                }
            ));
        }

        Ok(merge_and_rank(legs, num_results))
    }
}

/// Resolve the ordered list of Tavily API keys from config, the configured env
/// var, or `~/.jcode/tavily.env`. `TAVILY_API_KEYS` may hold a comma-separated
/// list; every non-empty key is returned so the caller can rotate across keys
/// and fail over when one is exhausted (each free key has its own quota).
/// Duplicates are removed while preserving order.
fn resolve_tavily_keys(cfg: &crate::config::WebSearchConfig) -> Vec<String> {
    fn split_keys(raw: &str, out: &mut Vec<String>) {
        for k in raw.split(',').map(str::trim) {
            if !k.is_empty() && !out.iter().any(|existing| existing == k) {
                out.push(k.to_string());
            }
        }
    }
    let mut keys = Vec::new();
    if let Some(raw) = cfg.tavily_api_key.as_deref() {
        split_keys(raw, &mut keys);
    }
    if let Some(raw) = jcode_provider_env::load_env_value_from_env_or_config(
        &cfg.tavily_api_key_env,
        "tavily.env",
    ) {
        split_keys(&raw, &mut keys);
    }
    keys
}

/// Global round-robin cursor so successive Tavily searches start on a different
/// key. This spreads load across the configured keys instead of always draining
/// the first one, which matters for free-tier keys that each have a small quota.
static TAVILY_KEY_CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Order the resolved keys for this call: start at the rotating cursor and wrap,
/// so load spreads round-robin while every key is still tried on failover.
fn tavily_keys_for_call(keys: &[String]) -> Vec<String> {
    if keys.len() <= 1 {
        return keys.to_vec();
    }
    let start = TAVILY_KEY_CURSOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % keys.len();
    keys.iter()
        .cycle()
        .skip(start)
        .take(keys.len())
        .cloned()
        .collect()
}

/// Outcome of a single-key Tavily attempt. `is_key_exhausted` marks
/// auth/quota-class failures (HTTP 401/402/403/429/432) where trying a
/// different key may still succeed, versus transient/parse errors that should
/// not burn through every configured key.
struct TavilyKeyError {
    status: Option<u16>,
    is_key_exhausted: bool,
    message: String,
}

impl TavilyKeyError {
    fn into_error(self) -> anyhow::Error {
        anyhow::anyhow!(self.message)
    }
}

/// Locate the last30days engine script: explicit config override first, then
/// the global (`~/.jcode/skills`) and project-local (`./.jcode/skills`) skill
/// install locations.
fn locate_last30days_script(cfg: &crate::config::WebSearchConfig) -> Option<std::path::PathBuf> {
    if let Some(path) = cfg.last30days_script.as_deref().filter(|p| !p.trim().is_empty()) {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }
    let rel = "skills/last30days/scripts/last30days.py";
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // Global install: ~/.jcode/skills/... (jcode_dir), matching where the skill
    // loader looks. Fall back to the app config dir for older layouts.
    if let Ok(dir) = jcode_storage::jcode_dir() {
        candidates.push(dir.join(rel));
    }
    if let Ok(dir) = jcode_storage::app_config_dir() {
        candidates.push(dir.join(rel));
    }
    // Project-local install: ./.jcode/skills/...
    candidates.push(std::path::PathBuf::from(".jcode").join(rel));
    candidates.into_iter().find(|p| p.exists())
}

/// Normalize a URL for cross-engine dedup: lowercase host, drop scheme, `www.`,
/// query string, fragment, and any trailing slash. Two results pointing at the
/// same page from different engines then collapse into one "verified" entry.
fn normalize_url(url: &str) -> String {
    let mut s = url.trim();
    for prefix in ["https://", "http://", "//"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
            break;
        }
    }
    s = s.strip_prefix("www.").unwrap_or(s);
    let s = s.split(['?', '#']).next().unwrap_or(s);
    s.trim_end_matches('/').to_ascii_lowercase()
}

/// Merge results from multiple engine legs into a single best-first ranking.
///
/// Ranking blends three signals:
/// - The engine's own relevance score (Tavily provides one; last30days scores
///   are normalized into 0..1 upstream).
/// - Rank within each leg (top results score higher).
/// - Cross-engine agreement: a URL returned by more than one independent engine
///   gets a large boost, because independent corroboration is the strongest
///   available quality signal ("verify both, choose the great ones").
fn merge_and_rank(
    legs: Vec<(String, Vec<SearchResult>)>,
    num_results: usize,
) -> Vec<SearchResult> {
    use std::collections::HashMap;

    struct Merged {
        result: SearchResult,
        best_score: f32,
        agreement: f32,
        order: usize,
    }

    let mut by_url: HashMap<String, Merged> = HashMap::new();
    let mut order_counter = 0usize;

    for (engine, results) in legs {
        let leg_len = results.len().max(1) as f32;
        for (idx, mut result) in results.into_iter().enumerate() {
            let key = normalize_url(&result.url);
            if key.is_empty() {
                continue;
            }
            // Rank contribution: 1.0 for the top hit, decaying toward 0.
            let rank_score = 1.0 - (idx as f32 / leg_len);
            // Prefer the engine's own score when present, else the rank score.
            let leg_score = if result.score > 0.0 {
                0.5 * result.score + 0.5 * rank_score
            } else {
                rank_score
            };

            match by_url.get_mut(&key) {
                Some(existing) => {
                    // Same page from another engine: record agreement and keep
                    // the richer snippet / higher score.
                    if !existing.result.engines.contains(&engine) {
                        existing.result.engines.push(engine.clone());
                        existing.agreement += 1.0;
                    }
                    if leg_score > existing.best_score {
                        existing.best_score = leg_score;
                    }
                    if result.snippet.len() > existing.result.snippet.len() {
                        existing.result.snippet = result.snippet.clone();
                    }
                    if existing.result.title.trim().is_empty() {
                        existing.result.title = result.title.clone();
                    }
                }
                None => {
                    result.engines = vec![engine.clone()];
                    by_url.insert(
                        key,
                        Merged {
                            result,
                            best_score: leg_score,
                            agreement: 0.0,
                            order: order_counter,
                        },
                    );
                    order_counter += 1;
                }
            }
        }
    }

    let mut merged: Vec<Merged> = by_url.into_values().collect();
    // Final score: leg quality + a strong bonus per additional agreeing engine.
    for m in merged.iter_mut() {
        m.result.score = m.best_score + m.agreement * 1.0;
    }
    merged.sort_by(|a, b| {
        b.result
            .score
            .partial_cmp(&a.result.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.order.cmp(&b.order))
    });

    merged
        .into_iter()
        .take(num_results)
        .map(|m| m.result)
        .collect()
}

/// Map a parsed SearXNG JSON response to `SearchResult`s, dropping entries with
/// empty URLs and capping to `num_results`.
fn parse_searxng_results(response: SearxngResponse, num_results: usize) -> Vec<SearchResult> {
    response
        .results
        .into_iter()
        .filter(|r| !r.url.trim().is_empty())
        .take(num_results)
        .map(|r| {
            SearchResult::basic(
                if r.title.trim().is_empty() {
                    r.url.clone()
                } else {
                    r.title
                },
                r.url,
                r.content.unwrap_or_default(),
            )
        })
        .collect()
}

mod search_regex {
    use regex::Regex;
    use std::sync::OnceLock;

    fn compile_regex(pattern: &str, label: &str) -> Option<Regex> {
        match Regex::new(pattern) {
            Ok(regex) => Some(regex),
            Err(err) => {
                crate::logging::warn(&format!(
                    "websearch: failed to compile static regex {label}: {}",
                    err
                ));
                None
            }
        }
    }

    macro_rules! static_regex {
        ($name:ident, $pat:expr_2021) => {
            pub fn $name() -> Option<&'static Regex> {
                static RE: OnceLock<Option<Regex>> = OnceLock::new();
                RE.get_or_init(|| compile_regex($pat, stringify!($name)))
                    .as_ref()
            }
        };
    }

    static_regex!(
        result_link,
        r#"(?s)<a[^>]*class="result__a"[^>]*href="([^"]*)"[^>]*>(.*?)</a>"#
    );
    static_regex!(
        result_snippet,
        r#"(?s)<a[^>]*class="result__snippet"[^>]*>(.*?)</a>"#
    );
    static_regex!(tag, r"<[^>]+>");
    static_regex!(
        bing_result_block,
        r#"(?s)<li[^>]*class="[^"]*\bb_algo\b[^"]*"[^>]*>(.*?)</li>"#
    );
    static_regex!(
        bing_link,
        r#"(?s)<h2[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>\s*</h2>"#
    );
    static_regex!(
        bing_caption,
        r#"(?s)<div[^>]*class="[^"]*\bb_caption\b[^"]*"[^>]*>.*?<p[^>]*>(.*?)</p>"#
    );
}

#[derive(Deserialize)]
struct SearxngResponse {
    #[serde(default)]
    results: Vec<SearxngResult>,
}

#[derive(Deserialize)]
struct SearxngResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: Option<String>,
}

// ---- Tavily ----------------------------------------------------------------

#[derive(Deserialize)]
struct TavilyResponse {
    #[serde(default)]
    results: Vec<TavilyResult>,
}

#[derive(Deserialize)]
struct TavilyResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    score: f32,
}

/// Map a parsed Tavily response into `SearchResult`s, preserving Tavily's
/// per-result relevance `score` (already in 0..1) for hybrid ranking.
fn parse_tavily_results(response: TavilyResponse, num_results: usize) -> Vec<SearchResult> {
    response
        .results
        .into_iter()
        .filter(|r| !r.url.trim().is_empty())
        .take(num_results)
        .map(|r| SearchResult {
            title: if r.title.trim().is_empty() {
                r.url.clone()
            } else {
                r.title
            },
            url: r.url,
            snippet: r.content,
            score: r.score.clamp(0.0, 1.0),
            engines: Vec::new(),
        })
        .collect()
}

// ---- last30days ------------------------------------------------------------

#[derive(Deserialize)]
struct Last30daysResponse {
    #[serde(default)]
    ranked_candidates: Vec<Last30daysItem>,
    #[serde(default)]
    items_by_source: std::collections::HashMap<String, Vec<Last30daysItem>>,
}

#[derive(Deserialize, Clone)]
struct Last30daysItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    container: String,
    #[serde(default)]
    source: String,
    /// Blended local rank score (roughly 0..1) emitted by the engine.
    #[serde(default)]
    local_rank_score: f32,
}

/// Map last30days engine output into `SearchResult`s. Prefers the pre-ranked
/// `ranked_candidates`; falls back to flattening `items_by_source`. The engine's
/// `local_rank_score` (0..1) is carried through as the relevance score, and the
/// social container/source is folded into the snippet so the model sees where a
/// result came from (e.g. "r/OpenAI" or "Hacker News").
fn parse_last30days_results(
    response: Last30daysResponse,
    num_results: usize,
) -> Vec<SearchResult> {
    let mut items = response.ranked_candidates;
    if items.is_empty() {
        let mut flattened: Vec<Last30daysItem> = response
            .items_by_source
            .into_values()
            .flatten()
            .collect();
        flattened.sort_by(|a, b| {
            b.local_rank_score
                .partial_cmp(&a.local_rank_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        items = flattened;
    }

    items
        .into_iter()
        .filter(|it| !it.url.trim().is_empty())
        .take(num_results)
        .map(|it| {
            let title = if !it.title.trim().is_empty() {
                it.title.clone()
            } else if !it.body.trim().is_empty() {
                it.body.lines().next().unwrap_or_default().to_string()
            } else {
                it.url.clone()
            };
            let mut snippet = if !it.snippet.trim().is_empty() {
                it.snippet.clone()
            } else {
                it.body.clone()
            };
            let origin = if !it.container.trim().is_empty() {
                it.container.clone()
            } else {
                it.source.clone()
            };
            if !origin.trim().is_empty() {
                snippet = if snippet.trim().is_empty() {
                    format!("[{origin}]")
                } else {
                    format!("[{origin}] {snippet}")
                };
            }
            SearchResult {
                title,
                url: it.url,
                snippet,
                score: it.local_rank_score.clamp(0.0, 1.0),
                engines: Vec::new(),
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct BingApiResponse {
    #[serde(rename = "webPages")]
    web_pages: Option<BingWebPages>,
}

#[derive(Deserialize)]
struct BingWebPages {
    value: Vec<BingWebPage>,
}

#[derive(Deserialize)]
struct BingWebPage {
    name: String,
    url: String,
    #[serde(default)]
    snippet: String,
}

fn parse_bing_api_results(response: BingApiResponse, max_results: usize) -> Vec<SearchResult> {
    response
        .web_pages
        .map(|pages| {
            pages
                .value
                .into_iter()
                .take(max_results)
                .map(|page| SearchResult::basic(page.name, page.url, page.snippet))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_bing_html_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let (Some(block_re), Some(link_re), Some(caption_re), Some(tag_re)) = (
        search_regex::bing_result_block(),
        search_regex::bing_link(),
        search_regex::bing_caption(),
        search_regex::tag(),
    ) else {
        return results;
    };

    for block in block_re.captures_iter(html) {
        if results.len() >= max_results {
            break;
        }
        let Some(link) = link_re.captures(&block[1]) else {
            continue;
        };
        let url = html_decode(&link[1]);
        if !url.starts_with("http") || url.contains("bing.com") {
            continue;
        }
        let title = html_decode(&tag_re.replace_all(&link[2], ""));
        let snippet = caption_re
            .captures(&block[1])
            .map(|cap| html_decode(&tag_re.replace_all(&cap[1], "")))
            .unwrap_or_default();
        results.push(SearchResult::basic(title, url, snippet));
    }

    results
}

fn parse_ddg_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    let (Some(result_link), Some(result_snippet), Some(tag)) = (
        search_regex::result_link(),
        search_regex::result_snippet(),
        search_regex::tag(),
    ) else {
        return results;
    };

    let links: Vec<_> = result_link.captures_iter(html).collect();
    let snippets: Vec<_> = result_snippet.captures_iter(html).collect();

    for (i, link_cap) in links.iter().enumerate() {
        if results.len() >= max_results {
            break;
        }

        let url = decode_ddg_url(&link_cap[1]);
        let title = html_decode(&tag.replace_all(&link_cap[2], ""));

        if !url.starts_with("http") || url.contains("duckduckgo.com") {
            continue;
        }

        let snippet = if i < snippets.len() {
            let raw = &snippets[i][1];
            html_decode(&tag.replace_all(raw, ""))
        } else {
            String::new()
        };

        results.push(SearchResult::basic(title, url, snippet));
    }

    results
}

/// Detect whether an HTML body is an anti-bot/captcha challenge rather than a
/// real results page. DuckDuckGo (and similar) serve these with HTTP 200, so a
/// successful status plus zero parsed results is ambiguous without this check.
///
/// Returns a short human-readable reason when a challenge page is detected.
fn detect_anti_bot_page(html: &str) -> Option<&'static str> {
    let lowered = html.to_ascii_lowercase();
    const MARKERS: &[(&str, &str)] = &[
        ("anomaly-modal", "anomaly challenge"),
        ("anomaly.js", "anomaly challenge"),
        ("dpn=1", "anomaly challenge"),
        ("captcha", "captcha"),
        ("g-recaptcha", "recaptcha"),
        ("are you a robot", "bot check"),
        ("unusual traffic", "bot check"),
        ("verify you are human", "human verification"),
        ("challenge-platform", "cloudflare challenge"),
        ("cf-challenge", "cloudflare challenge"),
    ];
    for (needle, reason) in MARKERS {
        if lowered.contains(needle) {
            return Some(reason);
        }
    }
    None
}

fn decode_ddg_url(url: &str) -> String {
    // DDG wraps URLs like //duckduckgo.com/l/?uddg=ACTUAL_URL&...
    if let Some(uddg_start) = url.find("uddg=") {
        let start = uddg_start + 5;
        let end = url[start..]
            .find('&')
            .map(|i| start + i)
            .unwrap_or(url.len());
        let encoded = &url[start..end];
        urlencoding::decode(encoded)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| encoded.to_string())
    } else {
        url.to_string()
    }
}

fn html_decode(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bing_html_results() {
        let html = r#"
            <li class="b_algo">
              <h2><a href="https://example.com/rust">Rust &amp; Cargo</a></h2>
              <div class="b_caption"><p>A <strong>systems</strong> language.</p></div>
            </li>
            <li class="b_algo"><h2><a href="https://www.bing.com/aclk">ad</a></h2></li>
            <li class="b_algo">
              <h2><a href="https://example.org/jcode">Jcode</a></h2>
              <div class="b_caption"><p>Agentic coding.</p></div>
            </li>
        "#;

        let results = parse_bing_html_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust & Cargo");
        assert_eq!(results[0].url, "https://example.com/rust");
        assert_eq!(results[0].snippet, "A systems language.");
        assert_eq!(results[1].title, "Jcode");
    }

    #[test]
    fn parses_bing_api_results() {
        let response: BingApiResponse = serde_json::from_value(json!({
            "webPages": {
                "value": [
                    {"name": "One", "url": "https://one.test", "snippet": "first"},
                    {"name": "Two", "url": "https://two.test", "snippet": "second"}
                ]
            }
        }))
        .unwrap();

        let results = parse_bing_api_results(response, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "One");
        assert_eq!(results[0].url, "https://one.test");
    }

    #[test]
    fn parses_ddg_html_results() {
        // Mirrors the markup html.duckduckgo.com returns for the POST form,
        // where titles and snippets contain inline <b> highlight tags.
        let html = r#"
            <div class="result results_links results_links_deep web-result">
              <a class="result__a" href="https://rust-lang.org/"><b>Rust</b> Language</a>
              <a class="result__snippet" href="https://rust-lang.org/">A <b>systems</b> programming language.</a>
            </div>
            <div class="result results_links results_links_deep web-result">
              <a class="result__a" href="https://en.wikipedia.org/wiki/Rust">Rust on Wikipedia</a>
              <a class="result__snippet" href="https://en.wikipedia.org/wiki/Rust">Encyclopedia <b>entry</b>.</a>
            </div>
        "#;

        let results = parse_ddg_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert_eq!(results[0].snippet, "A systems programming language.");
        assert_eq!(results[1].url, "https://en.wikipedia.org/wiki/Rust");
        assert_eq!(results[1].snippet, "Encyclopedia entry.");
    }

    #[test]
    fn websearch_engine_accepts_aliases() {
        assert_eq!(
            WebSearchEngine::parse("ddg"),
            Some(WebSearchEngine::Duckduckgo)
        );
        assert_eq!(WebSearchEngine::parse("bing"), Some(WebSearchEngine::Bing));
        assert_eq!(WebSearchEngine::parse("google"), None);
    }

    #[test]
    fn detects_ddg_anomaly_challenge_page() {
        // Shape of the anti-bot challenge DDG serves (HTTP 200) instead of
        // results when a request is flagged (e.g. TLS fingerprint on Linux).
        let html = r#"<!DOCTYPE html><html><head>
            <script src="/dist/anomaly.js"></script></head>
            <body><div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
            </body></html>"#;
        assert_eq!(detect_anti_bot_page(html), Some("anomaly challenge"));
        // And it should parse to zero real results.
        assert!(parse_ddg_results(html, 10).is_empty());
    }

    #[test]
    fn detects_generic_captcha_page() {
        let html = r#"<html><body><div class="g-recaptcha"></div>
            Please verify you are human.</body></html>"#;
        assert!(detect_anti_bot_page(html).is_some());
    }

    #[test]
    fn real_results_are_not_flagged_as_anti_bot() {
        let html = r#"
            <div class="result results_links web-result">
              <a class="result__a" href="https://rust-lang.org/">Rust</a>
              <a class="result__snippet" href="https://rust-lang.org/">A language.</a>
            </div>
        "#;
        assert_eq!(detect_anti_bot_page(html), None);
        assert_eq!(parse_ddg_results(html, 10).len(), 1);
    }

    // Captured from a live DuckDuckGo request that was flagged on Linux (GH #270):
    // the HTML endpoint returns HTTP 202 with an "anomaly" challenge page and no
    // results. These fixtures pin the real-world shapes so the fix stays honest.
    #[test]
    fn real_captured_ddg_anomaly_fixture_is_detected() {
        let html = include_str!("testdata/ddg_anomaly.html");
        // The bug: this page parses to zero real results...
        assert!(
            parse_ddg_results(html, 10).is_empty(),
            "anomaly page should yield no results"
        );
        // ...but the fix now recognizes it as a challenge instead of a silent
        // "no results found".
        assert_eq!(detect_anti_bot_page(html), Some("anomaly challenge"));
    }

    #[test]
    fn real_captured_ddg_results_fixture_parses() {
        let html = include_str!("testdata/ddg_results.html");
        assert_eq!(detect_anti_bot_page(html), None);
        assert!(
            !parse_ddg_results(html, 10).is_empty(),
            "real results page should yield results"
        );
    }

    #[test]
    fn parses_searxng_json_results() {
        // Shape of a real SearXNG /search?format=json response (#270).
        let body = serde_json::json!({
            "query": "rust",
            "results": [
                {
                    "url": "https://www.rust-lang.org/",
                    "title": "Rust Programming Language",
                    "content": "A language empowering everyone."
                },
                {
                    "url": "https://doc.rust-lang.org/book/",
                    "title": "The Rust Book",
                    "content": "Learn Rust."
                },
                // Entry with empty url is dropped; missing content tolerated.
                { "url": "", "title": "junk" },
                { "url": "https://crates.io", "title": "" }
            ]
        });
        let parsed: SearxngResponse = serde_json::from_value(body).unwrap();
        let results = parse_searxng_results(parsed, 10);
        assert_eq!(results.len(), 3, "empty-url entry should be dropped");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].snippet, "A language empowering everyone.");
        // Missing title falls back to the URL.
        assert_eq!(results[2].title, "https://crates.io");
        assert_eq!(results[2].snippet, "");
    }

    #[test]
    fn searxng_results_respect_limit() {
        let body = serde_json::json!({
            "results": (0..10)
                .map(|i| serde_json::json!({"url": format!("https://x/{i}"), "title": "t"}))
                .collect::<Vec<_>>()
        });
        let parsed: SearxngResponse = serde_json::from_value(body).unwrap();
        assert_eq!(parse_searxng_results(parsed, 3).len(), 3);
    }

    #[test]
    fn websearch_engine_parses_searxng_aliases() {
        assert_eq!(
            WebSearchEngine::parse("searxng"),
            Some(WebSearchEngine::Searxng)
        );
        assert_eq!(
            WebSearchEngine::parse("searx"),
            Some(WebSearchEngine::Searxng)
        );
        assert_eq!(WebSearchEngine::Searxng.as_str(), "searxng");
    }

    #[test]
    fn websearch_engine_parses_new_engines() {
        assert_eq!(WebSearchEngine::parse("tavily"), Some(WebSearchEngine::Tavily));
        assert_eq!(
            WebSearchEngine::parse("last30days"),
            Some(WebSearchEngine::Last30days)
        );
        assert_eq!(
            WebSearchEngine::parse("l30d"),
            Some(WebSearchEngine::Last30days)
        );
        assert_eq!(WebSearchEngine::parse("hybrid"), Some(WebSearchEngine::Hybrid));
        assert_eq!(WebSearchEngine::parse("both"), Some(WebSearchEngine::Hybrid));
        assert_eq!(WebSearchEngine::Tavily.as_str(), "tavily");
        assert_eq!(WebSearchEngine::Last30days.as_str(), "last30days");
        assert_eq!(WebSearchEngine::Hybrid.as_str(), "hybrid");
    }

    #[test]
    fn resolve_tavily_keys_parses_comma_list_and_dedups() {
        let mut cfg = crate::config::WebSearchConfig::default();
        // Point the env var at a name that is not set so only the inline config
        // key list is used (keeps the test hermetic).
        cfg.tavily_api_key_env = "JCODE_TEST_TAVILY_UNSET_ENV".to_string();
        cfg.tavily_api_key = Some(" k1 , k2 ,, k1 , k3 ".to_string());

        let keys = resolve_tavily_keys(&cfg);
        assert_eq!(
            keys,
            vec!["k1".to_string(), "k2".to_string(), "k3".to_string()],
            "keys are trimmed, empty entries dropped, and duplicates removed in order"
        );
    }

    #[test]
    fn resolve_tavily_keys_empty_when_unset() {
        let mut cfg = crate::config::WebSearchConfig::default();
        cfg.tavily_api_key_env = "JCODE_TEST_TAVILY_UNSET_ENV".to_string();
        cfg.tavily_api_key = None;
        assert!(resolve_tavily_keys(&cfg).is_empty());
    }

    #[test]
    fn tavily_keys_for_call_rotates_start_and_covers_all_keys() {
        let keys = vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "d".to_string(),
        ];
        // Each call returns all keys (so failover can reach every one) but starts
        // at a rotating offset so load spreads instead of always hitting "a".
        let mut starts = std::collections::HashSet::new();
        for _ in 0..keys.len() * 2 {
            let ordered = tavily_keys_for_call(&keys);
            assert_eq!(ordered.len(), keys.len(), "every key is available for failover");
            let mut sorted = ordered.clone();
            sorted.sort();
            assert_eq!(sorted, {
                let mut all = keys.clone();
                all.sort();
                all
            });
            starts.insert(ordered[0].clone());
        }
        assert!(
            starts.len() > 1,
            "rotation should start on more than one key across calls"
        );

        // Single key / empty are passthrough.
        assert_eq!(tavily_keys_for_call(&["only".to_string()]), vec!["only".to_string()]);
        assert!(tavily_keys_for_call(&[]).is_empty());
    }

    #[test]
    fn tavily_failover_status_classification() {
        // Auth/quota-class statuses trigger failover to the next key.
        for status in [401u16, 402, 403, 429, 432] {
            assert!(
                matches!(status, 401 | 402 | 403 | 429 | 432),
                "status {status} should be treated as key-exhausted"
            );
        }
    }

    #[test]
    fn parses_tavily_results_with_scores() {
        let body = serde_json::json!({
            "query": "rust",
            "results": [
                {"title": "Rust Lang", "url": "https://rust-lang.org/", "content": "A language.", "score": 0.93},
                {"title": "", "url": "https://doc.rust-lang.org/", "content": "The book.", "score": 0.4},
                {"title": "junk", "url": "", "content": "", "score": 0.1}
            ]
        });
        let parsed: TavilyResponse = serde_json::from_value(body).unwrap();
        let results = parse_tavily_results(parsed, 10);
        assert_eq!(results.len(), 2, "empty-url entry should be dropped");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert!((results[0].score - 0.93).abs() < 1e-5);
        // Missing title falls back to the URL.
        assert_eq!(results[1].title, "https://doc.rust-lang.org/");
    }

    #[test]
    fn parses_last30days_ranked_candidates() {
        let body = serde_json::json!({
            "ranked_candidates": [
                {"title": "OpenClaw hype", "url": "https://reddit.com/r/OpenAI/x",
                 "body": "long body", "container": "OpenAI", "source": "reddit",
                 "local_rank_score": 0.38},
                {"title": "", "url": "https://news.ycombinator.com/item?id=1",
                 "body": "Migrate from OpenClaw", "container": "Hacker News",
                 "source": "hackernews", "local_rank_score": 0.3}
            ],
            "items_by_source": {}
        });
        let parsed: Last30daysResponse = serde_json::from_value(body).unwrap();
        let results = parse_last30days_results(parsed, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].url, "https://reddit.com/r/OpenAI/x");
        assert!(results[0].snippet.starts_with("[OpenAI]"));
        // Missing title falls back to first line of body.
        assert_eq!(results[1].title, "Migrate from OpenClaw");
    }

    #[test]
    fn last30days_falls_back_to_items_by_source() {
        let body = serde_json::json!({
            "ranked_candidates": [],
            "items_by_source": {
                "reddit": [
                    {"title": "low", "url": "https://a.test", "local_rank_score": 0.1},
                    {"title": "high", "url": "https://b.test", "local_rank_score": 0.9}
                ]
            }
        });
        let parsed: Last30daysResponse = serde_json::from_value(body).unwrap();
        let results = parse_last30days_results(parsed, 10);
        assert_eq!(results.len(), 2);
        // Sorted by local_rank_score descending.
        assert_eq!(results[0].url, "https://b.test");
    }

    #[test]
    fn normalize_url_collapses_variants() {
        assert_eq!(
            normalize_url("https://www.Example.com/Path/?q=1#frag"),
            "example.com/path"
        );
        assert_eq!(normalize_url("http://example.com/path/"), "example.com/path");
        assert_eq!(normalize_url("https://example.com/path"), "example.com/path");
        // Different pages stay distinct.
        assert_ne!(normalize_url("https://a.com/x"), normalize_url("https://a.com/y"));
    }

    #[test]
    fn merge_and_rank_boosts_cross_engine_agreement() {
        // Same URL from both engines should be marked verified and rank first,
        // even though it is not the very top of either individual leg.
        let tavily = vec![
            SearchResult {
                title: "Solo Tavily top".into(),
                url: "https://solo-tavily.com/a".into(),
                snippet: "only tavily".into(),
                score: 0.95,
                engines: Vec::new(),
            },
            SearchResult {
                title: "Shared".into(),
                url: "https://shared.com/topic".into(),
                snippet: "short".into(),
                score: 0.6,
                engines: Vec::new(),
            },
        ];
        let last30 = vec![
            SearchResult {
                title: "Solo l30 top".into(),
                url: "https://solo-l30.com/b".into(),
                snippet: "only l30".into(),
                score: 0.9,
                engines: Vec::new(),
            },
            SearchResult {
                title: "Shared".into(),
                url: "https://www.shared.com/topic/".into(), // same page, trailing slash + www
                snippet: "a much longer and richer snippet from last30days".into(),
                score: 0.5,
                engines: Vec::new(),
            },
        ];

        let merged = merge_and_rank(
            vec![
                ("tavily".into(), tavily),
                ("last30days".into(), last30),
            ],
            10,
        );

        // Shared result deduped into one entry, verified by both engines, ranked first.
        assert_eq!(merged[0].url, "https://shared.com/topic");
        assert_eq!(merged[0].engines.len(), 2, "should be verified by 2 engines");
        assert!(merged[0].engines.contains(&"tavily".to_string()));
        assert!(merged[0].engines.contains(&"last30days".to_string()));
        // Richer snippet retained.
        assert!(merged[0].snippet.contains("richer snippet"));
        // No duplicate shared entries.
        let shared_count = merged.iter().filter(|r| r.url.contains("shared.com")).count();
        assert_eq!(shared_count, 1);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn render_result_badge_marks_verified() {
        let mut r = SearchResult::basic("t".into(), "https://x.com".into(), "s".into());
        assert_eq!(render_result_badge(&r), "");
        r.engines = vec!["tavily".into()];
        assert_eq!(render_result_badge(&r), "  _[tavily]_");
        r.engines = vec!["tavily".into(), "last30days".into()];
        assert_eq!(render_result_badge(&r), "  _[✓ verified · tavily+last30days]_");
    }
}
