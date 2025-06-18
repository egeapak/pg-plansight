use clap::Parser;
use std::path::PathBuf;
use tokio::io::{self};

use pg_loganalyze::{ui::app::App, ui::state::log_parsing_state::LogParsingState};

#[derive(Parser)]
#[command(name = "pg_loganalyze")]
#[command(about = "A TUI tool for analyzing PostgreSQL auto_explain logs")]
struct Cli {
    #[arg(help = "Path(s) to the PostgreSQL log file(s)")]
    log_files: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let cli = Cli::parse();

    if cli.log_files.is_empty() {
        eprintln!("Error: At least one log file must be specified");
        std::process::exit(1);
    }

    let mut app = App::new();
    let initial_state = LogParsingState::new(cli.log_files);

    app.run(Box::new(initial_state)).await
}
