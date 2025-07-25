use arboard::Clipboard;
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hashbrown::HashMap;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect_tui::into_span;

use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::query_detail_state::QueryDetailState;
use chrono::{DateTime, Utc};
use pg_loganalyze_core::{PostgreSQLLogParser, ProcessedQuery, QueryPlan};

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

pub struct ResultsState {
    parsed_queries: Vec<QueryPlan>,
    processed_queries: HashMap<u64, ProcessedQuery>,
    sorted_query_hashes: Vec<u64>,
    selected_query_index: usize,
    sort_state: SortState,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    query_scroll: u16,
    plan_scroll: u16,
    plan_horizontal_scroll: u16,
    focused_pane: FocusedPane,
    highlighted_sql_cache: HashMap<String, Text<'static>>,
    last_selected_query: Option<usize>,
    date_range_start: Option<DateTime<Utc>>,
    date_range_end: Option<DateTime<Utc>>,
}

impl ResultsState {
    pub fn new(
        queries: Vec<QueryPlan>,
        date_range_start: Option<DateTime<Utc>>,
        date_range_end: Option<DateTime<Utc>>,
    ) -> Self {
        let mut instance = Self {
            parsed_queries: queries,
            processed_queries: HashMap::new(),
            sorted_query_hashes: Vec::new(),
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
            highlighted_sql_cache: HashMap::new(),
            last_selected_query: None,
            date_range_start,
            date_range_end,
        };

        // Process queries and build cache
        instance.build_processed_queries_cache();
        instance
    }

    fn build_processed_queries_cache(&mut self) {
        let mut parser = PostgreSQLLogParser::new();
        let processed_queries = parser.get_processed_queries(&self.parsed_queries);
        self.sorted_query_hashes = processed_queries.keys().cloned().collect();
        self.processed_queries = processed_queries;

        // Recalculate overall date range from grouped query date ranges
        if !self.processed_queries.is_empty() {
            let mut min_dates = Vec::new();
            let mut max_dates = Vec::new();

            for query in self.processed_queries.values() {
                min_dates.push(query.statistics.min_timestamp);
                max_dates.push(query.statistics.max_timestamp);
            }

            self.date_range_start = min_dates.iter().min().copied();
            self.date_range_end = max_dates.iter().max().copied();
        }

        self.sort_processed_queries();
    }

