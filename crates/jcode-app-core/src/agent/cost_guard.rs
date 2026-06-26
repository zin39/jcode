//! Cost-ceiling guard.
//!
//! Two jobs, both keyed off the cross-process spend ledger
//! (`provider_activity`, persisted to `~/.jcode/provider_activity.json`):
//!
//! 1. **Record** real per-call API-key spend SERVER-SIDE. The TUI already records
//!    spend, but the headless daemon (where a long autonomous coordinator runs —
//!    the path that burned ~$95/day) never did, so `/usage` and any ceiling were
//!    blind to it. [`Agent::record_call_spend`] fixes that after every API call.
//! 2. **Enforce** a daily ceiling. Before each turn, [`Agent::enforce_cost_ceiling`]
//!    aborts if today's spend on the active credential has reached
//!    `JCODE_COST_CEILING_USD`, so a runaway agent can't keep burning money.
//!
//! Only billed-per-token credentials are affected (see
//! [`crate::provider::Provider::billing_source_key`]); OAuth/subscription logins
//! opt out of both. Disabled by default — set `JCODE_COST_CEILING_USD` to enable
//! the ceiling.

use crate::agent::Agent;
use jcode_provider_core::{RouteBillingKind, RouteCheapnessEstimate};

/// Parse a raw `JCODE_COST_CEILING_USD` value into an enabled ceiling. `None`,
/// non-numeric, zero, or negative all mean "no ceiling" (disabled).
fn parse_cost_ceiling(raw: Option<String>) -> Option<f64> {
    let parsed: f64 = raw?.trim().parse().ok()?;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed)
}

/// The configured daily cost ceiling in USD, or `None` when disabled.
fn cost_ceiling_usd() -> Option<f64> {
    parse_cost_ceiling(std::env::var("JCODE_COST_CEILING_USD").ok())
}

/// USD cost of one API call given its token counts and the route's metered
/// pricing. `*_price_per_mtok_micros` are USD-per-million-tokens scaled by 1e6,
/// so a token's cost is `tokens * micros / 1e12`. Cache writes have no dedicated
/// published rate; Anthropic's 5-minute cache-write is ~1.25x the input rate, so
/// we approximate with that.
fn call_cost_usd(
    est: &RouteCheapnessEstimate,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> f64 {
    fn part(tokens: u64, micros: Option<u64>) -> f64 {
        micros.map_or(0.0, |m| tokens as f64 * m as f64 / 1e12)
    }
    let input_micros = est.input_price_per_mtok_micros;
    part(input, input_micros)
        + part(output, est.output_price_per_mtok_micros)
        + part(cache_read, est.cache_read_price_per_mtok_micros)
        + part(cache_write, input_micros.map(|m| m.saturating_mul(5) / 4))
}

impl Agent {
    /// Record this call's spend into the cross-provider ledger (best-effort,
    /// offloaded so the turn loop never blocks on file I/O). No-op for
    /// OAuth/subscription credentials or models we cannot price.
    pub(crate) fn record_call_spend(&self) {
        let Some(source_key) = self.provider.billing_source_key() else {
            return;
        };
        let model = self.provider.model();
        let Some(est) = crate::provider::pricing::metered_pricing_for_source(&source_key, &model)
        else {
            return;
        };
        // Only meter genuinely per-token billing.
        if est.billing_kind != RouteBillingKind::Metered {
            return;
        }
        let cost = call_cost_usd(
            &est,
            self.last_usage.input_tokens,
            self.last_usage.output_tokens,
            self.last_usage.cache_read_input_tokens.unwrap_or(0),
            self.last_usage.cache_creation_input_tokens.unwrap_or(0),
        );
        if !cost.is_finite() || cost <= 0.0 {
            return;
        }
        // Ledger writes hit the filesystem; never block the async turn loop.
        tokio::task::spawn_blocking(move || {
            crate::provider_activity::record_spend(&source_key, cost);
        });
    }

    /// Abort the turn if today's spend on the active billed credential has reached
    /// the configured ceiling. No-op when the ceiling is unset or the credential
    /// is not billed per token.
    pub(crate) fn enforce_cost_ceiling(&self) -> anyhow::Result<()> {
        let Some(ceiling) = cost_ceiling_usd() else {
            return Ok(());
        };
        let Some(source_key) = self.provider.billing_source_key() else {
            return Ok(());
        };
        if let Some(spend) = crate::provider_activity::spend_snapshot(&source_key)
            && spend.day_usd >= ceiling
        {
            crate::logging::warn(&format!(
                "[cost_guard] daily ceiling ${:.2} reached for {} (spent ${:.2} today); aborting turn",
                ceiling, source_key, spend.day_usd
            ));
            return Err(anyhow::anyhow!(
                "daily cost ceiling ${:.2} reached for {} (spent ${:.2} today). Raise JCODE_COST_CEILING_USD, switch credentials, or resume tomorrow.",
                ceiling,
                source_key,
                spend.day_usd
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{call_cost_usd, parse_cost_ceiling};
    use jcode_provider_core::{RouteCheapnessEstimate, RouteCostConfidence, RouteCostSource};

    #[test]
    fn parse_cost_ceiling_handles_all_cases() {
        assert_eq!(parse_cost_ceiling(None), None);
        assert_eq!(parse_cost_ceiling(Some("20".to_string())), Some(20.0));
        assert_eq!(parse_cost_ceiling(Some("  12.5 ".to_string())), Some(12.5));
        // 0, negative, and garbage all disable the ceiling.
        assert_eq!(parse_cost_ceiling(Some("0".to_string())), None);
        assert_eq!(parse_cost_ceiling(Some("-5".to_string())), None);
        assert_eq!(parse_cost_ceiling(Some("abc".to_string())), None);
        assert_eq!(parse_cost_ceiling(Some(String::new())), None);
    }

    fn opus_like_pricing() -> RouteCheapnessEstimate {
        // input $5/Mtok, output $25/Mtok, cache-read $0.50/Mtok (micros = USD*1e6).
        RouteCheapnessEstimate::metered(
            RouteCostSource::PublicApiPricing,
            RouteCostConfidence::High,
            5_000_000,
            25_000_000,
            Some(500_000),
            None,
        )
    }

    #[test]
    fn call_cost_usd_sums_input_and_output() {
        // 1M input + 1M output = $5 + $25 = $30.
        let cost = call_cost_usd(&opus_like_pricing(), 1_000_000, 1_000_000, 0, 0);
        assert!((cost - 30.0).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn call_cost_usd_includes_cache_read_and_approximated_write() {
        // 1M cache-read @ $0.50 = $0.50; 1M cache-write ≈ 1.25 * $5 = $6.25.
        let cost = call_cost_usd(&opus_like_pricing(), 0, 0, 1_000_000, 1_000_000);
        assert!((cost - 6.75).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn call_cost_usd_zero_tokens_is_zero() {
        assert_eq!(call_cost_usd(&opus_like_pricing(), 0, 0, 0, 0), 0.0);
    }
}
