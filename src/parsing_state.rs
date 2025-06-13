use async_trait::async_trait;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Table, Row, Cell},
};
use std::path::PathBuf;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::{App, AppState, StateChange};
use crate::{
    log_parser::PostgreSQLLogParser,
    models::{QueryPlan, QueryStatistics},
};

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
    selected_query_index: usize,
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
            selected_query_index: 0,
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
                }
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
                self.status_message =
                    format!("Parsing in progress... {:.1}%", self.progress * 100.0);
            }
        }

        if let Some(task) = self.parsing_task.take() {
            if task.is_finished() {
                match task.await {
                    Ok(Ok((queries, statistics))) => {
                        self.progress = 1.0;
                        self.status_message =
                            format!("Successfully parsed {} queries", queries.len());

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

        let mut status_lines = vec![Line::from(Span::styled(
            &self.status_message,
            Style::default().fg(Color::Yellow),
        ))];

        if let Some(ref error) = self.error_message {
            status_lines.push(Line::from(Span::styled(
                format!("Error: {}", error),
                Style::default().fg(Color::Red),
            )));
        }

        if self.progress >= 1.0 {
            status_lines.push(Line::from(""));
            status_lines.push(Line::from(Span::styled(
                "Press Up/Down to navigate queries, 'r' to reparse, or 'q' to quit",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if self.parsing_task.is_none() {
            status_lines.push(Line::from(""));
            status_lines.push(Line::from(Span::styled(
                "Press 'p' to start parsing, or 'q' to quit",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
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
        if let (Some(queries), Some(stats)) = (&self.parsed_queries, &self.statistics) {
            // Create horizontal split pane layout
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                .split(area);

            // Left pane: Table with count and mean time columns
            self.render_queries_table(f, chunks[0], queries, stats);

            // Right pane: Selected query details
            self.render_query_details(f, chunks[1], queries);
        }
    }

    fn render_queries_table(&self, f: &mut Frame, area: Rect, queries: &[QueryPlan], _stats: &QueryStatistics) {
        use std::collections::HashMap;
        
        // Group queries by normalized query text and calculate statistics
        let mut query_groups: HashMap<String, (usize, f64)> = HashMap::new();
        
        for query in queries {
            let normalized_query = self.normalize_query(&query.query_text);
            let (count, total_time) = query_groups.entry(normalized_query).or_insert((0, 0.0));
            *count += 1;
            *total_time += query.duration_ms;
        }

        // Convert to sorted vector for display
        let mut query_stats: Vec<(String, usize, f64)> = query_groups
            .into_iter()
            .map(|(query, (count, total_time))| (query, count, total_time / count as f64))
            .collect();

        // Sort by count (descending) then by mean time (descending)
        query_stats.sort_by(|a, b| {
            b.1.cmp(&a.1).then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
        });

        // Create table rows
        let rows: Vec<Row> = query_stats
            .iter()
            .enumerate()
            .map(|(index, (query, count, mean_time))| {
                let query_preview = if query.len() > 50 {
                    format!("{}...", &query[..47])
                } else {
                    query.clone()
                };
                
                let style = if index == self.selected_query_index {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::default()
                };

                Row::new(vec![
                    Cell::from(count.to_string()),
                    Cell::from(format!("{:.2}", mean_time)),
                    Cell::from(query_preview),
                ]).style(style)
            })
            .collect();

        let table = Table::new(rows, vec![
            Constraint::Length(8),  // Count column
            Constraint::Length(12), // Mean time column
            Constraint::Min(0),     // Query column (takes remaining space)
        ])
        .header(Row::new(vec![
            Cell::from("Count").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Mean (ms)").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("Query").style(Style::default().add_modifier(Modifier::BOLD)),
        ]))
        .block(Block::default().borders(Borders::ALL).title("Query Statistics"))
        .column_spacing(1);

        f.render_widget(table, area);
    }

    fn render_query_details(&self, f: &mut Frame, area: Rect, queries: &[QueryPlan]) {
        use std::collections::HashMap;
        
        // Group queries by normalized query text
        let mut query_groups: HashMap<String, Vec<&QueryPlan>> = HashMap::new();
        for query in queries {
            let normalized_query = self.normalize_query(&query.query_text);
            query_groups.entry(normalized_query).or_default().push(query);
        }

        // Convert to sorted vector to match table order
        let mut grouped_queries: Vec<(String, Vec<&QueryPlan>)> = query_groups.into_iter().collect();
        grouped_queries.sort_by(|a, b| {
            let count_a = a.1.len();
            let count_b = b.1.len();
            let mean_a: f64 = a.1.iter().map(|q| q.duration_ms).sum::<f64>() / count_a as f64;
            let mean_b: f64 = b.1.iter().map(|q| q.duration_ms).sum::<f64>() / count_b as f64;
            
            count_b.cmp(&count_a).then_with(|| mean_b.partial_cmp(&mean_a).unwrap_or(std::cmp::Ordering::Equal))
        });

        if let Some((selected_query, instances)) = grouped_queries.get(self.selected_query_index) {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(6),  // Query text
                    Constraint::Length(4),  // Statistics
                    Constraint::Min(0),     // Plan details
                ])
                .split(area);

            // Query text
            let query_text = Paragraph::new(selected_query.clone())
                .block(Block::default().borders(Borders::ALL).title("Query Text"))
                .wrap(ratatui::widgets::Wrap { trim: true });
            f.render_widget(query_text, chunks[0]);

            // Statistics for this query
            let total_time: f64 = instances.iter().map(|q| q.duration_ms).sum();
            let min_time = instances.iter().map(|q| q.duration_ms).fold(f64::INFINITY, f64::min);
            let max_time = instances.iter().map(|q| q.duration_ms).fold(0.0, f64::max);
            let mean_time = total_time / instances.len() as f64;

            let stats_lines = vec![
                Line::from(format!("Executions: {}", instances.len())),
                Line::from(format!("Min/Mean/Max: {:.2}/{:.2}/{:.2} ms", min_time, mean_time, max_time)),
            ];

            let stats_widget = Paragraph::new(stats_lines)
                .block(Block::default().borders(Borders::ALL).title("Statistics"));
            f.render_widget(stats_widget, chunks[1]);

            // Plan details (show the plan from the slowest execution)
            if let Some(slowest_query) = instances.iter().max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap()) {
                let plan_text = Paragraph::new(slowest_query.plan.clone())
                    .block(Block::default().borders(Borders::ALL).title("Execution Plan (Slowest)"))
                    .wrap(ratatui::widgets::Wrap { trim: true });
                f.render_widget(plan_text, chunks[2]);
            }
        } else {
            let no_selection = Paragraph::new("No query selected")
                .block(Block::default().borders(Borders::ALL).title("Query Details"));
            f.render_widget(no_selection, area);
        }
    }

    fn normalize_query(&self, query: &str) -> String {
        // Simple query normalization - remove extra whitespace and normalize case
        query.split_whitespace()
            .collect::<Vec<&str>>()
            .join(" ")
            .to_lowercase()
    }

    fn get_unique_query_count(&self, queries: &[QueryPlan]) -> usize {
        use std::collections::HashSet;
        let mut unique_queries = HashSet::new();
        for query in queries {
            unique_queries.insert(self.normalize_query(&query.query_text));
        }
        unique_queries.len()
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
                self.selected_query_index = 0;
                StateChange::Keep
            }
            KeyCode::Char('v') => StateChange::Keep,
            KeyCode::Up => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if self.selected_query_index > 0 {
                        self.selected_query_index -= 1;
                    }
                }
                StateChange::Keep
            }
            KeyCode::Down => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if let Some(queries) = &self.parsed_queries {
                        let max_index = self.get_unique_query_count(queries).saturating_sub(1);
                        if self.selected_query_index < max_index {
                            self.selected_query_index += 1;
                        }
                    }
                }
                StateChange::Keep
            }
            // Handle null key (used for continuous updates)
            KeyCode::Null => StateChange::Keep,
            _ => StateChange::Keep,
        }
    }
}
