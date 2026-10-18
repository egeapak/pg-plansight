//! Fixed latency histogram buckets shared by the extension's metrics views and
//! the exporter's `[postgres]` source. The bounds are stored implicitly in
//! cumulative bucket arrays, so changing them needs a data migration.

/// Upper bounds in seconds, matching the exporter's default `histogram_buckets`.
/// A bucket array has one count per bound plus a final overflow count.
pub const LATENCY_BUCKET_BOUNDS_SECONDS: [f64; 9] =
    [0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0];

/// Length of a bucket array: one per bound plus the overflow bucket.
pub const LATENCY_BUCKET_COUNT: usize = LATENCY_BUCKET_BOUNDS_SECONDS.len() + 1;

/// Index of the bucket a duration falls into: the first bound it does not exceed
/// (Prometheus `le` semantics), or the overflow bucket.
pub fn latency_bucket_index(duration_ms: f64) -> usize {
    let seconds = duration_ms / 1000.0;
    LATENCY_BUCKET_BOUNDS_SECONDS
        .iter()
        .position(|bound| seconds <= *bound)
        .unwrap_or(LATENCY_BUCKET_BOUNDS_SECONDS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_land_in_the_first_bound_they_do_not_exceed() {
        assert_eq!(latency_bucket_index(0.5), 0);
        assert_eq!(latency_bucket_index(1.0), 0, "le is inclusive");
        assert_eq!(latency_bucket_index(5.0), 1);
        assert_eq!(latency_bucket_index(50.0), 2);
        assert_eq!(latency_bucket_index(299_000.0), 8);
        assert_eq!(latency_bucket_index(301_000.0), 9, "overflow bucket");
    }

    #[test]
    fn bucket_count_includes_overflow() {
        assert_eq!(LATENCY_BUCKET_COUNT, 10);
    }
}
