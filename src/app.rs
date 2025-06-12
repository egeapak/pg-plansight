use async_trait::async_trait;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Frame, Terminal};
use std::io;
use tokio::time::Duration;

pub enum StateChange {
    Keep,
    Change(Box<dyn AppState>),
    Exit,
}

#[async_trait]
pub trait AppState {
    fn ui(&mut self, f: &mut Frame, app: &App);
    async fn process_key(&mut self, code: KeyCode, app: &mut App) -> StateChange;
}

pub struct App {
    should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            should_quit: false,
        }
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub async fn run(&mut self, initial_state: Box<dyn AppState>) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let mut current_state = initial_state;

        while !self.should_quit {
            terminal.draw(|f| current_state.ui(f, self))?;

            let mut handled_event = false;
            
            if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event) = event::read() {
                    if let Event::Key(key) = event {
                        handled_event = true;
                        match current_state.process_key(key.code, self).await {
                            StateChange::Keep => {}
                            StateChange::Change(new_state) => {
                                current_state = new_state;
                            }
                            StateChange::Exit => {
                                self.quit();
                            }
                        }
                    }
                }
            }

            // Only update state automatically if no key event was handled
            if !handled_event {
                match current_state.process_key(KeyCode::Null, self).await {
                    StateChange::Keep => {}
                    StateChange::Change(new_state) => {
                        current_state = new_state;
                    }
                    StateChange::Exit => {
                        self.quit();
                    }
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