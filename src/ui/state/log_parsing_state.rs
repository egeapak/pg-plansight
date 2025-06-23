use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;
use tokio::task::JoinHandle;

use crate::ui::state::results_state::ResultsState;
use crate::{
    expand_files,
    ui::app::{App, AppState, StateChange},
};
use crate::{
    log_parser::PostgreSQLLogParser,
    models::{ParseProgress, QueryPlan},
};

#[derive(Debug, Clone)]
pub struct FileProgress {
    pub path: PathBuf,
    pub progress: f64,
    pub status: String,
    pub completed: bool,
    pub error: Option<String>,
}

pub struct LogParsingState {
    log_file_paths: Vec<PathBuf>,
    parsing_task: Option<JoinHandle<()>>,
    progress_receiver: Option<mpsc::Receiver<ParseProgress>>,
    file_progress: Vec<FileProgress>,
    overall_progress: f64,
    status_message: String,
    error_message: Option<String>,
    parsing_start_time: Option<Instant>,
    parsing_end_time: Option<Instant>,
    max_parallel_threads: usize,
    total_queries_parsed: Arc<AtomicUsize>,
    final_result: Option<anyhow::Result<Vec<QueryPlan>>>,
    parsing_complete: bool,
    awaiting_user_input: bool,
}

impl LogParsingState {
    pub fn new(log_file_paths: Vec<PathBuf>) -> Self {
        let log_file_paths = expand_files(&log_file_paths);
        let file_progress: Vec<FileProgress> = log_file_paths
            .iter()
            .map(|path| FileProgress {
                path: path.clone(),
                progress: 0.0,
                status: "Waiting".to_string(),
                completed: false,
                error: None,
            })
            .collect();

        let mut instance = Self {
            log_file_paths,
            parsing_task: None,
            progress_receiver: None,
            file_progress,
            overall_progress: 0.0,
            status_message: "Ready to parse log files".to_string(),
            error_message: None,
            parsing_start_time: None,
            parsing_end_time: None,
            max_parallel_threads: rayon::current_num_threads(),
            total_queries_parsed: Arc::new(AtomicUsize::new(0)),
            final_result: None,
            parsing_complete: false,
            awaiting_user_input: false,
        };

        // Start parsing immediately
        instance.start_parsing();
        instance
    }

    fn start_parsing(&mut self) {
        let file_paths = self.log_file_paths.clone();
        self.status_message = format!(
            "Starting parallel parsing of {} log file(s) using {} threads...",
            file_paths.len(),
            self.max_parallel_threads
        );
        self.overall_progress = 0.0;
        self.parsing_start_time = Some(Instant::now());

        // Get the receiver from the new async parsing function
        let progress_receiver = PostgreSQLLogParser::parse_multiple_files_async(file_paths);
        self.progress_receiver = Some(progress_receiver);

        // Create a dummy task to maintain the same interface
        let task = tokio::spawn(async move {
            // The actual parsing is handled by the background thread
            // This task just exists to maintain compatibility
        });

        self.parsing_task = Some(task);
    }

