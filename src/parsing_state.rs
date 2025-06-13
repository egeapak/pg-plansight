use arboard::Clipboard;
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table},
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect_tui::into_span;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::{App, AppState, StateChange};
use crate::{
    log_parser::PostgreSQLLogParser,
    models::{QueryPlan, QueryStatistics},
};

#[derive(Debug, Clone, PartialEq)]
pub enum SortOrder {
    Count,
    Mean,
    Min,
    Max,
    StdDev,
}

#[derive(Debug, Clone)]
pub struct SortState {
    pub order: SortOrder,
    pub ascending: bool,
}

#[derive(Debug, Clone)]
pub enum FocusedPane {
    QueryList,
    QueryDetails,
    ExecutionPlan,
}

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
    sort_state: SortState,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    query_scroll: u16,
    plan_scroll: u16,
    plan_horizontal_scroll: u16,
    focused_pane: FocusedPane,
    formatted_sql_cache: HashMap<String, String>,
}

impl ParsingState {
    pub fn new(log_file_path: PathBuf) -> Self {
        let mut instance = Self {
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
            sort_state: SortState {
                order: SortOrder::Count,
                ascending: false,
            },
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            focused_pane: FocusedPane::QueryList,
            formatted_sql_cache: HashMap::new(),
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
                        self.populate_sql_cache(); // Pre-format all SQL queries
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
    }

    fn render_results_screen(&self, f: &mut Frame, area: Rect) {
        if let (Some(queries), Some(stats)) = (&self.parsed_queries, &self.statistics) {
            // Create vertical layout: main content + status
            let main_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(area);

            // Create horizontal split pane layout for main content
            let content_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(main_chunks[0]);

            // Left pane: Table with count and mean time columns
            self.render_queries_table(f, content_chunks[0], queries, stats);

            // Right pane: Selected query details
            self.render_query_details(f, content_chunks[1], queries);

            // Status bar at the bottom
            let status_lines = vec![Line::from(Span::styled(
                "Navigate: Up/Down Tab(focus) | Sort: c(ount) m(ean) n(min) x(max) s(tddev) | Scroll: PgUp/PgDn Left/Right | Copy: Ctrl+S(ql) Ctrl+E(xec) | q(uit)",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))];

            let status = Paragraph::new(status_lines)
                .block(Block::default().borders(Borders::ALL).title("Controls"));
            f.render_widget(status, main_chunks[1]);
        }
    }

    fn get_sorted_query_groups<'a>(
        &self,
        queries: &'a [QueryPlan],
    ) -> Vec<(String, Vec<&'a QueryPlan>)> {
        use std::collections::HashMap;

        // Group queries by normalized query text
        let mut query_groups: HashMap<String, Vec<&'a QueryPlan>> = HashMap::new();
        for query in queries {
            let normalized_query = self.normalize_query(&query.query_text);
            query_groups
                .entry(normalized_query)
                .or_default()
                .push(query);
        }

        // Convert to sorted vector
        let mut grouped_queries: Vec<(String, Vec<&'a QueryPlan>)> =
            query_groups.into_iter().collect();

        // Sort based on current sort state
        match self.sort_state.order {
            SortOrder::Count => {
                grouped_queries.sort_by(|a, b| {
                    let primary = if self.sort_state.ascending {
                        a.1.len().cmp(&b.1.len())
                    } else {
                        b.1.len().cmp(&a.1.len())
                    };
                    primary.then_with(|| a.0.cmp(&b.0)) // Secondary sort by query text
                });
            }
            SortOrder::Mean => {
                grouped_queries.sort_by(|a, b| {
                    let mean_a: f64 =
                        a.1.iter().map(|q| q.duration_ms).sum::<f64>() / a.1.len() as f64;
                    let mean_b: f64 =
                        b.1.iter().map(|q| q.duration_ms).sum::<f64>() / b.1.len() as f64;
                    let primary = if self.sort_state.ascending {
                        mean_a
                            .partial_cmp(&mean_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        mean_b
                            .partial_cmp(&mean_a)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| a.0.cmp(&b.0)) // Secondary sort by query text
                });
            }
            SortOrder::Min => {
                grouped_queries.sort_by(|a, b| {
                    let min_a =
                        a.1.iter()
                            .map(|q| q.duration_ms)
                            .fold(f64::INFINITY, f64::min);
                    let min_b =
                        b.1.iter()
                            .map(|q| q.duration_ms)
                            .fold(f64::INFINITY, f64::min);
                    let primary = if self.sort_state.ascending {
                        min_a
                            .partial_cmp(&min_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        min_b
                            .partial_cmp(&min_a)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| a.0.cmp(&b.0)) // Secondary sort by query text
                });
            }
            SortOrder::Max => {
                grouped_queries.sort_by(|a, b| {
                    let max_a = a.1.iter().map(|q| q.duration_ms).fold(0.0, f64::max);
                    let max_b = b.1.iter().map(|q| q.duration_ms).fold(0.0, f64::max);
                    let primary = if self.sort_state.ascending {
                        max_a
                            .partial_cmp(&max_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        max_b
                            .partial_cmp(&max_a)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| a.0.cmp(&b.0)) // Secondary sort by query text
                });
            }
            SortOrder::StdDev => {
                grouped_queries.sort_by(|a, b| {
                    let mean_a: f64 =
                        a.1.iter().map(|q| q.duration_ms).sum::<f64>() / a.1.len() as f64;
                    let mean_b: f64 =
                        b.1.iter().map(|q| q.duration_ms).sum::<f64>() / b.1.len() as f64;

                    let var_a =
                        a.1.iter()
                            .map(|q| (q.duration_ms - mean_a).powi(2))
                            .sum::<f64>()
                            / a.1.len() as f64;
                    let var_b =
                        b.1.iter()
                            .map(|q| (q.duration_ms - mean_b).powi(2))
                            .sum::<f64>()
                            / b.1.len() as f64;
                    let std_a = var_a.sqrt();
                    let std_b = var_b.sqrt();

                    let primary = if self.sort_state.ascending {
                        std_a
                            .partial_cmp(&std_b)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        std_b
                            .partial_cmp(&std_a)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| a.0.cmp(&b.0)) // Secondary sort by query text
                });
            }
        }

        grouped_queries
    }

    fn render_queries_table(
        &self,
        f: &mut Frame,
        area: Rect,
        queries: &[QueryPlan],
        _stats: &QueryStatistics,
    ) {
        let grouped_queries = self.get_sorted_query_groups(queries);

        // Create table rows
        let rows: Vec<Row> = grouped_queries
            .iter()
            .enumerate()
            .map(|(index, (query, instances))| {
                let count = instances.len();
                let times: Vec<f64> = instances.iter().map(|q| q.duration_ms).collect();
                let sum: f64 = times.iter().sum();
                let mean_time = sum / count as f64;
                let min_time = times.iter().fold(f64::INFINITY, |a, &b| a.min(b));
                let max_time = times.iter().fold(0.0f64, |a, &b| a.max(b));

                // Calculate standard deviation
                let variance = times
                    .iter()
                    .map(|time| (time - mean_time).powi(2))
                    .sum::<f64>()
                    / count as f64;
                let std_dev = variance.sqrt();

                let query_preview = if query.len() > 30 {
                    format!("{}...", &query[..27])
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
                    Cell::from(format!("{:.2}", min_time)),
                    Cell::from(format!("{:.2}", max_time)),
                    Cell::from(format!("{:.2}", std_dev)),
                    Cell::from(query_preview),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            rows,
            vec![
                Constraint::Length(7), // Count column
                Constraint::Length(9), // Mean time column
                Constraint::Length(9), // Min time column
                Constraint::Length(9), // Max time column
                Constraint::Length(9), // Std dev column
                Constraint::Min(0),    // Query column (takes remaining space)
            ],
        )
        .header(Row::new(vec![
            Cell::from(self.get_header_text("Count", &SortOrder::Count))
                .style(self.get_header_style(&SortOrder::Count)),
            Cell::from(self.get_header_text("Mean", &SortOrder::Mean))
                .style(self.get_header_style(&SortOrder::Mean)),
            Cell::from(self.get_header_text("Min", &SortOrder::Min))
                .style(self.get_header_style(&SortOrder::Min)),
            Cell::from(self.get_header_text("Max", &SortOrder::Max))
                .style(self.get_header_style(&SortOrder::Max)),
            Cell::from(self.get_header_text("StdDev", &SortOrder::StdDev))
                .style(self.get_header_style(&SortOrder::StdDev)),
            Cell::from("Query").style(Style::default().add_modifier(Modifier::BOLD)),
        ]))
        .block(if matches!(self.focused_pane, FocusedPane::QueryList) {
            Block::default()
                .borders(Borders::ALL)
                .title("Query Statistics")
                .border_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .title_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
        } else {
            Block::default()
                .borders(Borders::ALL)
                .title("Query Statistics")
                .border_style(Style::default().fg(Color::Gray))
        })
        .column_spacing(1);

        f.render_widget(table, area);
    }

    fn render_query_details(&self, f: &mut Frame, area: Rect, queries: &[QueryPlan]) {
        let grouped_queries = self.get_sorted_query_groups(queries);

        if let Some((selected_query, instances)) = grouped_queries.get(self.selected_query_index) {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(5),  // Statistics (made taller for stddev)
                    Constraint::Length(10), // Query text (made taller)
                    Constraint::Min(0),     // Plan details
                ])
                .split(area);

            // Statistics for this query (moved to top)
            let total_time: f64 = instances.iter().map(|q| q.duration_ms).sum();
            let min_time = instances
                .iter()
                .map(|q| q.duration_ms)
                .fold(f64::INFINITY, f64::min);
            let max_time = instances.iter().map(|q| q.duration_ms).fold(0.0, f64::max);
            let mean_time = total_time / instances.len() as f64;

            // Calculate standard deviation
            let variance = instances
                .iter()
                .map(|q| (q.duration_ms - mean_time).powi(2))
                .sum::<f64>()
                / instances.len() as f64;
            let std_dev = variance.sqrt();

            let stats_lines = vec![
                Line::from(format!("Executions: {}", instances.len())),
                Line::from(format!(
                    "Min/Mean/Max: {:.2}/{:.2}/{:.2} ms",
                    min_time, mean_time, max_time
                )),
                Line::from(format!("Std Dev: {:.2} ms", std_dev)),
            ];

            let stats_widget = Paragraph::new(stats_lines).block(
                if matches!(self.focused_pane, FocusedPane::QueryDetails) {
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Statistics")
                        .border_style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )
                } else {
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Statistics")
                        .border_style(Style::default().fg(Color::Gray))
                },
            );
            f.render_widget(stats_widget, chunks[0]);

            // Query text (formatted and highlighted)
            let formatted_query = self.format_sql(selected_query);
            let highlighted_text = self.highlight_sql(&formatted_query);
            let query_text = Paragraph::new(highlighted_text)
                .block(if matches!(self.focused_pane, FocusedPane::QueryDetails) {
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Query Text (Formatted & Highlighted)")
                        .border_style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )
                        .title_style(
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        )
                } else {
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Query Text (Formatted & Highlighted)")
                        .border_style(Style::default().fg(Color::Gray))
                })
                .style(Style::default().bg(self.get_syntax_background_color()))
                .wrap(ratatui::widgets::Wrap { trim: false })
                .scroll((self.query_scroll, 0));
            f.render_widget(query_text, chunks[1]);

            // Plan details (show the plan from the slowest execution)
            if let Some(slowest_query) = instances
                .iter()
                .max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap())
            {
                let plan_text = Paragraph::new(slowest_query.plan.clone())
                    .block(if matches!(self.focused_pane, FocusedPane::ExecutionPlan) {
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Execution Plan (Slowest)")
                            .border_style(
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            )
                            .title_style(
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            )
                    } else {
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Execution Plan (Slowest)")
                            .border_style(Style::default().fg(Color::Gray))
                    })
                    .style(Style::default().bg(self.get_syntax_background_color()))
                    .scroll((self.plan_scroll, self.plan_horizontal_scroll));
                f.render_widget(plan_text, chunks[2]);
            }
        } else {
            let no_selection = Paragraph::new("No query selected").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Details"),
            );
            f.render_widget(no_selection, area);
        }
    }

