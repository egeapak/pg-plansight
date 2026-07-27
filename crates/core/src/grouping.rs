//! Streaming query grouping.
//!
//! The batch path ([`PostgreSQLLogParser::get_processed_queries`]) takes a fully
//! materialized `&[QueryPlan]` and reduces it to one [`ProcessedQuery`] per
//! fingerprint. That requires every plan of the run to be resident at once, and
//! a `QueryPlan` is expensive: the raw plan text, a `PlanLine` copy of that same
//! text, the node tree with per-node `original_text`, and three near-copies of
//! the SQL. Measured on a synthetic auto_explain log, that is ~4.4 KB resident
//! per plan — 6.2x the log bytes just to hold the parsed plans, rising to ~7.8x
//! at the peak of the grouping pass, which also holds the representative clones.
//! So a 2 GB rotated log needs ~12.5 GB before grouping starts and ~15.6 GB at
//! peak.
//!
//! [`QueryGrouper`] folds each plan into its group as it is produced and keeps
//! only the group's representative, so peak memory is
//!
//! ```text
//!   distinct fingerprints x sizeof(QueryPlan)  +  executions x 24 bytes
//! ```
//!
//! instead of `executions x sizeof(QueryPlan)`. The number of distinct
//! fingerprints is a property of the application (literals are parameterized
//! away by normalization), not of the log's length, so the first term stops
//! growing once the workload's query shapes have been seen.
//!
//! Two honest caveats on that formula:
//!
//! * The 24 bytes is `sizeof(ExecutionRecord)`, but the vector holding them
//!   grows by doubling, so the real cost is 24 bytes per *capacity* slot — up to
//!   2x the naive figure — and [`finalize_group`] transiently allocates a
//!   further 8 bytes per execution for the duration sort. Budget ~1.7x the
//!   formula's second term.
//! * Parsing several files in parallel gives each file its own grouper, so the
//!   first term is multiplied by however many are alive at once. The merge is a
//!   rayon `reduce`, which folds adjacent results as they finish rather than
//!   holding all of them, so that is bounded by the in-flight set rather than by
//!   the file count.
//!
//! The output is intended to be *identical* to the batch path, not merely
//! equivalent: same representative (slowest, last-wins on ties), same execution
//! order within a group, same statistics. `streamed_grouping_matches_batched`
//! asserts exactly that, against the batch path as the oracle.

use chrono::{DateTime, Utc};
use hashbrown::HashMap;
use std::collections::BTreeMap;

use crate::models::{DateFilter, ExecutionRecord, ProcessedQuery, QueryGroupStatistics, QueryPlan};
use crate::parser_utils::QueryStatisticsCalculator;
use crate::sql_analysis::normalize_query_enhanced;

/// Upper bound on the persistent fingerprint cache. Long-lived groupers (the
/// exporter daemon reuses one across poll cycles) otherwise grow an entry per
/// distinct raw query text forever.
pub const MAX_FINGERPRINT_CACHE_ENTRIES: usize = 100_000;

/// Distinct-fingerprint count at which a memory warning is emitted.
///
/// With streaming grouping this — not the execution count — is what drives peak
/// memory, since one representative plan is retained per fingerprint. A normal
/// workload settles in the hundreds; reaching this many means normalization is
/// failing to collapse something (each malformed statement falls back to a
/// per-text fingerprint) and memory will grow with the log.
///
/// The number is chosen so the warning is still actionable. A retained group
/// measures ~7.4 KB (representative plan plus its execution vector), so 50,000
/// groups is roughly 370 MB already committed — enough to be worth reporting,
/// while an earlier warning at the previous 200,000 would only have arrived
/// past 1.5 GB, by which point an operator can no longer do anything about it.
pub const GROUP_RETENTION_WARN_THRESHOLD: usize = 50_000;

/// Bounded LRU cache mapping a query-text hash to its normalized fingerprint.
///
/// A long-lived parser would otherwise grow one entry per distinct query text
/// forever — and distinct *texts* are unbounded even when distinct
/// *fingerprints* are not, since `WHERE id = 1` and `WHERE id = 2` are two texts
/// with one fingerprint. Evicting the least-recently-used entry when full keeps
/// the hot working set warm at steady cost — unlike clearing the whole cache at
/// the threshold, which periodically dropped every entry and re-normalized the
/// entire next batch (a recurring CPU sawtooth, and permanently useless for a
/// working set just over the cap).
#[derive(Debug)]
pub struct FingerprintCache {
    cap: usize,
    tick: u64,
    /// hash -> (fingerprint, last-access tick).
    entries: HashMap<u64, (String, u64)>,
    /// last-access tick -> hash; the first key is the least-recently-used entry.
    order: BTreeMap<u64, u64>,
}

