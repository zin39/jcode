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
