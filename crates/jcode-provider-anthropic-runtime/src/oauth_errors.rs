//! Classifiers for Anthropic OAuth failure modes.
//!
//! Split out of lib.rs, which is over the code-size ratchet. Keeping these
//! together also keeps the org-policy guidance defined in exactly one place,
//! so the retry path and the final-attempt path cannot drift apart.

pub(super) fn is_oauth_auth_error(error_str: &str) -> bool {
    error_str.contains("oauth token has expired")
        || error_str.contains("token has expired")
        || error_str.contains("authentication_error")
        || error_str.contains("invalid token")
        || error_str.contains("invalid_grant")
        || error_str.contains("does not meet scope requirement")
        || ((error_str.contains("401 unauthorized") || error_str.contains("403 forbidden"))
            && (error_str.contains("oauth") || error_str.contains("token")))
}

/// Guidance shown when an organization forbids OAuth outright.
///
/// Defined once so the retry path and the final-attempt path cannot drift.
pub(super) const OAUTH_ORG_POLICY_GUIDANCE: &str = "\n\nYour Claude OAuth access token was rejected by the organization's policy — this organization does not allow OAuth authentication.\n\nTo fix this:\n• Switch to a different OAuth account or organization that allows OAuth, or\n• Use an API key route instead (`jcode login --provider claude-api`).";

/// Detect the Anthropic "OAuth not allowed for this organization" 403.
/// This is a non-retryable org-policy rejection — the org itself forbids OAuth
/// authentication. Do NOT force-refresh the token; the access token is valid
/// but the organization does not permit OAuth-based access.
pub(super) fn is_oauth_org_policy_error(error_str: &str) -> bool {
    error_str.contains("403")
        && (error_str.contains("oauth authentication is currently not allowed")
            || error_str.contains("oauth_not_allowed_for_organization"))
}

/// Detect Anthropic's "your Claude Code client is too old for this model" 400.
///
/// Anthropic gates newer models on the client version advertised in the
/// User-Agent. When jcode's advertised version falls behind, the API returns a
/// 400 `claude_code_version_too_old` whose message reads "Claude Code X does
/// not support this model", plus advice to run `claude update`. Both are
/// actively misleading here: the model exists and is selectable, nothing about
/// the user's Claude Code install is at fault, and running `claude update`
/// changes nothing because jcode advertises its own hardcoded version.
///
/// Matching on the machine-readable `error_code` first keeps this robust
/// against message rewording; the prose fallback covers older responses that
/// omit the code.
pub(super) fn is_claude_code_version_too_old_error(error_str: &str) -> bool {
    error_str.contains("claude_code_version_too_old")
        || (error_str.contains("does not support this model")
            && error_str.contains("or newer is required"))
}

/// Guidance shown when Anthropic rejects the advertised Claude Code version.
///
/// Points at the real fix (jcode's own constant) instead of the API's
/// `claude update` advice, which cannot help.
pub(super) const CLAUDE_CODE_VERSION_TOO_OLD_GUIDANCE: &str = "\n\nThis is not a problem with the model or your Claude Code install. Anthropic gates newer models on the client version jcode advertises, and jcode's advertised version is older than this model requires. Running `claude update` will not help.\n\nTo fix this:\n• Update jcode (`jcode update`), which ships a newer advertised version, or\n• If you are building jcode from source, bump `ANTHROPIC_CLAUDE_CODE_VERSION` in `crates/jcode-provider-core/src/anthropic.rs` to at least the version named above.";

pub(super) fn is_oauth_catalog_auth_error(error_str: &str) -> bool {
    let lower = error_str.to_ascii_lowercase();
    // An org-policy rejection is not refreshable: the token is valid, the
    // organization simply forbids OAuth. Refreshing burns a round trip and,
    // when no refresh token exists, replaces the real cause with a misleading
    // "failed to refresh" error. Let the caller surface the original 403.
    if is_oauth_org_policy_error(&lower) {
        return false;
    }
    lower.contains("401 unauthorized")
        || lower.contains("403 forbidden")
        || is_oauth_auth_error(&lower)
}

/// Whether the stored Claude credentials actually carry a refresh token.
///
/// Hand-configured `sk-ant-oat01` access tokens are long-lived and have no
/// refresh token, so a forced refresh can only ever fail. Checking first lets
/// the caller surface the real authentication error and fall through to
/// account failover instead of dead-ending on a refresh that was never
/// possible.
pub(super) fn claude_refresh_token_available() -> bool {
    jcode_base::auth::claude::load_credentials()
        .map(|creds| !creds.refresh_token.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long-lived `sk-ant-oat01` tokens carry no refresh token, so the forced
    /// refresh path must not run for them.
    ///
    /// Regression: it did, and its failure ("No refresh token available in
    /// Claude credentials") replaced the real authentication error and returned
    /// early, so the user saw a misleading "run jcode login" instead of the
    /// actual cause. Measured 15 such log entries.
    #[test]
    fn credentials_without_a_refresh_token_do_not_advertise_one() {
        let _env_lock = jcode_base::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("temp home");
        // Scope HOME as well: load_credentials also consults external sources
        // such as ~/.claude/.credentials.json, and on a developer machine those
        // are real and would mask the file under test.
        let previous_jcode_home = std::env::var_os("JCODE_HOME");
        let previous_home = std::env::var_os("HOME");
        jcode_base::env::set_var("JCODE_HOME", temp.path());
        jcode_base::env::set_var("HOME", temp.path());

        // Ask the auth layer where it will actually look rather than assuming
        // the temp root: it resolves to a config subdirectory.
        let auth_path = jcode_base::auth::claude::jcode_path().expect("auth path");
        if let Some(parent) = auth_path.parent() {
            std::fs::create_dir_all(parent).expect("create auth dir");
        }
        let write_auth = |refresh: &str| {
            let body = serde_json::json!({
                "anthropic_accounts": [{
                    "label": "claude-1",
                    "access": "sk-ant-oat01-test",
                    "refresh": refresh,
                    "expires": 9_999_999_999_999i64,
                    "scopes": ["user:inference"],
                }],
                "active_anthropic_account": "claude-1",
            });
            std::fs::write(&auth_path, serde_json::to_string(&body).expect("encode"))
                .expect("write auth file");
        };

        write_auth("");
        assert!(
            !claude_refresh_token_available(),
            "an empty refresh token must not be reported as available"
        );

        write_auth("refresh-token-value");
        assert!(
            claude_refresh_token_available(),
            "a real refresh token must still be reported as available"
        );

        match previous_jcode_home {
            Some(value) => jcode_base::env::set_var("JCODE_HOME", value),
            None => jcode_base::env::remove_var("JCODE_HOME"),
        }
        match previous_home {
            Some(value) => jcode_base::env::set_var("HOME", value),
            None => jcode_base::env::remove_var("HOME"),
        }
    }
}
