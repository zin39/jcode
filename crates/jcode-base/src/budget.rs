//! Process-global spend ledger for session budget tracking.
//!
//! Keyed by *root* session ID (the top-level session; subagents resolve to the
//! same root). All amounts are in **microdollars** (u64) to avoid f64 precision
//! loss. 1 microdollar = $0.000001 USD.
//!
//! The ledger is additive and opt-in: if no budget is configured nothing here
//! is ever consulted.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static LEDGER: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn ledger() -> &'static Mutex<HashMap<String, u64>> {
    LEDGER.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Accumulate `micros` additional spend for `root_id`.
///
/// Uses saturating addition so pathological token counts cannot overflow.
pub fn record_spend(root_id: &str, micros: u64) {
    let mut map = ledger().lock().unwrap();
    let entry = map.entry(root_id.to_owned()).or_insert(0);
    *entry = entry.saturating_add(micros);
}

/// Total microdollars spent so far for `root_id` (0 if nothing recorded).
pub fn spent_micros(root_id: &str) -> u64 {
    ledger()
        .lock()
        .unwrap()
        .get(root_id)
        .copied()
        .unwrap_or(0)
}

/// Convert USD to microdollars (round to nearest).
pub fn usd_to_micros(usd: f64) -> u64 {
    (usd * 1_000_000.0).round() as u64
}

/// Compute the microdollar cost of one API call given token counts and pricing.
///
/// Mirrors `ResolvedTokenPricing::cost_for_usage` in `jcode-tui/misc_ui.rs` but
/// works purely in integer micros so the server side has no TUI dependency.
///
/// All `*_price_per_mtok_micros` values are **microdollars per million tokens**
/// (the unit stored in `RouteCheapnessEstimate`). Missing prices (None) are
/// treated as 0 (no cost contribution).
///
/// Cache-write premium: if `is_anthropic` is true and `cache_creation_tokens > 0`,
/// cache writes are billed at 1.25× the input rate (5-min TTL approximation,
/// same default as the TUI path).
pub fn usage_cost_micros(
    input_price_per_mtok_micros: Option<u64>,
    output_price_per_mtok_micros: Option<u64>,
    cache_read_price_per_mtok_micros: Option<u64>,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_creation_tokens: u64,
    is_anthropic: bool,
) -> u64 {
    let input_price = input_price_per_mtok_micros.unwrap_or(0);
    let output_price = output_price_per_mtok_micros.unwrap_or(0);
    let cache_read_price = cache_read_price_per_mtok_micros.unwrap_or(input_price);

    // integer micros: tokens * price_per_mtok_micros / 1_000_000
    let input_cost = input_tokens.saturating_mul(input_price) / 1_000_000;
    let output_cost = output_tokens.saturating_mul(output_price) / 1_000_000;
    let cache_read_cost = cache_read_tokens.saturating_mul(cache_read_price) / 1_000_000;

    // Cache-write premium (1.25× → ×125/100) for Anthropic split-accounting.
    let cache_write_cost = if is_anthropic && cache_creation_tokens > 0 {
        cache_creation_tokens
            .saturating_mul(input_price)
            .saturating_mul(125)
            / 100
            / 1_000_000
    } else {
        0
    };

    input_cost
        .saturating_add(output_cost)
        .saturating_add(cache_read_cost)
        .saturating_add(cache_write_cost)
}

/// Decision returned by `budget_decision()`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum BudgetDecision {
    /// Spend is within budget; proceed normally.
    Allow,
    /// Spend has reached 80% of budget; proceed but emit a warning.
    Warn,
    /// Spend has met or exceeded budget cap; the call must be blocked.
    Stop,
}

/// Pure budget enforcement decision.
///
/// `spent_micros`: accumulated microdollar spend for this root.
/// `cap_micros`: the configured cap in microdollars.
///
/// Returns `Stop` when `spent >= cap`, `Warn` when `spent >= 0.8 * cap`,
/// `Allow` otherwise.
pub fn budget_decision(spent_micros: u64, cap_micros: u64) -> BudgetDecision {
    if spent_micros >= cap_micros {
        BudgetDecision::Stop
    } else if spent_micros * 10 >= cap_micros * 8 {
        // spent/cap >= 0.8 without floating point
        BudgetDecision::Warn
    } else {
        BudgetDecision::Allow
    }
}

