use arboard::Clipboard;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Chart, Axis, Dataset, GraphType},
};
use std::collections::HashMap;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect_tui::into_span;

use crate::models::{ProcessedQuery, QueryPlan};
use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::results_state::ResultsState;

pub struct QueryDetailState {
    query: ProcessedQuery,
    query_hash: u64,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    query_scroll: u16,
    plan_scroll: u16,
    plan_horizontal_scroll: u16,
    highlighted_sql_cache: HashMap<String, Text<'static>>,
    parsed_queries: Vec<QueryPlan>,
    date_range_start: Option<DateTime<Utc>>,
    date_range_end: Option<DateTime<Utc>>,
}

impl QueryDetailState {
    pub fn new(
        query: ProcessedQuery,
        query_hash: u64,
        parsed_queries: Vec<QueryPlan>,
        date_range_start: Option<DateTime<Utc>>,
        date_range_end: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            query,
            query_hash,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            highlighted_sql_cache: HashMap::new(),
            parsed_queries,
            date_range_start,
            date_range_end,
        }
    }

    fn render_detail_page(&mut self, f: &mut Frame, area: Rect) {
        // Main layout: header + content
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Date range header
                Constraint::Min(0),    // Main content
                Constraint::Length(3), // Status bar
            ])
            .split(area);

        // Date range header
        self.render_date_range_header(f, main_chunks[0]);

        // Main content: left and right columns
        let content_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main_chunks[1]);

        // Left column: query (top) and statistics (bottom)
        self.render_left_column(f, content_chunks[0]);

        // Right column: plan (top) and histogram (bottom)
        self.render_right_column(f, content_chunks[1]);

        // Status bar
        self.render_status_bar(f, main_chunks[2]);
    }

    fn render_date_range_header(&self, f: &mut Frame, area: Rect) {
        let stats = &self.query.statistics;
        let header_text = if stats.min_timestamp.date_naive() == stats.max_timestamp.date_naive() {
            format!("Query Date: {}", stats.min_timestamp.format("%Y-%m-%d"))
        } else {
            format!(
                "Query Date Range: {} to {}",
                stats.min_timestamp.format("%Y-%m-%d"),
                stats.max_timestamp.format("%Y-%m-%d")
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

    fn render_left_column(&mut self, f: &mut Frame, area: Rect) {
        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Fill(2), Constraint::Length(5)])
            .split(area);

        // Top left: Query text
        self.render_query_text(f, left_chunks[0]);

        // Bottom left: Statistics
        self.render_statistics(f, left_chunks[1]);
    }

    fn render_right_column(&mut self, f: &mut Frame, area: Rect) {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Fill(2), Constraint::Fill(1)])
            .split(area);

        // Top right: Query plan
        self.render_query_plan(f, right_chunks[0]);

        // Bottom right: Histogram
        self.render_histogram(f, right_chunks[1]);
    }

    fn render_query_text(&mut self, f: &mut Frame, area: Rect) {
        let formatted_query = self.query.formatted_query.clone();
        let highlighted_text = self.highlight_sql(&formatted_query);
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
            .style(Style::default().bg(self.get_syntax_background_color()))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .scroll((self.query_scroll, 0));

        f.render_widget(query_text, area);
    }

    fn render_statistics(&self, f: &mut Frame, area: Rect) {
        let stats = &self.query.statistics;
        let stats_lines = vec![
            Line::from(format!(
                "Min: {:.2}ms  Mean: {:.2}ms  Max: {:.2}ms  StdDev: {:.2}ms",
                stats.min_duration_ms,
                stats.mean_duration_ms,
                stats.max_duration_ms,
                stats.std_dev_ms
            )),
            Line::from(format!(
                "P90: {:.2}ms  P95: {:.2}ms  P99: {:.2}ms",
                stats.percentiles.p90, stats.percentiles.p95, stats.percentiles.p99
            )),
        ];

        let stats_widget = Paragraph::new(stats_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Statistics")
                .border_style(Style::default().fg(Color::Green))
                .title_style(
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
        );

        f.render_widget(stats_widget, area);
    }

    fn render_query_plan(&self, f: &mut Frame, area: Rect) {
        let plan_paragraph = Paragraph::new(self.query.plan.clone())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Plan")
                    .border_style(Style::default().fg(Color::Yellow))
                    .title_style(
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
            )
            .style(Style::default().bg(self.get_syntax_background_color()))
            .scroll((self.plan_scroll, self.plan_horizontal_scroll));

        f.render_widget(plan_paragraph, area);
    }

    fn render_histogram(&self, f: &mut Frame, area: Rect) {
        let stats = &self.query.statistics;

        if stats.hourly_histogram.is_empty() {
            return;
        }

        // Calculate available space for data points
        let available_width = area.width.saturating_sub(4) as usize; // Account for borders and padding
        let max_points = available_width / 2; // Rough estimate for chart points

        // Sort by actual datetime and prepare data for chart
        let mut sorted_hours: Vec<_> = stats.hourly_histogram.iter().collect();
        sorted_hours.sort_by(|(datetime_a, _), (datetime_b, _)| datetime_a.cmp(datetime_b));

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
            let count = stats.hourly_histogram.get(&current_hour)
                .map(|metrics| metrics.count)
                .unwrap_or(0);
            complete_timeline.push((current_hour, count));
            
            // Move to next hour
            current_hour = current_hour + chrono::Duration::hours(1);
        }

        // Determine if we should show date context
        let show_date_context = if complete_timeline.len() > 1 {
            let first_date = complete_timeline[0].0.date_naive();
            let last_date = complete_timeline[complete_timeline.len() - 1].0.date_naive();
            first_date != last_date
        } else {
            false
        };

        // Group data into buckets based on available space
        let mut chart_data: Vec<(f64, f64)> = Vec::new();
        let mut chart_labels: Vec<String> = Vec::new();

        if complete_timeline.len() <= max_points {
            // If we have fewer data points than available space, show all
            for (i, (datetime, count)) in complete_timeline.iter().enumerate() {
                chart_data.push((i as f64, *count as f64));

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

                for i in start_idx..end_idx {
                    total_count += complete_timeline[i].1;
                    bucket_datetimes.push(complete_timeline[i].0);
                }

                chart_data.push((bucket_idx as f64, total_count as f64));

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

        // Create a descriptive title with time range
        let title = if chart_labels.is_empty() {
            "Execution Timeline".to_string()
        } else if chart_labels.len() == 1 {
            format!("Executions at {}", chart_labels[0])
        } else {
            let start_time = &chart_labels[0];
            let end_time = &chart_labels[chart_labels.len() - 1];
            format!("Execution Timeline: {} to {}", start_time, end_time)
        };

        // Calculate bounds for the chart
        let max_value = chart_data.iter().map(|(_, y)| *y).fold(0.0, f64::max);
        let max_x = if chart_data.is_empty() { 0.0 } else { chart_data.len() as f64 - 1.0 };

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

        // Create the dataset
        let dataset = Dataset::default()
            .name("Executions")
            .marker(ratatui::symbols::Marker::Dot)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&chart_data);

        // Create y-axis labels
        let max_value_str = format!("{}", max_value as u64);
        let x_label_refs: Vec<&str> = x_labels.iter().map(|s| s.as_str()).collect();
        
        // Create the chart
        let chart = Chart::new(vec![dataset])
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

    fn render_status_bar(&self, f: &mut Frame, area: Rect) {
        let status_lines = vec![Line::from(Span::styled(
            "Navigate: Up/Down (scroll) | Left/Right (horizontal scroll) | Copy: Ctrl+S(ql) Ctrl+E(xec) | Back: Esc | Quit: q",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ))];

        let status = Paragraph::new(status_lines)
            .block(Block::default().borders(Borders::ALL).title("Controls"));
        f.render_widget(status, area);
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
            if line.trim().is_empty() {
                lines.push(Line::from(""));
                continue;
            }

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
                    lines.push(Line::from(line.to_string()));
                }
            }
        }

        let text = Text::from(lines);

        // Cache the result
        if self.highlighted_sql_cache.len() < 100 {
            self.highlighted_sql_cache
                .insert(sql.to_string(), text.clone());
        }

        text
    }

    fn get_syntax_background_color(&self) -> Color {
        let theme = &self.theme_set.themes["base16-ocean.dark"];
        if let Some(bg_color) = theme.settings.background {
            Color::Rgb(bg_color.r, bg_color.g, bg_color.b)
        } else {
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
}

#[async_trait]
impl AppState for QueryDetailState {
    fn ui(&mut self, f: &mut Frame, _app: &App) {
        let area = f.area();
        self.render_detail_page(f, area);
    }

    async fn process_key(&mut self, key_event: KeyEvent, _app: &mut App) -> StateChange {
        // Handle Ctrl+S and Ctrl+E for clipboard operations
        if key_event.modifiers.contains(KeyModifiers::CONTROL) {
            match key_event.code {
                KeyCode::Char('s') => {
                    let _ = self.copy_to_clipboard(&self.query.formatted_query);
                    return StateChange::Keep;
                }
                KeyCode::Char('e') => {
                    let _ = self.copy_to_clipboard(&self.query.plan);
                    return StateChange::Keep;
                }
                _ => {}
            }
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Esc => {
                // Return to results view
                let results_state = ResultsState::new(
                    self.parsed_queries.clone(),
                    self.date_range_start,
                    self.date_range_end,
                );
                StateChange::Change(Box::new(results_state))
            }
            KeyCode::Up => {
                self.query_scroll = self.query_scroll.saturating_sub(1);
                StateChange::Keep
            }
            KeyCode::Down => {
                self.query_scroll += 1;
                StateChange::Keep
            }
            KeyCode::PageUp => {
                self.query_scroll = self.query_scroll.saturating_sub(5);
                StateChange::Keep
            }
            KeyCode::PageDown => {
                self.query_scroll += 5;
                StateChange::Keep
            }
            KeyCode::Left => {
                self.plan_horizontal_scroll = self.plan_horizontal_scroll.saturating_sub(1);
                StateChange::Keep
            }
            KeyCode::Right => {
                self.plan_horizontal_scroll += 1;
                StateChange::Keep
            }
            KeyCode::Null => StateChange::Keep,
            _ => StateChange::Keep,
        }
    }

    fn is_noninteractive(&self) -> bool {
        false
    }
}
