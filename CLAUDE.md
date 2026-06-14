# Plansight

A Rust TUI application for analyzing PostgreSQL auto_explain extension logs.

## Project Structure
- **Language**: Rust (edition 2024)
- **UI Framework**: Ratatui with crossterm
- **Key Dependencies**: clap, tokio, regex, chrono, sqlparser, rayon

## Core Functionality
- Parses PostgreSQL logs containing auto_explain output
- Extracts query plans, execution times, and statistics
- Groups similar queries and calculates aggregate statistics
- Provides interactive TUI for browsing results
- Supports compressed log files (gzip, bzip2)
- Export/import analysis results as JSON for archiving and sharing

## Key Files
- `src/main.rs` - CLI entry point
- `src/log_parser.rs` - Core parsing logic with parallel processing
- `src/models.rs` - Data structures for queries, plans, and statistics
- `src/export.rs` - Export/import functionality for analysis results
- `src/ui/` - TUI implementation with state management
- `logs/` - Sample PostgreSQL log files for testing
- `docs/EXPORT_IMPORT.md` - Export/import feature documentation

## Build & Run
```bash
cargo build
cargo run -- logs/postgresql-*.log
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