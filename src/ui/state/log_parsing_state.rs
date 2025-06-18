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

use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::results_state::ResultsState;
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
    max_parallel_threads: usize,
    total_queries_parsed: Arc<AtomicUsize>,
    final_result: Option<anyhow::Result<Vec<QueryPlan>>>,
}

impl LogParsingState {
    pub fn new(log_file_paths: Vec<PathBuf>) -> Self {
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
            max_parallel_threads: rayon::current_num_threads(),
            total_queries_parsed: Arc::new(AtomicUsize::new(0)),
            final_result: None,
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

                    // Transition to results state
                    let results_state = ResultsState::new(queries);
                    return Some(StateChange::Change(Box::new(results_state)));
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
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Length(3), // Overall progress
                Constraint::Min(5),    // File progress gauges (dynamic)
                Constraint::Length(4), // Status/Error (increased for help text)
            ])
            .split(area);

        let active_files = self
            .file_progress
            .iter()
            .filter(|fp| !fp.completed && fp.error.is_none())
            .count();
        let queries_count = self.total_queries_parsed.load(Ordering::Relaxed);
        let title_text = format!(
            "Parallel PostgreSQL Log Parser - {} files ({} active, max {} threads, {} queries)",
            self.file_progress.len(),
            active_files,
            self.max_parallel_threads,
            queries_count
        );
        let title = Paragraph::new(title_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Parallel Multi-File Parser"),
            )
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(title, main_chunks[0]);

        // Overall progress with detailed status
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
        let overall_title = format!(
            "Overall Progress ({}/{} files, {} active, {} failed)",
            completed_files,
            self.file_progress.len(),
            active_files,
            failed_files
        );

        let overall_color = if failed_files > 0 && completed_files == 0 {
            Color::Red
        } else if failed_files > 0 {
            Color::Yellow
        } else if self.overall_progress >= 1.0 {
            Color::Green
        } else {
            Color::Cyan
        };

        let progress_block = Block::default().borders(Borders::ALL).title(overall_title);
        let progress = Gauge::default()
            .block(progress_block)
            .gauge_style(Style::default().fg(overall_color))
            .percent((self.overall_progress * 100.0) as u16)
            .label(format!("{:.1}%", self.overall_progress * 100.0));
        f.render_widget(progress, main_chunks[1]);

        // File progress gauges
        let file_area = main_chunks[2];
        let num_files = self.file_progress.len();

        if num_files > 0 {
            // Dynamically calculate height per file based on available space
            let available_height = file_area.height as usize;
            let min_height_per_file = 3;
            let max_height_per_file = 4;

            let height_per_file = if num_files * min_height_per_file <= available_height {
                if num_files * max_height_per_file <= available_height {
                    max_height_per_file
                } else {
                    available_height / num_files
                }
            } else {
                min_height_per_file
            };

            // Create constraints for each file
            let file_constraints: Vec<Constraint> = (0..num_files)
                .map(|_| Constraint::Length(height_per_file as u16))
                .collect();

            let file_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(file_constraints)
                .split(file_area);

            for (index, fp) in self.file_progress.iter().enumerate() {
                if index < file_chunks.len() {
                    let file_name = fp
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Unknown");

                    // Truncate long filenames to fit better
                    let display_name = if file_name.len() > 30 {
                        format!("...{}", &file_name[file_name.len() - 27..])
                    } else {
                        file_name.to_string()
                    };

                    let (gauge_style, title_style, progress_percent, status_icon) =
                        if fp.error.is_some() {
                            (
                                Style::default().fg(Color::Red),
                                Style::default().fg(Color::Red),
                                0u16,
                                "❌",
                            )
                        } else if fp.completed {
                            (
                                Style::default().fg(Color::Green),
                                Style::default().fg(Color::Green),
                                100u16,
                                "✅",
                            )
                        } else if fp.progress > 0.0 {
                            (
                                Style::default().fg(Color::Cyan),
                                Style::default().fg(Color::White),
                                (fp.progress * 100.0) as u16,
                                "🔄",
                            )
                        } else {
                            (
                                Style::default().fg(Color::Gray),
                                Style::default().fg(Color::Gray),
                                0u16,
                                "⏳",
                            )
                        };

                    let status_text = if let Some(ref error) = fp.error {
                        format!("ERROR: {}", error)
                    } else {
                        fp.status.clone()
                    };

                    let label = format!(
                        "{} {:.1}% - {}",
                        status_icon,
                        fp.progress * 100.0,
                        status_text
                    );
                    let title = format!("{}. {} {}", index + 1, status_icon, display_name);

                    let file_gauge = Gauge::default()
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(title)
                                .title_style(title_style),
                        )
                        .gauge_style(gauge_style)
                        .percent(progress_percent)
                        .label(label);

                    f.render_widget(file_gauge, file_chunks[index]);
                }
            }
        }

        // Status/Error message area with help text
        if let Some(ref error) = self.error_message {
            let error_widget = Paragraph::new(error.as_str())
                .block(Block::default().borders(Borders::ALL).title("Error"))
                .style(Style::default().fg(Color::Red));
            f.render_widget(error_widget, main_chunks[3]);
        } else {
            let (elapsed_time, performance_info) = if let Some(start_time) = self.parsing_start_time
            {
                let elapsed = start_time.elapsed().as_secs_f64();
                let queries_count = self.total_queries_parsed.load(Ordering::Relaxed);
                let rate = if elapsed > 0.0 && queries_count > 0 {
                    format!(" | {:.1} queries/sec", queries_count as f64 / elapsed)
                } else {
                    String::new()
                };
                (format!(" (Elapsed: {:.1}s)", elapsed), rate)
            } else {
                (String::new(), String::new())
            };
            let status_with_help = format!(
                "{}{}{}\nParallel threads: {} | CPU cores: {}\n\nPress 'q' to quit",
                self.status_message,
                elapsed_time,
                performance_info,
                self.max_parallel_threads,
                rayon::current_num_threads()
            );
            let status_widget = Paragraph::new(status_with_help.as_str())
                .block(Block::default().borders(Borders::ALL).title("Status"))
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(status_widget, main_chunks[3]);
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
