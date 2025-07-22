use async_trait::async_trait;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use std::{io, path::PathBuf};
use pg_loganalyze_core::DateFilter;
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
            event::poll(Duration::from_millis(100)).unwrap()
        } else {
            true
        };

        if should_read {
            let event = event::read()?;
            if let Event::Key(key) = event {
                return Ok(key);
            }
        }

        Ok(KeyEvent::new(KeyCode::Null, KeyModifiers::NONE))
    }

    pub async fn start(mut self, paths: Vec<PathBuf>, date_filter: DateFilter) -> io::Result<()> {
        let state = LogParsingState::new(paths, date_filter);

        self.run(Box::new(state)).await
    }

    async fn run(&mut self, initial_state: Box<dyn AppState>) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
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

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        Ok(())
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