/// Reset the ledger entry for `root_id` (useful for tests; no-op in production).
#[doc(hidden)]
pub fn reset_for_test(root_id: &str) {
    let mut map = ledger().lock().unwrap();
    map.remove(root_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use a unique prefix per test to avoid interference from the global ledger.

    #[test]
    fn test_record_and_spent_micros_start_at_zero() {
        let id = "test-root-zero";
        reset_for_test(id);
        assert_eq!(spent_micros(id), 0);
    }

    #[test]
    fn test_record_spend_accumulates() {
        let id = "test-root-accum";
        reset_for_test(id);
        record_spend(id, 1_000);
        record_spend(id, 2_000);
        assert_eq!(spent_micros(id), 3_000);
    }

    #[test]
    fn test_record_spend_saturates_on_overflow() {
        let id = "test-root-overflow";
        reset_for_test(id);
        record_spend(id, u64::MAX - 1);
        record_spend(id, 10); // would overflow; saturating_add clamps to u64::MAX
        assert_eq!(spent_micros(id), u64::MAX);
    }

    #[test]
    fn test_usage_cost_micros_zero_tokens() {
        let cost = usage_cost_micros(Some(3_000_000), Some(15_000_000), None, 0, 0, 0, 0, false);
        assert_eq!(cost, 0);
    }

    #[test]
    fn test_usage_cost_micros_basic_input_output() {
        // 1M input tokens at $3/MTok = $3 = 3_000_000 micros
        // 1M output tokens at $15/MTok = $15 = 15_000_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None,
            1_000_000,
            1_000_000,
            0,
            0,
            false,
        );
        assert_eq!(cost, 18_000_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_read_cheaper() {
        // 1M cache-read tokens at $0.30/MTok = $0.30 = 300_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            Some(300_000),
            0,
            0,
            1_000_000,
            0,
            false,
        );
        assert_eq!(cost, 300_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_read_falls_back_to_input_price() {
        // cache_read_price_per_mtok_micros = None → falls back to input price
        // 1M cache-read at input price $3/MTok = 3_000_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None, // no cache read price
            0,
            0,
            1_000_000,
            0,
            false,
        );
        assert_eq!(cost, 3_000_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_write_anthropic_premium() {
        // 1M cache-creation tokens at $3/MTok * 1.25 = $3.75 = 3_750_000 micros
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None,
            0,
            0,
            0,
            1_000_000,
            true, // is_anthropic
        );
        assert_eq!(cost, 3_750_000);
    }

    #[test]
    fn test_usage_cost_micros_cache_write_no_premium_non_anthropic() {
        let cost = usage_cost_micros(
            Some(3_000_000),
            Some(15_000_000),
            None,
            0,
            0,
            0,
            1_000_000,
            false, // not anthropic
        );
        assert_eq!(cost, 0); // no write cost for non-anthropic
    }

    #[test]
    fn test_usage_cost_micros_missing_prices_are_zero() {
        let cost = usage_cost_micros(None, None, None, 1_000_000, 1_000_000, 1_000_000, 0, false);
        assert_eq!(cost, 0);
    }

    #[test]
    fn test_budget_decision_allow() {
        assert_eq!(budget_decision(0, 1_000_000), BudgetDecision::Allow);
        assert_eq!(budget_decision(799_999, 1_000_000), BudgetDecision::Allow);
    }

    #[test]
    fn test_budget_decision_warn_at_80_percent() {
        assert_eq!(budget_decision(800_000, 1_000_000), BudgetDecision::Warn);
        assert_eq!(budget_decision(999_999, 1_000_000), BudgetDecision::Warn);
    }

    #[test]
    fn test_budget_decision_stop_at_100_percent() {
        assert_eq!(budget_decision(1_000_000, 1_000_000), BudgetDecision::Stop);
        assert_eq!(budget_decision(1_500_000, 1_000_000), BudgetDecision::Stop);
    }

    #[test]
    fn test_usd_to_micros() {
        assert_eq!(usd_to_micros(1.0), 1_000_000);
        assert_eq!(usd_to_micros(0.5), 500_000);
        assert_eq!(usd_to_micros(0.0), 0);
    }
}
