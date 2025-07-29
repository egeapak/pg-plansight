use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Gauge, Paragraph},
};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;
use tokio::task::JoinHandle;

use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::results_state::ResultsState;
use pg_loganalyze_core::{DateFilter, ParseProgress, PostgreSQLLogParser, QueryPlan, expand_files, ProcessedQuery};
use hashbrown::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum ProcessingPhase {
    DateRange,
    QueryNormalization,
    StatisticalAnalysis,
    HistogramGeneration,
    Complete,
}

#[derive(Debug, Clone)]
pub enum ProcessingProgress {
    PhaseStarted(ProcessingPhase),
    PhaseProgress { phase: ProcessingPhase, progress: f64, message: String },
    PhaseComplete(ProcessingPhase),
    AllComplete(HashMap<String, ProcessedQuery>),
    Error(String),
}

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
    date_filter: DateFilter,
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
    // Post-processing fields
    post_processing_started: bool,
    post_processing_complete: bool,
    post_processing_start_time: Option<Instant>,
    date_range_start: Option<DateTime<Utc>>,
    date_range_end: Option<DateTime<Utc>>,
    date_range_complete: bool,
    // Multi-phase post-processing
    processing_phase: ProcessingPhase,
    processed_queries: Option<HashMap<String, pg_loganalyze_core::ProcessedQuery>>,
    processing_task: Option<JoinHandle<()>>,
    processing_receiver: Option<mpsc::Receiver<ProcessingProgress>>,
}

impl LogParsingState {
    pub fn new(log_file_paths: Vec<PathBuf>, date_filter: DateFilter) -> Self {
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
            date_filter,
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
            // Post-processing fields
            post_processing_started: false,
            post_processing_complete: false,
            post_processing_start_time: None,
            date_range_start: None,
            date_range_end: None,
            date_range_complete: false,
            // Multi-phase post-processing
            processing_phase: ProcessingPhase::DateRange,
            processed_queries: None,
            processing_task: None,
            processing_receiver: None,
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
        let progress_receiver =
            PostgreSQLLogParser::parse_multiple_files_async(file_paths, self.date_filter.clone());
        self.progress_receiver = Some(progress_receiver);

        // Create a dummy task to maintain the same interface
        let task = tokio::spawn(async move {
            // The actual parsing is handled by the background thread
            // This task just exists to maintain compatibility
        });

        self.parsing_task = Some(task);
    }

