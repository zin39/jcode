//! Retry policy for sidecar (background) provider requests.
//!
//! Sidecar work such as memory extraction runs against the same account as
//! foreground turns, so it loses the race for a busy provider's quota. Split
//! out of sidecar.rs, which is over the code-size ratchet.

/// Attempts for a sidecar request, including the first.
pub(crate) const SIDECAR_MAX_ATTEMPTS: u32 = 3;

/// Backoff before the second attempt, doubled each retry. Only used when the
/// provider does not send a `Retry-After` hint of its own.
pub(crate) const SIDECAR_RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Whether a failed sidecar request is worth retrying.
///
/// Deliberately narrow: a rate limit or a transient server error will pass on a
/// later attempt, but an auth or quota rejection will not, and retrying those
/// just delays the inevitable while burning the provider's budget.
pub(crate) fn sidecar_error_is_retryable(error: &str) -> bool {
    let lowered = error.to_ascii_lowercase();
    if lowered.contains("insufficient")
        || lowered.contains("out of credits")
        || lowered.contains("usage limit")
        // Codex/ChatGPT reports an exhausted plan as a 429 carrying
        // `usage_limit_reached`. The spaced spelling above did not match that
        // underscored form, so an exhausted plan looked transient and was
        // retried three times before failing anyway. Measured 2026-07-31: 696
        // of these in one day, each burning a full round-trip.
        || lowered.contains("usage_limit")
        || lowered.contains("quota")
        || lowered.contains("401")
        || lowered.contains("403")
    {
        return false;
    }
    lowered.contains("429")
        || lowered.contains("rate limit")
        || lowered.contains("rate_limit")
        || lowered.contains("overloaded")
        || lowered.contains("500 internal server error")
        || lowered.contains("502")
        || lowered.contains("503")
        || lowered.contains("504")
}

/// Run a sidecar request, retrying transient failures.
///
/// Sidecar work loses the race for a busy provider's quota, and a single
/// attempt meant a transient 429 silently discarded the result: 82 memory
/// extractions were dropped that way in one day. Honors the server's own
/// `Retry-After` hint when it sends one, and falls back to exponential backoff.
pub(crate) async fn with_retries<F, Fut>(
    max_attempts: u32,
    mut request: F,
) -> anyhow::Result<String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<String>>,
{
    let mut delay = SIDECAR_RETRY_BASE_DELAY;
    let mut last_error = None;
    for attempt in 0..max_attempts {
        let error = match request().await {
            Ok(text) => return Ok(text),
            Err(error) => error,
        };

        if attempt + 1 >= max_attempts || !sidecar_error_is_retryable(&error.to_string()) {
            return Err(error);
        }

        let wait =
            jcode_provider_core::retry_after::retry_after_from_error(&error).unwrap_or(delay);
        crate::logging::info(&format!(
            "Sidecar request failed transiently; retrying in {:?} (attempt {}/{})",
            wait,
            attempt + 2,
            max_attempts
        ));
        tokio::time::sleep(wait).await;
        delay = delay.saturating_mul(2);
        last_error = Some(error);
    }

    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("Sidecar request failed with no attempts recorded")))
}

/// How long a backend stays demoted after a permanent (quota/auth) rejection.
///
/// A quota rejection is not permanent in wall-clock terms: plans reset. The
/// cooldown has to expire on its own so a recovered account is picked up
/// without a restart, but it must be long enough that the dead backend is not
/// re-probed on every single call. Measured on 2026-07-31, a quota-dead
/// backend cost 696 failed round-trips in one day because nothing remembered
/// that it had just failed.
pub(crate) const BACKEND_DEMOTION_COOLDOWN: std::time::Duration =
    std::time::Duration::from_secs(300);

