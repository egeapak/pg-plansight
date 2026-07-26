//! The streaming grouping path must be indistinguishable from the batch path.
//!
//! `QueryGrouper` exists to bound memory, not to change results. Everything the
//! TUI, the exporter and the export format read — the representative plan, the
//! execution list, every statistic — has to come out the same whether plans were
//! folded as they were parsed or grouped from a fully materialized vector. These
//! tests compare the two outputs field by field.

use pg_plansight_core::{DateFilter, PostgreSQLLogParser, ProcessedQuery, QueryGrouper};

/// A log with several query shapes, repeated executions per shape, varying
/// durations (including exact ties, which decide the representative), and
/// timestamps that are deliberately not monotonic per group.
fn sample_log() -> String {
    let mut out = String::new();
    let shapes = [
        ("SELECT * FROM users WHERE id = {}", "users", "users_pkey"),
        (
            "SELECT * FROM orders WHERE user_id = {}",
            "orders",
            "orders_user_idx",
        ),
        (
            "UPDATE sessions SET seen = now() WHERE id = {}",
            "sessions",
            "sessions_pkey",
        ),
    ];
    for i in 0..60usize {
        let (sql, table, index) = shapes[i % shapes.len()];
        // Durations repeat so that duration ties occur within a group; the
        // batch path resolves them to the LAST maximum, and so must the fold.
        let duration = 10.0 + ((i % 7) as f64) * 5.0;
        let minute = (i * 7) % 60;
        let second = (i * 13) % 60;
        out.push_str(&format!(
            "2025-06-15 09:{minute:02}:{second:02}.{:03} UTC [{i}] LOG:  duration: {duration:.3} ms  plan:\n",
            i % 1000
        ));
        out.push_str(&format!(
            "\tQuery Text: {}\n",
            sql.replace("{}", &i.to_string())
        ));
        out.push_str("\tLimit  (cost=0.43..599.04 rows=1000 width=56)\n");
        out.push_str(&format!(
            "\t  ->  Index Scan using {index} on {table} t  (cost=0.43..95610.13 rows=159718 width=56)\n"
        ));
        out.push_str(&format!("\t        Index Cond: (t.id = {i})\n"));
    }
    // A statement sqlparser cannot parse: it must fall back to grouping by
    // exact text on both paths.
    out.push_str("2025-06-15 10:00:00.000 UTC [999] LOG:  duration: 1.500 ms  plan:\n");
    out.push_str("\tQuery Text: NOT VALID SQL ;;; @@\n");
    out.push_str("\tResult  (cost=0.00..0.01 rows=1 width=4)\n");
    out.push_str("2025-06-15 10:00:01.000 UTC [999] LOG:  end of log\n");
    out
}

/// Compare two grouped maps field by field, with a message naming what differs.
fn assert_groups_identical(
    streamed: &hashbrown::HashMap<String, ProcessedQuery>,
    batched: &hashbrown::HashMap<String, ProcessedQuery>,
) {
    assert_eq!(
        streamed.len(),
        batched.len(),
        "different number of fingerprints"
    );

    for (fingerprint, want) in batched {
        let got = streamed
            .get(fingerprint)
            .unwrap_or_else(|| panic!("streamed output is missing fingerprint {fingerprint}"));

        assert_eq!(
            got.representative_plan, want.representative_plan,
            "representative plan differs for {fingerprint}"
        );

        let (g, w) = (&got.statistics, &want.statistics);
        assert_eq!(g.count, w.count, "count differs for {fingerprint}");
        assert_eq!(
            g.total_duration_ms, w.total_duration_ms,
            "total differs for {fingerprint}"
        );
        assert_eq!(
            g.min_duration_ms, w.min_duration_ms,
            "min differs for {fingerprint}"
        );
        assert_eq!(
            g.max_duration_ms, w.max_duration_ms,
            "max differs for {fingerprint}"
        );
        assert_eq!(
            g.mean_duration_ms, w.mean_duration_ms,
            "mean differs for {fingerprint}"
        );
        assert_eq!(
            g.std_dev_ms, w.std_dev_ms,
            "std_dev differs for {fingerprint}"
        );
        assert_eq!(
            g.min_timestamp, w.min_timestamp,
            "min_timestamp differs for {fingerprint}"
        );
        assert_eq!(
            g.max_timestamp, w.max_timestamp,
            "max_timestamp differs for {fingerprint}"
        );
        assert_eq!(
            (
                g.percentiles.p25,
                g.percentiles.p50,
                g.percentiles.p90,
                g.percentiles.p95,
                g.percentiles.p99
            ),
            (
                w.percentiles.p25,
                w.percentiles.p50,
                w.percentiles.p90,
                w.percentiles.p95,
                w.percentiles.p99
            ),
            "percentiles differ for {fingerprint}"
        );

        // Execution order matters: the regression engine reads these in order.
        let g_exec: Vec<_> = g
            .executions
            .iter()
            .map(|e| (e.timestamp, e.duration_ms))
            .collect();
        let w_exec: Vec<_> = w
            .executions
            .iter()
            .map(|e| (e.timestamp, e.duration_ms))
            .collect();
        assert_eq!(
            g_exec, w_exec,
            "execution records differ (order or content) for {fingerprint}"
        );

        assert_eq!(
            g.hourly_histogram.len(),
            w.hourly_histogram.len(),
            "hourly histogram bucket count differs for {fingerprint}"
        );
        for (hour, want_bucket) in &w.hourly_histogram {
            let got_bucket = g
                .hourly_histogram
                .get(hour)
                .unwrap_or_else(|| panic!("missing hour bucket {hour} for {fingerprint}"));
            assert_eq!(got_bucket.count, want_bucket.count);
            assert_eq!(got_bucket.total_duration_ms, want_bucket.total_duration_ms);
            assert_eq!(got_bucket.min_duration_ms, want_bucket.min_duration_ms);
            assert_eq!(got_bucket.max_duration_ms, want_bucket.max_duration_ms);
            assert_eq!(got_bucket.mean_duration_ms, want_bucket.mean_duration_ms);
        }
    }
}

