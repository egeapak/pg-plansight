use clap::Parser;
use std::path::PathBuf;
use tokio::io::{self};

mod app;
mod log_parser;
pub mod models;
mod log_parsing_state;
mod results_state;
mod parsing_state;
mod test_parser;
mod parser_utils;

use app::App;
use log_parsing_state::LogParsingState;

#[derive(Parser)]
#[command(name = "pg_auto_explain_analyzer")]
#[command(about = "A TUI tool for analyzing PostgreSQL auto_explain logs")]
struct Cli {
    #[arg(help = "Path to the PostgreSQL log file")]
    log_file: PathBuf,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse();

    // Test parsing performance if requested
    if std::env::var("TEST_PARSER").is_ok() {
        test_parser::test_parsing();
        return Ok(());
    }

    // Test simple parsing if requested
    if std::env::var("DEBUG_PARSER").is_ok() {
        test_parser::test_simple_parsing();
        return Ok(());
    }

    let mut app = App::new();
    let initial_state = LogParsingState::new(cli.log_file);

    app.run(Box::new(initial_state)).await
}
