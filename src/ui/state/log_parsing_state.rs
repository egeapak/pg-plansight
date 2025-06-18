use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::results_state::ResultsState;
use crate::{log_parser::PostgreSQLLogParser, models::QueryPlan};

pub struct LogParsingState {
    log_file_path: PathBuf,
    parsing_task: Option<JoinHandle<anyhow::Result<Vec<QueryPlan>>>>,
    progress_receiver: Option<mpsc::UnboundedReceiver<f64>>,
    progress: f64,
    status_message: String,
    error_message: Option<String>,
    parsing_start_time: Option<Instant>,
}

impl LogParsingState {
    pub fn new(log_file_path: PathBuf) -> Self {
        let mut instance = Self {
            log_file_path,
            parsing_task: None,
            progress_receiver: None,
            progress: 0.0,
            status_message: "Ready to parse log file".to_string(),
            error_message: None,
            parsing_start_time: None,
        };

        // Start parsing immediately
        instance.start_parsing();
        instance
    }

    fn start_parsing(&mut self) {
        let file_path = self.log_file_path.clone();
        self.status_message = "Starting to parse log file...".to_string();
        self.progress = 0.0;
        self.parsing_start_time = Some(Instant::now());

        // Create a channel for progress updates
        let (progress_sender, progress_receiver) = mpsc::unbounded_channel();
        self.progress_receiver = Some(progress_receiver);

        let task = tokio::spawn(async move {
            let mut parser = PostgreSQLLogParser::new();
            parser.parse_file_with_progress(&file_path, move |progress| {
                let _ = progress_sender.send(progress);
            })
        });

        self.parsing_task = Some(task);
    }

    async fn check_parsing_progress(&mut self) -> Option<StateChange> {
        // Check for progress updates from the parsing task
        if let Some(ref mut progress_receiver) = self.progress_receiver {
            while let Ok(progress) = progress_receiver.try_recv() {
                self.progress = progress;
                self.status_message =
                    format!("Parsing in progress... {:.1}%", self.progress * 100.0);
            }
        }

        if let Some(task) = self.parsing_task.take() {
            if task.is_finished() {
                match task.await {
                    Ok(Ok(queries)) => {
                        self.progress = 1.0;
                        self.status_message =
                            format!("Successfully parsed {} queries", queries.len());

                        // Transition to results state
                        let results_state = ResultsState::new(queries);
                        return Some(StateChange::Change(Box::new(results_state)));
                    }
                    Ok(Err(err)) => {
                        self.error_message = Some(format!("Failed to parse: {:?}", err));
                        self.status_message = "Parsing failed".to_string();
                        self.progress = 0.0;
                        self.progress_receiver = None;
                    }
                    Err(join_err) => {
                        self.error_message = Some(format!("Task failed: {}", join_err));
                        self.status_message = "Parsing failed".to_string();
                        self.progress = 0.0;
                        self.progress_receiver = None;
                    }
                }
                // Task is consumed, don't put it back
            } else {
                // Task is not finished, put it back
                self.parsing_task = Some(task);
            }
        }

        None
    }

    fn render_parsing_screen(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(5),
                Constraint::Length(3), // Error message area
            ])
            .split(area);

        let title = Paragraph::new("PostgreSQL Log Parser")
            .block(Block::default().borders(Borders::ALL).title("Parser"))
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(title, chunks[0]);

        let file_info = Paragraph::new(format!("File: {}", self.log_file_path.display()))
            .block(Block::default().borders(Borders::ALL).title("File Info"));
        f.render_widget(file_info, chunks[1]);

        let progress_block = Block::default().borders(Borders::ALL).title("Progress");
        let progress = Gauge::default()
            .block(progress_block)
            .gauge_style(Style::default().fg(Color::Green))
            .percent((self.progress * 100.0) as u16)
            .label(format!("{:.1}%", self.progress * 100.0));
        f.render_widget(progress, chunks[2]);

        // Error message area
        if let Some(ref error) = self.error_message {
            let error_widget = Paragraph::new(error.as_str())
                .block(Block::default().borders(Borders::ALL).title("Error"))
                .style(Style::default().fg(Color::Red));
            f.render_widget(error_widget, chunks[3]);
        } else {
            let status_widget = Paragraph::new(self.status_message.as_str())
                .block(Block::default().borders(Borders::ALL).title("Status"))
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(status_widget, chunks[3]);
        }
    }
}

#[async_trait]
impl AppState for LogParsingState {
    fn ui(&mut self, f: &mut Frame, _app: &App) {
        let area = f.area();
        self.render_parsing_screen(f, area);
    }

    async fn process_key(&mut self, key_event: KeyEvent, _app: &mut App) -> StateChange {
        // Check parsing progress first
        if let Some(state_change) = self.check_parsing_progress().await {
            return state_change;
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            // Handle null key (used for continuous updates)
            KeyCode::Null => StateChange::Keep,
            _ => StateChange::Keep,
        }
    }
}
