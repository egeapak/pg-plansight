# F10 — Feature-gate regression detection (drop `statrs` from the extension)

## Goal
Make the statistically-heavy regression detector (and its `statrs` dependency)
optional, behind a default-on `regression-analysis` feature, so the embeddable
extension build (`pg-plansight-core` with `default-features = false`) links **zero
`statrs`** with no loss of the functionality the extension actually uses. Achieve
it through a **single interface** (`RegressionEngine`) that both builds call, so
no `#[cfg]` leaks into the call sites.

## Why it's safe (from the usage map)
- `StatisticalCalculator` (statistics.rs) is consumed **only** by
  `RegressionDetector` (regression.rs). Nothing in tui/exporter/extension uses it.
- `regression.rs` is 27 pure serde **data types** + the one **`RegressionDetector`**
  computation type. The data types must stay always-available (they're the type of
  `ProcessedQuery.regression_analysis` and the output of the basic path).
- The extension never calls `analyze_regression` (`get_processed_queries` leaves
  the field `None`); only the TUI populates it, and the TUI builds with default
  features (detector on). So gating changes no shipped behavior.

---

## The interface (the seam you asked for)

A single trait both builds use; the statrs-backed implementation is compiled in
only with the feature, and a lightweight one is always available. All size
thresholds live inside the engines, so `log_parser` becomes feature-agnostic.

```rust
// crates/core/src/sql_analysis/regression.rs  (ALWAYS compiled)

/// Turns a performance time-series into a `RegressionAnalysis`.
///
/// The full statistical engine (`StatisticalRegressionEngine`, statrs-backed) is
/// compiled in with the `regression-analysis` feature; otherwise the basic
/// engine is used. Callers go through this trait and never see the feature.
pub trait RegressionEngine {
    /// `None` when there is too little data to say anything (< 3 points).
    fn analyze(&self, data: &[PerformanceDataPoint]) -> Option<RegressionAnalysis>;
}

/// Heuristic engine with no heavy stats — always available. Holds the logic
/// currently in `log_parser::create_basic_regression_analysis`, ported to operate
/// on `PerformanceDataPoint` (it only needs `timestamp` + `execution_time_ms`).
pub struct BasicRegressionEngine {
    pub thresholds: RegressionThresholds,
}
impl RegressionEngine for BasicRegressionEngine {
    fn analyze(&self, data: &[PerformanceDataPoint]) -> Option<RegressionAnalysis> {
        if data.len() < 3 { return None; }
        Some(basic_regression(data, &self.thresholds))
    }
}

#[cfg(feature = "regression-analysis")]
pub struct StatisticalRegressionEngine { /* config */ }
#[cfg(feature = "regression-analysis")]
impl RegressionEngine for StatisticalRegressionEngine {
    fn analyze(&self, data: &[PerformanceDataPoint]) -> Option<RegressionAnalysis> {
        if data.len() < 3 { return None; }
        if data.len() < 10 {
            return BasicRegressionEngine::default().analyze(data); // stats need ≥10
        }
        RegressionDetector::new()
            .analyze(data).ok()
            .or_else(|| BasicRegressionEngine::default().analyze(data))
    }
}

/// The engine for this build: statistical with the feature, basic without.
pub fn default_regression_engine() -> Box<dyn RegressionEngine> {
    #[cfg(feature = "regression-analysis")]
    { Box::new(StatisticalRegressionEngine::default()) }
    #[cfg(not(feature = "regression-analysis"))]
    { Box::new(BasicRegressionEngine::default()) }
}
```

`log_parser::analyze_regression` collapses to (no `#[cfg]`):
```rust
pub fn analyze_regression(&self, executions: &[ExecutionRecord]) -> Option<RegressionAnalysis> {
    let data: Vec<PerformanceDataPoint> = plans.iter().map(|p| PerformanceDataPoint {
        timestamp: p.timestamp,
        execution_time_ms: p.duration_ms,
        memory_usage_mb: None, cpu_usage_percent: None,
        io_operations: None, cache_hit_ratio: None,
    }).collect();
    crate::sql_analysis::default_regression_engine().analyze(&data)
}
```

