use arboard::Clipboard;
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hashbrown::HashMap;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table, TableState,
    },
};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// Convert one syntect highlighted segment `(style, text)` into a ratatui
/// [`Span`]. This replaces the `syntect-tui` crate, which is pinned to
/// ratatui 0.29 and blocks upgrading ratatui. It maps syntect's RGB foreground
/// and background plus its font style (bold/italic/underline) onto a ratatui
/// [`Style`], matching what `syntect_tui::into_span` produced.
fn syntect_segment_to_span(style: &syntect::highlighting::Style, text: &str) -> Span<'static> {
    use syntect::highlighting::FontStyle;
    // Match syntect-tui's colour translation: a syntect colour with alpha 0
    // means "unset" (transparent), so leave fg/bg untouched in that case rather
    // than forcing an opaque black — otherwise every span would get a solid
    // background block instead of the terminal's background showing through.
    let mut span_style = Style::default();
    let fg = style.foreground;
    if fg.a > 0 {
        span_style = span_style
            .fg(Color::Rgb(fg.r, fg.g, fg.b))
            .underline_color(Color::Rgb(fg.r, fg.g, fg.b));
    }
    let bg = style.background;
    if bg.a > 0 {
        span_style = span_style.bg(Color::Rgb(bg.r, bg.g, bg.b));
    }
    if style.font_style.contains(FontStyle::BOLD) {
        span_style = span_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        span_style = span_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        span_style = span_style.add_modifier(Modifier::UNDERLINED);
    }
    Span::styled(text.to_string(), span_style)
}

use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::query_detail_view::{AnalysisStatus, AnalysisTab, QueryDetailView};
use chrono::{DateTime, Utc};
use pg_plansight_core::{PostgreSQLLogParser, ProcessedQuery, QueryPlan};

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

pub enum ViewMode {
    List,
    Detail {
        query_fingerprint: String,
        detail_view: Box<QueryDetailView>,
    },
}

/// Type alias for hotkey group: (title, color, vec of (key, description) tuples)
type HotkeyGroup<'a> = (&'a str, Color, Vec<(&'a str, &'a str)>);

/// Build a borrowed view of a cached [`Text`] so it can be handed to a
/// [`Paragraph`] without deep-cloning it. Each span borrows its string data
/// (`Cow::Borrowed`) from `text`, so no owned `String` is allocated on the
/// per-frame cache-hit path — only the lightweight line/span structure is
/// rebuilt. Styles and alignment are preserved, so the rendered output is
/// identical to cloning the `Text`.
fn borrow_text<'a>(text: &'a Text<'a>) -> Text<'a> {
    Text {
        lines: text
            .lines
            .iter()
            .map(|line| Line {
                spans: line
                    .spans
                    .iter()
                    .map(|span| Span::styled(span.content.as_ref(), span.style))
                    .collect(),
                style: line.style,
                alignment: line.alignment,
            })
            .collect(),
        style: text.style,
        alignment: text.alignment,
    }
}

pub struct ResultsState {
    // Core data - owned by this state
    parsed_queries: Vec<QueryPlan>,
    processed_queries: HashMap<String, ProcessedQuery>,
    sorted_query_fingerprints: Vec<String>,

    // Parser for lazy analysis
    parser: PostgreSQLLogParser,

    // List view state
    selected_query_index: usize,
    /// Scroll offset/selection driver for the query table; without stateful
    /// rendering the table always draws from row 0 and any selection below
    /// the viewport is invisible.
    query_table_state: TableState,
    sort_state: SortState,
    query_scroll: u16,
    plan_scroll: u16,
    plan_horizontal_scroll: u16,
    focused_pane: FocusedPane,
    last_selected_query: Option<usize>,
    /// Transient feedback line ("Copied", "Export failed: ...") shown in the
    /// status bar; clipboard/export outcomes were previously discarded.
    /// The bool is `is_error`, driving the styling (green vs red).
    notification: Option<(String, bool, std::time::Instant)>,
    /// Cached compact plan rendering for the selected query, keyed by
    /// fingerprint: re-rendering the tree on every draw is wasted work.
    plan_render_cache: Option<(String, Text<'static>)>,
    /// Persistent clipboard handle. On X11 the clipboard contents live in the
    /// owning process; dropping a fresh handle right after set_text loses the
    /// copy unless a clipboard manager is running.
    clipboard: Option<Clipboard>,

    // Shared rendering resources
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    highlighted_sql_cache: HashMap<String, Text<'static>>,

    // Date range info
    date_range_start: Option<DateTime<Utc>>,
    date_range_end: Option<DateTime<Utc>>,

    // Current view mode
    view_mode: ViewMode,
}

impl ResultsState {
    /// Helper function to create styled hotkey help text with bold titles and colored keys
    fn create_hotkey_line(groups: Vec<HotkeyGroup>) -> Line<'static> {
        let mut spans = Vec::new();

        for (group_idx, (title, color, hotkeys)) in groups.iter().enumerate() {
            // Add separator between groups
            if group_idx > 0 {
                spans.push(Span::styled(" | ", Style::default().fg(Color::DarkGray)));
            }

            // Add bold title
            spans.push(Span::styled(
                format!("{}: ", title),
                Style::default().add_modifier(Modifier::BOLD),
            ));

            // Add hotkeys with colored keys and normal descriptions
            for (key_idx, (key, description)) in hotkeys.iter().enumerate() {
                if key_idx > 0 {
                    spans.push(Span::raw(" "));
                }

                // Colored key
                spans.push(Span::styled(
                    (*key).to_string(),
                    Style::default().fg(*color),
                ));

                // Normal description in parentheses
                if !description.is_empty() {
                    spans.push(Span::styled(
                        format!("({})", description),
                        Style::default().fg(Color::Gray),
                    ));
                }
            }
        }

        Line::from(spans)
    }

    pub fn new(
        queries: Vec<QueryPlan>,
        date_range_start: Option<DateTime<Utc>>,
        date_range_end: Option<DateTime<Utc>>,
    ) -> Self {
        let mut instance = Self {
            parsed_queries: queries,
            processed_queries: HashMap::new(),
            sorted_query_fingerprints: Vec::new(),
            parser: PostgreSQLLogParser::new(),
            selected_query_index: 0,
            query_table_state: TableState::default(),
            sort_state: SortState {
                order: SortOrder::Count,
                ascending: false,
            },
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            focused_pane: FocusedPane::QueryList,
            last_selected_query: None,
            notification: None,
            plan_render_cache: None,
            clipboard: None,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            highlighted_sql_cache: HashMap::new(),
            date_range_start,
            date_range_end,
            view_mode: ViewMode::List,
        };

        // Process queries and build cache
        instance.build_processed_queries_cache();
        instance
    }

    /// Create a new ResultsState from imported processed queries
    pub fn from_imported_data(
        processed_queries: HashMap<String, ProcessedQuery>,
        date_range_start: Option<DateTime<Utc>>,
        date_range_end: Option<DateTime<Utc>>,
    ) -> Self {
        let sorted_query_fingerprints: Vec<String> = processed_queries.keys().cloned().collect();

        let mut instance = Self {
            parsed_queries: Vec::new(), // Empty since we imported
            processed_queries,
            sorted_query_fingerprints,
            parser: PostgreSQLLogParser::new(),
            selected_query_index: 0,
            query_table_state: TableState::default(),
            sort_state: SortState {
                order: SortOrder::Count,
                ascending: false,
            },
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            focused_pane: FocusedPane::QueryList,
            last_selected_query: None,
            notification: None,
            plan_render_cache: None,
            clipboard: None,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            highlighted_sql_cache: HashMap::new(),
            date_range_start,
            date_range_end,
            view_mode: ViewMode::List,
        };

        // Sort the imported queries
        instance.sort_processed_queries();
        instance
    }

