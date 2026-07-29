//! Cross-run memory of which cheap-route models are actually usable.
//!
//! The in-run circuit breaker stops re-probing a dead route *within* one
//! `cheap_route` call, but it starts empty every call. A route that is
//! permanently broken (model id removed by the provider, key not entitled,
//! an unsupported request parameter) therefore cost a fresh timeout on every
//! single run: an 11-candidate menu with a 90s budget can burn 16 minutes
//! rediscovering what the previous run already learned.
//!
//! This module persists *config* failures only. Those are deterministic
//! provider rejections ("Invalid model id", "not activated", 401/404) that
//! cannot be fixed by retrying, so remembering them is safe. Timeouts and
//! rate limits are deliberately NOT persisted: they are transient, and a
//! model that was briefly slow must be allowed back.
//!
//! Entries expire so a route repaired upstream (balance topped up, model
//! re-enabled) recovers on its own without the user clearing state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How long a remembered config failure suppresses a route.
///
/// Long enough to keep many consecutive runs fast, short enough that fixing
/// the provider side (adding credit, enabling a model) takes effect the same
/// working session without the user knowing this cache exists.
const QUARANTINE: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeadRoute {
    /// Unix seconds when the route was quarantined.
    at: u64,
    /// Provider error that condemned it, kept so users can see *why* a model
    /// stopped being offered instead of it silently vanishing.
    reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RouteHealthFile {
    #[serde(default)]
    dead: HashMap<String, DeadRoute>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path() -> Option<PathBuf> {
    match jcode_storage::jcode_dir() {
        Ok(dir) => Some(dir.join("cheap-route-health.json")),
        Err(err) => {
            // Without a home directory the cache is simply unavailable;
            // routing still works, it just re-learns dead routes each run.
            crate::logging::warn(&format!(
                "cheap-route health cache unavailable (no jcode dir): {err}"
            ));
            None
        }
    }
}

fn load() -> RouteHealthFile {
    let Some(path) = path() else {
        return RouteHealthFile::default();
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        // A missing cache is the normal first-run state, not a problem.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return RouteHealthFile::default();
        }
        Err(err) => {
            crate::logging::warn(&format!(
                "cheap-route health cache unreadable ({}); treating all routes as healthy: {err}",
                path.display()
            ));
            return RouteHealthFile::default();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(file) => file,
        Err(err) => {
            // A corrupt cache must never break routing: fall back to
            // "everything is healthy" and let the run re-learn. Say so, or a
            // permanently unparseable cache silently disables the optimisation.
            crate::logging::warn(&format!(
                "cheap-route health cache is corrupt; re-learning route health: {err}"
            ));
            RouteHealthFile::default()
        }
    }
}

fn store(file: &RouteHealthFile) {
    let Some(path) = path() else {
        return;
    };
    if let Some(parent) = path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        crate::logging::warn(&format!(
            "cannot create cheap-route health cache dir {}: {err}",
            parent.display()
        ));
        return;
    }
    let json = match serde_json::to_vec_pretty(file) {
        Ok(json) => json,
        Err(err) => {
            crate::logging::warn(&format!("cannot serialize cheap-route health cache: {err}"));
            return;
        }
    };
    // Losing this cache only costs a slower next run, so a write failure is
    // reported and tolerated rather than propagated into the routing path.
    if let Err(err) = std::fs::write(&path, json) {
        crate::logging::warn(&format!(
            "cannot persist cheap-route health cache {}: {err}",
            path.display()
        ));
    }
}

/// Models currently quarantined by a previous run, with the reason each was
/// condemned. Expired entries are dropped as a side effect, so a route heals
/// itself without user action.
pub(crate) fn quarantined() -> HashMap<String, String> {
    let mut file = load();
    let now = now_secs();
    let before = file.dead.len();
    file.dead
        .retain(|_, dead| now.saturating_sub(dead.at) < QUARANTINE.as_secs());
    if file.dead.len() != before {
        store(&file);
    }
    file.dead
        .into_iter()
        .map(|(model, dead)| (model, dead.reason))
        .collect()
}

/// Remember that `model` is configured wrongly and should be skipped until the
/// quarantine expires.
///
/// Only call this for deterministic provider rejections. Passing a timeout here
/// would suppress a model that was merely slow once.
pub(crate) fn quarantine(model: &str, reason: &str) {
    let mut file = load();
    file.dead.insert(
        model.to_string(),
        DeadRoute {
            at: now_secs(),
            // Provider errors can be enormous (full JSON bodies); keep the
            // cache small and readable.
            reason: reason.chars().take(200).collect(),
        },
    );
    store(&file);
}

/// Clear a model's quarantine as soon as it succeeds, so a recovered route is
/// trusted again immediately rather than waiting out the expiry.
pub(crate) fn mark_healthy(model: &str) {
    let mut file = load();
    if file.dead.remove(model).is_some() {
        store(&file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_entries_are_dropped_but_fresh_ones_survive() {
        let now = now_secs();
        let mut file = RouteHealthFile::default();
        file.dead.insert(
            "stale-model".to_string(),
            DeadRoute {
                at: now - QUARANTINE.as_secs() - 1,
                reason: "invalid model id".to_string(),
            },
        );
        file.dead.insert(
            "fresh-model".to_string(),
            DeadRoute {
                at: now,
                reason: "not activated".to_string(),
            },
        );

        file.dead
            .retain(|_, dead| now.saturating_sub(dead.at) < QUARANTINE.as_secs());

        assert!(
            !file.dead.contains_key("stale-model"),
            "a route quarantined longer than the expiry must be retried again"
        );
        assert!(
            file.dead.contains_key("fresh-model"),
            "a recently condemned route must stay quarantined"
        );
    }

    #[test]
    fn corrupt_cache_is_treated_as_all_healthy() {
        let parsed: RouteHealthFile = serde_json::from_slice(b"{ not json").unwrap_or_default();
        assert!(
            parsed.dead.is_empty(),
            "a corrupt health cache must not suppress every route"
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Isolate the cache behind a temp `JCODE_HOME`.
    ///
    /// `JCODE_HOME` is process-global, so it must be taken under the shared
    /// storage test lock. Using a private lock here instead let these tests
    /// race `isolate_config()` in the cheap-route suite, which repointed
    /// `JCODE_HOME` mid-run and made unrelated routing tests read the wrong
    /// config. Reuse the one lock the rest of the codebase already uses.
    struct IsolatedHome {
        _env: std::sync::MutexGuard<'static, ()>,
        _temp: tempfile::TempDir,
    }

    fn isolate_home() -> IsolatedHome {
        let _env = jcode_base::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("temp dir");
        // SAFETY: the storage test lock is held, so no other test reads or
        // writes the environment concurrently.
        unsafe { std::env::set_var("JCODE_HOME", temp.path()) };
        IsolatedHome { _env, _temp: temp }
    }

    #[test]
    fn a_quarantined_route_is_remembered_across_runs_and_cleared_on_success() {
        let _home = isolate_home();

        assert!(
            quarantined().is_empty(),
            "a fresh install starts with every route healthy"
        );

        quarantine("dead-model", "Invalid model id");

        // The point of persisting: a *later* run (a fresh read of the file)
        // must still skip the route instead of re-paying its timeout.
        let seen = quarantined();
        assert_eq!(seen.len(), 1, "the dead route survives into the next run");
        assert!(
            seen.get("dead-model")
                .is_some_and(|reason| reason.contains("Invalid model id")),
            "the reason is kept so a vanished model is explainable, got {seen:?}"
        );

        mark_healthy("dead-model");
        assert!(
            quarantined().is_empty(),
            "a route that succeeds is trusted again immediately, not after the expiry"
        );
    }

    #[test]
    fn an_enormous_provider_error_does_not_bloat_the_cache() {
        let _home = isolate_home();

        quarantine("chatty-model", &"x".repeat(10_000));

        let reason = quarantined()
            .remove("chatty-model")
            .expect("route was quarantined");
        assert!(
            reason.chars().count() <= 200,
            "provider errors can be whole JSON bodies; the cache must stay small"
        );
    }
}
