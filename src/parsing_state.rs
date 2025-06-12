use async_trait::async_trait;
use crossterm::event::KeyCode;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph},
    Frame,
};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::{App, AppState, StateChange};
use crate::log_parser::{PostgreSQLLogParser, QueryPlan, QueryStatistics};

pub struct ParsingState {
    log_file_path: PathBuf,
    parsing_task: Option<JoinHandle<Result<(Vec<QueryPlan>, QueryStatistics), String>>>,
    progress_receiver: Option<mpsc::UnboundedReceiver<f64>>,
    progress: f64,
    status_message: String,
    parsed_queries: Option<Vec<QueryPlan>>,
    statistics: Option<QueryStatistics>,
    error_message: Option<String>,
    parsing_complete: bool,
    parsing_start_time: Option<Instant>,
}

impl ParsingState {
    pub fn new(log_file_path: PathBuf) -> Self {
        Self {
            log_file_path,
            parsing_task: None,
            progress_receiver: None,
            progress: 0.0,
            status_message: "Ready to parse log file".to_string(),
            parsed_queries: None,
            statistics: None,
            error_message: None,
            parsing_complete: false,
            parsing_start_time: None,
        }
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
            let parser = PostgreSQLLogParser::new();
            match parser.parse_file_with_progress(&file_path, move |progress| {
                let _ = progress_sender.send(progress);
            }) {
                Ok(queries) => {
                    // Calculate statistics in the async task to avoid blocking UI
                    let statistics = parser.get_query_statistics(&queries);
                    Ok((queries, statistics))
                },
                Err(e) => Err(format!("Failed to parse log file: {}", e)),
            }
        });

        self.parsing_task = Some(task);
    }

    async fn check_parsing_progress(&mut self) {
        // Check for progress updates from the parsing task
        if let Some(ref mut progress_receiver) = self.progress_receiver {
            while let Ok(progress) = progress_receiver.try_recv() {
                self.progress = progress;
                self.status_message = format!("Parsing in progress... {:.1}%", self.progress * 100.0);
            }
        }

        if let Some(task) = self.parsing_task.take() {
            if task.is_finished() {
                match task.await {
                    Ok(Ok((queries, statistics))) => {
                        self.progress = 1.0;
                        self.status_message = format!("Successfully parsed {} queries", queries.len());
                        
                        self.parsed_queries = Some(queries);
                        self.statistics = Some(statistics);
                        self.error_message = None;
                        self.parsing_complete = true;
                        self.progress_receiver = None; // Clean up the receiver
                    }
                    Ok(Err(err)) => {
                        self.error_message = Some(err);
                        self.status_message = "Parsing failed".to_string();
                        self.progress = 0.0;
                        self.parsing_complete = true;
                        self.progress_receiver = None;
                    }
                    Err(join_err) => {
                        self.error_message = Some(format!("Task failed: {}", join_err));
                        self.status_message = "Parsing failed".to_string();
                        self.progress = 0.0;
                        self.parsing_complete = true;
                        self.progress_receiver = None;
                    }
                }
                // Task is consumed, don't put it back
            } else {
                // Task is not finished, put it back
                self.parsing_task = Some(task);
            }
        }
    }

    fn render_parsing_screen(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(5),
                Constraint::Min(0),
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

        let mut status_lines = vec![
            Line::from(Span::styled(&self.status_message, Style::default().fg(Color::Yellow))),
        ];

        if let Some(ref error) = self.error_message {
            status_lines.push(Line::from(Span::styled(
                format!("Error: {}", error),
                Style::default().fg(Color::Red),
            )));
        }

        if self.progress >= 1.0 {
            status_lines.push(Line::from(""));
            status_lines.push(Line::from(Span::styled(
                "Press 'v' to view results, 'r' to reparse, or 'q' to quit",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )));
        } else if self.parsing_task.is_none() {
            status_lines.push(Line::from(""));
            status_lines.push(Line::from(Span::styled(
                "Press 'p' to start parsing, or 'q' to quit",
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )));
        } else {
            status_lines.push(Line::from(""));
            status_lines.push(Line::from(Span::styled(
                "Parsing in progress... Press 'q' to quit",
                Style::default().fg(Color::Yellow),
            )));
        }

        let status = Paragraph::new(status_lines)
            .block(Block::default().borders(Borders::ALL).title("Status"));
        f.render_widget(status, chunks[3]);
    }

    fn render_results_screen(&self, f: &mut Frame, area: Rect) {
        if let (Some(_queries), Some(stats)) = (&self.parsed_queries, &self.statistics) {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(8),
                    Constraint::Min(0),
                ])
                .split(area);

            let stats_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(chunks[0]);

            let summary_lines = vec![
                Line::from(format!("Total Queries: {}", stats.total_queries)),
                Line::from(format!("Unique Queries: {}", stats.unique_queries)),
                Line::from(format!("Total Duration: {:.2} ms", stats.total_duration_ms)),
                Line::from(format!("Average Duration: {:.2} ms", stats.average_duration_ms)),
                Line::from(format!("Slowest Query: {:.2} ms", stats.slowest_query_duration_ms)),
            ];

            let summary = Paragraph::new(summary_lines)
                .block(Block::default().borders(Borders::ALL).title("Summary"));
            f.render_widget(summary, stats_chunks[0]);

            let frequent_items: Vec<ListItem> = stats.most_frequent_queries
                .iter()
                .map(|(query, count)| {
                    let query_preview = if query.len() > 40 {
                        format!("{}...", &query[..37])
                    } else {
                        query.clone()
                    };
                    ListItem::new(format!("{}: {} times", query_preview, count))
                })
                .collect();

            let frequent_queries = List::new(frequent_items)
                .block(Block::default().borders(Borders::ALL).title("Most Frequent"));
            f.render_widget(frequent_queries, stats_chunks[1]);

            let slowest_items: Vec<ListItem> = stats.slowest_queries
                .iter()
                .map(|query| {
                    let query_preview = if query.query_text.len() > 60 {
                        format!("{}...", &query.query_text[..57])
                    } else {
                        query.query_text.clone()
                    };
                    ListItem::new(format!("{:.2}ms: {}", query.duration_ms, query_preview))
                })
                .collect();

            let slowest_queries = List::new(slowest_items)
                .block(Block::default().borders(Borders::ALL).title("Slowest Queries"));
            f.render_widget(slowest_queries, chunks[1]);
        }
    }
}