    async fn check_parsing_progress(&mut self) -> Option<StateChange> {
        // Don't process progress updates if parsing is already complete
        if self.parsing_complete {
            return None;
        }

        let mut should_close_receiver = false;

        // Check for progress updates from the parsing task
        if let Some(ref mut progress_receiver) = self.progress_receiver {
            while let Ok(progress) = progress_receiver.try_recv() {
                match progress {
                    ParseProgress::Progress {
                        file_index,
                        progress,
                        ..
                    } => {
                        if file_index < self.file_progress.len() {
                            self.file_progress[file_index].progress = progress;

                            if progress >= 1.0 {
                                self.file_progress[file_index].completed = true;
                                self.file_progress[file_index].status = "Completed".to_string();
                            } else {
                                self.file_progress[file_index].status =
                                    format!("Parsing... {:.1}%", progress * 100.0);
                            }
                        }
                    }
                    ParseProgress::Error {
                        file_index, error, ..
                    } => {
                        if file_index < self.file_progress.len() {
                            self.file_progress[file_index].progress = 0.0;
                            self.file_progress[file_index].completed = true;
                            self.file_progress[file_index].status = "Failed".to_string();
                            self.file_progress[file_index].error = Some(error);
                        }
                    }
                    ParseProgress::Complete { result } => {
                        self.final_result = Some(result);
                        should_close_receiver = true;
                    }
                }

                // Calculate overall progress (completed files count as 1.0, failed files as 0.0)
                let total_progress: f64 = self
                    .file_progress
                    .iter()
                    .map(|fp| {
                        if fp.error.is_some() {
                            0.0 // Failed files don't contribute to progress
                        } else {
                            fp.progress
                        }
                    })
                    .sum();
                self.overall_progress = total_progress / self.file_progress.len() as f64;

                let completed_files = self
                    .file_progress
                    .iter()
                    .filter(|fp| fp.completed && fp.error.is_none())
                    .count();
                let failed_files = self
                    .file_progress
                    .iter()
                    .filter(|fp| fp.error.is_some())
                    .count();

                let active_files = self
                    .file_progress
                    .iter()
                    .filter(|fp| !fp.completed && fp.error.is_none())
                    .count();
                if failed_files > 0 {
                    self.status_message = format!(
                        "Parsing files in parallel... {:.1}% ({}/{} completed, {} active, {} failed)",
                        self.overall_progress * 100.0,
                        completed_files,
                        self.file_progress.len(),
                        active_files,
                        failed_files
                    );
                } else {
                    self.status_message = format!(
                        "Parsing files in parallel... {:.1}% ({}/{} completed, {} active)",
                        self.overall_progress * 100.0,
                        completed_files,
                        self.file_progress.len(),
                        active_files
                    );
                }
            }
        }

        // Close receiver if needed
        if should_close_receiver {
            self.progress_receiver = None;
        }

        // Check if we have a final result
        if let Some(result) = self.final_result.take() {
            match result {
                Ok(queries) => {
                    self.overall_progress = 1.0;
                    // Mark any remaining files as completed
                    for fp in &mut self.file_progress {
                        if !fp.completed && fp.error.is_none() {
                            fp.progress = 1.0;
                            fp.completed = true;
                            fp.status = "Completed".to_string();
                        }
                    }

                    let successful_files = self
                        .file_progress
                        .iter()
                        .filter(|fp| fp.error.is_none())
                        .count();
                    let failed_files = self
                        .file_progress
                        .iter()
                        .filter(|fp| fp.error.is_some())
                        .count();

                    self.total_queries_parsed
                        .store(queries.len(), Ordering::Relaxed);
                    let elapsed = self
                        .parsing_start_time
                        .map(|t| t.elapsed().as_secs_f64())
                        .unwrap_or(0.0);
                    let queries_per_sec = if elapsed > 0.0 {
                        queries.len() as f64 / elapsed
                    } else {
                        0.0
                    };

                    if failed_files > 0 {
                        self.status_message = format!(
                            "Parallel parsing complete! {} queries from {}/{} files ({} failed) - {:.1} queries/sec",
                            queries.len(),
                            successful_files,
                            self.file_progress.len(),
                            failed_files,
                            queries_per_sec
                        );
                    } else {
                        self.status_message = format!(
                            "Parallel parsing complete! {} queries from {} files - {:.1} queries/sec using {} threads",
                            queries.len(),
                            self.file_progress.len(),
                            queries_per_sec,
                            self.max_parallel_threads
                        );
                    }

                    // Mark parsing as complete but wait for user input
                    self.parsing_complete = true;
                    self.awaiting_user_input = true;
                    self.parsing_end_time = Some(Instant::now());
                    self.final_result = Some(Ok(queries));
                }
                Err(err) => {
                    self.error_message = Some(format!("Failed to parse: {:?}", err));
                    self.status_message = "Parsing failed".to_string();
                    self.overall_progress = 0.0;
                }
            }
        }

        None
    }

