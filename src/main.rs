use clap::Parser;
use std::path::PathBuf;
use tokio::io::{self};

use pg_loganalyze::{ui::app::App, ui::state::log_parsing_state::LogParsingState};

#[derive(Parser)]
#[command(name = "pg_loganalyze")]
#[command(about = "A TUI tool for analyzing PostgreSQL auto_explain logs")]
struct Cli {
    #[arg(help = "Path(s) to the PostgreSQL log file(s)", required = true)]
    log_files: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let mut app = App::new();
    let initial_state = LogParsingState::new(cli.log_files);

    app.run(Box::new(initial_state)).await
}