impl FingerprintCache {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cap,
            tick: 0,
            entries: HashMap::with_capacity(cap.min(1024)),
            order: BTreeMap::new(),
        }
    }

    /// Return the fingerprint for `hash`, refreshing its recency on a hit.
    pub fn get(&mut self, hash: u64) -> Option<String> {
        let (fingerprint, old_tick) = {
            let entry = self.entries.get(&hash)?;
            (entry.0.clone(), entry.1)
        };
        self.tick += 1;
        let now = self.tick;
        self.order.remove(&old_tick);
        self.order.insert(now, hash);
        if let Some(entry) = self.entries.get_mut(&hash) {
            entry.1 = now;
        }
        Some(fingerprint)
    }

    /// Insert or refresh `hash`, evicting the least-recently-used entry when the
    /// cap would be exceeded (`cap == 0` disables the bound).
    pub fn insert(&mut self, hash: u64, fingerprint: String) {
        self.tick += 1;
        let now = self.tick;
        if let Some(entry) = self.entries.get_mut(&hash) {
            self.order.remove(&entry.1);
            entry.0 = fingerprint;
            entry.1 = now;
            self.order.insert(now, hash);
            return;
        }
        if self.cap > 0
            && self.entries.len() >= self.cap
            && let Some((&lru_tick, &lru_hash)) = self.order.iter().next()
        {
            self.order.remove(&lru_tick);
            self.entries.remove(&lru_hash);
        }
        self.entries.insert(hash, (fingerprint, now));
        self.order.insert(now, hash);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.tick = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for FingerprintCache {
    fn default() -> Self {
        Self::with_capacity(MAX_FINGERPRINT_CACHE_ENTRIES)
    }
}

/// Calculate a fast hash for a query string using xxHash.
pub(crate) fn query_hash(query: &str) -> u64 {
    xxhash_rust::xxh3::xxh3_64(query.as_bytes())
}

/// Resolve a query text to its fingerprint, consulting `cache` first.
///
/// Malformed SQL that `sqlparser` rejects falls back to the raw text hash, so
/// such statements group by exact text rather than collapsing into one bucket.
pub(crate) fn fingerprint_for(cache: &mut FingerprintCache, query_text: &str) -> String {
    let hash = query_hash(query_text);
    if let Some(cached) = cache.get(hash) {
        return cached;
    }
    let fingerprint = match normalize_query_enhanced(query_text) {
        Ok(result) => result.fingerprint,
        Err(_) => format!("{:016x}", hash),
    };
    cache.insert(hash, fingerprint.clone());
    fingerprint
}

/// One fingerprint's in-progress aggregate: the representative plan plus the
/// lightweight per-execution records the statistics are computed from.
#[derive(Debug)]
struct GroupAccumulator {
    /// Slowest execution seen so far. Ties resolve to the *last* one, matching
    /// the batch path's `is_ge` comparison against `Iterator::max_by`.
    representative: QueryPlan,
    representative_duration: f64,
    executions: Vec<ExecutionRecord>,
    min_timestamp: DateTime<Utc>,
    max_timestamp: DateTime<Utc>,
}

impl GroupAccumulator {
    fn new(plan: QueryPlan) -> Self {
        let timestamp = plan.timestamp;
        let duration_ms = plan.duration_ms;
        Self {
            representative_duration: duration_ms,
            representative: plan,
            executions: vec![ExecutionRecord {
                timestamp,
                duration_ms,
            }],
            min_timestamp: timestamp,
            max_timestamp: timestamp,
        }
    }

    fn push(&mut self, plan: QueryPlan) {
        let timestamp = plan.timestamp;
        let duration_ms = plan.duration_ms;
        if timestamp < self.min_timestamp {
            self.min_timestamp = timestamp;
        }
        if timestamp > self.max_timestamp {
            self.max_timestamp = timestamp;
        }
        self.executions.push(ExecutionRecord {
            timestamp,
            duration_ms,
        });
        // `is_ge` makes the LAST maximum win on duration ties, matching
        // `Iterator::max_by`; `total_cmp` orders NaN deterministically instead
        // of collapsing to `false` the way `>=` would.
        if duration_ms.total_cmp(&self.representative_duration).is_ge() {
            self.representative_duration = duration_ms;
            self.representative = plan;
        }
        // `plan` is dropped here unless it became the representative — this is
        // the whole point of the streaming path.
    }

    /// Fold `other` in, preserving the ordering semantics of a single sequential
    /// pass in which every execution of `self` preceded every execution of
    /// `other`. Callers must merge in that order (see
    /// `PostgreSQLLogParser::parse_multiple_files_async`, which merges per-file
    /// groupers in file order).
    fn merge(&mut self, other: GroupAccumulator) {
        if other.min_timestamp < self.min_timestamp {
            self.min_timestamp = other.min_timestamp;
        }
        if other.max_timestamp > self.max_timestamp {
            self.max_timestamp = other.max_timestamp;
        }
        self.executions.extend(other.executions);
        if other
            .representative_duration
            .total_cmp(&self.representative_duration)
            .is_ge()
        {
            self.representative_duration = other.representative_duration;
            self.representative = other.representative;
        }
    }

    fn finish(self) -> ProcessedQuery {
        finalize_group(
            self.representative,
            self.executions,
            self.min_timestamp,
            self.max_timestamp,
        )
    }
}

/// Assemble the statistics for one group. Shared by the streaming path and by
/// [`PostgreSQLLogParser::get_processed_queries`] so the two cannot drift.
pub(crate) fn finalize_group(
    representative_plan: QueryPlan,
    executions: Vec<ExecutionRecord>,
    min_timestamp: DateTime<Utc>,
    max_timestamp: DateTime<Utc>,
) -> ProcessedQuery {
    let count = executions.len();

    // Sum/mean/std-dev/min/max/percentiles in one fused calculation with a
    // single shared sort.
    let mut durations: Vec<f64> = executions.iter().map(|e| e.duration_ms).collect();
    let stats = QueryStatisticsCalculator::calculate_group_duration_stats(&mut durations);
    let hourly_histogram = QueryStatisticsCalculator::generate_hourly_histogram(&executions);

    let statistics = QueryGroupStatistics {
        count,
        total_duration_ms: stats.total,
        min_duration_ms: stats.min,
        max_duration_ms: stats.max,
        mean_duration_ms: stats.mean,
        std_dev_ms: stats.std_dev,
        min_timestamp,
        max_timestamp,
        percentiles: stats.percentiles,
        hourly_histogram,
        executions,
    };

    ProcessedQuery {
        representative_plan,
        statistics,
        // Phase 2/3 analysis stays lazy — the TUI fills these in after grouping.
        complexity_score: None,
        metadata: None,
        regression_analysis: None,
        plan_analysis: None,
    }
}

/// Folds plans into per-fingerprint groups as they are produced, retaining one
/// representative plan per group rather than every plan of the run.
///
/// The fingerprint cache survives [`take_groups`](Self::take_groups), so a
/// long-lived caller (the exporter reuses one grouper across poll cycles) keeps
/// a warm cache while still getting per-cycle isolation of the group map.
#[derive(Debug)]
pub struct QueryGrouper {
    groups: HashMap<String, GroupAccumulator>,
    cache: FingerprintCache,
    /// Memo for the immediately preceding query text. Consecutive executions of
    /// one statement are the common shape in a log, and this answers them
    /// without the LRU's recency bookkeeping. The batch path used an unbounded
    /// per-batch `HashMap<&str, String>` for this; that cannot be carried over,
    /// because keys are *raw texts* (unbounded) rather than fingerprints.
    last: Option<(u64, String)>,
    /// Optional time window. Applied before folding so out-of-window plans are
    /// never retained — the batch path filtered only after every plan of every
    /// file was already resident, which made `--since` useless for memory.
    filter: Option<DateFilter>,
    /// Stop folding after this many *accepted* plans (0 = unlimited).
    max_plans: usize,
    accepted: usize,
    truncated: bool,
    /// Distinct-fingerprint count that triggers the high-cardinality warning.
    /// Overridable so the behaviour is testable without folding a threshold's
    /// worth of real plans, and so an embedder under tighter memory limits than
    /// a CLI — the pg extension runs inside a backend — can lower it.
    group_warn_threshold: usize,
    /// Emit the high-cardinality warning at most once per grouper. Deliberately
    /// *not* reset by [`take_groups`](Self::take_groups): the point is one
    /// report per process, not one per poll cycle.
    retention_warned: bool,
}

impl QueryGrouper {
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
            cache: FingerprintCache::default(),
            last: None,
            filter: None,
            max_plans: 0,
            accepted: 0,
            truncated: false,
            group_warn_threshold: GROUP_RETENTION_WARN_THRESHOLD,
            retention_warned: false,
        }
    }

    /// Drop plans outside `filter` instead of folding them.
    pub fn with_filter(mut self, filter: DateFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Stop accepting plans after `max_plans` (`0` disables the cap). Plans past
    /// the cap are dropped and [`truncated`](Self::truncated) becomes true;
    /// which plans survive is the same "first N in parse order" the batch path's
    /// `&plans[..max]` slice produced.
    pub fn with_max_plans(mut self, max_plans: usize) -> Self {
        self.max_plans = max_plans;
        self
    }

    /// Override the distinct-fingerprint count at which the high-cardinality
    /// warning fires (see [`GROUP_RETENTION_WARN_THRESHOLD`]).
    pub fn with_group_warn_threshold(mut self, threshold: usize) -> Self {
        self.group_warn_threshold = threshold;
        self
    }

    /// True once the high-cardinality warning has fired for this grouper.
    pub fn warned_high_cardinality(&self) -> bool {
        self.retention_warned
    }

    /// Change the plan cap on an existing grouper.
    ///
    /// Distinct from [`with_max_plans`](Self::with_max_plans) because a
    /// long-lived caller must be able to apply a reconfiguration — the exporter
    /// reloads its config on SIGHUP — without rebuilding the grouper and
    /// throwing away its warm fingerprint cache.
    pub fn set_max_plans(&mut self, max_plans: usize) {
        self.max_plans = max_plans;
    }

    /// True once the plan cap has been reached, so the caller can stop reading.
    pub fn is_full(&self) -> bool {
        self.max_plans > 0 && self.accepted >= self.max_plans
    }

    /// True if any plan was dropped because the cap was reached.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Record that input was dropped because the cap was reached. The read loop
    /// calls this when it stops early with bytes still pending; `fold` cannot
    /// observe it itself, since a full grouper is never offered another plan.
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Number of plans folded in (excludes filtered and truncated plans).
    pub fn accepted(&self) -> usize {
        self.accepted
    }

    /// Number of distinct fingerprints seen so far — the term that drives peak
    /// memory on the streaming path.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    pub fn fingerprint_cache_size(&self) -> usize {
        self.cache.len()
    }

    pub fn clear_fingerprint_cache(&mut self) {
        self.cache.clear();
        self.last = None;
    }

    fn fingerprint(&mut self, query_text: &str) -> String {
        let hash = query_hash(query_text);
        if let Some((last_hash, fingerprint)) = &self.last
            && *last_hash == hash
        {
            return fingerprint.clone();
        }
        let fingerprint = fingerprint_for(&mut self.cache, query_text);
        self.last = Some((hash, fingerprint.clone()));
        fingerprint
    }

    /// Fold one plan into its group. Returns false if the plan was dropped
    /// (outside the time window, or past the plan cap).
    pub fn fold(&mut self, plan: QueryPlan) -> bool {
        if let Some(filter) = &self.filter
            && !filter.matches(plan.timestamp)
        {
            return false;
        }
        if self.is_full() {
            self.truncated = true;
            return false;
        }
        self.accepted += 1;

        let fingerprint = self.fingerprint(&plan.query_text);
        match self.groups.get_mut(&fingerprint) {
            Some(group) => group.push(plan),
            None => {
                self.groups.insert(fingerprint, GroupAccumulator::new(plan));
                self.warn_if_high_cardinality();
            }
        }
        true
    }

    /// Report once per grouper that the retained-representative count has grown
    /// past the point where it drives memory.
    ///
    /// Called from every path that can add a group — `fold` and `merge` alike.
    /// Checking only `fold` missed the multi-file case entirely, where each
    /// file's grouper can stay under the threshold and only the merged map
    /// exceeds it.
    fn warn_if_high_cardinality(&mut self) {
        if self.retention_warned || self.groups.len() < self.group_warn_threshold {
            return;
        }
        self.retention_warned = true;
        tracing::warn!(
            groups = self.groups.len(),
            "Very large number of distinct query fingerprints. One representative \
             plan is retained per fingerprint, so memory grows with this count. \
             A normal workload settles in the hundreds; this many usually means \
             normalization is not collapsing something (statements sqlparser \
             cannot parse fall back to grouping by exact text). Narrow the window \
             with --since/--until if this run is at risk of exhausting memory."
        );
    }

    /// Fold every plan of `other` in. `other` must cover a range that follows
    /// `self`'s in parse order, so that representatives and execution ordering
    /// match a single sequential pass.
    pub fn merge(&mut self, other: QueryGrouper) {
        self.accepted += other.accepted;
        self.truncated |= other.truncated;
        for (fingerprint, group) in other.groups {
            match self.groups.get_mut(&fingerprint) {
                Some(existing) => existing.merge(group),
                None => {
                    self.groups.insert(fingerprint, group);
                }
            }
        }
        self.warn_if_high_cardinality();
        self.last = None;
    }

    /// Finalize and take the accumulated groups, leaving the grouper empty and
    /// ready to accumulate again. The fingerprint cache is deliberately kept.
    pub fn take_groups(&mut self) -> HashMap<String, ProcessedQuery> {
        self.accepted = 0;
        self.truncated = false;
        self.last = None;
        let groups = std::mem::take(&mut self.groups);
        finalize_groups(groups)
    }

    /// Consume the grouper and produce its groups.
    pub fn finish(mut self) -> HashMap<String, ProcessedQuery> {
        self.take_groups()
    }
}