    fn check_parsing_progress_sync(&mut self) {
        // Don't process progress updates if parsing is already complete
        if self.parsing_complete {
            return;
        }

        let mut should_close_receiver = false;

        // Check for progress updates from the parsing task
        if let Some(ref mut progress_receiver) = self.progress_receiver {
            while let Ok(progress) = progress_receiver.try_recv() {
                match progress {
                    ParseProgress::Progress {
                        file_index,
                        progress,
                        queries_parsed,
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

                        // Update the total queries parsed counter
                        self.total_queries_parsed
                            .fetch_add(queries_parsed, Ordering::AcqRel);
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

                    // Mark parsing as complete and start post-processing
                    self.parsing_complete = true;
                    self.parsing_end_time = Some(Instant::now());
                    self.final_result = Some(Ok(queries.clone()));

                    // Start post-processing automatically
                    self.start_post_processing(queries);
                }
                Err(err) => {
                    self.error_message = Some(format!("Failed to parse: {err:?}"));
                    self.status_message = "Parsing failed".to_string();
                    self.overall_progress = 0.0;
                }
            }
        }
    }

    async fn check_parsing_progress(&mut self) -> Option<StateChange> {
        // Just call the sync version and don't block
        self.check_parsing_progress_sync();
        
        // Also check post-processing progress
        self.check_post_processing_progress();
        
        None
    }

    fn check_post_processing_progress(&mut self) {
        if !self.post_processing_started || self.post_processing_complete {
            return;
        }

        if let Some(ref mut receiver) = self.processing_receiver {
            while let Ok(progress) = receiver.try_recv() {
                match progress {
                    ProcessingProgress::PhaseStarted(phase) => {
                        self.processing_phase = phase.clone();
                        self.status_message = match phase {
                            ProcessingPhase::DateRange => "Calculating date ranges...".to_string(),
                            ProcessingPhase::QueryNormalization => "Normalizing and grouping queries...".to_string(),
                            ProcessingPhase::StatisticalAnalysis => "Computing statistical analysis...".to_string(),
                            ProcessingPhase::HistogramGeneration => "Generating execution histograms...".to_string(),
                            ProcessingPhase::Complete => "Post-processing complete!".to_string(),
                        };
                    },
                    ProcessingProgress::PhaseProgress { phase, progress: _prog, message } => {
                        self.processing_phase = phase;
                        self.status_message = message;
                    },
                    ProcessingProgress::PhaseComplete(phase) => {
                        self.status_message = format!("{:?} phase complete", phase);
                    },
                    ProcessingProgress::AllComplete(processed_queries) => {
                        self.processed_queries = Some(processed_queries);
                        self.processing_phase = ProcessingPhase::Complete;
                        self.post_processing_complete = true;
                        self.awaiting_user_input = true;
                        self.status_message = "Post-processing complete! Press ENTER to view results".to_string();
                        self.processing_receiver = None;
                        break;
                    },
                    ProcessingProgress::Error(error) => {
                        self.error_message = Some(format!("Post-processing failed: {}", error));
                        self.processing_receiver = None;
                        break;
                    },
                }
            }
        }
    }

    fn start_post_processing(&mut self, queries: Vec<QueryPlan>) {
        self.post_processing_started = true;
        self.post_processing_start_time = Some(Instant::now());
        self.processing_phase = ProcessingPhase::DateRange;

        // Calculate date range first (existing functionality)
        self.calculate_date_range(&queries);

        // Start the heavy processing in the background
        self.start_heavy_processing(queries);
    }

    fn calculate_date_range(&mut self, queries: &[QueryPlan]) {
        if !queries.is_empty() {
            let timestamps: Vec<_> = queries.par_iter().map(|q| q.timestamp()).collect();
            let min_date = *timestamps.par_iter().min().unwrap();
            let max_date = *timestamps.par_iter().max().unwrap();

            self.date_range_start = Some(min_date);
            self.date_range_end = Some(max_date);
            self.date_range_complete = true;
        }
    }

    fn start_heavy_processing(&mut self, queries: Vec<QueryPlan>) {
        let (tx, rx) = mpsc::channel();
        self.processing_receiver = Some(rx);

        let task = tokio::spawn(async move {
            let mut parser = PostgreSQLLogParser::new();

            // Phase 1: Query Normalization
            if let Err(e) = tx.send(ProcessingProgress::PhaseStarted(ProcessingPhase::QueryNormalization)) {
                eprintln!("Failed to send phase start: {}", e);
                return;
            }

            // Phase 2: Statistical Analysis  
            if let Err(e) = tx.send(ProcessingProgress::PhaseStarted(ProcessingPhase::StatisticalAnalysis)) {
                eprintln!("Failed to send phase start: {}", e);
                return;
            }

            // Phase 3: Histogram Generation
            if let Err(e) = tx.send(ProcessingProgress::PhaseStarted(ProcessingPhase::HistogramGeneration)) {
                eprintln!("Failed to send phase start: {}", e);
                return;
            }

            // Do the actual heavy processing (this is the existing get_processed_queries logic)
            let processed_queries = parser.get_processed_queries(&queries);

            // Send completion
            if let Err(e) = tx.send(ProcessingProgress::AllComplete(processed_queries)) {
                eprintln!("Failed to send completion: {}", e);
            }
        });

        self.processing_task = Some(task);
    }

    fn render_parsing_screen(&self, f: &mut Frame, area: Rect) {
        // Create horizontal split layout
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Min(8),    // Main content area (split horizontally)
                Constraint::Length(6), // Status area
            ])
            .split(area);

        // Title
        let title_text = "PostgreSQL Log Analyzer";
        let title = Paragraph::new(title_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Parsing & Post-Processing Dashboard"),
            )
            .style(Style::default().fg(Color::Cyan));
        f.render_widget(title, main_chunks[0]);

