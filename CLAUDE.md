# Plansight

A Rust TUI application for analyzing PostgreSQL auto_explain extension logs.

## Project Structure
A Cargo workspace (edition 2024) with the following members:
- **Language**: Rust (edition 2024; the pgrx extension and bench-harness use 2021)
- **UI Framework**: Ratatui with crossterm
- **Key Dependencies**: clap, tokio, regex, chrono, sqlparser, rayon

## Core Functionality
- Parses PostgreSQL logs containing auto_explain output
- Extracts query plans, execution times, and statistics
- Groups similar queries and calculates aggregate statistics
- Provides interactive TUI for browsing results
- Supports compressed log files (gzip, bzip2)
- Export/import analysis results as JSON for archiving and sharing
- In-database capture via a pgrx PostgreSQL extension (PG 13–18)

## Key Files
- `crates/tui/src/main.rs` - CLI entry point (the `pg-plansight` binary)
- `crates/core/src/log_parser.rs` - Core parsing logic with parallel processing
- `crates/core/src/models.rs` - Data structures for queries, plans, and statistics
- `crates/core/src/grouping.rs` - Streaming query grouping (`QueryGrouper`)
- `crates/core/src/export.rs` - Export/import functionality for analysis results
- `crates/core/src/analysis/` - Modular query analyzers
- `crates/tui/src/ui/` - TUI implementation with state management
- `crates/exporter/` - Prometheus/OpenTelemetry exporter daemon
- `crates/pg_extension/` - pgrx PostgreSQL extension (independent workspace)
- `crates/core/examples/` - Runnable library examples
- `docs/EXPORT_IMPORT.md` - Export/import feature documentation

## Build & Run
```bash
cargo build
cargo run -- /path/to/postgresql-*.log
```

## Testing
```bash
cargo test
cargo bench  # Performance benchmarks
```

## Code Quality
**IMPORTANT**: After each development session, run the following commands to ensure code quality:

```bash
# Format all code according to Rust style guidelines
cargo fmt --all

# Run clippy with all features and treat all warnings as errors
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

These checks must pass before committing code. The clippy command checks:
- All workspace members (`--workspace`)
- All features enabled (`--all-features`)
- All targets including tests and benchmarks (`--all-targets`)
- All warnings treated as errors (`-D warnings`)