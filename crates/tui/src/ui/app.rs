use async_trait::async_trait;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use pg_plansight_core::DateFilter;
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use std::{io, path::PathBuf};
use tokio::time::Duration;

use super::state::log_parsing_state::LogParsingState;

pub enum StateChange {
    Keep,
    Change(Box<dyn AppState>),
    Exit,
}

#[async_trait]
pub trait AppState {
    fn ui(&mut self, f: &mut Frame, app: &App);
    async fn process_key(&mut self, key_event: KeyEvent, app: &mut App) -> StateChange;
    fn is_noninteractive(&self) -> bool;
}

/// Restores the terminal (raw mode, alternate screen, mouse capture) no
/// matter how `run` exits — normal return, `?` early-return, or panic. A
/// panic hook alone is not enough: `?` on a draw/read error skips straight
/// past any inline cleanup, leaving the user's shell in raw mode.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }

    fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

/// Install a panic hook that restores the terminal before the default hook
/// prints the message, so the report is readable instead of being smeared
/// across a raw-mode alternate screen.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default_hook(info);
    }));
}

pub struct App {
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self { should_quit: false }
    }

    fn quit(&mut self) {
        self.should_quit = true;
    }

    fn wait_event(&self, is_noninteractive: bool) -> io::Result<KeyEvent> {
        let should_read = if is_noninteractive {
            event::poll(Duration::from_millis(100))?
        } else {
            true
        };

        if should_read {
            let event = event::read()?;
            // Only react to Press: Windows delivers Release (and Repeat)
            // events too, which would dispatch every keystroke twice.
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                return Ok(key);
            }
        }

        Ok(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE))
    }

    pub async fn start(mut self, paths: Vec<PathBuf>, date_filter: DateFilter) -> io::Result<()> {
        let state = LogParsingState::new(paths, date_filter);

        self.run(Box::new(state)).await
    }

    pub async fn start_from_import(mut self, import_path: PathBuf) -> io::Result<()> {
        use super::state::results_state::ResultsState;
        use pg_plansight_core::AnalysisExport;

        // Load the export file
        let export = AnalysisExport::from_file(&import_path)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Convert to processed queries (std::HashMap)
        let processed_queries = export.to_processed_queries();

        // Convert std::HashMap to hashbrown::HashMap
        let processed_queries: hashbrown::HashMap<_, _> = processed_queries.into_iter().collect();

        // Create results state directly from imported data
        let state = ResultsState::from_imported_data(
            processed_queries,
            Some(export.analysis_period.start),
            Some(export.analysis_period.end),
        );

        self.run(Box::new(state)).await
    }

    async fn run(&mut self, initial_state: Box<dyn AppState>) -> io::Result<()> {
        install_panic_hook();
        let _guard = TerminalGuard::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        let mut current_state = initial_state;

        while !self.should_quit {
            terminal.draw(|f| current_state.ui(f, self))?;

            let key = self.wait_event(current_state.is_noninteractive())?;

            let next_state = current_state.process_key(key, self).await;

            match next_state {
                StateChange::Keep => {}
                StateChange::Change(new_state) => {
                    current_state = new_state;
                }
                StateChange::Exit => {
                    self.quit();
                }
            }
        }

        // Terminal restoration happens in TerminalGuard::drop (also on `?`
        // early-returns above and on panic via the hook).
        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
