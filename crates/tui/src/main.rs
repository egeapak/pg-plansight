use chrono::{DateTime, Utc};
use clap::Parser;
use std::path::PathBuf;
use tokio::io::{self};

use pg_plansight::ui::App;
use pg_plansight_core::{DateFilter, parse_relative_date};

fn parse_date_arg(s: &str) -> Result<DateTime<Utc>, String> {
    parse_relative_date(s).map_err(|e| e.to_string())
}

#[derive(Parser)]
#[command(name = "pg_plansight")]
#[command(about = "A TUI tool for analyzing PostgreSQL auto_explain logs")]
#[command(version)]
struct Cli {
    #[arg(help = "Path(s) to the PostgreSQL log file(s)", required_unless_present_any = ["import", "export"])]
    log_files: Vec<PathBuf>,

    #[arg(long, value_parser = parse_date_arg, help = "Only include logs from this time onwards (e.g., 2h, 3d, 1w, 2024-01-01T10:30:00)")]
    since: Option<DateTime<Utc>>,

    #[arg(long, value_parser = parse_date_arg, help = "Only include logs up to this time (e.g., 1h, 2d, 2024-01-01T15:00:00)")]
    until: Option<DateTime<Utc>>,

    #[arg(
        long,
        help = "Import analysis from a previously exported JSON file",
        conflicts_with_all = ["export", "log_files", "since", "until"]
    )]
    import: Option<PathBuf>,

    #[arg(
        long,
        help = "Parse logs and export to JSON file without opening TUI (non-interactive mode)"
    )]
    export: Option<PathBuf>,
}

async fn non_interactive_export(
    log_files: Vec<PathBuf>,
    date_filter: DateFilter,
    export_path: PathBuf,
) -> io::Result<()> {
    use pg_plansight_core::{AnalysisExport, ParseProgress, PostgreSQLLogParser, expand_files};

    println!("Parsing logs in non-interactive mode...");

    // Expand file patterns (e.g., globs)
    let expanded_files = expand_files(&log_files);

    if expanded_files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No log files found to parse",
        ));
    }

    println!("Found {} log file(s) to process", expanded_files.len());

    // Parse all log files - this returns a receiver for progress updates
    let rx = PostgreSQLLogParser::parse_multiple_files_async(expanded_files.clone(), date_filter);

    // Consume progress messages until we get the final result.
    // queries_parsed is a delta since the previous update, so keep a running
    // total for display.
    let mut total_queries_parsed = 0usize;
    let plans = loop {
        match rx.recv() {
            Ok(ParseProgress::Progress {
                file_path,
                progress,
                queries_parsed,
                ..
            }) => {
                total_queries_parsed += queries_parsed;
                println!(
                    "  Processing {}: {:.1}% ({} queries total)",
                    file_path.file_name().unwrap_or_default().to_string_lossy(),
                    progress * 100.0,
                    total_queries_parsed
                );
            }
            Ok(ParseProgress::Error {
                file_path, error, ..
            }) => {
                eprintln!("  Error parsing {}: {}", file_path.display(), error);
            }
            Ok(ParseProgress::Complete { result }) => {
                break result.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            }
            Err(_) => {
                return Err(io::Error::other("Parse channel closed unexpectedly"));
            }
        }
    };

    println!("Parsed {} query plans", plans.len());

    // Get processed queries with statistics
    let mut parser = PostgreSQLLogParser::new();
    let processed_queries = parser.get_processed_queries(&plans);

    println!("Grouped into {} unique queries", processed_queries.len());

    // Convert hashbrown::HashMap to std::HashMap for export
    let std_queries: std::collections::HashMap<_, _> = processed_queries.into_iter().collect();

    // Create export with source file names
    let source_files: Vec<String> = expanded_files
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let export = AnalysisExport::from_processed_queries(std_queries, source_files);

    // Export to file
    export.to_file(&export_path).map_err(io::Error::other)?;

    println!(
        "Successfully exported analysis to: {}",
        export_path.display()
    );
    println!("  - Query groups: {}", export.query_count);
    println!("  - Total executions: {}", export.execution_count);

    Ok(())
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let date_filter = DateFilter::new(cli.since, cli.until);

    // Check for non-interactive export mode
    if let Some(export_path) = cli.export {
        if cli.log_files.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "No log files specified for export",
            ));
        }
        return non_interactive_export(cli.log_files, date_filter, export_path).await;
    }

    let app = App::new();

    // Check if importing from JSON
    if let Some(import_path) = cli.import {
        app.start_from_import(import_path).await
    } else {
        app.start(cli.log_files, date_filter).await
    }
}