#[test]
fn streamed_grouping_matches_batched() {
    let log = sample_log();

    // Batch path: materialize every plan, then group.
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
    assert!(plans.len() > 50, "fixture should produce a real workload");
    let batched = parser.get_processed_queries(&plans);

    // Streaming path: fold each plan into its group and drop it.
    let mut streaming_parser = PostgreSQLLogParser::new();
    let mut grouper = QueryGrouper::new();
    streaming_parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    assert_eq!(
        grouper.accepted(),
        plans.len(),
        "the fold must see exactly the plans the batch path materialized"
    );
    let streamed = grouper.finish();

    assert_groups_identical(&streamed, &batched);
}

#[test]
fn merging_per_file_groupers_matches_one_sequential_pass() {
    // Multi-file parsing folds each file into its own grouper and merges them in
    // file order. That must be indistinguishable from folding the concatenated
    // input in one pass — including which execution wins the representative slot
    // when the same duration appears in two files.
    let log = sample_log();
    let split_at = log[..log.len() / 2]
        .rfind("\n2025-")
        .map(|i| i + 1)
        .expect("fixture has a line boundary near the middle");
    let (first, second) = log.split_at(split_at);

    let mut parser = PostgreSQLLogParser::new();
    let mut whole = QueryGrouper::new();
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut whole)
        .unwrap();
    let sequential = whole.finish();

    let mut merged = QueryGrouper::new();
    for part in [first, second] {
        let mut part_grouper = QueryGrouper::new();
        parser
            .parse_string_into_grouper(part, |_, _| {}, &mut part_grouper)
            .unwrap();
        merged.merge(part_grouper);
    }
    assert_eq!(
        merged.accepted(),
        sequential
            .values()
            .map(|q| q.statistics.count)
            .sum::<usize>()
    );
    let merged = merged.finish();

    assert_groups_identical(&merged, &sequential);
}

#[test]
fn filter_is_applied_before_folding() {
    // The date window used to be applied only after every plan of every file was
    // already resident, so `--since` narrowed the results without narrowing peak
    // memory. Folding applies it first: an out-of-window plan is never retained,
    // and never reaches a group.
    let log = sample_log();
    let since = chrono::DateTime::parse_from_rfc3339("2025-06-15T09:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let mut parser = PostgreSQLLogParser::new();
    let mut grouper = QueryGrouper::new().with_filter(DateFilter::new(Some(since), None));
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    let filtered = grouper.finish();

    assert!(!filtered.is_empty(), "some plans are inside the window");
    for query in filtered.values() {
        assert!(
            query.statistics.min_timestamp >= since,
            "a plan before the window survived the fold"
        );
        for execution in &query.statistics.executions {
            assert!(execution.timestamp >= since);
        }
    }

    // And it agrees with filtering the materialized vector after the fact.
    let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
    let kept: Vec<_> = plans.into_iter().filter(|p| p.timestamp >= since).collect();
    let batched = parser.get_processed_queries(&kept);
    assert_groups_identical(&filtered, &batched);
}

#[test]
fn plan_cap_stops_the_read_and_keeps_the_first_n() {
    // The exporter's per-file query cap. The batch path parsed everything and
    // sliced `&plans[..max]`; the fold stops the read loop instead. Both keep the
    // first N in parse order.
    let log = sample_log();
    const CAP: usize = 17;

    let mut parser = PostgreSQLLogParser::new();
    let mut grouper = QueryGrouper::new().with_max_plans(CAP);
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    assert_eq!(grouper.accepted(), CAP, "the cap bounds what is folded");
    assert!(grouper.truncated(), "truncation must be reported");
    let capped = grouper.finish();

    let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
    let batched = parser.get_processed_queries(&plans[..CAP]);
    assert_groups_identical(&capped, &batched);
}

#[test]
fn take_groups_isolates_cycles_but_keeps_the_cache() {
    // The exporter reuses one grouper across poll cycles: each cycle must see
    // only its own plans, while the fingerprint cache stays warm.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();
    let mut grouper = QueryGrouper::new();

    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    let first = grouper.take_groups();
    let first_total: usize = first.values().map(|q| q.statistics.count).sum();
    let warm_cache = grouper.fingerprint_cache_size();
    assert!(warm_cache > 0);

    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    let second = grouper.take_groups();
    let second_total: usize = second.values().map(|q| q.statistics.count).sum();

    assert_eq!(
        first_total, second_total,
        "the second cycle must not carry the first cycle's executions"
    );
    assert_groups_identical(&second, &first);
    assert!(
        grouper.fingerprint_cache_size() >= warm_cache,
        "the fingerprint cache must survive take_groups"
    );
}