> Behavior parity: with the feature ON this reproduces today's exact dispatch
> (<3 None, <10 basic, ≥10 detector). With it OFF, large datasets get the basic
> analysis instead of the statistical one — a graceful degrade, not an error.

---

## File-by-file changes

### `crates/core/Cargo.toml`
```toml
[features]
default = ["parallel", "file-io", "regression-analysis"]
regression-analysis = ["dep:statrs"]
...
# make statrs optional
statrs = { version = "0.16", optional = true }
```
(`approx` stays a dev-dependency — only the gated stats tests use it.)

### `crates/core/src/sql_analysis/statistics.rs`
Entire module behind the feature. In `sql_analysis/mod.rs`:
```rust
#[cfg(feature = "regression-analysis")]
pub mod statistics;
```

### `crates/core/src/sql_analysis/regression.rs`
- **Keep always:** all 27 data types (RegressionAnalysis … PerformanceDataPoint),
  the new `RegressionEngine` trait, `BasicRegressionEngine`,
  `default_regression_engine()`, and `basic_regression(data, thresholds)` (the
  ported basic logic).
- **Gate (`#[cfg(feature = "regression-analysis")]`):** `use ...StatisticalCalculator`,
  `RegressionDetector` (struct + impls), `StatisticalRegressionEngine`, and the
  detector test module.

### `crates/core/src/sql_analysis/mod.rs`
```rust
pub use regression::{
    MetricRegression, PerformanceDataPoint, PerformanceMetric, RegressionAnalysis,
    RegressionSeverity, RegressionStatus,
    RegressionEngine, BasicRegressionEngine, default_regression_engine, // new, always
};
#[cfg(feature = "regression-analysis")]
pub use regression::RegressionDetector; // gated
```

### `crates/core/src/log_parser.rs`
- Replace `analyze_regression` body with the engine call (above).
- Move `create_basic_regression_analysis` (≈ lines 799–958) into
  `regression::basic_regression(&[PerformanceDataPoint], &RegressionThresholds)`.
  It only reads `timestamp` + `duration_ms`, which map to
  `PerformanceDataPoint.timestamp` + `.execution_time_ms` — a mechanical port.
  Remove the now-unused `use ...RegressionDetector`.

### `crates/core/src/models.rs`
No change — `ProcessedQuery.regression_analysis: Option<RegressionAnalysis>` keeps
using the always-available data type.

### `crates/pg_extension/Cargo.toml`
No change needed: it already sets `default-features = false`, which now also drops
`regression-analysis` → `statrs` gone. (Optionally add a comment noting it.)

---

## Tests
- **Gate** the statrs-dependent tests behind the feature: `statistics.rs` tests,
  the `RegressionDetector` tests in `regression.rs`, and `mod regression_tests`
  in `sql_analysis/tests.rs` (all use the detector). Wrap each
  `#[cfg(all(test, feature = "regression-analysis"))]`.
- **Add (always-on):** `basic_regression`/`BasicRegressionEngine` unit tests
  (insufficient-data → None; a clear upward trend → Degrading status) so the
  no-feature build has real coverage.
- Default `cargo test` keeps running the full detector suite (feature on).

## Verification
- `cd crates/pg_extension && cargo tree --no-default-features --features pg16 -e normal | grep -c statrs` → **0**.
- `cargo build --no-default-features --features pg16` (extension) and the pgrx
  suite (`RUST_TEST_THREADS=1 cargo pgrx test … pg16`) still pass.
- Core both ways: `cargo test -p pg-plansight-core` (default, detector tests run)
  **and** `cargo test -p pg-plansight-core --no-default-features`
  (compiles + basic tests run, no statrs).
- `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean,
  plus a `--no-default-features` clippy on core.
- Whole workspace `cargo test` green (tui/exporter unaffected — default features).

## Risk / notes
- **Behavior change** only in a non-default build: a `--no-default-features` core
  (i.e. the extension) gives *basic* regression for ≥10-point series instead of
  statistical. The extension never calls this path, so its shipped behavior is
  unchanged; the TUI/exporter keep the detector.
- **Scope:** the one substantive edit is porting `create_basic_regression_analysis`
  to `basic_regression(&[PerformanceDataPoint])`. Everything else is `#[cfg]`
  attributes + moving a re-export. Field mapping verified (timestamp + duration only).
