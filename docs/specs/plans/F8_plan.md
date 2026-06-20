# F8 — Extension accumulated metrics: implementation plan

Source spec: `docs/specs/F8_extension_metrics.md`.
Crate: `crates/pg_extension` (independent pgrx workspace).
Build/test target used throughout: `--no-default-features --features pg16`.

---

## 0. Feasibility decision (grounded in captured fields)

I read the exact field sets that flow from the executor hook to `StatRow`:

- **`Capture`** (`aggregate.rs:14-23`): `timestamp`, `duration_ms`, `query_text`,
  `plan_text`, `query_id`. **No** numeric WAL bytes, **no** numeric buffer
  counters, **no** plan hash / plan identifier.
- **Ring `Rec`** (`ring.rs:24-32`): `epoch_secs`, `duration_ms`, `query_id`,
  `sql_len`, `plan_len`, `sql`, `plan`. Same — no WAL/buffer numbers, no plan id.
- **`StatRow`** (`aggregate.rs:37-61`): `fingerprint`, `query_id`,
  `normalized_query`, `representative_sql`, `representative_plan`, `calls`,
  `total_time_ms`, `sum_sq_time_ms`, `min_time_ms`, `max_time_ms`,
  `first_seen_epoch`, `last_seen_epoch`, `complexity`, `metadata`,
  `plan_analysis`, `histogram`.
- The hook *does* request `INSTRUMENT_WAL` / `INSTRUMENT_BUFFERS`
  (`hook.rs:306-309`) when `track_io` is on, but those numbers are only ever
  embedded as **text** inside the rendered `EXPLAIN` plan; they are never
  extracted into a numeric field on `Capture` or the ring `Rec`.

| F8 sub-feature | Verdict | Reason |
|---|---|---|
| **#1 Coefficient of variation (CV)** | **FEASIBLE NOW** | Pure-Rust; derivable from existing `calls`, `total_time_ms`, `sum_sq_time_ms`. No new captured data. |
| **#2 SLO-breach counter** | **FEASIBLE NOW** | Needs only a new GUC + a per-capture compare of the already-captured `duration_ms`; new additive `StatRow` field. No capture-path redesign. |
| **#3 Cumulative WAL bytes** | **DEFERRED** | `Capture`/`Rec` carry no numeric WAL bytes; only text in the plan. Adding it requires threading a new numeric field through the hook (`InstrEndLoop` on per-node WAL), the ring `Rec` (fixed shmem layout), and `aggregate_captures` — a capture-path redesign explicitly out of scope for F8 "without a capture-path redesign". |
| **#4 Distinct-plan / plan-change** | **DEFERRED** | No stable plan id is captured. A plan hash would have to be computed in the hook (or from `plan_text`) and added to `Capture`/`Rec` — same capture-path redesign objection. |

This plan implements **#1 and #2** in full and records #3/#4 as a documented
deferral note (no code).

---

## 1. Pure-Rust math additions in `aggregate.rs`

These are the only changes the unit tests cover and the heart of F8.

### 1a. New field on `StatRow`

Add one additive counter (place it after `max_time_ms`, before
`first_seen_epoch`):

```rust
/// Count of captured executions whose duration exceeded the active
/// `plansight.slo_threshold_ms` at capture time. Additive across merges; 0
/// when the SLO GUC is disabled (threshold <= 0). Independent of timing
/// counters so the threshold can change over the life of a fingerprint.
pub slo_breaches: i64,
```

No new field is needed for CV: it derives from `calls`, `total_time_ms`,
`sum_sq_time_ms` already present.

### 1b. CV / stddev helpers (associated functions on `StatRow`)

Pure functions so they are callable from both the unit tests and (optionally) a
caller; the canonical surfacing is the SQL view, but these mirror its formula so
the math is unit-tested in Rust. Add to `impl StatRow` (new `impl` block):

