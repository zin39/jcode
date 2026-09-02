//! Anthropic provider shared helpers (compatibility shim).
//!
//! The direct Anthropic Messages API *runtime* (`AnthropicProvider`) now lives
//! in the downstream `jcode-provider-anthropic-runtime` crate so provider
//! edits do not rebuild the base -> app-core -> tui spine. The binary's
//! composition root registers it via [`crate::provider::external`].
//!
//! Base keeps the pieces its own auth/usage/sidecar code (and the runtime
//! crate) share:
//! - the OAuth attribution headers + Claude CLI user agent used for
//!   subscription API calls,
//! - API-key resolution (`load_anthropic_api_key`, `has_anthropic_api_key`),
//! - the process-wide cache-TTL toggle, and
//! - the static model list.

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

pub use jcode_provider_core::CredentialMode as AnthropicCredentialMode;
use jcode_provider_core::{
    ANTHROPIC_OAUTH_BETA_HEADERS, anthropic_effectively_1m,
    anthropic_stainless_arch as stainless_arch, anthropic_stainless_os as stainless_os,
};

static CACHE_TTL_1H: AtomicBool = AtomicBool::new(true);

/// Enable or disable the 1-hour cache TTL (default: 1-hour)
pub fn set_cache_ttl_1h(enabled: bool) {
    CACHE_TTL_1H.store(enabled, Ordering::Relaxed);
}

/// Check if 1-hour cache TTL is enabled
pub fn is_cache_ttl_1h() -> bool {
    CACHE_TTL_1H.load(Ordering::Relaxed)
}

/// User-Agent for OAuth requests, matching the official Claude Code CLI.
///
/// Derived from the single `ANTHROPIC_CLAUDE_CODE_VERSION` source of truth so
/// it cannot drift from the billing header and eval `app_version`. Anthropic
/// rejects new models outright when this version is too old.
pub const CLAUDE_CLI_USER_AGENT: &str = concat!(
    "claude-cli/",
    jcode_provider_core::anthropic_claude_code_version!(),
    " (external, sdk-cli)"
);

pub const OAUTH_BETA_HEADERS: &str = ANTHROPIC_OAUTH_BETA_HEADERS;

/// Whether a model id effectively runs with the 1M-token context beta.
pub fn effectively_1m(model: &str) -> bool {
    anthropic_effectively_1m(model)
}

pub fn new_oauth_request_id() -> String {
    Uuid::new_v4().to_string()
}

/// Attach the OAuth attribution headers the official Claude CLI sends.
/// Shared by the runtime crate's request path and base's usage probes.
pub fn apply_oauth_attribution_headers(
    req: reqwest::RequestBuilder,
    session_id: &str,
) -> reqwest::RequestBuilder {
    req.header("x-client-request-id", new_oauth_request_id())
        .header("x-app", "cli")
        .header("X-Claude-Code-Session-Id", session_id)
        .header("X-Stainless-Arch", stainless_arch())
        .header("X-Stainless-Lang", "js")
        .header("X-Stainless-OS", stainless_os())
        .header("X-Stainless-Package-Version", "0.81.0")
        .header("X-Stainless-Retry-Count", "0")
        .header("X-Stainless-Runtime", "node")
        .header("X-Stainless-Runtime-Version", "v24.3.0")
        .header("X-Stainless-Timeout", "600")
        .header("anthropic-dangerous-direct-browser-access", "true")
}

/// Available models
pub const AVAILABLE_MODELS: &[&str] = &[
    "claude-opus-5",
    "claude-fable-5-1",
    "claude-fable-5",
    "claude-opus-4-8",
    "claude-opus-4-6",
    "claude-opus-4-6[1m]",
    "claude-sonnet-5",
    "claude-sonnet-4-6",
    "claude-sonnet-4-6[1m]",
    "claude-haiku-4-5",
    "claude-opus-4-5",
    "claude-sonnet-4-5",
    "claude-sonnet-4-20250514",
];

pub fn load_anthropic_api_key() -> Result<String> {
    if std::env::var("JCODE_ANTHROPIC_AUTH")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    {
        return Ok(String::new());
    }
    if let Ok(env_name) = std::env::var("JCODE_ANTHROPIC_API_KEY_NAME") {
        let env_name = env_name.trim();
        if !env_name.is_empty() {
            if let Ok(value) = std::env::var(env_name)
                && !value.trim().is_empty()
            {
                return Ok(value);
            }
            if let Ok(env_file) = std::env::var("JCODE_ANTHROPIC_ENV_FILE")
                && let Some(value) = crate::provider_catalog::load_env_value_from_config_file(
                    env_name,
                    env_file.trim(),
                )
                && !value.trim().is_empty()
            {
                return Ok(value);
            }
            anyhow::bail!(
                "Anthropic-compatible profile credential '{}' is not configured",
                env_name
            );
        }
    }
    if let Ok(value) = std::env::var("ANTHROPIC_AUTH_TOKEN")
        && !value.trim().is_empty()
    {
        return Ok(value);
    }
    let key = crate::provider_catalog::load_api_key_from_env_or_config(
        "ANTHROPIC_API_KEY",
        "anthropic.env",
    )
    .context("No Anthropic API key found")?;
    if std::env::var("JCODE_LOG_SERVICE_TIER").is_ok() {
        let prefix: String = key.chars().take(14).collect();
        eprintln!(
            "[anthropic] resolved API key prefix={prefix}... (len={})",
            key.len()
        );
    }
    Ok(key)
}

pub fn has_anthropic_api_key() -> bool {
    load_anthropic_api_key().is_ok()
}

#[cfg(test)]
mod tests {
    /// No Anthropic-bound request may hardcode a `claude-cli` version.
    ///
    /// Anthropic gates model availability on the advertised client version and
    /// rejects anything too old with a 400 whose message reads "does not
    /// support this model". A stale literal on any Anthropic call site
    /// therefore makes new models look nonexistent, which is exactly how
    /// `claude-fable-5-1` broke. Every such site must derive from
    /// `ANTHROPIC_CLAUDE_CODE_VERSION`.
    ///
    /// Scanned by source text because the failure mode is a *literal* that
    /// bypasses the constant, which no type-level check can catch. Providers
    /// that merely impersonate claude-cli for their own gating (Kimi, Zai,
    /// Alibaba) are excluded: their endpoints are not Anthropic and do not
    /// version-gate models.
    #[test]
    fn anthropic_call_sites_never_hardcode_a_claude_cli_version() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("repo root")
            .to_path_buf();

        // Paths whose `claude-cli/...` literal is a non-Anthropic provider's
        // own expectation, documented at each site.
        let allowed: &[&str] = &[
            "crates/jcode-provider-openrouter-runtime/src/lib.rs",
            "src/cli/auth_test/choice.rs",
        ];

        let mut offenders = Vec::new();
        let mut stack = vec![repo_root.join("crates"), repo_root.join("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&repo_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if allowed.contains(&rel.as_str()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (idx, line) in text.lines().enumerate() {
                    // A literal version right after `claude-cli/`. The derived
                    // form uses `concat!` and never spells digits inline.
                    let Some(rest) = line.split("claude-cli/").nth(1) else {
                        continue;
                    };
                    if rest.starts_with(|c: char| c.is_ascii_digit()) {
                        offenders.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these Anthropic call sites hardcode a claude-cli version instead of deriving it \
             from ANTHROPIC_CLAUDE_CODE_VERSION, which makes new models fail as \
             \"does not support this model\":\n{}",
            offenders.join("\n")
        );
    }
}