/// Finalize every group, in parallel where the feature allows.
fn finalize_groups(groups: HashMap<String, GroupAccumulator>) -> HashMap<String, ProcessedQuery> {
    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        groups
            .into_par_iter()
            .map(|(fingerprint, group)| (fingerprint, group.finish()))
            .collect()
    }
    #[cfg(not(feature = "parallel"))]
    {
        groups
            .into_iter()
            .map(|(fingerprint, group)| (fingerprint, group.finish()))
            .collect()
    }
}

impl Default for QueryGrouper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_cache_evicts_least_recently_used() {
        let mut cache = FingerprintCache::with_capacity(2);
        cache.insert(1, "a".to_string());
        cache.insert(2, "b".to_string());
        // Touch key 1 so key 2 becomes the least-recently-used.
        assert_eq!(cache.get(1).as_deref(), Some("a"));
        // Inserting a third key evicts the LRU (key 2), not key 1.
        cache.insert(3, "c".to_string());
        assert_eq!(cache.get(2), None, "LRU entry must be evicted");
        assert_eq!(cache.get(1).as_deref(), Some("a"), "touched entry survives");
        assert_eq!(cache.get(3).as_deref(), Some("c"));
        assert_eq!(cache.len(), 2, "cache stays bounded at its capacity");
    }

    #[test]
    fn fingerprint_cache_reinsert_refreshes_without_growing() {
        let mut cache = FingerprintCache::with_capacity(2);
        cache.insert(1, "a".to_string());
        cache.insert(1, "a2".to_string()); // same key updates in place
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(1).as_deref(), Some("a2"));
    }

    #[test]
    fn fingerprint_cache_zero_cap_is_unbounded() {
        let mut cache = FingerprintCache::with_capacity(0);
        for i in 0..10 {
            cache.insert(i, format!("f{i}"));
        }
        assert_eq!(cache.len(), 10, "cap 0 disables eviction");
    }

    #[test]
    fn take_groups_resets_groups_but_keeps_the_cache() {
        let mut grouper = QueryGrouper::new();
        let plan = crate::capture::query_plan_from_capture(
            chrono::Utc::now(),
            1.0,
            "SELECT 1".to_string(),
            "Result  (cost=0.00..0.01 rows=1 width=4)",
        )
        .unwrap();
        assert!(grouper.fold(plan));
        assert_eq!(grouper.group_count(), 1);

        let first = grouper.take_groups();
        assert_eq!(first.len(), 1);
        assert_eq!(grouper.group_count(), 0, "groups reset for the next batch");
        assert_eq!(grouper.accepted(), 0);
        assert!(
            grouper.fingerprint_cache_size() > 0,
            "the fingerprint cache must survive so a long-lived caller stays warm"
        );
    }
}
