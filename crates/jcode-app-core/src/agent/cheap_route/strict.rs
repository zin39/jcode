//! Strict cheap-routing: never silently escalate to an expensive model.
//!
//! Cheap routing is normally a cascade that escalates to the coordinator's own
//! model when cheap routes run out. That is the standard FrugalGPT/RouteLLM
//! design and the right default, but it means a drained balance or a dead key
//! quietly bills the frontier model, with the spend only visible on an invoice.
//!
//! These helpers are the decision points that cascade behaviour turns on. They
//! are pure so each escalation path is unit-testable without a live provider or
//! the config singleton.

use super::model_is_cheap_route_banned;
use anyhow::anyhow;

/// Resolve the model used for HARD subtasks (difficulty above the threshold).
///
/// Non-strict (default) falls back to the coordinator's own model when
/// `cheap_route_strong_model` is unset, which is the standard cascade design.
/// That fallback is also the single largest source of silent frontier spend:
/// with the setting unset, EVERY hard subtask bills the model you are chatting
/// with. Under `cheap_route_strict` there is no implicit fallback — an unset
/// strong model means hard subtasks stay on the cheap list instead.
///
/// Returns `None` when strict mode has no configured strong model, meaning
/// "use the cheapest-first list", never "use the coordinator".
pub(super) fn resolve_strong_model(
    configured: Option<&str>,
    current_model: &str,
    strict: bool,
) -> Option<String> {
    let configured = configured
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);
    match configured {
        Some(model) => Some(model),
        // Strict: refuse to silently promote the coordinator into the strong
        // tier. Cheap-but-weaker beats expensive-and-unrequested.
        None if strict => None,
        None => Some(current_model.to_string()),
    }
}

/// Whether the coordinator's own model may be appended as the last-resort
/// candidate.
///
/// This is the escape hatch that keeps a subtask alive when every cheap route
/// fails. Under `cheap_route_strict` it is removed entirely: the user asked for
/// cheap-only, so exhausting the cheap routes must fail loudly rather than bill
/// the frontier model. A banned model is never eligible in either mode.
pub(super) fn coordinator_is_allowed_as_last_resort(current_model: &str, strict: bool) -> bool {
    !current_model.is_empty() && !strict && !model_is_cheap_route_banned(current_model)
}

/// The strong-tier model, or an empty string meaning "no strong tier".
///
/// Empty leaves hard subtasks on the cheapest-first list, which is the intended
/// strict behaviour: cheap-but-weaker beats expensive-and-unrequested.
pub(super) fn strong_model_or_empty(current_model: &str, strict: bool) -> String {
    let configured = crate::config::config()
        .agents
        .cheap_route_strong_model
        .clone();
    resolve_strong_model(configured.as_deref(), current_model, strict).unwrap_or_default()
}

/// Error for a total cheap-route blackout under strict mode.
///
/// Names the model it refused to use so the failure is actionable rather than
/// an unexplained stall.
pub(super) fn blackout_error(current_model: &str) -> anyhow::Error {
    anyhow!(
        "cheap-route blackout: every cheap route is unavailable (dead credential, \
         drained balance, or rate limit) and agents.cheap_route_strict is on, so work \
         will NOT be escalated to '{current_model}'. Fix a cheap provider, or set \
         cheap_route_strict = false to allow escalation."
    )
}

#[cfg(test)]
mod strict_routing_tests {
    use super::{coordinator_is_allowed_as_last_resort, resolve_strong_model};

    /// L4: a total cheap-route blackout must fail with an ACTIONABLE error.
    ///
    /// Regression guard for 70035ce0b, which made an empty ranking fall through
    /// to "use the parent model" — so a drained balance or dead key silently
    /// billed the frontier model and only surfaced on an invoice. The message
    /// must name the model it refused to use and say how to change the policy,
    /// otherwise the failure is just an unexplained stall.
    #[test]
    fn blackout_error_names_the_refused_model_and_the_escape_hatch() {
        let err = super::blackout_error("claude-opus-4-8").to_string();
        assert!(
            err.contains("claude-opus-4-8"),
            "must name the model it refused to escalate to: {err}"
        );
        assert!(
            err.contains("NOT be escalated"),
            "must state that escalation was refused, not merely that something failed: {err}"
        );
        assert!(
            err.contains("cheap_route_strict"),
            "must name the setting so the user can opt back into cascading: {err}"
        );
        // The three real causes seen live on this machine: 401 dead credential,
        // 402 drained balance, 429 rate limit. The message should point at them
        // rather than leaving the user to guess which provider broke.
        for cause in ["dead credential", "drained balance", "rate limit"] {
            assert!(err.contains(cause), "should mention {cause:?}: {err}");
        }
    }

    /// L2: with `cheap_route_strong_model` unset, the non-strict cascade promotes
    /// the coordinator into the strong tier — so every hard subtask bills the
    /// model you are chatting with. Strict mode must refuse that promotion.
    #[test]
    fn strict_never_promotes_coordinator_to_strong_tier() {
        // Default (cascade) behaviour is preserved: unset => coordinator.
        assert_eq!(
            resolve_strong_model(None, "claude-opus-4-8", false).as_deref(),
            Some("claude-opus-4-8"),
            "non-strict must keep the documented cascade fallback"
        );

        // Strict: no configured strong model means fall back to the CHEAP list,
        // never to the coordinator.
        assert_eq!(
            resolve_strong_model(None, "claude-opus-4-8", true),
            None,
            "strict must not silently route hard subtasks to the coordinator"
        );

        // An explicitly configured strong model is honoured in both modes.
        for strict in [false, true] {
            assert_eq!(
                resolve_strong_model(Some("glm-5.2"), "claude-opus-4-8", strict).as_deref(),
                Some("glm-5.2"),
            );
        }

        // Whitespace-only is treated as unset, not as a model named " ".
        assert_eq!(
            resolve_strong_model(Some("   "), "claude-opus-4-8", true),
            None
        );
    }

    /// L3: the coordinator is appended as a last-resort candidate on every run.
    /// Strict mode removes that escape hatch so exhausting the cheap routes
    /// fails instead of billing the frontier model.
    #[test]
    fn strict_drops_the_coordinator_last_resort_candidate() {
        // Non-strict keeps the escape hatch (model not in any ban list here).
        assert!(
            coordinator_is_allowed_as_last_resort("some-unbanned-model", false),
            "non-strict must retain the last-resort fallback"
        );

        // Strict removes it entirely.
        assert!(
            !coordinator_is_allowed_as_last_resort("some-unbanned-model", true),
            "strict must not append the coordinator as a fallback candidate"
        );

        // An empty coordinator model is never a candidate in either mode.
        for strict in [false, true] {
            assert!(!coordinator_is_allowed_as_last_resort("", strict));
        }
    }
}