    pub fn new_with_processed_queries(
        processed_queries: HashMap<String, ProcessedQuery>,
        date_range_start: Option<DateTime<Utc>>,
        date_range_end: Option<DateTime<Utc>>,
    ) -> Self {
        let sorted_query_fingerprints: Vec<String> = processed_queries.keys().cloned().collect();

        let mut instance = Self {
            // The processed map already embeds representative plans; keeping
            // the raw plan vector here doubled memory without any reader.
            parsed_queries: Vec::new(),
            processed_queries,
            sorted_query_fingerprints,
            parser: PostgreSQLLogParser::new(),
            selected_query_index: 0,
            query_table_state: TableState::default(),
            sort_state: SortState {
                order: SortOrder::Count,
                ascending: false,
            },
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            focused_pane: FocusedPane::QueryList,
            last_selected_query: None,
            notification: None,
            plan_render_cache: None,
            clipboard: None,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            highlighted_sql_cache: HashMap::new(),
            date_range_start,
            date_range_end,
            view_mode: ViewMode::List,
        };

        // Sort the already processed queries
        instance.sort_processed_queries();
        instance
    }

    fn build_processed_queries_cache(&mut self) {
        let processed_queries = self.parser.get_processed_queries(&self.parsed_queries);
        self.sorted_query_fingerprints = processed_queries.keys().cloned().collect();
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
                self.sorted_query_fingerprints
                    .sort_by(|fingerprint_a, fingerprint_b| {
                        let query_a = &processed_queries[fingerprint_a];
                        let query_b = &processed_queries[fingerprint_b];
                        let primary = if self.sort_state.ascending {
                            query_a.statistics.count.cmp(&query_b.statistics.count)
                        } else {
                            query_b.statistics.count.cmp(&query_a.statistics.count)
                        };
                        primary.then_with(|| {
                            query_a.normalized_query().cmp(query_b.normalized_query())
                        })
                    });
            }
            SortOrder::Mean => {
                self.sorted_query_fingerprints
                    .sort_by(|fingerprint_a, fingerprint_b| {
                        let query_a = &processed_queries[fingerprint_a];
                        let query_b = &processed_queries[fingerprint_b];
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
                        primary.then_with(|| {
                            query_a.normalized_query().cmp(query_b.normalized_query())
                        })
                    });
            }
            SortOrder::Min => {
                self.sorted_query_fingerprints
                    .sort_by(|fingerprint_a, fingerprint_b| {
                        let query_a = &processed_queries[fingerprint_a];
                        let query_b = &processed_queries[fingerprint_b];
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
                        primary.then_with(|| {
                            query_a.normalized_query().cmp(query_b.normalized_query())
                        })
                    });
            }
            SortOrder::Max => {
                self.sorted_query_fingerprints
                    .sort_by(|fingerprint_a, fingerprint_b| {
                        let query_a = &processed_queries[fingerprint_a];
                        let query_b = &processed_queries[fingerprint_b];
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
                        primary.then_with(|| {
                            query_a.normalized_query().cmp(query_b.normalized_query())
                        })
                    });
            }
            SortOrder::StdDev => {
                self.sorted_query_fingerprints
                    .sort_by(|fingerprint_a, fingerprint_b| {
                        let query_a = &processed_queries[fingerprint_a];
                        let query_b = &processed_queries[fingerprint_b];
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
                        primary.then_with(|| {
                            query_a.normalized_query().cmp(query_b.normalized_query())
                        })
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

        // Status bar at the bottom with improved hotkey styling
        let status_line = Self::create_hotkey_line(vec![
            (
                "Navigate",
                Color::Yellow,
                vec![("Up/Down", ""), ("Tab", "focus"), ("Enter", "detail")],
            ),
            (
                "Sort",
                Color::Cyan,
                vec![
                    ("c", "ount"),
                    ("m", "ean"),
                    ("n", "min"),
                    ("x", "max"),
                    ("s", "tddev"),
                ],
            ),
            (
                "Scroll",
                Color::Magenta,
                vec![("PgUp/PgDn", ""), ("Left/Right", "")],
            ),
            (
                "Copy",
                Color::Green,
                vec![("Ctrl+S", "ql"), ("Ctrl+E", "xec")],
            ),
            ("Quit", Color::Red, vec![("q", "")]),
        ]);

        let status = if let Some((message, is_error)) = self.active_notification() {
            Paragraph::new(Line::from(Span::styled(
                message.to_string(),
                Style::default()
                    .fg(if is_error { Color::Red } else { Color::Green })
                    .add_modifier(Modifier::BOLD),
            )))
            .block(Block::default().borders(Borders::ALL).title("Status"))
        } else {
            Paragraph::new(vec![status_line])
                .block(Block::default().borders(Borders::ALL).title("Controls"))
        };
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

    fn render_queries_table(&mut self, f: &mut Frame, area: Rect) {
        // Drive the table's internal offset from the selection so ratatui
        // keeps the selected row inside the viewport (scrolling).
        self.query_table_state
            .select(if self.sorted_query_fingerprints.is_empty() {
                None
            } else {
                Some(
                    self.selected_query_index
                        .min(self.sorted_query_fingerprints.len() - 1),
                )
            });
        // Create table rows using cached processed queries
        let rows: Vec<Row> = self
            .sorted_query_fingerprints
            .iter()
            .enumerate()
            .map(|(index, fingerprint)| {
                let processed_query = &self.processed_queries[fingerprint];
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

        f.render_stateful_widget(table, area, &mut self.query_table_state);
    }

    fn render_query_details(&mut self, f: &mut Frame, area: Rect) {
        // Check if we need to update highlighting cache
        if self.last_selected_query != Some(self.selected_query_index) {
            self.last_selected_query = Some(self.selected_query_index);
        }

        if let Some(selected_fingerprint) = self
            .sorted_query_fingerprints
            .get(self.selected_query_index)
        {
            // Borrow only what the panes need — cloning the whole
            // ProcessedQuery copied every execution record on every frame.
            use crate::plan_renderer::PlanRenderer;
            let selected_processed_query = &self.processed_queries[selected_fingerprint];
            let formatted_query = selected_processed_query
                .representative_plan
                .formatted_query
                .clone();
            let stats = &selected_processed_query.statistics;
            let (count, min_ms, mean_ms, max_ms, std_dev_ms) = (
                stats.count,
                stats.min_duration_ms,
                stats.mean_duration_ms,
                stats.max_duration_ms,
                stats.std_dev_ms,
            );
            let (min_timestamp, max_timestamp) = (stats.min_timestamp, stats.max_timestamp);
            if self
                .plan_render_cache
                .as_ref()
                .is_none_or(|(cached_for, _)| cached_for != selected_fingerprint)
            {
                let renderer = PlanRenderer::new();
                let rendered = renderer
                    .render_plan_compact(selected_processed_query.representative_plan.parsed());
                self.plan_render_cache = Some((selected_fingerprint.clone(), rendered));
            }

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(7), // Statistics (stddev + first/last seen)
                    Constraint::Fill(2),   // Query text (made taller)
                    Constraint::Fill(1),   // Plan details
                ])
                .split(area);

            // Statistics for this query (moved to top)
            let stats_lines = vec![
                Line::from(format!("Executions: {}", count)),
                Line::from(format!(
                    "Min/Mean/Max: {:.2}/{:.2}/{:.2} ms",
                    min_ms, mean_ms, max_ms
                )),
                Line::from(format!("Std Dev: {:.2} ms", std_dev_ms)),
                Line::from(format!(
                    "First seen: {}",
                    min_timestamp.format("%Y-%m-%d %H:%M:%S UTC")
                )),
                Line::from(format!(
                    "Last seen:  {}",
                    max_timestamp.format("%Y-%m-%d %H:%M:%S UTC")
                )),
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

            // Plan details (show the plan from the slowest execution).
            // Build the paragraph from a borrowed view of the cached plan text
            // rather than deep-cloning the entire `Text` every frame. This must
            // happen after the `&mut self` `highlight_sql` call above so the
            // immutable borrow of `plan_render_cache` does not conflict.
            let cached_plan_text = &self
                .plan_render_cache
                .as_ref()
                .expect("plan render cache populated above")
                .1;
            let plan_paragraph = Paragraph::new(borrow_text(cached_plan_text))
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
        self.sorted_query_fingerprints.len()
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
                        .map(|segment| syntect_segment_to_span(&segment.0, segment.1))
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
            format!("{base_text} {arrow}")
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

    fn copy_to_clipboard(&mut self, content: &str) -> Result<(), String> {
        if self.clipboard.is_none() {
            self.clipboard =
                Some(Clipboard::new().map_err(|e| format!("Failed to access clipboard: {e}"))?);
        }
        // Safe: just initialized above on the None path.
        self.clipboard
            .as_mut()
            .expect("clipboard initialized above")
            .set_text(content)
            .map_err(|e| format!("Failed to copy to clipboard: {e}"))
    }

    /// Show a transient success message in the status bar.
    fn notify(&mut self, message: impl Into<String>) {
        self.notification = Some((message.into(), false, std::time::Instant::now()));
    }

    /// Show a transient error message in the status bar (styled red).
    fn notify_error(&mut self, message: impl Into<String>) {
        self.notification = Some((message.into(), true, std::time::Instant::now()));
    }

    /// The active notification (message, is_error), if it has not expired yet.
    fn active_notification(&self) -> Option<(&str, bool)> {
        const NOTIFICATION_TTL: std::time::Duration = std::time::Duration::from_secs(5);
        match &self.notification {
            Some((message, is_error, at)) if at.elapsed() < NOTIFICATION_TTL => {
                Some((message, *is_error))
            }
            _ => None,
        }
    }

    fn copy_with_feedback(&mut self, content: String, what: &str) {
        match self.copy_to_clipboard(&content) {
            Ok(()) => self.notify(format!("{what} copied to clipboard")),
            Err(e) => self.notify_error(e),
        }
    }

    fn get_current_sql(&self) -> Option<&str> {
        if let Some(selected_fingerprint) = self
            .sorted_query_fingerprints
            .get(self.selected_query_index)
        {
            Some(
                &self.processed_queries[selected_fingerprint]
                    .representative_plan
                    .formatted_query,
            )
        } else {
            None
        }
    }

    fn get_current_execution_plan(&self) -> Option<&str> {
        if let Some(selected_fingerprint) = self
            .sorted_query_fingerprints
            .get(self.selected_query_index)
        {
            Some(
                self.processed_queries[selected_fingerprint]
                    .representative_plan
                    .raw_plan(),
            )
        } else {
            None
        }
    }

    fn export_to_json(&self) -> Result<String, String> {
        use chrono::Local;
        use pg_plansight_core::AnalysisExport;

        // Generate filename with timestamp
        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let filename = format!("pg_analysis_{}.json", timestamp);

        // Convert hashbrown::HashMap to std::HashMap for serialization
        let std_map: std::collections::HashMap<_, _> =
            self.processed_queries.clone().into_iter().collect();

        // Create export
        let export = AnalysisExport::from_processed_queries(
            std_map,
            vec![], // We don't have source files info in ResultsState
        );

        // Export to file
        export
            .to_file(&filename)
            .map_err(|e| format!("Failed to export: {}", e))?;

        Ok(filename)
    }

    fn switch_to_detail_view(&mut self) {
        if let Some(selected_fingerprint) = self
            .sorted_query_fingerprints
            .get(self.selected_query_index)
            .cloned()
        {
            // Create detail view (no lazy analysis needed - all done in post-processing)
            let mut detail_view = QueryDetailView::new();
            detail_view.start_analysis_delay();

            self.view_mode = ViewMode::Detail {
                query_fingerprint: selected_fingerprint,
                detail_view: Box::new(detail_view),
            };
        }
    }

    fn switch_to_list_view(&mut self) {
        self.view_mode = ViewMode::List;
    }

    fn process_list_key(&mut self, key_event: KeyEvent) -> StateChange {
        // Handle Ctrl+S, Ctrl+E, and Ctrl+X for clipboard operations
        if key_event.modifiers.contains(KeyModifiers::CONTROL) {
            match key_event.code {
                KeyCode::Char('s') => {
                    if let Some(sql) = self.get_current_sql().map(str::to_string) {
                        self.copy_with_feedback(sql, "SQL");
                    }
                    return StateChange::Keep;
                }
                KeyCode::Char('e') => {
                    if let Some(plan) = self.get_current_execution_plan().map(str::to_string) {
                        self.copy_with_feedback(plan, "Execution plan");
                    }
                    return StateChange::Keep;
                }
                KeyCode::Char('x') => {
                    // Export analysis to JSON
                    match self.export_to_json() {
                        Ok(filename) => self.notify(format!("Exported to: {filename}")),
                        Err(e) => self.notify_error(e),
                    }
                    return StateChange::Keep;
                }
                _ => {}
            }
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Enter => {
                // Switch to detail view
                self.switch_to_detail_view();
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
                            self.plan_horizontal_scroll = 0;
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

    fn highlight_sql_static(
        sql: &str,
        highlighted_sql_cache: &mut HashMap<String, Text<'static>>,
        syntax_set: &SyntaxSet,
        theme_set: &ThemeSet,
    ) -> Text<'static> {
        // Check cache first
        if let Some(cached) = highlighted_sql_cache.get(sql) {
            return cached.clone();
        }

        let syntax = syntax_set
            .find_syntax_by_extension("sql")
            .unwrap_or_else(|| syntax_set.find_syntax_plain_text());

        let theme = &theme_set.themes["base16-ocean.dark"];

        let mut lines = Vec::new();
        for line in sql.lines() {
            if line.trim().is_empty() {
                lines.push(Line::from(""));
                continue;
            }

            let mut highlighter = HighlightLines::new(syntax, theme);
            match highlighter.highlight_line(line, syntax_set) {
                Ok(highlighted_line) => {
                    let spans: Vec<Span<'static>> = highlighted_line
                        .iter()
                        .map(|segment| syntect_segment_to_span(&segment.0, segment.1))
                        .collect();
                    lines.push(Line::from(spans));
                }
                Err(_) => {
                    lines.push(Line::from(line.to_string()));
                }
            }
        }

        let text = Text::from(lines);

        // Cache the result
        if highlighted_sql_cache.len() < 100 {
            highlighted_sql_cache.insert(sql.to_string(), text.clone());
        }

        text
    }

    #[allow(clippy::too_many_arguments)]
    fn render_detail_view_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
        highlighted_sql_cache: &mut HashMap<String, Text<'static>>,
        syntax_set: &SyntaxSet,
        theme_set: &ThemeSet,
        notification: Option<(&str, bool)>,
    ) {
        // Update analysis state
        Self::update_analysis_static(detail_view, query);

        // Main layout: header + content + status
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Date range header
                Constraint::Min(0),    // Main content
                Constraint::Length(3), // Status bar
            ])
            .split(area);

        // Date range header
        Self::render_date_range_header_static(f, main_chunks[0], query);

        // Main content: left and right columns
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main_chunks[1]);

        // Left column: query (top) and statistics (bottom)
        Self::render_left_column_static(
            f,
            content_chunks[0],
            query,
            detail_view,
            highlighted_sql_cache,
            syntax_set,
            theme_set,
        );

        // Right column: plan (top) and histogram (bottom)
        Self::render_right_column_static(f, content_chunks[1], query, detail_view);

        // Status bar
        Self::render_status_bar_static(f, main_chunks[2], notification);
    }

    fn update_analysis_static(detail_view: &mut QueryDetailView, query: &ProcessedQuery) {
        // Check if we need to start analysis
        if let AnalysisStatus::Delayed(start_time) = detail_view.analysis_status
            && start_time.elapsed().as_millis() >= 200
        {
            Self::start_analysis_static(detail_view, query);
        }

        // Check for analysis completion
        if let Some(receiver) = &mut detail_view.analysis_receiver {
            match receiver.try_recv() {
                Ok(Ok(result)) => {
                    detail_view.analysis_result = Some(result);
                    detail_view.analysis_status = AnalysisStatus::Completed;
                    detail_view.analysis_receiver = None;
                }
                Ok(Err(error)) => {
                    detail_view.analysis_status = AnalysisStatus::Failed(error);
                    detail_view.analysis_receiver = None;
                }
                Err(_) => {} // Still running
            }
        }
    }

    fn start_analysis_static(detail_view: &mut QueryDetailView, _query: &ProcessedQuery) {
        // Skip analysis for now to avoid complexity - just mark as completed
        detail_view.analysis_status = AnalysisStatus::Completed;
    }

    fn render_date_range_header_static(f: &mut Frame, area: Rect, query: &ProcessedQuery) {
        let stats = &query.statistics;
        let header_text = if stats.min_timestamp.date_naive() == stats.max_timestamp.date_naive() {
            format!(
                "First/Last seen: {} ({} → {})",
                stats.min_timestamp.format("%Y-%m-%d"),
                stats.min_timestamp.format("%H:%M:%S"),
                stats.max_timestamp.format("%H:%M:%S")
            )
        } else {
            format!(
                "First/Last seen: {} → {}",
                stats.min_timestamp.format("%Y-%m-%d %H:%M:%S"),
                stats.max_timestamp.format("%Y-%m-%d %H:%M:%S")
            )
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

    fn render_left_column_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
        highlighted_sql_cache: &mut HashMap<String, Text<'static>>,
        syntax_set: &SyntaxSet,
        theme_set: &ThemeSet,
    ) {
        let constraints = vec![
            Constraint::Fill(3),    // Query text (expanded)
            Constraint::Length(3),  // Tab selector
            Constraint::Length(15), // Statistics/Analysis content
        ];
        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Query text
        Self::render_query_text_static(
            f,
            left_chunks[0],
            query,
            detail_view,
            highlighted_sql_cache,
            syntax_set,
            theme_set,
        );

        // Analysis tabs
        Self::render_analysis_tabs_static(f, left_chunks[1], detail_view);

        // Selected analysis content
        Self::render_selected_analysis_static(f, left_chunks[2], query, detail_view);
    }

    fn render_query_text_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
        highlighted_sql_cache: &mut HashMap<String, Text<'static>>,
        syntax_set: &SyntaxSet,
        theme_set: &ThemeSet,
    ) {
        let highlighted_text = Self::highlight_sql_static(
            &query.representative_plan.formatted_query,
            highlighted_sql_cache,
            syntax_set,
            theme_set,
        );

        let query_text = Paragraph::new(highlighted_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Text")
                    .border_style(Style::default().fg(Color::Blue))
                    .title_style(
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .style(Style::default().bg(Self::get_syntax_background_color_static(theme_set)))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((detail_view.query_scroll, 0));
        f.render_widget(query_text, area);
    }

    fn get_syntax_background_color_static(theme_set: &ThemeSet) -> Color {
        // Get the background color from the syntax highlighting theme
        let theme = &theme_set.themes["base16-ocean.dark"];

        // Convert syntect Color to ratatui Color
        if let Some(bg_color) = theme.settings.background {
            Color::Rgb(bg_color.r, bg_color.g, bg_color.b)
        } else {
            // Fallback to a dark background if theme doesn't specify one
            Color::Rgb(46, 52, 64)
        }
    }

    fn render_analysis_tabs_static(f: &mut Frame, area: Rect, detail_view: &QueryDetailView) {
        let tab_names = [
            ("1", "Stats", AnalysisTab::Statistics),
            ("2", "Complex", AnalysisTab::Complexity),
            ("3", "Meta", AnalysisTab::Metadata),
            ("4", "Regress", AnalysisTab::Regression),
            ("5", "Insights", AnalysisTab::AnalysisInsights),
        ];

        let mut tab_spans = vec![];
        for (i, (key, name, tab)) in tab_names.iter().enumerate() {
            if i > 0 {
                tab_spans.push(Span::raw(" | "));
            }

            let style = if *tab == detail_view.selected_tab {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };

            tab_spans.push(Span::styled(format!("[{}] {}", key, name), style));
        }

        let tabs_widget = Paragraph::new(Line::from(tab_spans))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Analysis Views")
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .alignment(ratatui::layout::Alignment::Center);
        f.render_widget(tabs_widget, area);
    }

    fn render_selected_analysis_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
    ) {
        match detail_view.selected_tab {
            AnalysisTab::Statistics => Self::render_statistics_static(f, area, query),
            AnalysisTab::Complexity => {
                Self::render_complexity_analysis_static(f, area, query, detail_view)
            }
            AnalysisTab::Metadata => {
                Self::render_metadata_analysis_static(f, area, query, detail_view)
            }
            AnalysisTab::Regression => {
                Self::render_regression_analysis_static(f, area, query, detail_view)
            }
            AnalysisTab::AnalysisInsights => {
                Self::render_analysis_insights_tab_static(f, area, query, detail_view)
            }
        }
    }

    fn render_statistics_static(f: &mut Frame, area: Rect, query: &ProcessedQuery) {
        let stats = &query.statistics;

        // Use fixed-width labels and proper alignment
        let stats_lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", "Count:"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>12}", stats.count),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", "Min Duration:"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>10.2}ms", stats.min_duration_ms),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", "Mean Duration:"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>10.2}ms", stats.mean_duration_ms),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", "Max Duration:"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>10.2}ms", stats.max_duration_ms),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", "Std Dev:"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>10.2}ms", stats.std_dev_ms),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(format!("{:<15}", "P25:"), Style::default().fg(Color::White)),
                Span::styled(
                    format!("{:>10.2}ms", stats.percentiles.p25),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("{:<15}", "P50 (Median):"),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>10.2}ms", stats.percentiles.p50),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::styled(format!("{:<15}", "P90:"), Style::default().fg(Color::White)),
                Span::styled(
                    format!("{:>10.2}ms", stats.percentiles.p90),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::styled(format!("{:<15}", "P95:"), Style::default().fg(Color::White)),
                Span::styled(
                    format!("{:>10.2}ms", stats.percentiles.p95),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
            Line::from(vec![
                Span::styled(format!("{:<15}", "P99:"), Style::default().fg(Color::White)),
                Span::styled(
                    format!("{:>10.2}ms", stats.percentiles.p99),
                    Style::default().fg(Color::Cyan),
                ),
            ]),
        ];

        let stats_widget = Paragraph::new(stats_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Performance Statistics")
                .border_style(Style::default().fg(Color::Green))
                .title_style(
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
        );

        f.render_widget(stats_widget, area);
    }

    fn render_right_column_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
    ) {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Fill(1), Constraint::Fill(1)])
            .split(area);

