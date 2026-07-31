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
}
