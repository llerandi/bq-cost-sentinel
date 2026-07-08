//! Pure cost-calculation logic, kept free of async/HTTP so it can be unit
//! tested in isolation from the rest of the proxy.

/// Number of bytes in one Tebibyte (1024^4), matching BigQuery's on-demand
/// billing unit.
pub const BYTES_PER_TIB: f64 = 1_099_511_627_776.0;

/// Converts the number of bytes a query would process (as returned by the
/// BigQuery dry-run API) into an estimated cost, using the configured price
/// per Tebibyte.
pub fn calculate_cost(bytes_processed: u64, price_per_tib: f64) -> f64 {
    (bytes_processed as f64 / BYTES_PER_TIB) * price_per_tib
}

/// Returns `true` when `cost` strictly exceeds the configured budget.
/// A query costing exactly the limit is allowed through.
pub fn exceeds_budget(cost: f64, max_cost: f64) -> bool {
    cost > max_cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_tib_costs_exactly_the_configured_price() {
        let cost = calculate_cost(BYTES_PER_TIB as u64, 6.25);
        assert!((cost - 6.25).abs() < 1e-9);
    }

    #[test]
    fn zero_bytes_cost_nothing() {
        assert_eq!(calculate_cost(0, 6.25), 0.0);
    }

    #[test]
    fn half_a_tib_costs_half_the_price() {
        let half_tib_bytes = (BYTES_PER_TIB / 2.0) as u64;
        let cost = calculate_cost(half_tib_bytes, 10.0);
        assert!((cost - 5.0).abs() < 1e-6);
    }

    #[test]
    fn zero_price_per_tib_means_zero_cost() {
        assert_eq!(calculate_cost(BYTES_PER_TIB as u64, 0.0), 0.0);
    }

    #[test]
    fn does_not_panic_or_overflow_on_a_huge_byte_count() {
        // Guards the u64 -> f64 cast against overflow/NaN for very large scans.
        let cost = calculate_cost(u64::MAX, 6.25);
        assert!(cost.is_finite());
        assert!(cost > 0.0);
    }

    #[test]
    fn budget_check_is_a_strict_inequality() {
        assert!(
            !exceeds_budget(10.0, 10.0),
            "a cost exactly equal to the limit should not be blocked"
        );
        assert!(exceeds_budget(10.01, 10.0));
        assert!(!exceeds_budget(0.0, 0.0));
    }
}