#[async_trait]
impl AppState for ParsingState {
    fn ui(&mut self, f: &mut Frame, _app: &App) {
        let area = f.area();

        if self.progress >= 1.0 && self.parsed_queries.is_some() {
            self.render_results_screen(f, area);
        } else {
            self.render_parsing_screen(f, area);
        }
    }

    async fn process_key(&mut self, code: KeyCode, _app: &mut App) -> StateChange {
        // Only check parsing progress if a task is actually running and parsing isn't complete
        if self.parsing_task.is_some() && !self.parsing_complete {
            self.check_parsing_progress().await;
        }

        match code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Char('p') => {
                if self.parsing_task.is_none() && self.progress < 1.0 {
                    self.start_parsing();
                }
                StateChange::Keep
            }
            KeyCode::Char('r') => {
                self.parsing_task = None;
                self.progress_receiver = None;
                self.progress = 0.0;
                self.parsed_queries = None;
                self.statistics = None;
                self.error_message = None;
                self.status_message = "Ready to parse log file".to_string();
                self.parsing_complete = false;
                self.parsing_start_time = None;
                StateChange::Keep
            }
            KeyCode::Char('v') => {
                StateChange::Keep
            }
            // Handle null key (used for continuous updates)
            KeyCode::Null => {
                StateChange::Keep
            }
            _ => {
                StateChange::Keep
            }
        }
    }
}