/// Records which sidecar backends recently rejected us permanently, so the next
/// call can skip straight to one that still works.
///
/// Keyed by a short backend label rather than the enum so this module stays
/// free of `sidecar.rs` types (that file is at the code-size ratchet).
static DEMOTED_BACKENDS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, std::time::Instant>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Mark `backend` as unusable for [`BACKEND_DEMOTION_COOLDOWN`].
pub(crate) fn demote_backend(backend: &'static str) {
    if let Ok(mut demoted) = DEMOTED_BACKENDS.lock() {
        demoted.insert(backend, std::time::Instant::now());
    }
}

/// Whether `backend` is currently demoted. Expired entries are dropped, so a
/// backend whose quota has reset becomes eligible again on its own.
pub(crate) fn backend_is_demoted(backend: &str) -> bool {
    let Ok(mut demoted) = DEMOTED_BACKENDS.lock() else {
        // A poisoned lock must not make every backend look dead; failing open
        // costs one wasted call, failing closed disables memory entirely.
        return false;
    };
    let Some(at) = demoted.get(backend).copied() else {
        return false;
    };
    if at.elapsed() >= BACKEND_DEMOTION_COOLDOWN {
        demoted.remove(backend);
        return false;
    }
    true
}

/// Clear all demotions. Used by tests and by credential changes, where the
/// previous rejection says nothing about the new account.
pub fn clear_demoted_backends() {
    if let Ok(mut demoted) = DEMOTED_BACKENDS.lock() {
        demoted.clear();
    }
}