    fn render_parsing_screen(&self, f: &mut Frame, area: Rect) {
        // Create centered layout for dashboard
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Min(8),    // Main dashboard area
                Constraint::Length(6), // Status/Stats area
            ])
            .split(area);

        // Title
        let queries_count = self.total_queries_parsed.load(Ordering::Relaxed);
        let title_text = "PostgreSQL Log Parser";
        let title = Paragraph::new(title_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Parsing Dashboard"),
            )
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(title, main_chunks[0]);

        // Center the dashboard content
        let dashboard_area = main_chunks[1];
        let center_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(60),
                Constraint::Percentage(20),
            ])
            .split(dashboard_area);

        let dashboard_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // Overall progress gauge
                Constraint::Length(3), // File counter
                Constraint::Length(3), // Query counter
                Constraint::Length(4), // Performance statistics
            ])
            .split(center_chunks[1]);

        // Overall Progress Gauge (centered)
        let completed_files = self
            .file_progress
            .iter()
            .filter(|fp| fp.completed && fp.error.is_none())
            .count();
        let failed_files = self
            .file_progress
            .iter()
            .filter(|fp| fp.error.is_some())
            .count();

        let overall_color = if failed_files > 0 && completed_files == 0 {
            Color::Red
        } else if failed_files > 0 {
            Color::Yellow
        } else if self.overall_progress >= 1.0 {
            Color::Green
        } else {
            Color::Cyan
        };

        let progress_title = if self.parsing_complete {
            "Parsing Complete!"
        } else {
            "Overall Progress"
        };

        let progress_block = Block::default()
            .borders(Borders::ALL)
            .title(progress_title)
            .title_style(Style::default().fg(overall_color));
        let progress = Gauge::default()
            .block(progress_block)
            .gauge_style(Style::default().fg(overall_color))
            .percent((self.overall_progress * 100.0) as u16)
            .label(format!("{:.1}%", self.overall_progress * 100.0));
        f.render_widget(progress, dashboard_chunks[0]);

        // Files Counter
        let total_files = self.file_progress.len();
        let processing_files = self
            .file_progress
            .iter()
            .filter(|fp| !fp.completed && fp.error.is_none())
            .count();
        
        let files_text = if self.parsing_complete {
            format!("{}/{} files processed", completed_files, total_files)
        } else {
            format!("{}/{} files ({} processing)", completed_files, total_files, processing_files)
        };
        
        let files_widget = Paragraph::new(files_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Files"),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(files_widget, dashboard_chunks[1]);

        // Queries Counter
        let queries_text = format!("{} queries parsed", queries_count);
        let queries_widget = Paragraph::new(queries_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Queries"),
            )
            .style(Style::default().fg(Color::White));
        f.render_widget(queries_widget, dashboard_chunks[2]);

        // Performance Statistics (always shown)
        let performance_info = if let Some(start_time) = self.parsing_start_time {
            let elapsed = if let Some(end_time) = self.parsing_end_time {
                // Use fixed duration from start to end time
                end_time.duration_since(start_time).as_secs_f64()
            } else {
                // Still parsing, use current elapsed time
                start_time.elapsed().as_secs_f64()
            };
            
            if elapsed > 0.0 && queries_count > 0 {
                format!("{:.1} queries/sec", queries_count as f64 / elapsed)
            } else {
                "0.0 queries/sec".to_string()
            }
        } else {
            "0.0 queries/sec".to_string()
        };

        let stats_text = format!("{} | {} threads", performance_info, self.max_parallel_threads);
        let stats_color = if self.parsing_complete {
            Color::Green
        } else {
            Color::Cyan
        };
        
        let stats_widget = Paragraph::new(stats_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Performance"),
            )
            .style(Style::default().fg(stats_color));
        f.render_widget(stats_widget, dashboard_chunks[3]);

        // Status/Statistics area
        if let Some(ref error) = self.error_message {
            let error_widget = Paragraph::new(error.as_str())
                .block(Block::default().borders(Borders::ALL).title("Error"))
                .style(Style::default().fg(Color::Red));
            f.render_widget(error_widget, main_chunks[2]);
        } else {
            let (elapsed_time, performance_info) = if let Some(start_time) = self.parsing_start_time {
                let elapsed = if let Some(end_time) = self.parsing_end_time {
                    // Use fixed duration from start to end time
                    end_time.duration_since(start_time).as_secs_f64()
                } else {
                    // Still parsing, use current elapsed time
                    start_time.elapsed().as_secs_f64()
                };
                
                let rate = if elapsed > 0.0 && queries_count > 0 {
                    format!("{:.1} queries/sec", queries_count as f64 / elapsed)
                } else {
                    "0.0 queries/sec".to_string()
                };
                (format!("Elapsed: {:.1}s", elapsed), rate)
            } else {
                ("Elapsed: 0.0s".to_string(), "0.0 queries/sec".to_string())
            };

            let status_text = if self.awaiting_user_input {
                format!(
                    "{}\n\nPress ENTER to view results or 'q' to quit",
                    self.status_message
                )
            } else {
                format!(
                    "{}\n{} | {} | Threads: {}\n\nPress 'q' to quit",
                    self.status_message,
                    elapsed_time,
                    performance_info,
                    self.max_parallel_threads
                )
            };

            let status_widget = Paragraph::new(status_text)
                .block(Block::default().borders(Borders::ALL).title("Status"))
                .style(Style::default().fg(if self.awaiting_user_input { Color::Green } else { Color::Yellow }));
            f.render_widget(status_widget, main_chunks[2]);
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
            KeyCode::Enter => {
                if self.awaiting_user_input {
                    // User pressed Enter, transition to results
                    if let Some(Ok(queries)) = self.final_result.take() {
                        let results_state = ResultsState::new(queries);
                        return StateChange::Change(Box::new(results_state));
                    }
                }
                StateChange::Keep
            }
            // Handle null key (used for continuous updates)
            KeyCode::Null => StateChange::Keep,
            _ => StateChange::Keep,
        }
    }

    fn is_noninteractive(&self) -> bool {
        // Stop auto-refresh when awaiting user input
        !self.awaiting_user_input
    }
}