    fn normalize_query(&self, query: &str) -> String {
        // Simple query normalization - preserve line structure for comments
        query
            .lines()
            .map(|line| {
                // Normalize whitespace within each line but preserve line breaks
                line.split_whitespace()
                    .collect::<Vec<&str>>()
                    .join(" ")
            })
            .collect::<Vec<String>>()
            .join("\n")
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

    fn format_sql(&self, sql: &str) -> String {
        // Check cache first
        if let Some(cached) = self.formatted_sql_cache.get(sql) {
            return cached.clone();
        }

        // If not in cache, format it (this should only happen during the first render of each query)
        let format_options = sqlformat::FormatOptions {
            indent: sqlformat::Indent::Spaces(4), // Use 4 spaces for better readability
            uppercase: true,                      // Uppercase SQL keywords
            lines_between_queries: 1,
        };

        sqlformat::format(sql, &sqlformat::QueryParams::None, format_options)
    }

    fn populate_sql_cache(&mut self) {
        if let Some(queries) = &self.parsed_queries {
            use std::collections::HashSet;
            let mut unique_queries = HashSet::new();

            // Get all unique query texts
            for query in queries {
                let normalized = self.normalize_query(&query.query_text);
                unique_queries.insert(normalized);
            }

            // Pre-format all unique queries
            for query in unique_queries {
                if !self.formatted_sql_cache.contains_key(&query) {
                    let format_options = sqlformat::FormatOptions {
                        indent: sqlformat::Indent::Spaces(4),
                        uppercase: true,
                        lines_between_queries: 1,
                    };
                    let formatted =
                        sqlformat::format(&query, &sqlformat::QueryParams::None, format_options);
                    self.formatted_sql_cache.insert(query, formatted);
                }
            }
        }
    }

    fn highlight_sql<'a>(&self, sql: &'a str) -> Text<'a> {
        let syntax = self
            .syntax_set
            .find_syntax_by_extension("sql")
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = &self.theme_set.themes["base16-ocean.dark"];

