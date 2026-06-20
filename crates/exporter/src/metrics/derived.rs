//! Pure helper functions for F7 derived per-query metrics.
//!
//! These are registry-free so they can be unit-tested directly and called from
//! `collector.rs`. Each guards divide-by-zero by returning `0.0`.

/// Coefficient of variation = stddev / mean. Guards mean == 0 -> 0.0.
pub(crate) fn coefficient_of_variation(mean: f64, stddev: f64) -> f64 {
    if mean == 0.0 { 0.0 } else { stddev / mean }
}

/// Share of grand total as a percentage. Guards grand_total == 0 -> 0.0.
pub(crate) fn time_share_pct(total: f64, grand_total: f64) -> f64 {
    if grand_total == 0.0 {
        0.0
    } else {
        total / grand_total * 100.0
    }
}

/// Rows per call. Guards calls == 0 -> 0.0.
/// NOTE (F7): emission deferred — no per-query aggregate rows value exists in
/// `QueryGroupStatistics` today. Helper + tests land now for when one does.
#[allow(dead_code)]
pub(crate) fn rows_per_call(rows: f64, calls: f64) -> f64 {
    if calls == 0.0 { 0.0 } else { rows / calls }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cv_basic() {
        assert_eq!(coefficient_of_variation(100.0, 50.0), 0.5);
    }

    #[test]
    fn test_cv_zero_mean_guard() {
        assert_eq!(coefficient_of_variation(0.0, 50.0), 0.0);
    }

    #[test]
    fn test_cv_zero_stddev() {
        assert_eq!(coefficient_of_variation(100.0, 0.0), 0.0);
    }

    #[test]
    fn test_time_share_basic() {
        assert_eq!(time_share_pct(25.0, 100.0), 25.0);
    }

    #[test]
    fn test_time_share_zero_grand_guard() {
        assert_eq!(time_share_pct(25.0, 0.0), 0.0);
    }

    #[test]
    fn test_time_share_full() {
        assert_eq!(time_share_pct(100.0, 100.0), 100.0);
    }

    #[test]
    fn test_rows_per_call_basic() {
        assert_eq!(rows_per_call(1000.0, 10.0), 100.0);
    }

    #[test]
    fn test_rows_per_call_zero_calls_guard() {
        assert_eq!(rows_per_call(1000.0, 0.0), 0.0);
    }
}