    fn sort_processed_queries(&mut self) {
        let processed_queries = &self.processed_queries;
        // Sort based on current sort state
        match self.sort_state.order {
            SortOrder::Count => {
                self.sorted_query_hashes.sort_by(|&hash_a, &hash_b| {
                    let query_a = &processed_queries[&hash_a];
                    let query_b = &processed_queries[&hash_b];
                    let primary = if self.sort_state.ascending {
                        query_a.statistics.count.cmp(&query_b.statistics.count)
                    } else {
                        query_b.statistics.count.cmp(&query_a.statistics.count)
                    };
                    primary.then_with(|| query_a.normalized_query().cmp(query_b.normalized_query()))
                });
            }
            SortOrder::Mean => {
                self.sorted_query_hashes.sort_by(|&hash_a, &hash_b| {
                    let query_a = &processed_queries[&hash_a];
                    let query_b = &processed_queries[&hash_b];
                    let primary = if self.sort_state.ascending {
                        query_a
                            .statistics
                            .mean_duration_ms
                            .partial_cmp(&query_b.statistics.mean_duration_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        query_b
                            .statistics
                            .mean_duration_ms
                            .partial_cmp(&query_a.statistics.mean_duration_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| query_a.normalized_query().cmp(query_b.normalized_query()))
                });
            }
            SortOrder::Min => {
                self.sorted_query_hashes.sort_by(|&hash_a, &hash_b| {
                    let query_a = &processed_queries[&hash_a];
                    let query_b = &processed_queries[&hash_b];
                    let primary = if self.sort_state.ascending {
                        query_a
                            .statistics
                            .min_duration_ms
                            .partial_cmp(&query_b.statistics.min_duration_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        query_b
                            .statistics
                            .min_duration_ms
                            .partial_cmp(&query_a.statistics.min_duration_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| query_a.normalized_query().cmp(query_b.normalized_query()))
                });
            }
            SortOrder::Max => {
                self.sorted_query_hashes.sort_by(|&hash_a, &hash_b| {
                    let query_a = &processed_queries[&hash_a];
                    let query_b = &processed_queries[&hash_b];
                    let primary = if self.sort_state.ascending {
                        query_a
                            .statistics
                            .max_duration_ms
                            .partial_cmp(&query_b.statistics.max_duration_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        query_b
                            .statistics
                            .max_duration_ms
                            .partial_cmp(&query_a.statistics.max_duration_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| query_a.normalized_query().cmp(query_b.normalized_query()))
                });
            }
            SortOrder::StdDev => {
                self.sorted_query_hashes.sort_by(|&hash_a, &hash_b| {
                    let query_a = &processed_queries[&hash_a];
                    let query_b = &processed_queries[&hash_b];
                    let primary = if self.sort_state.ascending {
                        query_a
                            .statistics
                            .std_dev_ms
                            .partial_cmp(&query_b.statistics.std_dev_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        query_b
                            .statistics
                            .std_dev_ms
                            .partial_cmp(&query_a.statistics.std_dev_ms)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    };
                    primary.then_with(|| query_a.normalized_query().cmp(query_b.normalized_query()))
                });
            }
        }
    }

    fn render_results_screen(&mut self, f: &mut Frame, area: Rect) {
        // Create vertical layout: header + main content + status
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        // Date range header
        self.render_date_range_header(f, main_chunks[0]);

        // Create horizontal split pane layout for main content
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(main_chunks[1]);

        // Left pane: Table with count and mean time columns
        self.render_queries_table(f, content_chunks[0]);

        // Right pane: Selected query details
        self.render_query_details(f, content_chunks[1]);

        // Status bar at the bottom
        let status_lines = vec![Line::from(Span::styled(
            "Navigate: Up/Down Tab(focus) Enter(detail) | Sort: c(ount) m(ean) n(min) x(max) s(tddev) | Scroll: PgUp/PgDn Left/Right | Copy: Ctrl+S(ql) Ctrl+E(xec) | q(uit)",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))];

        let status = Paragraph::new(status_lines)
            .block(Block::default().borders(Borders::ALL).title("Controls"));
        f.render_widget(status, main_chunks[2]);
    }

    fn render_date_range_header(&self, f: &mut Frame, area: Rect) {
        let header_text = match (self.date_range_start, self.date_range_end) {
            (Some(start), Some(end)) => {
                if start.date_naive() == end.date_naive() {
                    format!("Log Date: {}", start.format("%Y-%m-%d"))
                } else {
                    format!(
                        "Log Date Range: {} to {}",
                        start.format("%Y-%m-%d"),
                        end.format("%Y-%m-%d")
                    )
                }
            }
            _ => "No date range available".to_string(),
        };

        let header_paragraph = Paragraph::new(Line::from(Span::styled(
            header_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .block(Block::default().borders(Borders::ALL))
        .alignment(ratatui::layout::Alignment::Center);

        f.render_widget(header_paragraph, area);
    }

    fn render_queries_table(&self, f: &mut Frame, area: Rect) {
        // Create table rows using cached processed queries
        let rows: Vec<Row> = self
            .sorted_query_hashes
            .iter()
            .enumerate()
            .map(|(index, &hash)| {
                let processed_query = &self.processed_queries[&hash];
                let stats = &processed_query.statistics;

                let query_preview = processed_query
                    .representative_plan
                    .normalized_query
                    .as_str();

                let style = if index == self.selected_query_index {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::default()
                };

                Row::new(vec![
                    Cell::from(stats.count.to_string()),
                    Cell::from(format!("{:.2}", stats.mean_duration_ms)),
                    Cell::from(format!("{:.2}", stats.min_duration_ms)),
                    Cell::from(format!("{:.2}", stats.max_duration_ms)),
                    Cell::from(format!("{:.2}", stats.std_dev_ms)),
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
                Constraint::Fill(1),   // Query column (takes remaining space)
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

    fn render_query_details(&mut self, f: &mut Frame, area: Rect) {
        // Check if we need to update highlighting cache
        if self.last_selected_query != Some(self.selected_query_index) {
            self.last_selected_query = Some(self.selected_query_index);
        }

        if let Some(&selected_hash) = self.sorted_query_hashes.get(self.selected_query_index) {
            // Clone the processed query to avoid borrowing conflicts
            let selected_processed_query = self.processed_queries[&selected_hash].clone();
            let formatted_query = selected_processed_query
                .representative_plan
                .formatted_query
                .clone();
            // Use plan renderer to show structured parsed plan
            use crate::plan_renderer::PlanRenderer;
            let renderer = PlanRenderer::new();
            let parsed_plan = selected_processed_query.representative_plan.parsed();
            let plan_text = renderer.render_plan_compact(parsed_plan);
            let stats = selected_processed_query.statistics.clone();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(6), // Statistics (made taller for stddev)
                    Constraint::Fill(2),   // Query text (made taller)
                    Constraint::Fill(1),   // Plan details
                ])
                .split(area);

            // Statistics for this query (moved to top)
            let date_range_line =
                if stats.min_timestamp.date_naive() == stats.max_timestamp.date_naive() {
                    format!("Date: {}", stats.min_timestamp.format("%Y-%m-%d"))
                } else {
                    format!(
                        "Date Range: {} to {}",
                        stats.min_timestamp.format("%Y-%m-%d"),
                        stats.max_timestamp.format("%Y-%m-%d")
                    )
                };

            let stats_lines = vec![
                Line::from(format!("Executions: {}", stats.count)),
                Line::from(format!(
                    "Min/Mean/Max: {:.2}/{:.2}/{:.2} ms",
                    stats.min_duration_ms, stats.mean_duration_ms, stats.max_duration_ms
                )),
                Line::from(format!("Std Dev: {:.2} ms", stats.std_dev_ms)),
                Line::from(date_range_line),
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
            let plan_paragraph = Paragraph::new(plan_text)
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
            f.render_widget(plan_paragraph, chunks[2]);
        } else {
            let no_selection = Paragraph::new("No query selected").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Details"),
            );
            f.render_widget(no_selection, area);
        }
    }

    fn get_unique_query_count(&self) -> usize {
        self.sorted_query_hashes.len()
    }

    fn highlight_sql(&mut self, sql: &str) -> Text<'static> {
        // Check cache first
        if let Some(cached) = self.highlighted_sql_cache.get(sql) {
            return cached.clone();
        }
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
                    let spans: Vec<Span<'static>> = highlighted_line
                        .iter()
                        .filter_map(|segment| {
                            into_span(*segment)
                                .ok()
                                .map(|span| Span::styled(span.content.to_string(), span.style))
                        })
                        .collect();
                    lines.push(Line::from(spans));
                }
                Err(_) => {
                    // Fallback: preserve the original line including whitespace
                    lines.push(Line::from(line.to_string()));
                }
            }
        }

        let text = Text::from(lines);

        // Cache the result with limited cache size
        if self.highlighted_sql_cache.len() < 100 {
            self.highlighted_sql_cache
                .insert(sql.to_string(), text.clone());
        }

        text
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

    fn get_current_sql(&self) -> Option<&str> {
        if let Some(&selected_hash) = self.sorted_query_hashes.get(self.selected_query_index) {
            Some(
                &self.processed_queries[&selected_hash]
                    .representative_plan
                    .formatted_query,
            )
        } else {
            None
        }
    }

    fn get_current_execution_plan(&self) -> Option<&str> {
        if let Some(&selected_hash) = self.sorted_query_hashes.get(self.selected_query_index) {
            Some(
                self.processed_queries[&selected_hash]
                    .representative_plan
                    .raw_plan(),
            )
        } else {
            None
        }
    }
}

#[async_trait]
impl AppState for ResultsState {
    fn ui(&mut self, f: &mut Frame, _app: &App) {
        let area = f.area();
        self.render_results_screen(f, area);
    }

    async fn process_key(&mut self, key_event: KeyEvent, _app: &mut App) -> StateChange {
        // Handle Ctrl+S and Ctrl+E for clipboard operations
        if key_event.modifiers.contains(KeyModifiers::CONTROL) {
            match key_event.code {
                KeyCode::Char('s') => {
                    if let Some(sql) = self.get_current_sql() {
                        let _ = self.copy_to_clipboard(sql);
                    }
                    return StateChange::Keep;
                }
                KeyCode::Char('e') => {
                    if let Some(plan) = self.get_current_execution_plan() {
                        let _ = self.copy_to_clipboard(plan);
                    }
                    return StateChange::Keep;
                }
                _ => {}
            }
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Enter => {
                // Navigate to detail page for selected query
                if let Some(&selected_hash) =
                    self.sorted_query_hashes.get(self.selected_query_index)
                {
                    if let Some(selected_query) = self.processed_queries.get(&selected_hash) {
                        let detail_state = QueryDetailState::new(
                            selected_query.clone(),
                            selected_hash,
                            self.parsed_queries.clone(),
                            self.date_range_start,
                            self.date_range_end,
                        );
                        return StateChange::Change(Box::new(detail_state));
                    }
                }
                StateChange::Keep
            }
            KeyCode::Tab => {
                self.focused_pane = match self.focused_pane {
                    FocusedPane::QueryList => FocusedPane::QueryDetails,
                    FocusedPane::QueryDetails => FocusedPane::ExecutionPlan,
                    FocusedPane::ExecutionPlan => FocusedPane::QueryList,
                };
                StateChange::Keep
            }
            KeyCode::Char('c') => {
                if self.sort_state.order == SortOrder::Count {
                    self.sort_state.ascending = !self.sort_state.ascending;
                } else {
                    self.sort_state.order = SortOrder::Count;
                    self.sort_state.ascending = false;
                }
                self.sort_processed_queries();
                self.selected_query_index = 0;
                self.query_scroll = 0;
                self.plan_scroll = 0;
                self.plan_horizontal_scroll = 0;
                StateChange::Keep
            }
            KeyCode::Char('m') => {
                if self.sort_state.order == SortOrder::Mean {
                    self.sort_state.ascending = !self.sort_state.ascending;
                } else {
                    self.sort_state.order = SortOrder::Mean;
                    self.sort_state.ascending = false;
                }
                self.sort_processed_queries();
                self.selected_query_index = 0;
                self.query_scroll = 0;
                self.plan_scroll = 0;
                self.plan_horizontal_scroll = 0;
                StateChange::Keep
            }
            KeyCode::Char('n') => {
                if self.sort_state.order == SortOrder::Min {
                    self.sort_state.ascending = !self.sort_state.ascending;
                } else {
                    self.sort_state.order = SortOrder::Min;
                    self.sort_state.ascending = false;
                }
                self.sort_processed_queries();
                self.selected_query_index = 0;
                self.query_scroll = 0;
                self.plan_scroll = 0;
                self.plan_horizontal_scroll = 0;
                StateChange::Keep
            }
            KeyCode::Char('x') => {
                if self.sort_state.order == SortOrder::Max {
                    self.sort_state.ascending = !self.sort_state.ascending;
                } else {
                    self.sort_state.order = SortOrder::Max;
                    self.sort_state.ascending = false;
                }
                self.sort_processed_queries();
                self.selected_query_index = 0;
                self.query_scroll = 0;
                self.plan_scroll = 0;
                self.plan_horizontal_scroll = 0;
                StateChange::Keep
            }
            KeyCode::Char('s') => {
                if self.sort_state.order == SortOrder::StdDev {
                    self.sort_state.ascending = !self.sort_state.ascending;
                } else {
                    self.sort_state.order = SortOrder::StdDev;
                    self.sort_state.ascending = false;
                }
                self.sort_processed_queries();
                self.selected_query_index = 0;
                self.query_scroll = 0;
                self.plan_scroll = 0;
                self.plan_horizontal_scroll = 0;
                StateChange::Keep
            }
            KeyCode::Up => {
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
                StateChange::Keep
            }
            KeyCode::Down => {
                match self.focused_pane {
                    FocusedPane::QueryList => {
                        let max_index = self.get_unique_query_count().saturating_sub(1);
                        if self.selected_query_index < max_index {
                            self.selected_query_index += 1;
                            // Reset scroll when changing selection
                            self.query_scroll = 0;
                            self.plan_scroll = 0;
                        }
                    }
                    FocusedPane::QueryDetails => {
                        self.query_scroll += 1;
                    }
                    FocusedPane::ExecutionPlan => {
                        self.plan_scroll += 1;
                    }
                }
                StateChange::Keep
            }
            KeyCode::PageUp => {
                match self.focused_pane {
                    FocusedPane::QueryDetails => {
                        self.query_scroll = self.query_scroll.saturating_sub(5);
                    }
                    FocusedPane::ExecutionPlan => {
                        self.plan_scroll = self.plan_scroll.saturating_sub(5);
                    }
                    _ => {}
                }
                StateChange::Keep
            }
            KeyCode::PageDown => {
                match self.focused_pane {
                    FocusedPane::QueryDetails => {
                        self.query_scroll += 5;
                    }
                    FocusedPane::ExecutionPlan => {
                        self.plan_scroll += 5;
                    }
                    _ => {}
                }
                StateChange::Keep
            }
            KeyCode::Left => {
                if matches!(self.focused_pane, FocusedPane::ExecutionPlan) {
                    self.plan_horizontal_scroll = self.plan_horizontal_scroll.saturating_sub(1);
                }
                StateChange::Keep
            }
            KeyCode::Right => {
                if matches!(self.focused_pane, FocusedPane::ExecutionPlan) {
                    self.plan_horizontal_scroll += 1;
                }
                StateChange::Keep
            }
            // Handle null key (used for continuous updates)
            KeyCode::Null => StateChange::Keep,
            _ => StateChange::Keep,
        }
    }

    fn is_noninteractive(&self) -> bool {
        false
    }
}