        if self.parsing_complete {
            // Split the main content area horizontally when parsing is complete
            let content_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(50), // Left: Parsing statistics
                    Constraint::Percentage(50), // Right: Post-processing statistics
                ])
                .split(main_chunks[1]);

            // Render left pane (parsing statistics)
            self.render_parsing_pane(f, content_chunks[0]);

            // Render right pane (post-processing statistics)
            self.render_post_processing_pane(f, content_chunks[1]);
        } else {
            // Show only parsing statistics centered when parsing is in progress
            self.render_parsing_pane_full(f, main_chunks[1]);
        }

        // Status area
        self.render_status_area(f, main_chunks[2]);
    }

    fn render_parsing_pane(&self, f: &mut Frame, area: Rect) {
        // Create layout for parsing pane
        let pane_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // Overall progress gauge
                Constraint::Length(3), // File counter
                Constraint::Length(3), // Query counter
                Constraint::Length(4), // Performance statistics
            ])
            .split(area);

        let queries_count = self.total_queries_parsed.load(Ordering::Acquire);
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

        // Overall Progress Gauge
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
            "Parsing Progress"
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
        f.render_widget(progress, pane_chunks[0]);

        // Files Counter
        let total_files = self.file_progress.len();
        let processing_files = self
            .file_progress
            .iter()
            .filter(|fp| !fp.completed && fp.error.is_none())
            .count();

        let files_text = if self.parsing_complete {
            format!("{completed_files}/{total_files} files processed")
        } else {
            format!(
                "{completed_files}/{total_files} files ({processing_files} processing)"
            )
        };

        let files_widget = Paragraph::new(files_text)
            .block(Block::default().borders(Borders::ALL).title("Files"))
            .style(Style::default().fg(Color::White));
        f.render_widget(files_widget, pane_chunks[1]);

        // Queries Counter
        let queries_text = format!("{queries_count} queries parsed");
        let queries_widget = Paragraph::new(queries_text)
            .block(Block::default().borders(Borders::ALL).title("Queries"))
            .style(Style::default().fg(Color::White));
        f.render_widget(queries_widget, pane_chunks[2]);

        // Performance Statistics
        let performance_info = if let Some(start_time) = self.parsing_start_time {
            let elapsed = if let Some(end_time) = self.parsing_end_time {
                end_time.duration_since(start_time).as_secs_f64()
            } else {
                start_time.elapsed().as_secs_f64()
            };

            if elapsed > 0.0 && queries_count > 0 {
                format!("{:.1} queries/sec", queries_count as f64 / elapsed,)
            } else {
                "0.0 queries/sec".to_string()
            }
        } else {
            "0.0 queries/sec".to_string()
        };

        let stats_text = format!(
            "{} | {} threads",
            performance_info, self.max_parallel_threads
        );
        let stats_color = if self.parsing_complete {
            Color::Green
        } else {
            Color::Cyan
        };

        let stats_widget = Paragraph::new(stats_text)
            .block(Block::default().borders(Borders::ALL).title("Performance"))
            .style(Style::default().fg(stats_color));
        f.render_widget(stats_widget, pane_chunks[3]);
    }

    fn render_parsing_pane_full(&self, f: &mut Frame, area: Rect) {
        // Center the parsing statistics when no post-processing pane is shown
        let center_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(60),
                Constraint::Percentage(20),
            ])
            .split(area);

        // Use the same layout as the parsing pane but centered
        self.render_parsing_pane(f, center_chunks[1]);
    }

    fn render_post_processing_pane(&self, f: &mut Frame, area: Rect) {
        // Create layout for post-processing pane
        let pane_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // Post-processing progress gauge
                Constraint::Length(3), // Date range status
                Constraint::Length(3), // Additional stats placeholder
                Constraint::Length(4), // Post-processing performance
            ])
            .split(area);

        // Post-processing Progress Gauge
        let (post_progress, post_title) = if !self.parsing_complete {
            (0.0, "Waiting for Parsing...")
        } else if self.post_processing_complete {
            (1.0, "Post-Processing Complete!")
        } else {
            let phase_progress = match self.processing_phase {
                ProcessingPhase::DateRange => 0.1,
                ProcessingPhase::QueryNormalization => 0.3,
                ProcessingPhase::StatisticalAnalysis => 0.6,
                ProcessingPhase::HistogramGeneration => 0.9,
                ProcessingPhase::Complete => 1.0,
            };
            let phase_name = match self.processing_phase {
                ProcessingPhase::DateRange => "Date Range Analysis",
                ProcessingPhase::QueryNormalization => "Query Normalization",
                ProcessingPhase::StatisticalAnalysis => "Statistical Analysis",
                ProcessingPhase::HistogramGeneration => "Histogram Generation",
                ProcessingPhase::Complete => "Complete",
            };
            (phase_progress, phase_name)
        };
        
        let post_color = if !self.parsing_complete {
            Color::Gray
        } else if self.post_processing_complete {
            Color::Green
        } else {
            Color::Magenta
        };

        let post_progress_block = Block::default()
            .borders(Borders::ALL)
            .title(post_title)
            .title_style(Style::default().fg(post_color));
        let post_progress_gauge = Gauge::default()
            .block(post_progress_block)
            .gauge_style(Style::default().fg(post_color))
            .percent((post_progress * 100.0) as u16)
            .label(format!("{:.1}%", post_progress * 100.0));
        f.render_widget(post_progress_gauge, pane_chunks[0]);

        // Date Range Status
        let date_range_text = if !self.parsing_complete {
            "Waiting...".to_string()
        } else if self.date_range_complete {
            match (self.date_range_start, self.date_range_end) {
                (Some(start), Some(end)) => {
                    if start.date_naive() == end.date_naive() {
                        format!("✓ Date: {}", start.format("%Y-%m-%d"))
                    } else {
                        format!(
                            "✓ Range: {} to {}",
                            start.format("%Y-%m-%d"),
                            end.format("%Y-%m-%d")
                        )
                    }
                }
                _ => "✓ No date range".to_string(),
            }
        } else {
            "Processing date ranges...".to_string()
        };

        let date_range_widget = Paragraph::new(date_range_text)
            .block(Block::default().borders(Borders::ALL).title("Date Range"))
            .style(Style::default().fg(Color::White));
        f.render_widget(date_range_widget, pane_chunks[1]);

        // Additional Stats Placeholder
        let additional_text = if !self.parsing_complete {
            "Waiting...".to_string()
        } else {
            "Ready for analysis".to_string()
        };

        let additional_widget = Paragraph::new(additional_text)
            .block(Block::default().borders(Borders::ALL).title("Analysis"))
            .style(Style::default().fg(Color::White));
        f.render_widget(additional_widget, pane_chunks[2]);

        // Post-processing Performance
        let post_performance_info = if let Some(start_time) = self.post_processing_start_time {
            let elapsed = start_time.elapsed().as_secs_f64();
            let queries_count = self.total_queries_parsed.load(Ordering::Relaxed);
            if elapsed > 0.0 && queries_count > 0 {
                format!("{:.1} queries/sec", queries_count as f64 / elapsed)
            } else {
                "0.0 queries/sec".to_string()
            }
        } else {
            "0.0 queries/sec".to_string()
        };

        let post_stats_text = if !self.parsing_complete {
            "Waiting for parsing...".to_string()
        } else {
            format!("{post_performance_info} | Analysis")
        };

        let post_stats_color = if !self.parsing_complete {
            Color::Gray
        } else if self.post_processing_complete {
            Color::Green
        } else {
            Color::Magenta
        };

        let post_stats_widget = Paragraph::new(post_stats_text)
            .block(Block::default().borders(Borders::ALL).title("Performance"))
            .style(Style::default().fg(post_stats_color));
        f.render_widget(post_stats_widget, pane_chunks[3]);
    }

    fn render_status_area(&self, f: &mut Frame, area: Rect) {
        if let Some(ref error) = self.error_message {
            let error_widget = Paragraph::new(error.as_str())
                .block(Block::default().borders(Borders::ALL).title("Error"))
                .style(Style::default().fg(Color::Red));
            f.render_widget(error_widget, area);
        } else {
            let status_text = if self.awaiting_user_input {
                format!(
                    "{}\n\nPress ENTER to view results or 'q' to quit",
                    self.status_message
                )
            } else {
                let (elapsed_time, performance_info) =
                    if let Some(start_time) = self.parsing_start_time {
                        let elapsed = if let Some(end_time) = self.parsing_end_time {
                            end_time.duration_since(start_time).as_secs_f64()
                        } else {
                            start_time.elapsed().as_secs_f64()
                        };

                        let rate = if elapsed > 0.0 {
                            let queries_count = self.total_queries_parsed.load(Ordering::Relaxed);
                            format!("{:.1} queries/sec", queries_count as f64 / elapsed)
                        } else {
                            "0.0 queries/sec".to_string()
                        };
                        (format!("Elapsed: {elapsed:.1}s"), rate)
                    } else {
                        ("Elapsed: 0.0s".to_string(), "0.0 queries/sec".to_string())
                    };

                format!(
                    "{}\n{} | {} | Threads: {}\n\nPress 'q' to quit",
                    self.status_message, elapsed_time, performance_info, self.max_parallel_threads
                )
            };

            let status_widget = Paragraph::new(status_text)
                .block(Block::default().borders(Borders::ALL).title("Status"))
                .style(Style::default().fg(if self.awaiting_user_input {
                    Color::Green
                } else {
                    Color::Yellow
                }));
            f.render_widget(status_widget, area);
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
        // Check parsing progress first (this also handles the Null key for auto-refresh)
        if let Some(state_change) = self.check_parsing_progress().await {
            return state_change;
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Enter => {
                if self.awaiting_user_input {
                    // User pressed Enter, transition to results
                    if let Some(Ok(queries)) = self.final_result.take() {
                        let results_state = if let Some(processed_queries) = self.processed_queries.take() {
                            // Use pre-processed data if available
                            ResultsState::new_with_processed_queries(
                                queries, 
                                processed_queries, 
                                self.date_range_start, 
                                self.date_range_end
                            )
                        } else {
                            // Fall back to old method if no pre-processed data
                            ResultsState::new(queries, self.date_range_start, self.date_range_end)
                        };
                        return StateChange::Change(Box::new(results_state));
                    }
                }
                StateChange::Keep
            }
            // Handle null key (used for continuous updates during parsing)
            KeyCode::Null => {
                // This is the key that gets generated during auto-refresh cycles
                // The progress check above should handle updating the UI
                StateChange::Keep
            }
            _ => StateChange::Keep,
        }
    }

    fn is_noninteractive(&self) -> bool {
        // Return true during parsing to enable auto-refresh, false when awaiting user input
        !self.awaiting_user_input
    }
}