/// Build the backend try-order for a sidecar call: `selected` first, then every
/// other backend whose credentials are actually present, each at most once.
///
/// The selected backend keeps priority so normal operation is unchanged;
/// the rest exist only so one dead account cannot take memory down with it.
pub(crate) fn backend_chain(selected: &'static str) -> Vec<&'static str> {
    let available = [
        ("openai", crate::auth::codex::load_credentials().is_ok()),
        ("claude", crate::auth::claude::load_credentials().is_ok()),
        (
            "provider",
            crate::provider::active_provider_fork().is_some(),
        ),
    ];
    let mut chain = vec![selected];
    for (backend, usable) in available {
        if usable && !chain.contains(&backend) {
            chain.push(backend);
        }
    }
    chain
}

/// Run a request across a chain of backends, falling through to the next one
/// when the current backend rejects us permanently.
///
/// This is the loop that keeps memory alive when one account dies. A permanent
/// rejection (quota exhausted, bad auth) demotes that backend and moves on; a
/// transient failure is retried in place by [`with_retries`] and does not
/// demote, because it says nothing about whether the account still works.
///
/// Returns the first success, or the last error when every backend fails. When
/// every backend fails the caller still sees an error, so the memory rerank
/// keeps its fail-safe: surface nothing rather than inject unvetted results.
pub(crate) async fn with_backend_fallback<F, Fut>(
    backends: &[&'static str],
    max_attempts: u32,
    mut request: F,
) -> anyhow::Result<String>
where
    F: FnMut(&'static str) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<String>>,
{
    // If everything is already demoted, probe anyway: a total blackout until
    // the cooldown expires is worse than paying one speculative call.
    let all_demoted = backends.iter().all(|b| backend_is_demoted(b));
    let mut last_error = None;

    for backend in backends {
        if !all_demoted && backend_is_demoted(backend) {
            continue;
        }
        match with_retries(max_attempts, || request(backend)).await {
            Ok(text) => return Ok(text),
            Err(error) => {
                if !sidecar_error_is_retryable(&error.to_string()) {
                    demote_backend(backend);
                }
                last_error = Some(error);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("No sidecar backend is currently usable")))
}

#[cfg(test)]
mod tests {
    /// A transient rate limit must be retried, because the sidecar competes
    /// with foreground turns for the same account quota and a single attempt
    /// silently discarded results (82 memory extractions lost in one day).
    #[test]
    fn transient_failures_are_retried_and_permanent_ones_are_not() {
        for transient in [
            "OpenAI API error (429 Too Many Requests)",
            "status: 429 Too Many Requests",
            "rate_limit_error",
            "Overloaded",
            "502 Bad Gateway",
            "503 Service Unavailable",
        ] {
            assert!(
                super::sidecar_error_is_retryable(transient),
                "should retry: {transient:?}"
            );
        }

        // Retrying these only delays the failure and burns provider budget.
        for permanent in [
            "401 Unauthorized",
            "403 Forbidden",
            "Your account has insufficient balance",
            "You are out of credits",
            "monthly usage limit reached",
        ] {
            assert!(
                !super::sidecar_error_is_retryable(permanent),
                "should NOT retry: {permanent:?}"
            );
        }
    }

    /// A 429 that also names a permanent cause must not be retried: providers
    /// return out-of-credit rejections with a 429 status.
    #[test]
    fn a_rate_limited_status_with_a_permanent_cause_is_not_retried() {
        assert!(!super::sidecar_error_is_retryable(
            "429 Too Many Requests: your account has insufficient balance"
        ));
    }

    /// The loop must actually retry a transient failure and return the later
    /// success, rather than surfacing the first error.
    #[tokio::test]
    async fn a_transient_failure_is_retried_and_the_retry_result_is_returned() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = std::sync::Arc::clone(&attempts);
        let result = super::with_retries(3, || {
            let seen = std::sync::Arc::clone(&seen);
            async move {
                let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    anyhow::bail!("OpenAI API error (429 Too Many Requests)");
                }
                Ok("recovered".to_string())
            }
        })
        .await;

        assert_eq!(result.expect("should recover"), "recovered");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// A permanent failure must surface immediately: retrying an auth or quota
    /// rejection only delays it and burns provider budget.
    #[tokio::test]
    async fn a_permanent_failure_is_not_retried() {
        let attempts = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = std::sync::Arc::clone(&attempts);
        let result = super::with_retries(3, || {
            let seen = std::sync::Arc::clone(&seen);
            async move {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("401 Unauthorized")
            }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(
            attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a permanent failure must not be retried"
        );
    }

    /// The quota rejection that actually broke memory recall.
    ///
    /// Karan's Codex plan returned this exact shape 696 times in one day. It
    /// must be classified permanent, because that is what triggers demotion and
    /// failover to a backend that still works.
    #[test]
    fn a_codex_usage_limit_is_permanent_and_triggers_demotion() {
        let error = "OpenAI API error (429 Too Many Requests): \
                     {\"error\":{\"type\":\"usage_limit_reached\",\
                     \"message\":\"The usage limit has been reached\"}}";
        assert!(
            !super::sidecar_error_is_retryable(error),
            "a usage-limit 429 must not be retried against the same backend"
        );
    }

    /// A demoted backend is skipped until its cooldown expires, so a dead
    /// backend stops costing a failed round-trip on every single call.
    #[test]
    fn a_demoted_backend_is_skipped_until_the_cooldown_expires() {
        assert!(
            !super::backend_is_demoted("t4-fresh"),
            "nothing is demoted before a failure"
        );

        super::demote_backend("t4-dead");
        assert!(
            super::backend_is_demoted("t4-dead"),
            "a permanently rejected backend must be skipped"
        );
        assert!(
            !super::backend_is_demoted("t4-other"),
            "demoting one backend must not disable the others"
        );
    }

    /// Quota resets, so demotion must expire on its own. Without this the
    /// cooldown would be a permanent blackout until the process restarted.
    #[test]
    fn demotion_is_time_bounded_so_a_reset_plan_recovers() {
        assert!(
            super::BACKEND_DEMOTION_COOLDOWN >= std::time::Duration::from_secs(60),
            "cooldown must be long enough to stop per-call re-probing"
        );
        assert!(
            super::BACKEND_DEMOTION_COOLDOWN <= std::time::Duration::from_secs(3600),
            "cooldown must be short enough that a reset plan recovers without a restart"
        );
    }

    /// THE regression: a quota-dead first backend must hand off to a working
    /// one instead of taking memory down with it.
    ///
    /// Before this, `complete` retried only the selected backend, so Karan's
    /// exhausted Codex plan failed every judge vote and memory silently fell
    /// back to the no-LLM path 2,030 times across 8 days.
    #[tokio::test]
    async fn a_dead_backend_falls_through_to_a_working_one() {
        let tried = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&tried);

        let result = super::with_backend_fallback(&["t1-dead", "t1-live"], 3, |backend| {
            let seen = std::sync::Arc::clone(&seen);
            async move {
                seen.lock().expect("lock").push(backend);
                if backend == "t1-dead" {
                    anyhow::bail!("OpenAI API error (429 Too Many Requests): usage_limit_reached");
                }
                Ok("judged".to_string())
            }
        })
        .await;

        assert_eq!(
            result.expect("must recover via the second backend"),
            "judged"
        );
        assert_eq!(
            *tried.lock().expect("lock"),
            vec!["t1-dead", "t1-live"],
            "the dead backend is tried once, then the working one"
        );
        assert!(
            super::backend_is_demoted("t1-dead"),
            "a quota rejection must demote so the next call skips it"
        );
    }

    /// The fail-safe must survive: if EVERY backend is dead the caller still
    /// gets an error, so the memory rerank surfaces nothing rather than
    /// injecting unvetted results. Making memory "work" by lowering precision
    /// would be worse than the outage.
    #[tokio::test]
    async fn every_backend_failing_still_returns_an_error() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = std::sync::Arc::clone(&calls);

        let result = super::with_backend_fallback(&["t2-a", "t2-b"], 3, |_backend| {
            let seen = std::sync::Arc::clone(&seen);
            async move {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                anyhow::bail!("401 Unauthorized")
            }
        })
        .await;

        assert!(
            result.is_err(),
            "all backends dead must surface an error, never a fabricated answer"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "each backend is tried exactly once: no loop, no repeat"
        );
    }

    /// A transient failure must NOT demote: it says nothing about whether the
    /// account still works, and demoting on a blip would strand a healthy
    /// backend for the whole cooldown.
    #[tokio::test]
    async fn a_transient_failure_does_not_demote_the_backend() {
        let result = super::with_backend_fallback(&["t3-blip"], 1, |_backend| async move {
            anyhow::bail!("503 Service Unavailable")
        })
        .await;

        assert!(result.is_err());
        assert!(
            !super::backend_is_demoted("t3-blip"),
            "a transient blip must not sideline a healthy backend"
        );
    }

    /// A missing-model 404 is permanent: retrying cannot conjure the model, and
    /// re-probing that backend every call wastes a round-trip. Measured
    /// 2026-07-31: with demotion, fallback calls average 1098ms against a
    /// 955ms Claude-direct baseline, i.e. ~143ms of overhead rather than a
    /// whole failed request.
    #[test]
    fn a_missing_model_404_is_permanent_so_the_backend_is_demoted() {
        assert!(!super::sidecar_error_is_retryable(
            "OpenAI API error (404 Not Found): model gpt-does-not-exist-9 does not exist"
        ));
        assert!(!super::sidecar_error_is_retryable(
            "Claude API error (404 Not Found): model: claude-haiku-4-5-20241022"
        ));
    }

    /// The chain must lead with the selected backend and never repeat one,
    /// otherwise a dead backend would be tried twice per call.
    #[test]
    fn the_chain_leads_with_the_selected_backend_and_never_repeats() {
        let chain = super::backend_chain("claude");
        assert_eq!(
            chain.first().copied(),
            Some("claude"),
            "the selected backend keeps priority"
        );

        let mut unique = chain.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            chain.len(),
            "a backend must appear at most once: {chain:?}"
        );
    }
}