```rust
impl StatRow {
    /// Population standard deviation of execution time (ms), derived from the
    /// stored sums: sqrt(max(0, E[X^2] - E[X]^2)). Returns 0.0 when there are
    /// no calls (zero guard) or when fewer than one effective sample exists.
    pub fn stddev_time_ms(&self) -> f64 {
        Self::stddev(self.calls, self.total_time_ms, self.sum_sq_time_ms)
    }

    /// Coefficient of variation = stddev / mean. Returns 0.0 when calls == 0 or
    /// when the mean is 0.0 (zero guard — avoids NaN/inf). Unitless.
    pub fn cv(&self) -> f64 {
        Self::coeff_of_variation(self.calls, self.total_time_ms, self.sum_sq_time_ms)
    }

    /// Fraction (0.0–1.0) of captured executions that breached the SLO.
    /// 0.0 when calls == 0.
    pub fn slo_breach_pct(&self) -> f64 {
        if self.calls <= 0 {
            return 0.0;
        }
        self.slo_breaches as f64 / self.calls as f64
    }

    /// Population stddev from the three stored sums, with a zero/negative guard.
    fn stddev(calls: i64, total: f64, sum_sq: f64) -> f64 {
        if calls <= 0 {
            return 0.0;
        }
        let n = calls as f64;
        let mean = total / n;
        // E[X^2] - E[X]^2, floored at 0 so float error can't yield a NaN sqrt.
        let var = (sum_sq / n - mean * mean).max(0.0);
        var.sqrt()
    }

    /// CV from the three stored sums; 0.0 when calls == 0 or mean == 0.
    fn coeff_of_variation(calls: i64, total: f64, sum_sq: f64) -> f64 {
        if calls <= 0 {
            return 0.0;
        }
        let mean = total / calls as f64;
        if mean == 0.0 {
            return 0.0;
        }
        Self::stddev(calls, total, sum_sq) / mean
    }
}
```

Rationale for the zero guards: a single call gives `var = X^2 - X^2 = 0` →
stddev 0 → CV 0; `calls == 0` and `mean == 0.0` both short-circuit to `0.0` so
no NaN/inf ever reaches SQL. This matches the existing view formula
(`schema.sql:83-89`) exactly (`GREATEST(0, sum_sq/n - (total/n)^2)`).

### 1c. SLO-breach counting + merge (pure functions)

The hook will count breaches per execution before grouping. Provide a pure
helper so the *counting rule* is unit-tested without Postgres, and document the
merge as plain addition:

```rust
/// Count durations strictly greater than `threshold_ms`. A threshold <= 0.0
/// means "SLO disabled" → always 0 (so an unset GUC never inflates the count).
/// Pure; unit-tested. NaN durations never count (NaN > x is false).
pub fn count_slo_breaches(durations_ms: &[f64], threshold_ms: f64) -> i64 {
    if threshold_ms <= 0.0 {
        return 0;
    }
    durations_ms.iter().filter(|&&d| d > threshold_ms).count() as i64
}
```

Merge semantics: `slo_breaches` is summed exactly like `calls` /
`total_time_ms`. There is **no** separate merge function in Rust — the merge
happens in the SQL `UPSERT` (section 4). To unit-test the merge rule in pure
Rust without SPI, add a tiny helper used only by the test (and reusable):

```rust
/// Combine two partial breach counts (the additive merge the UPSERT performs).
/// Trivial, but named so the accumulation invariant is unit-tested in Rust.
pub fn merge_slo_breaches(a: i64, b: i64) -> i64 {
    a + b
}
```

### 1d. Populate the field in `build_stat_row`

`build_stat_row` currently computes `sum_sq` from the per-execution records
(`aggregate.rs:136-140`). Add the breach count over the same execution list,
reading the threshold once per batch. Because `build_stat_row` is pure (no GUC
access), thread the threshold in as a parameter rather than reading the GUC
inside `aggregate.rs` (keeps `aggregate.rs` "no Postgres calls"):

- Add a `slo_threshold_ms: f64` parameter to `build_stat_row`.
- Compute alongside `sum_sq`:
  ```rust
  let durations: Vec<f64> =
      stats.executions.iter().map(|e| e.duration_ms).collect();
  let slo_breaches = StatRow::count_slo_breaches(&durations, slo_threshold_ms);
  ```
  (Or fold into the existing single pass over `stats.executions` to avoid a
  second allocation — but the explicit `count_slo_breaches` call keeps the rule
  in one tested place; either is acceptable.)
- Set `slo_breaches` in the returned `StatRow`.
- Thread `slo_threshold_ms` through the two callers: `aggregate_log` and
  `aggregate_captures` each gain a `slo_threshold_ms: f64` parameter and pass it
  to `build_stat_row`.

Call-site updates (in `lib.rs` / `hook.rs`) read the new GUC (section 2) and
pass it:
- `lib.rs::plansight_ingest` → `aggregate::aggregate_log(log_text, GUC_SLO_THRESHOLD_MS.get())`
- `lib.rs::plansight_drain_now` → `aggregate_captures(captures, GUC_SLO_THRESHOLD_MS.get())`
- `hook.rs::persist_capture` → `aggregate_captures(vec![cap], GUC_SLO_THRESHOLD_MS.get())`
- `bgworker` drain path (search for `aggregate_captures(` in `bgworker.rs`) →
  same; pass `GUC_SLO_THRESHOLD_MS.get()`.

