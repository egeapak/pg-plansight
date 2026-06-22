# F11 — Make the core test suite compile/pass under any feature combination

## Problem
`cargo test -p pg-plansight-core --no-default-features` fails to compile: several
tests call APIs gated behind `file-io`, `parallel`, or `regression-analysis`, but
the tests themselves aren't gated. (Default features pass — all gated APIs exist.)
This predates F10 for file-io/parallel; F10 added the `regression-analysis` case.

## Which feature each gated API needs
- `parse_file_with_progress`, `parse_file_range_with_progress`, `to_file`,
  `from_file`, `flate2`/`bzip2` → **`file-io`**.
- `parse_multiple_files_async` → **`parallel` + `file-io`**.
- `StatisticalCalculator` (`sql_analysis::statistics`) → **`regression-analysis`**.

## Fixes (gate each test by exactly the feature(s) it needs)

### Whole-file gate — add `#![cfg(feature = "file-io")]` as the first line
Every test in these files writes temp files / uses compression:
1. `crates/core/tests/parser_integration_test.rs` (12 tests; `parse_file_*`).
2. `crates/core/tests/adversarial_input_test.rs` (top-level `use flate2::…` +
   `parse_file_with_progress`).

### Module gates — `crates/core/tests/new_coverage_tests.rs` (mixed file)
- `mod statistics_tests` (imports `StatisticalCalculator`) →
  `#[cfg(feature = "regression-analysis")]`.
- `mod log_parser_tests` (temp files, bzip2, `parse_file_with_progress`,
  `parse_multiple_files_async`) → `#[cfg(feature = "file-io")]`; and inside it the
  `parse_multiple_files_async_combines_results` test → additionally
  `#[cfg(feature = "parallel")]`.
- `mod sql_normalization_tests` → no change (pure).

### Lib unit tests
- `crates/core/src/log_parser.rs`: gate `fn test_plan_parsing_integration`
  (uses `parse_file_with_progress`) with `#[cfg(feature = "file-io")]`. Leave
  `test_actual_log_file_parsing` alone (it uses plain `std::fs::read_to_string`).
- `crates/core/src/export.rs`: gate `fn test_export_import_roundtrip` (uses
  `to_file`/`from_file`) with `#[cfg(feature = "file-io")]`. If
  `use tempfile::NamedTempFile` (and any `std::fs`/`Path` import) in that test
  module becomes unused without the feature, gate the import too (or move it into
  the gated fn) so `--no-default-features` stays warning-clean.

## Watch for
- Unused-import warnings under `--no-default-features` after gating (clippy
  `-D warnings` will flag them) — gate or relocate the now-conditional imports.
- Don't gate anything that compiles fine without the feature (keep default
  coverage identical).

## Verification (must pass BOTH ways)
- Default: `cargo test -p pg-plansight-core` green (same count as before) and
  `cargo clippy -p pg-plansight-core --all-targets --all-features -- -D warnings` clean.
- No features: `cargo test -p pg-plansight-core --no-default-features` **compiles
  and passes** (gated tests excluded), and
  `cargo clippy -p pg-plansight-core --no-default-features --all-targets -- -D warnings` clean.
- Spot-check single features compile: `--no-default-features --features file-io`
  and `--no-default-features --features regression-analysis`.
- `cargo fmt -p pg-plansight-core`.