- **Minimal alternative** (if you'd rather not introduce the trait): keep
  `create_basic_regression_analysis` in place and gate just the ≥10 branch with a
  one-function seam `advanced_regression(data) -> Option<_>` (`#[cfg]` inside it,
  returns `None` without the feature; caller `.or_else(basic)`). Smaller diff, but
  leaves the dispatch in `log_parser` rather than behind a clean interface.

## Docs
Note the new `regression-analysis` feature in `docs/DEVELOPMENT.md` (or core's
crate docs) and that the embeddable extension build omits it (and `statrs`).

---

## Phased rollout (each phase verified before the next)

The phases are **sequential** (one crate, interdependent edits). Each ends in a
green gate.

### Phase 1 — Introduce `RegressionEngine` (NO feature gating yet)
Behavior-preserving refactor; everything stays always-compiled (statrs still in).
- In `sql_analysis/regression.rs`, add: the `RegressionEngine` trait;
  `basic_regression(&[PerformanceDataPoint], &RegressionThresholds) -> RegressionAnalysis`
  (ported verbatim from `log_parser::create_basic_regression_analysis`, reading
  `timestamp`/`execution_time_ms` instead of `QueryPlan.timestamp`/`.duration_ms`);
  `BasicRegressionEngine`; `StatisticalRegressionEngine` (wraps `RegressionDetector`,
  with the `<10 → basic` fallback); and `default_regression_engine()` (returns the
  statistical one — no `#[cfg]` yet).
- Rewrite `log_parser::analyze_regression` to build `PerformanceDataPoint`s and call
  `default_regression_engine().analyze(&data)`; delete `create_basic_regression_analysis`.
- Add the new always-on re-exports in `sql_analysis/mod.rs`.
- **Gate:** `cargo test -p pg-plansight-core` green (parity), `cargo clippy
  -p pg-plansight-core --all-targets -- -D warnings` clean. Add a few
  `basic_regression` unit tests (insufficient data → None; clear upward trend →
  Degrading). No `statrs`/feature changes in this phase.

### Phase 2 — Add the `regression-analysis` feature + gating
- `crates/core/Cargo.toml`: `statrs = { version = "0.16", optional = true }`;
  `regression-analysis = ["dep:statrs"]`; add it to `default`.
- `#[cfg(feature = "regression-analysis")]` on: `pub mod statistics` (mod.rs),
  `RegressionDetector` (+ its impls), `StatisticalRegressionEngine` (+ impl), the
  statistical branch of `default_regression_engine()` (the `not(feature)` branch
  returns `BasicRegressionEngine`), the `RegressionDetector` re-export, and every
  statrs-using test (`statistics.rs` tests, the `RegressionDetector` tests in
  `regression.rs`, and `mod regression_tests` in `sql_analysis/tests.rs`).
- **Gate:** `cargo test -p pg-plansight-core` (default) green **and**
  `cargo test -p pg-plansight-core --no-default-features` green (compiles + basic
  tests, no statrs); `cargo clippy -p pg-plansight-core --no-default-features
  --all-targets -- -D warnings` clean; `cargo tree -p pg-plansight-core
  --no-default-features -e normal | grep -c statrs` → **0**.

### Phase 3 — End-to-end verification + docs (done by me)
- Extension: `cd crates/pg_extension && cargo tree --no-default-features --features
  pg16 -e normal | grep -c statrs` → **0**; `cargo build --no-default-features
  --features pg16` clean; run the pgrx suite serially (`RUST_TEST_THREADS=1
  cargo pgrx test … pg16`) → all pass.
- Whole workspace: `cargo fmt --all --check`, `cargo clippy --workspace
  --all-features --all-targets -- -D warnings`, `cargo test --workspace` green.
- Docs: note the `regression-analysis` feature (DEVELOPMENT.md) and that the
  extension build omits it + `statrs`.