> Note: because the breach is decided at *aggregation* time against the
> *current* threshold, a threshold change re-applies to the whole next batch but
> not retroactively to already-persisted rows — acceptable and documented. (An
> alternative "decide at capture and carry a per-record flag" would require a
> ring `Rec` layout change; out of scope, same reasoning as #3/#4.)

---

## 2. New GUC `plansight.slo_threshold_ms`

Mirror `plansight.min_duration_ms` exactly (a float GUC).

### 2a. Definition (`lib.rs`, in the GUC statics block ~line 65)

```rust
/// Executions whose duration exceeds this (ms) are counted as SLO breaches in
/// StatRow.slo_breaches. 0 (default) disables breach counting.
pub(crate) static GUC_SLO_THRESHOLD_MS: GucSetting<f64> = GucSetting::<f64>::new(0.0);
```

### 2b. Registration (`lib.rs::_PG_init`, next to the `min_duration_ms` define ~line 180)

```rust
GucRegistry::define_float_guc(
    c"plansight.slo_threshold_ms",
    c"Executions slower than this (ms) are counted as SLO breaches; 0 disables.",
    c"Applied at aggregation time against the current threshold (not retroactive). \
      Superuser-settable per session.",
    &GUC_SLO_THRESHOLD_MS,
    0.0,
    f64::MAX,
    GucContext::Suset,
    GucFlags::default(),
);
```

### 2c. Reading it

`GUC_SLO_THRESHOLD_MS.get()` returns `f64`; pass it to the aggregate functions
as in section 1d. No separate parser helper is needed (it is a plain float, like
`min_duration_ms`).

---

## 3. Hook change (`hook.rs`)

**Minimal change, intentionally.** Breach counting is done in `aggregate.rs`
against `duration_ms`, which is already carried end-to-end (`Capture.duration_ms`,
`Rec.duration_ms`). So the only `hook.rs` edits are call-site threading:

- `persist_capture` (`hook.rs:529-537`): change
  `aggregate_captures(vec![cap])` → `aggregate_captures(vec![cap], crate::GUC_SLO_THRESHOLD_MS.get())`,
  and add `GUC_SLO_THRESHOLD_MS` to the `use crate::{…}` import at the top
  (`hook.rs:33-37`).

No change is needed in `maybe_capture` / `executor_start` / `executor_end`: the
duration is already computed there and propagated. (We are **not** adding a
per-record breach flag to the ring, which would force a `Rec` layout change.)

> If a reviewer prefers counting at capture time (so the threshold is frozen per
> execution), that needs a new `Rec` field + ring layout bump — explicitly the
> "capture-path redesign" F8 says to avoid. The aggregation-time approach above
> is the chosen design.

---

## 4. SQL surfacing: table column + view columns

### 4a. `schema.sql` — `plansight.statements` table

Add the persisted, additive column (after `max_time_ms`, before `first_seen`):

```sql
    -- Cumulative count of captured executions that exceeded the active
    -- plansight.slo_threshold_ms at aggregation time (additive). 0 when the
    -- SLO GUC was disabled.
    slo_breaches       bigint           NOT NULL DEFAULT 0,
```

### 4b. `schema.sql` — `statements_summary` view (lines 71-95)

Add `cv`, `slo_breaches`, and `slo_breach_pct`:

```sql
    ...
    sqrt(
        GREATEST(0.0, sum_sq_time_ms / NULLIF(calls,0)
                      - power(total_time_ms / NULLIF(calls,0), 2))
    )                                               AS stddev_time_ms,
    -- Coefficient of variation = stddev / mean (unitless). NULL-safe via
    -- NULLIF on calls and on the mean (a 0 mean yields NULL, not div-by-zero).
    sqrt(
        GREATEST(0.0, sum_sq_time_ms / NULLIF(calls,0)
                      - power(total_time_ms / NULLIF(calls,0), 2))
    ) / NULLIF(total_time_ms / NULLIF(calls,0), 0)  AS cv,
    slo_breaches,
    slo_breaches::double precision / NULLIF(calls,0) AS slo_breach_pct,
    first_seen,
    ...
```

`top_by_total_time` is `SELECT *` over `statements_summary`, so it inherits the
new columns automatically (no edit). `query_timeline` is unaffected.

### 4c. `lib.rs` — UPSERT (`UPSERT_SQL`, lines 311-342)

- Add `slo_breaches` to the column list and a `$16` value placeholder.
- Add the additive merge in the `DO UPDATE SET`:
  ```sql
  slo_breaches = plansight.statements.slo_breaches + EXCLUDED.slo_breaches,
  ```
- In `persist_rows` (`lib.rs:383-403`), append `row.slo_breaches.into()` to the
  bound args (15 → 16 params).

### 4d. Docs (no code, but part of "surfacing")

- `docs/VIEWS_REFERENCE.md`: add `slo_breaches` to the `statements` table
  column table; add `cv`, `slo_breaches`, `slo_breach_pct` to the
  `statements_summary` "Added column" table with their formulas.
- Mention the new `plansight.slo_threshold_ms` GUC.

---

## 5. Test plan

### 5a. Pure-Rust unit tests — run under
`cargo test --no-default-features --features pg16 --lib`

Add to the existing `#[cfg(test)] mod tests` in `aggregate.rs` (lines 219-260).
These need **no** Postgres cluster (the existing tests in that module already
run this way — `malformed_plan_does_not_panic`, etc.). Build small `StatRow`
values directly, or call the pure helpers.

1. **`test_cv_computation`**
   - Durations `[10.0, 20.0]`: `calls=2`, `total=30.0`, `sum_sq=500.0`.
     mean = 15.0, var = 500/2 − 225 = 25.0, stddev = 5.0, cv = 5/15.
     ```rust
     let cv = StatRow::coeff_of_variation(2, 30.0, 500.0);
     assert!((cv - (5.0 / 15.0)).abs() < 1e-9);
     ```
   - Single call (`calls=1, total=10.0, sum_sq=100.0`): stddev 0 → cv 0.
     ```rust
     assert_eq!(StatRow::coeff_of_variation(1, 10.0, 100.0), 0.0);
     ```
   - Zero guard (`calls=0`): `assert_eq!(StatRow::coeff_of_variation(0, 0.0, 0.0), 0.0);`
   - Zero-mean guard (`calls=2, total=0.0, sum_sq=0.0`):
     `assert_eq!(StatRow::coeff_of_variation(2, 0.0, 0.0), 0.0);`
   - Also assert `StatRow::stddev(2, 30.0, 500.0) == 5.0`.

2. **`test_slo_breach_count`**
   - `count_slo_breaches(&[10.0, 200.0, 300.0], 100.0) == 2`.
   - Disabled threshold: `count_slo_breaches(&[10.0, 200.0, 300.0], 0.0) == 0`.
   - Negative threshold: `count_slo_breaches(&[200.0], -5.0) == 0`.
   - Boundary (strictly greater): `count_slo_breaches(&[100.0], 100.0) == 0`.
   - Empty: `count_slo_breaches(&[], 100.0) == 0`.

3. **`test_slo_breach_merge`**
   - `merge_slo_breaches(2, 3) == 5`.
   - Optionally also assert end-to-end via two `count_slo_breaches` calls on
     two partial duration slices summing to the count over the concatenation:
     ```rust
     let a = StatRow::count_slo_breaches(&[10.0, 200.0], 100.0); // 1
     let b = StatRow::count_slo_breaches(&[300.0, 50.0], 100.0); // 1
     assert_eq!(StatRow::merge_slo_breaches(a, b), 2);
     ```

4. **`test_slo_breach_pct`** (small extra, optional)
   - Build a `StatRow` (or call a pct helper) with `calls=4, slo_breaches=1`
     → `0.25`; `calls=0` → `0.0`.

> Confirm during implementation that `cargo test --no-default-features
> --features pg16 --lib` compiles and runs these (the spec flags this as a
> "confirm" item). The crate already has pure `#[cfg(test)]` tests in
> `aggregate.rs` and `ring.rs` that run this way, so the harness is proven.

### 5b. pgrx integration tests — `#[pg_test]` in `lib.rs::tests`
(run under `cargo pgrx test --no-default-features --features pg16` when a
cluster is available)

Add to the `#[pg_schema] mod tests` block (`lib.rs:765-1136`). Reuse the
existing `SAMPLE_LOG` (two executions, 10.5 ms and 20.5 ms, one group).

5. **`summary_exposes_cv_and_stddev`**
   ```rust
   crate::plansight_ingest(SAMPLE_LOG); // calls=2, total=31.0, sum_sq=530.5
   // mean=15.5, var=530.5/2 - 15.5^2 = 265.25 - 240.25 = 25.0, stddev=5.0
   let stddev = Spi::get_one::<f64>(
       "SELECT stddev_time_ms FROM plansight.statements_summary")
       .unwrap().unwrap();
   assert!((stddev - 5.0).abs() < 1e-6);
   let cv = Spi::get_one::<f64>(
       "SELECT cv FROM plansight.statements_summary").unwrap().unwrap();
   assert!((cv - 5.0 / 15.5).abs() < 1e-6, "cv = stddev/mean");
   ```

6. **`slo_breaches_counted_and_surfaced`**
   ```rust
   Spi::run("SET plansight.slo_threshold_ms = 15").unwrap();
   crate::plansight_ingest(SAMPLE_LOG); // durations 10.5, 20.5 → 1 breach (>15)
   let breaches = Spi::get_one::<i64>(
       "SELECT slo_breaches FROM plansight.statements").unwrap().unwrap();
   assert_eq!(breaches, 1);
   let pct = Spi::get_one::<f64>(
       "SELECT slo_breach_pct FROM plansight.statements_summary").unwrap().unwrap();
   assert!((pct - 0.5).abs() < 1e-6, "1 of 2 calls breached");
   Spi::run("SET plansight.slo_threshold_ms = 0").unwrap();
   ```

7. **`slo_breaches_additive_across_ingests`**
   ```rust
   Spi::run("SET plansight.slo_threshold_ms = 15").unwrap();
   crate::plansight_ingest(SAMPLE_LOG);
   crate::plansight_ingest(SAMPLE_LOG);
   let breaches = Spi::get_one::<i64>(
       "SELECT slo_breaches FROM plansight.statements").unwrap().unwrap();
   assert_eq!(breaches, 2, "1 breach per ingest, summed");
   Spi::run("SET plansight.slo_threshold_ms = 0").unwrap();
   ```

8. **`slo_threshold_zero_counts_no_breaches`**
   ```rust
   Spi::run("SET plansight.slo_threshold_ms = 0").unwrap();
   crate::plansight_ingest(SAMPLE_LOG);
   let breaches = Spi::get_one::<i64>(
       "SELECT slo_breaches FROM plansight.statements").unwrap().unwrap();
   assert_eq!(breaches, 0, "disabled SLO never counts");
   ```

> Note for tests 6/7/8: each `#[pg_test]` runs in its own transaction that is
> rolled back, so cross-test contamination of `slo_breaches` is not a concern;
> still reset the GUC at the end of each (as the existing hook tests do) for
> hygiene.

### 5c. Quality gates (CLAUDE.md)

From `crates/pg_extension`:
```bash
cargo fmt --all
cargo clippy --no-default-features --features pg16 --all-targets -- -D warnings
cargo build --no-default-features --features pg16
cargo test  --no-default-features --features pg16 --lib   # pure-Rust math
cargo pgrx test --no-default-features --features pg16      # if a cluster is up
```
(The root-workspace `--workspace --all-features` clippy from CLAUDE.md does not
cover this independent crate; run the crate-local clippy above.)

---

## 6. Change checklist (files touched)

| File | Change |
|---|---|
| `crates/pg_extension/src/aggregate.rs` | `StatRow.slo_breaches` field; `impl StatRow` with `cv`/`stddev_time_ms`/`slo_breach_pct`/`count_slo_breaches`/`merge_slo_breaches`; `slo_threshold_ms` param on `build_stat_row`, `aggregate_log`, `aggregate_captures`; populate `slo_breaches`; new unit tests. |
| `crates/pg_extension/src/lib.rs` | `GUC_SLO_THRESHOLD_MS` static + `define_float_guc`; thread threshold into `aggregate_log`/`aggregate_captures` calls; `UPSERT_SQL` column + `$16` + additive merge; `persist_rows` extra bound arg; new `#[pg_test]`s. |
| `crates/pg_extension/src/hook.rs` | import `GUC_SLO_THRESHOLD_MS`; pass it in `persist_capture`. |
| `crates/pg_extension/src/bgworker.rs` | pass threshold into its `aggregate_captures` call (grep `aggregate_captures(`). |
| `crates/pg_extension/sql/schema.sql` | `slo_breaches` column on `statements`; `cv`, `slo_breaches`, `slo_breach_pct` in `statements_summary`. |
| `docs/VIEWS_REFERENCE.md` | document new column + view columns + GUC. |

## 7. Deferred (documented, no code)

- **#3 Cumulative WAL bytes** and **#4 distinct-plan / plan-change tracking** are
  deferred: neither raw WAL bytes nor a stable plan identifier is carried on
  `Capture`/ring `Rec`, so implementing them requires a capture-path redesign
  (new numeric fields threaded through `hook.rs` → `ring::Rec` (fixed shmem
  layout bump) → `aggregate_captures`). Record this in the spec's deferral notes;
  revisit when the hook is extended to extract per-query WAL/buffer totals and a
  plan hash.
</content>
</invoke>
