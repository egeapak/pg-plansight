use chrono::{DateTime, Utc};
use clap::Parser;
use std::path::PathBuf;
use tokio::io::{self};

use pg_loganalyze::ui::App;
use pg_loganalyze_core::{DateFilter, parse_relative_date};

fn parse_date_arg(s: &str) -> Result<DateTime<Utc>, String> {
    parse_relative_date(s).map_err(|e| e.to_string())
}

#[derive(Parser)]
#[command(name = "pg_loganalyze")]
#[command(about = "A TUI tool for analyzing PostgreSQL auto_explain logs")]
struct Cli {
    #[arg(help = "Path(s) to the PostgreSQL log file(s)", required_unless_present = "import")]
    log_files: Vec<PathBuf>,

    #[arg(long, value_parser = parse_date_arg, help = "Only include logs from this time onwards (e.g., 2h, 3d, 1w, 2024-01-01T10:30:00)")]
    since: Option<DateTime<Utc>>,

    #[arg(long, value_parser = parse_date_arg, help = "Only include logs up to this time (e.g., 1h, 2d, 2024-01-01T15:00:00)")]
    until: Option<DateTime<Utc>>,

    #[arg(long, help = "Import analysis from a previously exported JSON file")]
    import: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let date_filter = DateFilter::new(cli.since, cli.until);
    let app = App::new();

    // Check if importing from JSON
    if let Some(import_path) = cli.import {
        app.start_from_import(import_path).await
    } else {
        app.start(cli.log_files, date_filter).await
    }
}