        // Plan details (top half)
        Self::render_ascii_plan_graph_static(f, right_chunks[0], query, detail_view);

        // Histogram (bottom half)
        Self::render_histogram_static(f, right_chunks[1], query);
    }

    fn render_ascii_plan_graph_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &QueryDetailView,
    ) {
        let parsed_plan = &query.representative_plan.parsed();
        let ascii_tree = detail_view.plan_renderer.render_plan(parsed_plan);
        let plan_graph = Paragraph::new(ascii_tree)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Plan Tree (Visual)")
                    .border_style(Style::default().fg(Color::Green))
                    .title_style(
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .style(Style::default().bg(Color::Rgb(46, 52, 64))) // Dark background
            .scroll((detail_view.ascii_plan_scroll, 0));
        f.render_widget(plan_graph, area);
    }

    fn render_status_bar_static(f: &mut Frame, area: Rect, notification: Option<(&str, bool)>) {
        if let Some((message, is_error)) = notification {
            let status = Paragraph::new(Line::from(Span::styled(
                message.to_string(),
                Style::default()
                    .fg(if is_error { Color::Red } else { Color::Green })
                    .add_modifier(Modifier::BOLD),
            )))
            .block(Block::default().borders(Borders::ALL).title("Status"));
            f.render_widget(status, area);
            return;
        }
        let status_line = Self::create_hotkey_line(vec![
            (
                "Navigate",
                Color::Yellow,
                vec![("Esc", "back"), ("Up/Down", "scroll")],
            ),
            (
                "Tabs",
                Color::Cyan,
                vec![
                    ("1", "Stats"),
                    ("2", "Complex"),
                    ("3", "Meta"),
                    ("4", "Regress"),
                    ("5", "Insights"),
                ],
            ),
            (
                "Copy",
                Color::Green,
                vec![("Ctrl+S", "ql"), ("Ctrl+E", "xec")],
            ),
            ("Quit", Color::Red, vec![("q", "")]),
        ]);

        let status = Paragraph::new(vec![status_line])
            .block(Block::default().borders(Borders::ALL).title("Controls"));
        f.render_widget(status, area);
    }

    // Placeholder methods for the analysis tabs
    fn render_complexity_analysis_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
    ) {
        let content = if let Some(complexity) = &query.complexity_score {
            format!(
                "Complexity Score: {:.2}\nClass: {:?}\nScore: {:.2}",
                complexity.total_score, complexity.classification, complexity.total_score
            )
        } else {
            "No complexity analysis available".to_string()
        };

        let widget = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Complexity Analysis")
                    .border_style(Style::default().fg(Color::Blue)),
            )
            .scroll((detail_view.analysis_scroll, 0));
        f.render_widget(widget, area);
    }

    fn render_metadata_analysis_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
    ) {
        let content = if let Some(metadata) = &query.metadata {
            format!(
                "Operation: {:?}\nTables: {}\nFunctions: {}",
                metadata.operation,
                metadata.table_references.len(),
                metadata.function_references.len()
            )
        } else {
            "No metadata analysis available".to_string()
        };

        let widget = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Metadata Analysis")
                    .border_style(Style::default().fg(Color::Green)),
            )
            .scroll((detail_view.analysis_scroll, 0));
        f.render_widget(widget, area);
    }

    fn render_regression_analysis_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
    ) {
        let content = if let Some(regression) = &query.regression_analysis {
            format!(
                "Status: {:?}\nMetric Regressions: {}\nConfidence: {:?}",
                regression.status,
                regression.metric_regressions.len(),
                regression.confidence_level
            )
        } else {
            "No regression analysis available".to_string()
        };

        let widget = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Performance Regression Analysis")
                    .border_style(Style::default().fg(Color::Red)),
            )
            .scroll((detail_view.analysis_scroll, 0));
        f.render_widget(widget, area);
    }

    fn render_analysis_insights_tab_static(
        f: &mut Frame,
        area: Rect,
        query: &ProcessedQuery,
        detail_view: &mut QueryDetailView,
    ) {
        use ratatui::text::{Line, Span};

        let mut lines = Vec::new();

        // Complexity Analysis Insights
        if let Some(complexity) = &query.complexity_score {
            lines.push(Line::from(vec![Span::styled(
                "🧮 Complexity Analysis:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(format!(
                "   Total Score: {:.2}",
                complexity.total_score
            )));
            lines.push(Line::from(format!(
                "   Classification: {:?}",
                complexity.classification
            )));

            lines.push(Line::from(format!(
                "   Tables: {}",
                complexity.breakdown.table_count
            )));
            lines.push(Line::from(format!(
                "   Joins: {}",
                complexity.breakdown.join_info.total_joins
            )));
            lines.push(Line::from(format!(
                "   Functions: {}",
                complexity.breakdown.function_info.total_functions
            )));
            lines.push(Line::from(""));
        }

        // Metadata Insights
        if let Some(metadata) = &query.metadata {
            lines.push(Line::from(vec![Span::styled(
                "🏷️ Query Metadata:",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(format!(
                "   Operation: {:?}",
                metadata.operation
            )));
            lines.push(Line::from(format!(
                "   Tables: {}",
                metadata.table_references.len()
            )));

            if !metadata.table_references.is_empty() {
                let table_list: Vec<String> = metadata
                    .table_references
                    .iter()
                    .map(|tr| tr.table.clone())
                    .collect();
                lines.push(Line::from(format!(
                    "   Table List: {}",
                    table_list.join(", ")
                )));
            }

            lines.push(Line::from(format!(
                "   Classification: {:?}",
                metadata.classification
            )));
            lines.push(Line::from(""));
        }

        // Regression Analysis Insights
        if let Some(regression) = &query.regression_analysis {
            lines.push(Line::from(vec![Span::styled(
                "📉 Performance Analysis:",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(format!("   Status: {:?}", regression.status)));

            lines.push(Line::from(format!(
                "   Trend: {:?}",
                regression.temporal_analysis.trend
            )));

            if !regression.metric_regressions.is_empty() {
                for metric in &regression.metric_regressions {
                    lines.push(Line::from(format!(
                        "   {:?}: {:?}",
                        metric.metric, metric.severity
                    )));
                }
            }

            if !regression.recommendations.is_empty() {
                lines.push(Line::from("   Recommendations:"));
                for rec in regression.recommendations.iter().take(3) {
                    lines.push(Line::from(format!(
                        "   • {:?}: {}",
                        rec.recommendation_type, rec.description
                    )));
                }
            }
            lines.push(Line::from(""));
        }

        // Plan Analysis Engine Results
        if let Some(plan_analysis) = &query.plan_analysis {
            lines.push(Line::from(vec![Span::styled(
                "🔍 Plan Analysis Engine:",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(format!(
                "   Analyzer Results: {}",
                plan_analysis.analyzer_results.len()
            )));
            lines.push(Line::from(format!(
                "   Performance Assessment: {:?}",
                plan_analysis.combined_result.summary.performance_assessment
            )));

            // Show key findings
            let all_findings = plan_analysis.combined_result.all_findings();
            if !all_findings.is_empty() {
                lines.push(Line::from("   Key Findings:"));
                for finding in all_findings.iter().take(3) {
                    lines.push(Line::from(format!(
                        "   • {:?}: {}",
                        finding.finding_type, finding.description
                    )));
                }
            }
            lines.push(Line::from(""));
        }

        // If no analysis available
        if lines.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                "Analysis data is being processed...",
                Style::default().fg(Color::Gray),
            )]));
            lines.push(Line::from(""));
            lines.push(Line::from("This information will be available once"));
            lines.push(Line::from("post-processing is complete."));
        } else {
            lines.push(Line::from(vec![Span::styled(
                "✅ Analysis Complete",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(
                "All insights generated from post-processing phase.",
            ));
        }

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Automated Analysis Insights")
                    .border_style(Style::default().fg(Color::Magenta))
                    .title_style(
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .scroll((detail_view.analysis_scroll, 0));
        f.render_widget(widget, area);
    }

    fn render_histogram_static(f: &mut Frame, area: Rect, query: &ProcessedQuery) {
        let stats = &query.statistics;

        if stats.hourly_histogram.is_empty() {
            // Show a message when there's no histogram data
            let no_data_paragraph = Paragraph::new("No histogram data available")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Execution Frequency")
                        .border_style(Style::default().fg(Color::Gray))
                        .title_style(Style::default().fg(Color::Gray)),
                )
                .style(Style::default().fg(Color::Gray))
                .alignment(ratatui::layout::Alignment::Center);

            f.render_widget(no_data_paragraph, area);
            return;
        }

        // Calculate available space for data points
        let available_width = area.width.saturating_sub(4) as usize; // Account for borders and padding
        let max_points = available_width / 2; // Rough estimate for chart points

        // Sort by actual datetime and prepare data for chart
        let mut sorted_hours: Vec<_> = stats.hourly_histogram.iter().collect();
        sorted_hours.sort_by_key(|(datetime, _)| *datetime);

        if sorted_hours.is_empty() {
            return;
        }

        // Create a complete timeline including hours with zero executions
        let first_hour = sorted_hours[0].0;
        let last_hour = sorted_hours[sorted_hours.len() - 1].0;

        // Generate all hours between first and last
        let mut complete_timeline = Vec::new();
        let mut current_hour = *first_hour;

        while current_hour <= *last_hour {
            let count = stats
                .hourly_histogram
                .get(&current_hour)
                .map(|metrics| metrics.count)
                .unwrap_or(0);
            complete_timeline.push((current_hour, count));

            // Move to next hour
            current_hour += chrono::Duration::hours(1);
        }

        // Determine if we should show date context
        let show_date_context = if complete_timeline.len() > 1 {
            let first_date = complete_timeline[0].0.date_naive();
            let last_date = complete_timeline[complete_timeline.len() - 1]
                .0
                .date_naive();
            first_date != last_date
        } else {
            false
        };

        // Create timeline data with data type classification
        let mut timeline_data = Vec::new();
        let mut chart_labels: Vec<String> = Vec::new();

        if complete_timeline.len() <= max_points {
            // If we have fewer data points than available space, show all
            for (i, (datetime, count)) in complete_timeline.iter().enumerate() {
                let x_pos = if complete_timeline.len() == 1 {
                    // Center single point
                    1.0
                } else {
                    i as f64
                };
                let y_pos = *count as f64;
                let has_data = *count > 0;

                timeline_data.push((x_pos, y_pos, has_data));

                let time_str = if show_date_context {
                    datetime.format("%m/%d %H:%M").to_string()
                } else {
                    datetime.format("%H:%M").to_string()
                };
                chart_labels.push(time_str);
            }
        } else {
            // Group data into buckets - use max_points as the target number of buckets
            let num_buckets = max_points.min(complete_timeline.len()); // Don't exceed available data
            let bucket_size = (complete_timeline.len() as f64 / num_buckets as f64).ceil() as usize;

            for bucket_idx in 0..num_buckets {
                let start_idx = bucket_idx * bucket_size;
                let end_idx = ((bucket_idx + 1) * bucket_size).min(complete_timeline.len());

                if start_idx >= complete_timeline.len() {
                    break;
                }

                // Calculate total count for this bucket (sum, not average)
                let mut total_count = 0;
                let mut bucket_datetimes = Vec::new();

                for item in complete_timeline.iter().take(end_idx).skip(start_idx) {
                    total_count += item.1;
                    bucket_datetimes.push(item.0);
                }

                let x_pos = bucket_idx as f64;
                let y_pos = total_count as f64;
                let has_any_data = total_count > 0;

                timeline_data.push((x_pos, y_pos, has_any_data));

                // Use the middle datetime of the bucket for labeling
                let middle_datetime = bucket_datetimes[bucket_datetimes.len() / 2];
                let time_str = if show_date_context {
                    middle_datetime.format("%m/%d %H:%M").to_string()
                } else {
                    middle_datetime.format("%H:%M").to_string()
                };
                chart_labels.push(time_str);
            }
        }

        // Split timeline into continuous "islands" of data and no-data periods
        let mut data_islands: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut no_data_islands: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut current_data_island: Vec<(f64, f64)> = Vec::new();
        let mut current_no_data_island: Vec<(f64, f64)> = Vec::new();
        let mut last_was_data = None;

        for (x_pos, y_pos, has_data) in timeline_data {
            if has_data && y_pos > 0.0 {
                // We have actual data
                if last_was_data == Some(false) {
                    // Transition from no-data to data - finish no-data island
                    if !current_no_data_island.is_empty() {
                        no_data_islands.push(current_no_data_island.clone());
                        current_no_data_island.clear();
                    }
                }
                current_data_island.push((x_pos, y_pos));
                last_was_data = Some(true);
            } else {
                // We have no data (zero)
                if last_was_data == Some(true) {
                    // Transition from data to no-data - finish data island
                    if !current_data_island.is_empty() {
                        data_islands.push(current_data_island.clone());
                        current_data_island.clear();
                    }
                }
                current_no_data_island.push((x_pos, 0.0));
                last_was_data = Some(false);
            }
        }

        // Don't forget the last island
        if !current_data_island.is_empty() {
            data_islands.push(current_data_island);
        }
        if !current_no_data_island.is_empty() {
            no_data_islands.push(current_no_data_island);
        }

        // Create a descriptive title with time range
        let title = if chart_labels.is_empty() {
            "Execution Timeline".to_string()
        } else if chart_labels.len() == 1 {
            format!("Executions at {}", chart_labels[0])
        } else {
            let start_time = &chart_labels[0];
            let end_time = &chart_labels[chart_labels.len() - 1];
            format!("Execution Timeline: {start_time} to {end_time}")
        };

        // Calculate bounds for the chart
        let all_points: Vec<(f64, f64)> = data_islands
            .iter()
            .flat_map(|island| island.iter())
            .chain(no_data_islands.iter().flat_map(|island| island.iter()))
            .cloned()
            .collect();
        let max_value = all_points.iter().map(|(_, y)| *y).fold(0.0, f64::max);

        // Ensure proper x-axis bounds even for single data points
        let max_x = if chart_labels.is_empty() {
            0.0
        } else if chart_labels.len() == 1 {
            // For single point, give it some space to be visible
            2.0
        } else {
            chart_labels.len() as f64 - 1.0
        };

        // Create X-axis labels (selective) - ensure we have the right count
        let mut x_labels = Vec::new();
        if chart_labels.len() <= 3 {
            // Show all labels if we have few data points
            for label in chart_labels.iter() {
                x_labels.push(label.clone());
            }
        } else {
            // Show only first, middle, and last labels
            let indices = vec![0, chart_labels.len() / 2, chart_labels.len() - 1];
            for &i in &indices {
                x_labels.push(chart_labels[i].clone());
            }
        }

        // Create datasets from islands with proper drawing order: gray (no-data) first, then blue (data) on top
        let mut datasets = Vec::new();

        // Add no-data islands first (background/gray lines)
        for (island_idx, island) in no_data_islands.iter().enumerate() {
            if !island.is_empty() {
                let name = if island_idx == 0 { "No Data" } else { "" }; // Only label the first island
                datasets.push(
                    Dataset::default()
                        .name(name)
                        .marker(ratatui::symbols::Marker::Dot)
                        .graph_type(GraphType::Line)
                        .style(Style::default().fg(Color::DarkGray))
                        .data(island),
                );
            }
        }

        // Add data islands second (foreground/blue lines)
        for (island_idx, island) in data_islands.iter().enumerate() {
            if !island.is_empty() {
                let name = if island_idx == 0 { "Executions" } else { "" }; // Only label the first island
                datasets.push(
                    Dataset::default()
                        .name(name)
                        .marker(ratatui::symbols::Marker::Dot)
                        .graph_type(GraphType::Line)
                        .style(Style::default().fg(Color::Blue))
                        .data(island),
                );
            }
        }

        // Create string references for x-axis labels
        let x_label_refs: Vec<&str> = x_labels.iter().map(|s| s.as_str()).collect();
        let max_value_str = format!("{:.0}", max_value);

        let chart = Chart::new(datasets)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(Style::default().fg(Color::Magenta))
                    .title_style(
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .x_axis(
                Axis::default()
                    .title("Time")
                    .style(Style::default().fg(Color::White))
                    .bounds([0.0, max_x])
                    .labels(x_label_refs),
            )
            .y_axis(
                Axis::default()
                    .title("Count")
                    .style(Style::default().fg(Color::White))
                    .bounds([0.0, max_value * 1.1])
                    .labels(vec!["0", &max_value_str]),
            );

        f.render_widget(chart, area);
    }
}

#[async_trait]
impl AppState for ResultsState {
    fn ui(&mut self, f: &mut Frame, _app: &App) {
        let area = f.area();
        let is_detail_view = matches!(self.view_mode, ViewMode::Detail { .. });

        if is_detail_view {
            let notification = self
                .active_notification()
                .map(|(message, is_error)| (message.to_string(), is_error));
            if let ViewMode::Detail {
                query_fingerprint,
                detail_view,
            } = &mut self.view_mode
                && let Some(query) = self.processed_queries.get(query_fingerprint)
            {
                Self::render_detail_view_static(
                    f,
                    area,
                    query,
                    detail_view,
                    &mut self.highlighted_sql_cache,
                    &self.syntax_set,
                    &self.theme_set,
                    notification
                        .as_ref()
                        .map(|(message, is_error)| (message.as_str(), *is_error)),
                );
            }
        } else {
            self.render_results_screen(f, area);
        }
    }

    async fn process_key(&mut self, key_event: KeyEvent, _app: &mut App) -> StateChange {
        let is_detail_view = matches!(self.view_mode, ViewMode::Detail { .. });

        if is_detail_view {
            // Handle Ctrl+S and Ctrl+E for clipboard operations
            if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                match key_event.code {
                    KeyCode::Char('s') => {
                        if let ViewMode::Detail {
                            query_fingerprint, ..
                        } = &self.view_mode
                            && let Some(query) = self.processed_queries.get(query_fingerprint)
                        {
                            let sql = query.representative_plan.formatted_query.clone();
                            self.copy_with_feedback(sql, "SQL");
                        }
                        return StateChange::Keep;
                    }
                    KeyCode::Char('e') => {
                        if let ViewMode::Detail {
                            query_fingerprint, ..
                        } = &self.view_mode
                            && let Some(query) = self.processed_queries.get(query_fingerprint)
                        {
                            let plan = query.representative_plan.raw_plan().to_string();
                            self.copy_with_feedback(plan, "Execution plan");
                        }
                        return StateChange::Keep;
                    }
                    _ => {}
                }
            }

            match key_event.code {
                KeyCode::Char('q') => StateChange::Exit,
                KeyCode::Esc | KeyCode::Backspace => {
                    // Return to list view
                    self.switch_to_list_view();
                    StateChange::Keep
                }
                // Tab navigation
                KeyCode::Char('1') => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.selected_tab = AnalysisTab::Statistics;
                    }
                    StateChange::Keep
                }
                KeyCode::Char('2') => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.selected_tab = AnalysisTab::Complexity;
                    }
                    StateChange::Keep
                }
                KeyCode::Char('3') => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.selected_tab = AnalysisTab::Metadata;
                    }
                    StateChange::Keep
                }
                KeyCode::Char('4') => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.selected_tab = AnalysisTab::Regression;
                    }
                    StateChange::Keep
                }
                KeyCode::Char('5') => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.selected_tab = AnalysisTab::AnalysisInsights;
                    }
                    StateChange::Keep
                }
                // Scrolling
                KeyCode::Up => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        match detail_view.selected_tab {
                            AnalysisTab::Statistics
                            | AnalysisTab::Complexity
                            | AnalysisTab::Metadata
                            | AnalysisTab::Regression => {
                                detail_view.analysis_scroll =
                                    detail_view.analysis_scroll.saturating_sub(1);
                            }
                            AnalysisTab::AnalysisInsights => {
                                detail_view.analysis_scroll =
                                    detail_view.analysis_scroll.saturating_sub(1);
                            }
                        }
                    }
                    StateChange::Keep
                }
                KeyCode::Down => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        match detail_view.selected_tab {
                            AnalysisTab::Statistics
                            | AnalysisTab::Complexity
                            | AnalysisTab::Metadata
                            | AnalysisTab::Regression => {
                                detail_view.analysis_scroll += 1;
                            }
                            AnalysisTab::AnalysisInsights => {
                                detail_view.analysis_scroll += 1;
                            }
                        }
                    }
                    StateChange::Keep
                }
                KeyCode::PageUp => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.analysis_scroll =
                            detail_view.analysis_scroll.saturating_sub(10);
                    }
                    StateChange::Keep
                }
                KeyCode::PageDown => {
                    if let ViewMode::Detail { detail_view, .. } = &mut self.view_mode {
                        detail_view.analysis_scroll += 10;
                    }
                    StateChange::Keep
                }
                _ => StateChange::Keep,
            }
        } else {
            self.process_list_key(key_event)
        }
    }

    fn is_noninteractive(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use pg_plansight_core::{
        ExecutionRecord, NodeType, ParsedPlan, PerformancePercentiles, PlanCost, PlanNode,
        PlanSource, ProcessedQuery, QueryGroupStatistics, QueryPlan, ScanType, TableReference,
    };

    #[test]
    fn syntect_segment_to_span_maps_style_and_gates_alpha() {
        use syntect::highlighting::{Color as SynColor, FontStyle, Style as SynStyle};

        // Opaque foreground, transparent background (alpha 0), bold + italic.
        let style = SynStyle {
            foreground: SynColor {
                r: 200,
                g: 100,
                b: 50,
                a: 255,
            },
            background: SynColor {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            font_style: FontStyle::BOLD | FontStyle::ITALIC,
        };
        let span = syntect_segment_to_span(&style, "SELECT");
        assert_eq!(span.content.as_ref(), "SELECT");
        assert_eq!(span.style.fg, Some(Color::Rgb(200, 100, 50)));
        // A transparent (alpha 0) background must stay unset — not become an
        // opaque black block, which is the regression this test guards against.
        assert_eq!(span.style.bg, None);
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));

        // An opaque background IS applied.
        let with_bg = SynStyle {
            foreground: SynColor {
                r: 1,
                g: 2,
                b: 3,
                a: 255,
            },
            background: SynColor {
                r: 10,
                g: 20,
                b: 30,
                a: 255,
            },
            font_style: FontStyle::empty(),
        };
        let span2 = syntect_segment_to_span(&with_bg, "x");
        assert_eq!(span2.style.bg, Some(Color::Rgb(10, 20, 30)));
        assert!(span2.style.add_modifier.is_empty());
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    fn fixed_ts(seconds: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(seconds, 0).single().unwrap()
    }

    fn make_plan_node() -> PlanNode {
        PlanNode::new(
            NodeType::Scan(ScanType::SeqScan {
                table: TableReference {
                    schema: None,
                    name: "test_table".to_string(),
                    alias: None,
                },
            }),
            PlanCost {
                startup_cost: 0.0,
                min_total_cost: 1.0,
                max_total_cost: 10.0,
                estimated_rows: 100,
                estimated_width: 8,
            },
            "Seq Scan on test_table".to_string(),
        )
    }

    fn make_query_plan(query_text: &str, duration_ms: f64, ts: DateTime<Utc>) -> QueryPlan {
        QueryPlan {
            timestamp: ts,
            duration_ms,
            query_text: query_text.to_string(),
            normalized_query: query_text.to_string(),
            formatted_query: query_text.to_string(),
            source: PlanSource::Text {
                raw_text: "Seq Scan on test_table (cost=0.00..1.00 rows=1 width=8)".to_string(),
                plan_lines: vec![],
            },
            parsed: ParsedPlan::new(make_plan_node()),
        }
    }

    fn make_processed_query(
        query_text: &str,
        count: usize,
        mean: f64,
        min: f64,
        max: f64,
        std_dev: f64,
        ts: DateTime<Utc>,
    ) -> ProcessedQuery {
        let executions: Vec<ExecutionRecord> = (0..count)
            .map(|_| ExecutionRecord {
                timestamp: ts,
                duration_ms: mean,
            })
            .collect();
        let statistics = QueryGroupStatistics {
            count,
            total_duration_ms: mean * count as f64,
            min_duration_ms: min,
            max_duration_ms: max,
            mean_duration_ms: mean,
            std_dev_ms: std_dev,
            min_timestamp: ts,
            max_timestamp: ts,
            percentiles: PerformancePercentiles {
                p25: 0.0,
                p50: 0.0,
                p90: 0.0,
                p95: 0.0,
                p99: 0.0,
            },
            hourly_histogram: std::collections::HashMap::new(),
            executions,
        };

        ProcessedQuery {
            representative_plan: make_query_plan(query_text, max, ts),
            statistics,
            complexity_score: None,
            metadata: None,
            regression_analysis: None,
            plan_analysis: None,
            execution_indices: vec![],
        }
    }

    /// Build a ResultsState with 3 queries having distinct statistics.
    ///
    /// - fingerprint_a: count=10, mean=100, min=50,  max=200, stddev=20
    /// - fingerprint_b: count=5,  mean=300, min=100, max=500, stddev=80
    /// - fingerprint_c: count=20, mean=50,  min=10,  max=90,  stddev=5
    fn build_three_query_state() -> ResultsState {
        let ts = fixed_ts(1_700_000_000);
        let mut queries: HashMap<String, ProcessedQuery> = HashMap::new();
        queries.insert(
            "fingerprint_a".to_string(),
            make_processed_query(
                "SELECT a FROM t WHERE id = ?",
                10,
                100.0,
                50.0,
                200.0,
                20.0,
                ts,
            ),
        );
        queries.insert(
            "fingerprint_b".to_string(),
            make_processed_query(
                "SELECT b FROM t WHERE id = ?",
                5,
                300.0,
                100.0,
                500.0,
                80.0,
                ts,
            ),
        );
        queries.insert(
            "fingerprint_c".to_string(),
            make_processed_query(
                "SELECT c FROM t WHERE id = ?",
                20,
                50.0,
                10.0,
                90.0,
                5.0,
                ts,
            ),
        );
        ResultsState::from_imported_data(queries, Some(ts), Some(ts))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // ─── Sort Logic ───────────────────────────────────────────────────────────

    #[test]
    fn test_sort_count_descending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Count;
        state.sort_state.ascending = false;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // count: c=20, a=10, b=5
        assert_eq!(fps[0], "fingerprint_c");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_b");
    }

    #[test]
    fn test_sort_count_ascending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Count;
        state.sort_state.ascending = true;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // count: b=5, a=10, c=20
        assert_eq!(fps[0], "fingerprint_b");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_c");
    }

    #[test]
    fn test_sort_mean_descending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Mean;
        state.sort_state.ascending = false;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // mean: b=300, a=100, c=50
        assert_eq!(fps[0], "fingerprint_b");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_c");
    }

    #[test]
    fn test_sort_mean_ascending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Mean;
        state.sort_state.ascending = true;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // mean: c=50, a=100, b=300
        assert_eq!(fps[0], "fingerprint_c");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_b");
    }

    #[test]
    fn test_sort_min_descending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Min;
        state.sort_state.ascending = false;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // min: b=100, a=50, c=10
        assert_eq!(fps[0], "fingerprint_b");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_c");
    }

    #[test]
    fn test_sort_min_ascending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Min;
        state.sort_state.ascending = true;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // min: c=10, a=50, b=100
        assert_eq!(fps[0], "fingerprint_c");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_b");
    }

    #[test]
    fn test_sort_max_descending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Max;
        state.sort_state.ascending = false;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // max: b=500, a=200, c=90
        assert_eq!(fps[0], "fingerprint_b");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_c");
    }

    #[test]
    fn test_sort_max_ascending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::Max;
        state.sort_state.ascending = true;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // max: c=90, a=200, b=500
        assert_eq!(fps[0], "fingerprint_c");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_b");
    }

    #[test]
    fn test_sort_stddev_descending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::StdDev;
        state.sort_state.ascending = false;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // stddev: b=80, a=20, c=5
        assert_eq!(fps[0], "fingerprint_b");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_c");
    }

    #[test]
    fn test_sort_stddev_ascending() {
        let mut state = build_three_query_state();
        state.sort_state.order = SortOrder::StdDev;
        state.sort_state.ascending = true;
        state.sort_processed_queries();
        let fps = &state.sorted_query_fingerprints;
        // stddev: c=5, a=20, b=80
        assert_eq!(fps[0], "fingerprint_c");
        assert_eq!(fps[1], "fingerprint_a");
        assert_eq!(fps[2], "fingerprint_b");
    }

    // ─── Sort Toggle ──────────────────────────────────────────────────────────

    #[test]
    fn test_sort_toggle_same_key_flips_direction() {
        let mut state = build_three_query_state();
        // Default is Count/descending after from_imported_data
        assert_eq!(state.sort_state.order, SortOrder::Count);
        assert!(!state.sort_state.ascending);

        // Press 'c' → same order → flip to ascending
        let result = state.process_list_key(key(KeyCode::Char('c')));
        assert!(matches!(result, StateChange::Keep));
        assert!(state.sort_state.ascending);

        // Press 'c' again → flip back
        let _ = state.process_list_key(key(KeyCode::Char('c')));
        assert!(!state.sort_state.ascending);
    }

    #[test]
    fn test_sort_toggle_different_key_resets_to_descending() {
        let mut state = build_three_query_state();
        // Force Count to ascending
        let _ = state.process_list_key(key(KeyCode::Char('c')));
        assert!(state.sort_state.ascending);

        // Press 'm' → different key → Mean/descending
        let _ = state.process_list_key(key(KeyCode::Char('m')));
        assert_eq!(state.sort_state.order, SortOrder::Mean);
        assert!(!state.sort_state.ascending);
    }

    #[test]
    fn test_all_sort_keys_set_correct_order() {
        let mut state = build_three_query_state();

        let _ = state.process_list_key(key(KeyCode::Char('n')));
        assert_eq!(state.sort_state.order, SortOrder::Min);

        let _ = state.process_list_key(key(KeyCode::Char('x')));
        assert_eq!(state.sort_state.order, SortOrder::Max);

        let _ = state.process_list_key(key(KeyCode::Char('s')));
        assert_eq!(state.sort_state.order, SortOrder::StdDev);

        let _ = state.process_list_key(key(KeyCode::Char('c')));
        assert_eq!(state.sort_state.order, SortOrder::Count);

        let _ = state.process_list_key(key(KeyCode::Char('m')));
        assert_eq!(state.sort_state.order, SortOrder::Mean);
    }

    // ─── Key Dispatch ─────────────────────────────────────────────────────────

    #[test]
    fn test_q_key_exits() {
        let mut state = build_three_query_state();
        let result = state.process_list_key(key(KeyCode::Char('q')));
        assert!(matches!(result, StateChange::Exit));
    }

    #[test]
    fn test_tab_cycles_panes() {
        let mut state = build_three_query_state();
        assert!(matches!(state.focused_pane, FocusedPane::QueryList));

        let _ = state.process_list_key(key(KeyCode::Tab));
        assert!(matches!(state.focused_pane, FocusedPane::QueryDetails));

        let _ = state.process_list_key(key(KeyCode::Tab));
        assert!(matches!(state.focused_pane, FocusedPane::ExecutionPlan));

        let _ = state.process_list_key(key(KeyCode::Tab));
        assert!(matches!(state.focused_pane, FocusedPane::QueryList));
    }

    #[test]
    fn test_up_at_zero_stays_zero() {
        let mut state = build_three_query_state();
        assert_eq!(state.selected_query_index, 0);
        let _ = state.process_list_key(key(KeyCode::Up));
        assert_eq!(state.selected_query_index, 0);
    }

    #[test]
    fn test_down_navigates_and_clamps_at_last() {
        let mut state = build_three_query_state();
        let _ = state.process_list_key(key(KeyCode::Down));
        assert_eq!(state.selected_query_index, 1);

        let _ = state.process_list_key(key(KeyCode::Down));
        assert_eq!(state.selected_query_index, 2);

        // Already at last (2 of 3) — should not move further
        let _ = state.process_list_key(key(KeyCode::Down));
        assert_eq!(state.selected_query_index, 2);
    }

    #[test]
    fn test_up_navigates_from_nonzero() {
        let mut state = build_three_query_state();
        state.selected_query_index = 2;
        let _ = state.process_list_key(key(KeyCode::Up));
        assert_eq!(state.selected_query_index, 1);
        let _ = state.process_list_key(key(KeyCode::Up));
        assert_eq!(state.selected_query_index, 0);
    }

    #[test]
    fn test_unknown_key_returns_keep() {
        let mut state = build_three_query_state();
        let result = state.process_list_key(key(KeyCode::Char('z')));
        assert!(matches!(result, StateChange::Keep));
    }

    // ─── get_current_sql / get_current_execution_plan ─────────────────────────

    #[test]
    fn test_get_current_sql_empty_state_returns_none() {
        let state = ResultsState::from_imported_data(HashMap::new(), None, None);
        assert!(state.get_current_sql().is_none());
    }

    #[test]
    fn test_get_current_execution_plan_empty_state_returns_none() {
        let state = ResultsState::from_imported_data(HashMap::new(), None, None);
        assert!(state.get_current_execution_plan().is_none());
    }

    #[test]
    fn test_get_current_sql_returns_value_when_queries_present() {
        let state = build_three_query_state();
        assert!(state.get_current_sql().is_some());
    }

    #[test]
    fn test_get_current_execution_plan_returns_value_when_queries_present() {
        let state = build_three_query_state();
        assert!(state.get_current_execution_plan().is_some());
    }

    #[test]
    fn test_get_current_sql_changes_after_navigation() {
        let mut state = build_three_query_state();
        let sql_at_0 = state.get_current_sql().unwrap().to_string();
        let _ = state.process_list_key(key(KeyCode::Down));
        let sql_at_1 = state.get_current_sql().unwrap().to_string();
        assert_ne!(sql_at_0, sql_at_1);
    }
}
