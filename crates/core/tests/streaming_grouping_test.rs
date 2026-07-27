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
        // Straddle an hour boundary so every group lands in more than one
        // `hourly_histogram` bucket — with a single bucket the histogram
        // comparison below proves almost nothing.
        let hour = 9 + (i % 2);
        out.push_str(&format!(
            "2025-06-15 {hour:02}:{minute:02}:{second:02}.{:03} UTC [{i}] LOG:  duration: {duration:.3} ms  plan:\n",
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

/// The fixture is only worth anything if it actually exercises the paths the
/// equivalence claim rests on. Assert its properties rather than describing
/// them in a comment that can quietly stop being true.
#[test]
fn fixture_exercises_ties_inversions_and_multiple_hour_buckets() {
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(&sample_log(), |_, _| {})
        .unwrap();
    let groups = parser.get_processed_queries(&plans);

    let mut groups_with_max_ties = 0;
    let mut groups_with_inversions = 0;
    let mut groups_with_multiple_buckets = 0;
    for query in groups.values() {
        let stats = &query.statistics;
        let at_max = stats
            .executions
            .iter()
            .filter(|e| e.duration_ms == stats.max_duration_ms)
            .count();
        if at_max > 1 {
            groups_with_max_ties += 1;
        }
        if stats
            .executions
            .windows(2)
            .any(|w| w[1].timestamp < w[0].timestamp)
        {
            groups_with_inversions += 1;
        }
        if stats.hourly_histogram.len() > 1 {
            groups_with_multiple_buckets += 1;
        }
    }

    assert!(
        groups_with_max_ties >= 2,
        "duration ties at the max decide the representative; fixture has {groups_with_max_ties} such groups"
    );
    assert!(
        groups_with_inversions >= 2,
        "out-of-order timestamps exercise min/max tracking; fixture has {groups_with_inversions} such groups"
    );
    assert!(
        groups_with_multiple_buckets >= 2,
        "the histogram comparison is vacuous with one bucket; fixture has {groups_with_multiple_buckets} multi-bucket groups"
    );
    assert!(
        groups
            .keys()
            .any(|f| f.len() == 16 && u64::from_str_radix(f, 16).is_ok()),
        "fixture must include a statement sqlparser rejects, which falls back to a hash fingerprint"
    );
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
    assert_eq!(
        grouper.accepted(),
        0,
        "a new cycle must not inherit the previous cycle's plan count"
    );

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

#[test]
fn truncation_does_not_leak_into_the_next_cycle() {
    // The exporter reads `truncated()` every poll cycle from a grouper it
    // reuses. A flag that survives `take_groups` would make one capped cycle
    // report truncation forever after.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();
    let mut grouper = QueryGrouper::new().with_max_plans(5);

    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    assert!(grouper.truncated(), "the capped cycle truncates");
    let _ = grouper.take_groups();
    assert!(
        !grouper.truncated(),
        "truncation must reset with the groups, or every later cycle reports it"
    );

    // An uncapped cycle on the same grouper must come back clean.
    grouper.set_max_plans(0);
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    assert!(!grouper.truncated(), "an uncapped cycle truncates nothing");
}

#[test]
fn set_max_plans_applies_to_a_reused_grouper() {
    // The exporter reconfigures the cap on SIGHUP without rebuilding the
    // grouper, so the setter must take effect on the next cycle and must not
    // discard the warm fingerprint cache.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();
    let mut grouper = QueryGrouper::new();

    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    let uncapped = grouper.accepted();
    let _ = grouper.take_groups();
    let warm_cache = grouper.fingerprint_cache_size();
    assert!(uncapped > 9);

    grouper.set_max_plans(9);
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();
    assert_eq!(grouper.accepted(), 9, "the reloaded cap must be enforced");
    assert!(grouper.truncated());
    assert!(
        grouper.fingerprint_cache_size() >= warm_cache,
        "reconfiguring must not throw away the fingerprint cache"
    );
}

#[test]
fn filter_runs_before_the_cap_so_dropped_plans_do_not_consume_budget() {
    // Order matters: if the cap were checked first, out-of-window plans arriving
    // after the cap would set `truncated` even though the window, not the cap,
    // excluded them — and in-window plans would lose budget to plans that were
    // never going to be kept.
    let log = sample_log();
    let since = chrono::DateTime::parse_from_rfc3339("2025-06-15T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let mut parser = PostgreSQLLogParser::new();
    let mut plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
    plans.retain(|p| p.timestamp >= since);
    let in_window = plans.len();
    let total = parser
        .parse_string_with_progress(&log, |_, _| {})
        .unwrap()
        .len();
    assert!(
        in_window > 0 && in_window < total,
        "the window must exclude some but not all plans ({in_window} of {total})"
    );

    // A cap well above the in-window count can never be reached, no matter how
    // many out-of-window plans precede those that survive.
    let cap = in_window + 1;
    let mut grouper = QueryGrouper::new()
        .with_filter(DateFilter::new(Some(since), None))
        .with_max_plans(cap);
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut grouper)
        .unwrap();

    assert_eq!(
        grouper.accepted(),
        in_window,
        "filtered-out plans must not consume cap budget"
    );
    assert!(
        !grouper.truncated(),
        "the cap was never reached, so nothing was truncated"
    );
}

#[test]
fn fold_reports_truncation_on_its_own() {
    // Callers that fold directly (the pg extension aggregates captures this way)
    // never go through the read loop, so `mark_truncated` is not involved --
    // `fold` itself has to report the cap being hit.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();

    let mut grouper = QueryGrouper::new().with_max_plans(3);
    let mut accepted = 0;
    for plan in plans {
        if grouper.fold(plan) {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 3);
    assert_eq!(grouper.accepted(), 3);
    assert!(
        grouper.truncated(),
        "fold must flag truncation without help from the read loop"
    );
}

/// End-to-end oracle for the only entry point that actually merges per-file
/// groupers.
///
/// The unit-level merge test covers `QueryGrouper::merge` itself, but nothing
/// covered `parse_multiple_files_async`, which is where file order is
/// established. Reversing the merge there previously went unnoticed by the whole
/// suite. The fixture makes order observable: the same fingerprint reaches its
/// maximum duration in every file, so the representative can only be correct if
/// the last file wins.
#[cfg(all(feature = "parallel", feature = "file-io"))]
#[test]
fn multi_file_parse_matches_a_single_pass_over_the_concatenation() {
    use pg_plansight_core::ParseProgress;
    use std::io::Write as _;

    // Three files, each containing the shared shape at the SAME maximum
    // duration (42.0) plus a file-specific slower-to-faster spread. Whichever
    // file is merged last owns the representative.
    let file_body = |marker: &str, minute: u32| {
        let mut out = String::new();
        for (i, duration) in [12.0_f64, 42.0, 7.0].iter().enumerate() {
            out.push_str(&format!(
                "2025-06-15 09:{minute:02}:{:02}.000 UTC [{i}] LOG:  duration: {duration:.3} ms  plan:\n",
                i * 5
            ));
            out.push_str("\tQuery Text: SELECT * FROM shared WHERE id = 7\n");
            out.push_str("\tSeq Scan on shared  (cost=0.00..1.00 rows=1 width=8)\n");
            // A per-file shape too, so the merge has disjoint keys to carry as
            // well as a shared one.
            out.push_str(&format!(
                "2025-06-15 09:{minute:02}:{:02}.000 UTC [{i}] LOG:  duration: {:.3} ms  plan:\n",
                i * 5 + 1,
                duration + 1.0
            ));
            out.push_str(&format!(
                "\tQuery Text: SELECT * FROM only_{marker} WHERE id = 7\n"
            ));
            out.push_str(&format!(
                "\tSeq Scan on only_{marker}  (cost=0.00..1.00 rows=1 width=8)\n"
            ));
        }
        out.push_str(&format!(
            "2025-06-15 09:{minute:02}:59.000 UTC [99] LOG:  checkpoint complete\n"
        ));
        out
    };

    let bodies = [file_body("a", 10), file_body("b", 20), file_body("c", 30)];
    let dir = std::env::temp_dir().join(format!("plansight-multifile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let path = dir.join(format!("pg-{i}.log"));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.flush().unwrap();
        paths.push(path);
    }

    let rx =
        PostgreSQLLogParser::parse_multiple_files_async(paths.clone(), DateFilter::new(None, None));
    let mut grouped = None;
    for msg in rx {
        if let ParseProgress::Complete { result } = msg {
            grouped = Some(result.expect("multi-file parse should succeed"));
            break;
        }
    }
    let grouped = grouped.expect("Complete message");

    // Oracle: one sequential pass over the files concatenated in the same order.
    let concatenated: String = bodies.concat();
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(&concatenated, |_, _| {})
        .unwrap();
    let expected = parser.get_processed_queries(&plans);

    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(grouped.plan_count, plans.len());
    assert_groups_identical(&grouped.groups, &expected);

    // Guard the guard: confirm the fixture really can tell the orders apart, so
    // this test cannot silently degrade into a tautology.
    let reversed: String = bodies.iter().rev().cloned().collect::<Vec<_>>().concat();
    let reversed_plans = parser
        .parse_string_with_progress(&reversed, |_, _| {})
        .unwrap();
    let reversed_groups = parser.get_processed_queries(&reversed_plans);
    let shared = expected
        .iter()
        .find(|(_, q)| q.original_query().contains("FROM shared"))
        .map(|(f, q)| (f.clone(), q.representative_plan.timestamp))
        .expect("shared fingerprint");
    let reversed_shared = reversed_groups
        .get(&shared.0)
        .expect("same fingerprint in both orders");
    assert_ne!(
        shared.1, reversed_shared.representative_plan.timestamp,
        "fixture must make merge order observable, otherwise this test proves nothing"
    );
}

/// The filter must be consulted before the cap *inside `fold`*, not merely in
/// aggregate.
///
/// The read-loop path cannot show this: it breaks out as soon as `is_full()`, so
/// no plan is ever offered after the cap is reached. Callers that fold directly
/// — the pg extension aggregating captures — do keep offering plans, and there
/// the order is observable: with the cap already met, an out-of-window plan must
/// be rejected by the *window* (silently) rather than by the cap (which would
/// claim data was truncated when the window is what excluded it).
#[test]
fn fold_checks_the_window_before_the_cap() {
    let log = sample_log();
    // An upper bound, so the tail of the log falls outside the window and keeps
    // arriving at `fold` after the cap has been satisfied.
    let until = chrono::DateTime::parse_from_rfc3339("2025-06-15T09:59:59Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser.parse_string_with_progress(&log, |_, _| {}).unwrap();
    let in_window = plans.iter().filter(|p| p.timestamp <= until).count();
    assert!(in_window > 0 && in_window < plans.len());
    let last_in_window = plans.iter().rposition(|p| p.timestamp <= until).unwrap();
    assert!(
        last_in_window < plans.len() - 1,
        "fixture must end with an out-of-window plan, or the ordering is untestable"
    );

    // Cap set exactly at the in-window count: it is reached, but never exceeded
    // by anything the window would have kept.
    let mut grouper = QueryGrouper::new()
        .with_filter(DateFilter::new(None, Some(until)))
        .with_max_plans(in_window);
    for plan in plans {
        grouper.fold(plan);
    }

    assert_eq!(grouper.accepted(), in_window);
    assert!(
        !grouper.truncated(),
        "plans excluded by the window must not be reported as truncated by the cap"
    );
}

#[test]
fn merge_propagates_truncation() {
    // A multi-file run where one file hit its cap must still report truncation
    // after the merge, or the loss is silent.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();

    let mut capped = QueryGrouper::new().with_max_plans(3);
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut capped)
        .unwrap();
    assert!(capped.truncated());

    let mut whole = QueryGrouper::new();
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut whole)
        .unwrap();
    assert!(!whole.truncated());

    whole.merge(capped);
    assert!(
        whole.truncated(),
        "truncation in any merged part must survive the merge"
    );
}

#[test]
fn high_cardinality_warning_fires_once_and_covers_the_merge_path() {
    // The warning is what tells an operator that nothing is grouping and memory
    // will track the log. It was previously checked only in `fold`, so a
    // multi-file run whose per-file groupers each stayed under the threshold
    // never warned however large the merged map became.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();

    // Threshold above what one fixture parse produces, so folding alone stays
    // quiet and only the merge can cross it.
    let mut a = QueryGrouper::new().with_group_warn_threshold(6);
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut a)
        .unwrap();
    assert!(
        a.group_count() < 6,
        "fixture must stay under the threshold on its own, got {}",
        a.group_count()
    );
    assert!(!a.warned_high_cardinality());

    // A second grouper with different shapes, also under the threshold alone.
    let other_log = log
        .replace("FROM users", "FROM archived_users")
        .replace("FROM orders", "FROM archived_orders");
    let mut b = QueryGrouper::new();
    parser
        .parse_string_into_grouper(&other_log, |_, _| {}, &mut b)
        .unwrap();
    assert!(!b.warned_high_cardinality());

    a.merge(b);
    assert!(
        a.group_count() >= 6,
        "the merged map must exceed the threshold for this test to mean anything"
    );
    assert!(
        a.warned_high_cardinality(),
        "merging past the threshold must warn; checking only fold missed multi-file runs"
    );

    // Once per grouper, and surviving take_groups: the condition is a property
    // of the workload, so repeating it every poll cycle would be noise.
    let _ = a.take_groups();
    assert!(a.warned_high_cardinality());
}

#[test]
fn empty_groupers_are_the_merge_identity() {
    // `parse_multiple_files_async` reduces with `QueryGrouper::new` as the
    // identity and substitutes an empty grouper for a file that failed to
    // parse, so empty-on-either-side has to be a no-op.
    let log = sample_log();
    let mut parser = PostgreSQLLogParser::new();

    let mut reference = QueryGrouper::new();
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut reference)
        .unwrap();
    let expected_accepted = reference.accepted();
    let expected = reference.finish();

    // empty <- full
    let mut left_empty = QueryGrouper::new();
    let mut full = QueryGrouper::new();
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut full)
        .unwrap();
    left_empty.merge(full);
    assert_eq!(left_empty.accepted(), expected_accepted);
    assert_groups_identical(&left_empty.finish(), &expected);

    // full <- empty
    let mut right_empty = QueryGrouper::new();
    parser
        .parse_string_into_grouper(&log, |_, _| {}, &mut right_empty)
        .unwrap();
    right_empty.merge(QueryGrouper::new());
    assert_eq!(right_empty.accepted(), expected_accepted);
    assert_groups_identical(&right_empty.finish(), &expected);

    // empty <- empty, and finishing an untouched grouper
    let mut both_empty = QueryGrouper::new();
    both_empty.merge(QueryGrouper::new());
    assert_eq!(both_empty.accepted(), 0);
    assert!(both_empty.finish().is_empty());
    assert!(QueryGrouper::new().finish().is_empty());
}
