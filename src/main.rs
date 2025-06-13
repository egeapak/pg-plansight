use clap::Parser;
use std::path::PathBuf;
use tokio::io::{self};

mod app;
mod log_parser;
mod log_parsing_state;
pub mod models;
mod results_state;

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

    let mut app = App::new();
    let initial_state = LogParsingState::new(cli.log_file);

    app.run(Box::new(initial_state)).await
}