        let mut lines = Vec::new();

        for line in sql.lines() {
            // Preserve empty lines
            if line.trim().is_empty() {
                lines.push(Line::from(""));
                continue;
            }

            // Create a fresh highlighter for each line to prevent comment state from persisting
            let mut highlighter = HighlightLines::new(syntax, theme);
            match highlighter.highlight_line(line, &self.syntax_set) {
                Ok(highlighted_line) => {
                    let spans: Vec<Span> = highlighted_line
                        .iter()
                        .filter_map(|segment| into_span(*segment).ok())
                        .collect();
                    lines.push(Line::from(spans));
                }
                Err(_) => {
                    // Fallback: preserve the original line including whitespace
                    lines.push(Line::from(line));
                }
            }
        }

        Text::from(lines)
    }

    fn get_header_text(&self, base_text: &str, column_order: &SortOrder) -> String {
        if self.sort_state.order == *column_order {
            let arrow = if self.sort_state.ascending {
                "↑"
            } else {
                "↓"
            };
            format!("{} {}", base_text, arrow)
        } else {
            base_text.to_string()
        }
    }

    fn get_header_style(&self, column_order: &SortOrder) -> Style {
        if self.sort_state.order == *column_order {
            Style::default().add_modifier(Modifier::BOLD).fg(Color::Red)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        }
    }

    fn get_syntax_background_color(&self) -> Color {
        // Get the background color from the syntax highlighting theme
        let theme = &self.theme_set.themes["base16-ocean.dark"];

        // Convert syntect Color to ratatui Color
        if let Some(bg_color) = theme.settings.background {
            Color::Rgb(bg_color.r, bg_color.g, bg_color.b)
        } else {
            // Fallback to a dark background if theme doesn't specify one
            Color::Rgb(46, 52, 64)
        }
    }

    fn copy_to_clipboard(&self, content: &str) -> Result<(), String> {
        match Clipboard::new() {
            Ok(mut clipboard) => clipboard
                .set_text(content)
                .map_err(|e| format!("Failed to copy to clipboard: {}", e)),
            Err(e) => Err(format!("Failed to access clipboard: {}", e)),
        }
    }

    fn get_current_sql(&self) -> Option<String> {
        if let Some(queries) = &self.parsed_queries {
            let grouped_queries = self.get_sorted_query_groups(queries);
            if let Some((selected_query, _)) = grouped_queries.get(self.selected_query_index) {
                Some(selected_query.clone())
            } else {
                None
            }
        } else {
            None
        }
    }

    fn get_current_execution_plan(&self) -> Option<String> {
        if let Some(queries) = &self.parsed_queries {
            let grouped_queries = self.get_sorted_query_groups(queries);
            if let Some((_, instances)) = grouped_queries.get(self.selected_query_index) {
                // Get the execution plan from the slowest execution
                if let Some(slowest_query) = instances
                    .iter()
                    .max_by(|a, b| a.duration_ms.partial_cmp(&b.duration_ms).unwrap())
                {
                    Some(slowest_query.plan.clone())
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
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

    async fn process_key(&mut self, key_event: KeyEvent, _app: &mut App) -> StateChange {
        // Only check parsing progress if a task is actually running and parsing isn't complete
        if self.parsing_task.is_some() && !self.parsing_complete {
            self.check_parsing_progress().await;
        }

        // Handle Ctrl+S and Ctrl+E for clipboard operations
        if key_event.modifiers.contains(KeyModifiers::CONTROL) {
            match key_event.code {
                KeyCode::Char('s') => {
                    if self.parsing_complete && self.parsed_queries.is_some() {
                        if let Some(sql) = self.get_current_sql() {
                            let formatted_sql = self.format_sql(&sql);
                            let _ = self.copy_to_clipboard(&formatted_sql);
                        }
                    }
                    return StateChange::Keep;
                }
                KeyCode::Char('e') => {
                    if self.parsing_complete && self.parsed_queries.is_some() {
                        if let Some(plan) = self.get_current_execution_plan() {
                            let _ = self.copy_to_clipboard(&plan);
                        }
                    }
                    return StateChange::Keep;
                }
                _ => {}
            }
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Char('v') => StateChange::Keep,
            KeyCode::Tab => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    self.focused_pane = match self.focused_pane {
                        FocusedPane::QueryList => FocusedPane::QueryDetails,
                        FocusedPane::QueryDetails => FocusedPane::ExecutionPlan,
                        FocusedPane::ExecutionPlan => FocusedPane::QueryList,
                    };
                }
                StateChange::Keep
            }
            KeyCode::Char('c') => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if self.sort_state.order == SortOrder::Count {
                        self.sort_state.ascending = !self.sort_state.ascending;
                    } else {
                        self.sort_state.order = SortOrder::Count;
                        self.sort_state.ascending = false;
                    }
                    self.selected_query_index = 0;
                    self.query_scroll = 0;
                    self.plan_scroll = 0;
                    self.plan_horizontal_scroll = 0;
                }
                StateChange::Keep
            }
            KeyCode::Char('m') => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if self.sort_state.order == SortOrder::Mean {
                        self.sort_state.ascending = !self.sort_state.ascending;
                    } else {
                        self.sort_state.order = SortOrder::Mean;
                        self.sort_state.ascending = false;
                    }
                    self.selected_query_index = 0;
                    self.query_scroll = 0;
                    self.plan_scroll = 0;
                    self.plan_horizontal_scroll = 0;
                }
                StateChange::Keep
            }
            KeyCode::Char('n') => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if self.sort_state.order == SortOrder::Min {
                        self.sort_state.ascending = !self.sort_state.ascending;
                    } else {
                        self.sort_state.order = SortOrder::Min;
                        self.sort_state.ascending = false;
                    }
                    self.selected_query_index = 0;
                    self.query_scroll = 0;
                    self.plan_scroll = 0;
                    self.plan_horizontal_scroll = 0;
                }
                StateChange::Keep
            }
            KeyCode::Char('x') => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if self.sort_state.order == SortOrder::Max {
                        self.sort_state.ascending = !self.sort_state.ascending;
                    } else {
                        self.sort_state.order = SortOrder::Max;
                        self.sort_state.ascending = false;
                    }
                    self.selected_query_index = 0;
                    self.query_scroll = 0;
                    self.plan_scroll = 0;
                    self.plan_horizontal_scroll = 0;
                }
                StateChange::Keep
            }
            KeyCode::Char('s') => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if self.sort_state.order == SortOrder::StdDev {
                        self.sort_state.ascending = !self.sort_state.ascending;
                    } else {
                        self.sort_state.order = SortOrder::StdDev;
                        self.sort_state.ascending = false;
                    }
                    self.selected_query_index = 0;
                    self.query_scroll = 0;
                    self.plan_scroll = 0;
                    self.plan_horizontal_scroll = 0;
                }
                StateChange::Keep
            }
            KeyCode::Up => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    match self.focused_pane {
                        FocusedPane::QueryList => {
                            if self.selected_query_index > 0 {
                                self.selected_query_index -= 1;
                                // Reset scroll when changing selection
                                self.query_scroll = 0;
                                self.plan_scroll = 0;
                                self.plan_horizontal_scroll = 0;
                            }
                        }
                        FocusedPane::QueryDetails => {
                            if self.query_scroll > 0 {
                                self.query_scroll -= 1;
                            }
                        }
                        FocusedPane::ExecutionPlan => {
                            if self.plan_scroll > 0 {
                                self.plan_scroll -= 1;
                            }
                        }
                    }
                }
                StateChange::Keep
            }
            KeyCode::Down => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    match self.focused_pane {
                        FocusedPane::QueryList => {
                            if let Some(queries) = &self.parsed_queries {
                                let max_index =
                                    self.get_unique_query_count(queries).saturating_sub(1);
                                if self.selected_query_index < max_index {
                                    self.selected_query_index += 1;
                                    // Reset scroll when changing selection
                                    self.query_scroll = 0;
                                    self.plan_scroll = 0;
                                }
                            }
                        }
                        FocusedPane::QueryDetails => {
                            self.query_scroll += 1;
                        }
                        FocusedPane::ExecutionPlan => {
                            self.plan_scroll += 1;
                        }
                    }
                }
                StateChange::Keep
            }
            KeyCode::PageUp => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    match self.focused_pane {
                        FocusedPane::QueryDetails => {
                            self.query_scroll = self.query_scroll.saturating_sub(5);
                        }
                        FocusedPane::ExecutionPlan => {
                            self.plan_scroll = self.plan_scroll.saturating_sub(5);
                        }
                        _ => {}
                    }
                }
                StateChange::Keep
            }
            KeyCode::PageDown => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    match self.focused_pane {
                        FocusedPane::QueryDetails => {
                            self.query_scroll += 5;
                        }
                        FocusedPane::ExecutionPlan => {
                            self.plan_scroll += 5;
                        }
                        _ => {}
                    }
                }
                StateChange::Keep
            }
            KeyCode::Left => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if matches!(self.focused_pane, FocusedPane::ExecutionPlan) {
                        self.plan_horizontal_scroll = self.plan_horizontal_scroll.saturating_sub(1);
                    }
                }
                StateChange::Keep
            }
            KeyCode::Right => {
                if self.parsing_complete && self.parsed_queries.is_some() {
                    if matches!(self.focused_pane, FocusedPane::ExecutionPlan) {
                        self.plan_horizontal_scroll += 1;
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
