use arboard::Clipboard;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph},
};
use std::collections::HashMap;
use std::time::Instant;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect_tui::into_span;
use tokio::sync::oneshot;

use crate::plan_renderer::PlanRenderer;
use crate::{Renderable, FindingRenderer};
use crate::ui::app::{App, AppState, StateChange};
use crate::ui::state::results_state::ResultsState;
use pg_loganalyze_core::{
    ProcessedQuery, QueryPlan,
    analysis::{
        AnalysisContext,
        analyzers::{
            CostAnalyzer, JoinAnalyzer, MemoryAnalyzer, RowEstimationAnalyzer, ScanAnalyzer,
        },
        engine::{AnalysisEngine, AnalysisEngineBuilder, EngineResult},
        consolidated_config::AnalysisConfiguration,
    },
    sql_analysis::{
        ComplexityClass, RegressionStatus, RegressionSeverity,
        metadata::ImpactLevel,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisStatus {
    NotStarted,
    Delayed(Instant), // Waiting for delay period
    Running,          // Analysis in progress
    Completed,        // Analysis finished
    Failed(String),   // Analysis failed with error
}

pub struct QueryDetailState {
    query: ProcessedQuery,
    query_fingerprint: String,
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
    query_scroll: u16,
    plan_scroll: u16,
    plan_horizontal_scroll: u16,
    ascii_plan_scroll: u16,
    highlighted_sql_cache: HashMap<String, Text<'static>>,
    parsed_queries: Vec<QueryPlan>,
    date_range_start: Option<DateTime<Utc>>,
    date_range_end: Option<DateTime<Utc>>,
    plan_renderer: PlanRenderer,
    // Analysis-related fields
    analysis_status: AnalysisStatus,
    analysis_result: Option<EngineResult>,
    analysis_engine: AnalysisEngine,
    analysis_config: AnalysisConfiguration,
    analysis_receiver: Option<oneshot::Receiver<Result<EngineResult, String>>>,
    analysis_delay_timer: Option<Instant>,
    analysis_scroll: u16,
    // New Phase 2 display state
    selected_tab: AnalysisTab,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisTab {
    Statistics,
    Complexity,
    Metadata,
    Regression,
    AnalysisInsights,
}

impl QueryDetailState {
    pub fn new(
        query: ProcessedQuery,
        query_fingerprint: String,
        parsed_queries: Vec<QueryPlan>,
        date_range_start: Option<DateTime<Utc>>,
        date_range_end: Option<DateTime<Utc>>,
    ) -> Self {
        // Build analysis engine with enhanced unified configuration
        // Use development-sensitive configuration to detect more issues in TUI
        let analysis_config = AnalysisConfiguration::default();

        let analysis_engine = AnalysisEngineBuilder::new()
            .add_analyzer(RowEstimationAnalyzer::new())
            .add_analyzer(ScanAnalyzer::new())
            .add_analyzer(JoinAnalyzer::new())
            .add_analyzer(CostAnalyzer::new())
            .add_analyzer(MemoryAnalyzer::new())
            .build();

        let mut state = Self {
            query,
            query_fingerprint,
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            ascii_plan_scroll: 0,
            highlighted_sql_cache: HashMap::new(),
            parsed_queries,
            date_range_start,
            date_range_end,
            plan_renderer: PlanRenderer::new(),
            // Initialize analysis fields
            analysis_status: AnalysisStatus::NotStarted,
            analysis_result: None,
            analysis_engine,
            analysis_config,
            analysis_receiver: None,
            analysis_delay_timer: None,
            analysis_scroll: 0,
            selected_tab: AnalysisTab::Statistics,
        };

        // Start analysis delay if we have a parsed plan
        // Always have parsed plan with new architecture
        {
            state.start_analysis_delay();
        }

        state
    }

    fn start_analysis_delay(&mut self) {
        self.analysis_delay_timer = Some(Instant::now());
        self.analysis_status = AnalysisStatus::Delayed(Instant::now());
    }

    fn check_analysis_delay(&mut self) {
        if let AnalysisStatus::Delayed(start_time) = self.analysis_status {
            if start_time.elapsed() >= std::time::Duration::from_millis(500) {
                self.launch_analysis();
            }
        }
    }

    fn launch_analysis(&mut self) {
        // Always have parsed plan with new architecture
        {
            let plan = self.query.representative_plan.parsed().clone();
            let (sender, receiver) = oneshot::channel();

            // Create a new engine with enhanced configured analyzers
            // Use the unified configuration system for consistent thresholds
            let engine = AnalysisEngineBuilder::new()
                .add_analyzer(RowEstimationAnalyzer::new())
                .add_analyzer(ScanAnalyzer::new())
                .add_analyzer(JoinAnalyzer::new())
                .add_analyzer(CostAnalyzer::new())
                .add_analyzer(MemoryAnalyzer::new())
                .build();

            // Note: Analyzers will use the enhanced config through the analysis context

            let context = AnalysisContext::new()
                .with_query_duration(self.query.statistics.mean_duration_ms)
                .with_work_mem_kb(4096) // Default work_mem
                .with_parallel_workers(2); // Default parallel workers

            // Spawn the analysis task
            tokio::spawn(async move {
                let result = engine.analyze(&plan, &context);

                let _ = sender.send(Ok(result)); // Ignore send errors if receiver is dropped
            });

            self.analysis_receiver = Some(receiver);
            self.analysis_status = AnalysisStatus::Running;
        }
    }

    fn check_analysis_completion(&mut self) {
        if let Some(receiver) = &mut self.analysis_receiver {
            match receiver.try_recv() {
                Ok(Ok(result)) => {
                    self.analysis_result = Some(result);
                    self.analysis_status = AnalysisStatus::Completed;
                    self.analysis_receiver = None; // Clean up
                }
                Ok(Err(e)) => {
                    self.analysis_status = AnalysisStatus::Failed(e);
                    self.analysis_receiver = None; // Clean up
                }
                Err(oneshot::error::TryRecvError::Empty) => {
                    // Analysis still running, nothing to do
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.analysis_status =
                        AnalysisStatus::Failed("Analysis task was cancelled".to_string());
                    self.analysis_receiver = None; // Clean up
                }
            }
        }
    }

    pub fn update_analysis(&mut self) {
        match &self.analysis_status {
            AnalysisStatus::Delayed(_) => {
                self.check_analysis_delay();
            }
            AnalysisStatus::Running => {
                self.check_analysis_completion();
            }
            _ => {}
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
        let constraints = vec![
            Constraint::Fill(3),    // Query text (expanded)
            Constraint::Length(3),  // Tab selector
            Constraint::Length(15), // Statistics/Analysis content
        ];

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        // Top left: Query text
        self.render_query_text(f, left_chunks[0]);

        // Middle left: (removed dedicated analysis insights panel)

        // Tab selector
        self.render_analysis_tabs(f, left_chunks[1]);

        // Bottom left: Selected analysis content
        self.render_selected_analysis(f, left_chunks[2]);
    }

    fn render_right_column(&mut self, f: &mut Frame, area: Rect) {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(3), // Plan visualization
                Constraint::Fill(1), // Histogram
            ])
            .split(area);

        // Top right: Plan Tree (Visual)
        self.render_ascii_plan_graph(f, right_chunks[0]);

        // Bottom right: Histogram
        self.render_histogram(f, right_chunks[1]);
    }

    fn render_query_text(&mut self, f: &mut Frame, area: Rect) {
        let formatted_query = self.query.representative_plan.formatted_query.to_string();
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

        // Use fixed-width labels and proper alignment
        let stats_lines = vec![
            Line::from(format!("{:<15} {:>12}", "Count:", stats.count)),
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

    fn render_analysis_tabs(&self, f: &mut Frame, area: Rect) {
        let tab_names = vec![
            ("1", "Stats", AnalysisTab::Statistics),
            ("2", "Complex", AnalysisTab::Complexity),
            ("3", "Meta", AnalysisTab::Metadata),
            ("4", "Regress", AnalysisTab::Regression),
            ("5", "Insights", AnalysisTab::AnalysisInsights),
        ];

        let tab_spans: Vec<Span> = tab_names
            .iter()
            .map(|(key, name, tab)| {
                let style = if *tab == self.selected_tab {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                        .bg(Color::DarkGray)
                } else {
                    Style::default().fg(Color::Gray)
                };
                Span::styled(format!(" [{}]{} ", key, name), style)
            })
            .collect();

        let tabs_widget = Paragraph::new(Line::from(tab_spans))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Analysis Views")
                    .border_style(Style::default().fg(Color::Cyan))
            )
            .alignment(ratatui::layout::Alignment::Center);

        f.render_widget(tabs_widget, area);
    }

    fn render_selected_analysis(&mut self, f: &mut Frame, area: Rect) {
        match self.selected_tab {
            AnalysisTab::Statistics => self.render_statistics(f, area),
            AnalysisTab::Complexity => self.render_complexity_analysis(f, area),
            AnalysisTab::Metadata => self.render_metadata_analysis(f, area),
            AnalysisTab::Regression => self.render_regression_analysis(f, area),
            AnalysisTab::AnalysisInsights => self.render_analysis_insights_tab(f, area),
        }
    }

    fn render_complexity_analysis(&self, f: &mut Frame, area: Rect) {
        let content = if let Some(complexity) = &self.query.complexity_score {
            let class_color = match complexity.classification {
                ComplexityClass::Simple => Color::Green,
                ComplexityClass::Moderate => Color::Yellow,
                ComplexityClass::Complex => Color::Red,
                ComplexityClass::VeryComplex => Color::Magenta,
            };

            vec![
                Line::from(vec![
                    Span::styled("Overall Score: ", Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:.1}/100", complexity.total_score),
                        Style::default().fg(class_color).add_modifier(Modifier::BOLD)
                    ),
                ]),
                Line::from(vec![
                    Span::styled("Classification: ", Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:?}", complexity.classification),
                        Style::default().fg(class_color).add_modifier(Modifier::BOLD)
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled("Component Breakdown:", Style::default().fg(Color::Cyan))),
                Line::from(format!(
                    "  Joins:      {:.1}/25",
                    complexity.components.join_complexity
                )),
                Line::from(format!(
                    "  Subqueries: {:.1}/20",
                    complexity.components.subquery_complexity
                )),
                Line::from(format!(
                    "  Functions:  {:.1}/15",
                    complexity.components.function_complexity
                )),
                Line::from(format!(
                    "  Conditions: {:.1}/15",
                    complexity.components.condition_complexity
                )),
                Line::from(format!(
                    "  Aggregation:{:.1}/10",
                    complexity.components.aggregation_complexity
                )),
                Line::from(format!(
                    "  Windows:    {:.1}/10",
                    complexity.components.window_complexity
                )),
                Line::from(""),
                Line::from(format!("Tables: {}", complexity.breakdown.table_count)),
                Line::from(format!("Total Joins: {}", complexity.breakdown.join_info.total_joins)),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    "No complexity analysis available",
                    Style::default().fg(Color::Gray),
                )),
                Line::from("Query may have failed to parse for AST analysis."),
            ]
        };

        let widget = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Complexity Analysis")
                    .border_style(Style::default().fg(Color::Blue))
                    .title_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
            )
            .scroll((self.analysis_scroll, 0));

        f.render_widget(widget, area);
    }

    fn render_metadata_analysis(&self, f: &mut Frame, area: Rect) {
        let content = if let Some(metadata) = &self.query.metadata {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("Operation: ", Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:?}", metadata.operation),
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                    ),
                ]),
                Line::from(vec![
                    Span::styled("Workload: ", Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:?}", metadata.classification.workload_type),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    ),
                ]),
                Line::from(""),
            ];

            // Tables
            if !metadata.table_references.is_empty() {
                lines.push(Line::from(Span::styled("Tables:", Style::default().fg(Color::Cyan))));
                for table in &metadata.table_references {
                    let table_display = if let Some(schema) = &table.schema {
                        format!("{}.{}", schema, table.table)
                    } else {
                        table.table.clone()
                    };
                    lines.push(Line::from(format!(
                        "  {} ({:?})",
                        table_display, table.access_type
                    )));
                }
                lines.push(Line::from(""));
            }

            // Functions
            if !metadata.function_references.is_empty() {
                lines.push(Line::from(Span::styled("Functions:", Style::default().fg(Color::Cyan))));
                for func in &metadata.function_references {
                    lines.push(Line::from(format!(
                        "  {} ({:?})",
                        func.name, func.category
                    )));
                }
                lines.push(Line::from(""));
            }

            // Performance Hints
            if !metadata.performance_hints.is_empty() {
                lines.push(Line::from(Span::styled("Performance Hints:", Style::default().fg(Color::Magenta))));
                for hint in &metadata.performance_hints {
                    let color = match hint.impact {
                        ImpactLevel::High => Color::Red,
                        ImpactLevel::Medium => Color::Yellow,
                        ImpactLevel::Low => Color::Green,
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  • ", Style::default().fg(color)),
                        Span::styled(&hint.description, Style::default().fg(Color::White)),
                    ]));
                }
            }

            lines
        } else {
            vec![
                Line::from(Span::styled(
                    "No metadata analysis available", 
                    Style::default().fg(Color::Gray),
                )),
                Line::from("Query may have failed to parse for metadata extraction."),
            ]
        };

        let widget = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Query Metadata Analysis")
                    .border_style(Style::default().fg(Color::Green))
                    .title_style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            )
            .scroll((self.analysis_scroll, 0));

        f.render_widget(widget, area);
    }

    fn render_regression_analysis(&self, f: &mut Frame, area: Rect) {
        let content = if let Some(regression) = &self.query.regression_analysis {
            let status_color = match regression.status {
                RegressionStatus::None => Color::Green,
                RegressionStatus::Minor => Color::Yellow,
                RegressionStatus::Significant => Color::Red,
                RegressionStatus::Critical => Color::Magenta,
                RegressionStatus::InsufficientData => Color::Gray,
            };

            let mut lines = vec![
                Line::from(vec![
                    Span::styled("Status: ", Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:?}", regression.status),
                        Style::default().fg(status_color).add_modifier(Modifier::BOLD)
                    ),
                ]),
                Line::from(""),
            ];

            // Metric regressions
            if !regression.metric_regressions.is_empty() {
                lines.push(Line::from(Span::styled("Detected Regressions:", Style::default().fg(Color::Cyan))));
                for metric_reg in &regression.metric_regressions {
                    let severity_color = match metric_reg.severity {
                        RegressionSeverity::Low => Color::Yellow,
                        RegressionSeverity::Medium => Color::Red,
                        RegressionSeverity::High => Color::Magenta,
                        RegressionSeverity::Critical => Color::Magenta,
                    };
                    lines.push(Line::from(vec![
                        Span::styled("  • ", Style::default().fg(severity_color)),
                        Span::styled(
                            format!("{:?}: ", metric_reg.metric),
                            Style::default().fg(Color::White)
                        ),
                        Span::styled(
                            format!("{:.1}% change ({:?})", metric_reg.percentage_change, metric_reg.severity),
                            Style::default().fg(severity_color)
                        ),
                    ]));
                }
                lines.push(Line::from(""));
            }

            // Recommendations
            if !regression.recommendations.is_empty() {
                lines.push(Line::from(Span::styled("Recommendations:", Style::default().fg(Color::Magenta))));
                for rec in &regression.recommendations {
                    lines.push(Line::from(vec![
                        Span::styled("  ✓ ", Style::default().fg(Color::Green)),
                        Span::styled(&rec.description, Style::default().fg(Color::White)),
                    ]));
                }
            }

            lines
        } else {
            vec![
                Line::from(Span::styled(
                    "No regression analysis available",
                    Style::default().fg(Color::Gray),
                )),
                Line::from("Need at least 10 executions for regression detection."),
            ]
        };

        let widget = Paragraph::new(content)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Performance Regression Analysis")
                    .border_style(Style::default().fg(Color::Red))
                    .title_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            )
            .scroll((self.analysis_scroll, 0));

        f.render_widget(widget, area);
    }

    fn render_analysis_insights_tab(&mut self, f: &mut Frame, area: Rect) {
        let title = match &self.analysis_status {
            AnalysisStatus::NotStarted => "Automated Analysis Insights",
            AnalysisStatus::Delayed(_) => "Analysis Insights - Starting...",
            AnalysisStatus::Running => "Analysis Insights - Running...",
            AnalysisStatus::Completed => "Automated Analysis Insights",
            AnalysisStatus::Failed(_) => "Analysis Insights - Failed",
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Magenta))
            .title_style(Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD));

        // Check if we have analysis results
        if let Some(result) = &self.analysis_result {
            self.render_analysis_results_content(f, area, result, block);
        } else {
            // Show status-specific content
            let content = match &self.analysis_status {
                AnalysisStatus::NotStarted => vec![
                    Line::from(Span::styled(
                        "⏳ Analysis will start automatically",
                        Style::default().fg(Color::Gray),
                    )),
                    Line::from(""),
                    Line::from("This tab shows automated insights from the analysis engine:"),
                    Line::from("• Row estimation accuracy"),
                    Line::from("• Scan efficiency analysis"),
                    Line::from("• Join optimization opportunities"),
                    Line::from("• Cost estimation validation"),
                    Line::from("• Memory usage patterns"),
                ],
                AnalysisStatus::Delayed(start_time) => {
                    let elapsed = start_time.elapsed().as_millis();
                    let remaining = 500_u128.saturating_sub(elapsed);
                    vec![
                        Line::from(Span::styled(
                            "⏳ Analyzing query plan...",
                            Style::default().fg(Color::Yellow),
                        )),
                        Line::from(Span::styled(
                            format!("   Starting in {:.1}s", remaining as f64 / 1000.0),
                            Style::default().fg(Color::Gray),
                        )),
                    ]
                }
                AnalysisStatus::Running => vec![
                    Line::from(Span::styled(
                        "🔄 Analysis in progress...",
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        "   RowEstimation ✓",
                        Style::default().fg(Color::Green),
                    )),
                    Line::from(Span::styled(
                        "   ScanAnalysis ⏳",
                        Style::default().fg(Color::Yellow),
                    )),
                    Line::from(Span::styled(
                        "   JoinAnalysis ⏳",
                        Style::default().fg(Color::Gray),
                    )),
                ],
                AnalysisStatus::Completed => vec![
                    Line::from(Span::styled(
                        "✅ Analysis completed",
                        Style::default().fg(Color::Green),
                    )),
                    Line::from(Span::styled(
                        "   No results available",
                        Style::default().fg(Color::Gray),
                    )),
                    Line::from(""),
                    Line::from(Span::styled("Press 'r' to re-run analysis", Style::default().fg(Color::Green))),
                ],
                AnalysisStatus::Failed(error) => vec![
                    Line::from(Span::styled(
                        "❌ Analysis failed",
                        Style::default().fg(Color::Red),
                    )),
                    Line::from(""),
                    Line::from(Span::styled(
                        error,
                        Style::default()
                            .fg(Color::Red)
                            .add_modifier(Modifier::ITALIC),
                    )),
                    Line::from(""),
                    Line::from(Span::styled("Press 'r' to retry analysis", Style::default().fg(Color::Green))),
                ],
            };

            let paragraph = Paragraph::new(content)
                .block(block)
                .scroll((self.analysis_scroll, 0));
            f.render_widget(paragraph, area);
        }
    }

    fn render_ascii_plan_graph(&self, f: &mut Frame, area: Rect) {
        let parsed_plan = &self.query.representative_plan.parsed();
        let ascii_tree = self.plan_renderer.render_plan(parsed_plan);
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
            .style(Style::default().bg(self.get_syntax_background_color()))
            .scroll((self.ascii_plan_scroll, 0));

        f.render_widget(plan_graph, area);
    }


    fn render_histogram(&self, f: &mut Frame, area: Rect) {
        let stats = &self.query.statistics;

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

                for i in start_idx..end_idx {
                    total_count += complete_timeline[i].1;
                    bucket_datetimes.push(complete_timeline[i].0);
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
                        .style(Style::default().fg(Color::Cyan))
                        .data(island),
                );
            }
        }

        // Create y-axis labels
        let max_value_str = format!("{}", max_value as u64);
        let x_label_refs: Vec<&str> = x_labels.iter().map(|s| s.as_str()).collect();

        // Create the chart
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


    fn render_analysis_results_content(
        &self,
        f: &mut Frame,
        area: Rect,
        result: &EngineResult,
        block: Block,
    ) {
        let summary = &result.combined_result.summary;

        let mut content = vec![
            Line::from(vec![
                Span::styled("📊 Assessment: ", Style::default().fg(Color::White)),
                Span::styled(
                    summary.performance_assessment.render(),
                    Style::default()
                        .fg(self.get_assessment_color(&summary.performance_assessment))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Total Issues: ", Style::default().fg(Color::White)),
                Span::styled(
                    format!("{}", summary.total_findings),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
        ];

        // Show all findings by severity
        let all_findings = result.combined_result.all_findings();

        if all_findings.is_empty() {
            content.push(Line::from(Span::styled(
                "✅ No performance issues detected!",
                Style::default().fg(Color::Green),
            )));

            // Show some basic metrics even when no issues
            self.add_basic_metrics(&mut content, result);
        } else {
            // Show critical issues first
            let critical_findings = result
                .combined_result
                .findings_by_severity(&pg_loganalyze_core::analysis::Severity::Critical);
            if !critical_findings.is_empty() {
                content.push(Line::from(Span::styled(
                    format!("🚨 Critical Issues ({})", critical_findings.len()),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )));

                for finding in critical_findings.iter() {
                    content.push(Line::from(Span::styled(
                        format!("  • {}", finding.render()),
                        Style::default().fg(Color::Red),
                    )));

                    // Show key evidence
                    if let Some(evidence) = finding.render_key_evidence() {
                        content.push(Line::from(Span::styled(
                            format!("    {evidence}"),
                            Style::default()
                                .fg(Color::Gray)
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }

            // Show high priority issues
            let high_findings = result
                .combined_result
                .findings_by_severity(&pg_loganalyze_core::analysis::Severity::High);
            if !high_findings.is_empty() {
                if !critical_findings.is_empty() {
                    content.push(Line::from(""));
                }
                content.push(Line::from(Span::styled(
                    format!("⚠️  High Priority ({})", high_findings.len()),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));

                for finding in high_findings.iter() {
                    content.push(Line::from(Span::styled(
                        format!("  • {}", finding.render()),
                        Style::default().fg(Color::Yellow),
                    )));

                    // Show key evidence
                    if let Some(evidence) = finding.render_key_evidence() {
                        content.push(Line::from(Span::styled(
                            format!("    {evidence}"),
                            Style::default()
                                .fg(Color::Gray)
                                .add_modifier(Modifier::ITALIC),
                        )));
                    }
                }
            }

            // Show medium/low findings summary
            let medium_findings = result
                .combined_result
                .findings_by_severity(&pg_loganalyze_core::analysis::Severity::Medium);
            let low_findings = result
                .combined_result
                .findings_by_severity(&pg_loganalyze_core::analysis::Severity::Low);

            if !medium_findings.is_empty() || !low_findings.is_empty() {
                content.push(Line::from(""));
                if !medium_findings.is_empty() {
                    content.push(Line::from(Span::styled(
                        format!("🟡 Medium: {} issues", medium_findings.len()),
                        Style::default().fg(Color::Blue),
                    )));
                }
                if !low_findings.is_empty() {
                    content.push(Line::from(Span::styled(
                        format!("🟢 Low: {} issues", low_findings.len()),
                        Style::default().fg(Color::Green),
                    )));
                }
            }
        }

        // Show scroll hint if there are findings
        if summary.total_findings > 0 {
            content.push(Line::from(""));
            content.push(Line::from(Span::styled(
                "Press 'j'/'k' to scroll • 'r' to re-run analysis",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::ITALIC),
            )));
        }

        let paragraph = Paragraph::new(content)
            .block(block)
            .scroll((self.analysis_scroll, 0))
            .wrap(ratatui::widgets::Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn get_analysis_color(&self) -> Color {
        match &self.analysis_status {
            AnalysisStatus::NotStarted => Color::Gray,
            AnalysisStatus::Delayed(_) => Color::Yellow,
            AnalysisStatus::Running => Color::Yellow,
            AnalysisStatus::Completed => {
                if let Some(result) = &self.analysis_result {
                    self.get_assessment_color(
                        &result.combined_result.summary.performance_assessment,
                    )
                } else {
                    Color::Green
                }
            }
            AnalysisStatus::Failed(_) => Color::Red,
        }
    }

    fn get_assessment_color(
        &self,
        assessment: &pg_loganalyze_core::analysis::PerformanceAssessment,
    ) -> Color {
        use pg_loganalyze_core::analysis::PerformanceAssessment;
        match assessment {
            PerformanceAssessment::Excellent => Color::Green,
            PerformanceAssessment::Good => Color::Cyan,
            PerformanceAssessment::Fair => Color::Yellow,
            PerformanceAssessment::Poor => Color::Red,
            PerformanceAssessment::Critical => Color::Magenta,
        }
    }

    fn truncate_text(&self, text: &str, max_len: usize) -> String {
        if text.len() <= max_len {
            text.to_string()
        } else {
            format!("{}...", &text[..max_len.saturating_sub(3)])
        }
    }




    fn add_basic_metrics(&self, content: &mut Vec<Line>, result: &EngineResult) {
        content.push(Line::from(""));
        content.push(Line::from(Span::styled(
            "📊 Basic Metrics:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));

        // Show analyzer execution summary
        let exec_summary = result.execution_summary();
        content.push(Line::from(Span::styled(
            format!(
                "  • Analyzers run: {}/{}",
                exec_summary.successful_count, exec_summary.total_analyzers
            ),
            Style::default().fg(Color::White),
        )));

        content.push(Line::from(Span::styled(
            format!(
                "  • Analysis time: {:.1}ms",
                result.total_duration.as_millis()
            ),
            Style::default().fg(Color::White),
        )));

        // Show some aggregate metrics if available
        for report in &result.combined_result.reports {
            if !report.metrics.is_empty() {
                content.push(Line::from(Span::styled(
                    format!(
                        "  • {}: {} metrics",
                        report.analyzer_name,
                        report.metrics.len()
                    ),
                    Style::default().fg(Color::Gray),
                )));
                break; // Just show one example to keep it brief
            }
        }

        if result.combined_result.summary.total_findings == 0 {
            content.push(Line::from(""));
            content.push(Line::from(Span::styled(
                "🎉 Your query looks well-optimized!",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
    }

    fn render_status_bar(&self, f: &mut Frame, area: Rect) {
        let status_lines = vec![
            Line::from(vec![
                Span::styled(
                    "Navigate: ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("Up/Down", Style::default().fg(Color::Cyan)),
                Span::styled(" (query) | ", Style::default().fg(Color::Gray)),
                Span::styled("Shift+Up/Down", Style::default().fg(Color::Green)),
                Span::styled(" (plan) | ", Style::default().fg(Color::Gray)),
                Span::styled("j/k", Style::default().fg(Color::Blue)),
                Span::styled(" (analysis) | ", Style::default().fg(Color::Gray)),
                Span::styled("Left/Right", Style::default().fg(Color::Yellow)),
                Span::styled(" (horizontal)", Style::default().fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::styled(
                    "Tabs: ",
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("1-5", Style::default().fg(Color::Yellow)),
                Span::styled(" (switch) | ", Style::default().fg(Color::Gray)),
                Span::styled("r", Style::default().fg(Color::Blue)),
                Span::styled(" (re-run) | ", Style::default().fg(Color::Gray)),
                Span::styled("Copy: Ctrl+S", Style::default().fg(Color::Magenta)),
                Span::styled("(sql) ", Style::default().fg(Color::Gray)),
                Span::styled("Ctrl+E", Style::default().fg(Color::Magenta)),
                Span::styled("(plan) | ", Style::default().fg(Color::Gray)),
                Span::styled("Back: Esc", Style::default().fg(Color::Red)),
                Span::styled(" | ", Style::default().fg(Color::Gray)),
                Span::styled("Quit: q", Style::default().fg(Color::Red)),
            ]),
        ];

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
                .map_err(|e| format!("Failed to copy to clipboard: {e}")),
            Err(e) => Err(format!("Failed to access clipboard: {e}")),
        }
    }
}

#[async_trait]
impl AppState for QueryDetailState {
    fn ui(&mut self, f: &mut Frame, _app: &App) {
        // Update analysis state
        self.update_analysis();

        let area = f.area();
        self.render_detail_page(f, area);
    }

    async fn process_key(&mut self, key_event: KeyEvent, _app: &mut App) -> StateChange {
        // Handle Ctrl+S and Ctrl+E for clipboard operations
        if key_event.modifiers.contains(KeyModifiers::CONTROL) {
            match key_event.code {
                KeyCode::Char('s') => {
                    let _ = self.copy_to_clipboard(&self.query.representative_plan.formatted_query);
                    return StateChange::Keep;
                }
                KeyCode::Char('e') => {
                    let _ = self.copy_to_clipboard(self.query.representative_plan.raw_plan());
                    return StateChange::Keep;
                }
                _ => {}
            }
        }

        match key_event.code {
            KeyCode::Char('q') => StateChange::Exit,
            KeyCode::Char('j') => {
                // Scroll analysis panel down
                self.analysis_scroll += 3;
                StateChange::Keep
            }
            KeyCode::Char('k') => {
                // Scroll analysis panel up
                self.analysis_scroll = self.analysis_scroll.saturating_sub(3);
                StateChange::Keep
            }
            KeyCode::Char('r') => {
                // Re-run analysis
                if matches!(
                    self.analysis_status,
                    AnalysisStatus::Completed | AnalysisStatus::Failed(_)
                ) {
                    self.analysis_result = None;
                    self.analysis_scroll = 0;
                    self.start_analysis_delay();
                }
                StateChange::Keep
            }
            KeyCode::Char('1') => {
                self.selected_tab = AnalysisTab::Statistics;
                StateChange::Keep
            }
            KeyCode::Char('2') => {
                self.selected_tab = AnalysisTab::Complexity;
                StateChange::Keep
            }
            KeyCode::Char('3') => {
                self.selected_tab = AnalysisTab::Metadata;
                StateChange::Keep
            }
            KeyCode::Char('4') => {
                self.selected_tab = AnalysisTab::Regression;
                StateChange::Keep
            }
            KeyCode::Char('5') => {
                self.selected_tab = AnalysisTab::AnalysisInsights;
                StateChange::Keep
            }
            KeyCode::Esc => {
                // Cancel any running analysis before returning
                if let Some(_receiver) = self.analysis_receiver.take() {
                    // Just drop the receiver, the task will complete but we won't read the result
                }

                // Return to results view
                let results_state = ResultsState::new(
                    self.parsed_queries.clone(),
                    self.date_range_start,
                    self.date_range_end,
                );
                StateChange::Change(Box::new(results_state))
            }
            KeyCode::Up => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Up: Scroll ASCII plan graph
                    self.ascii_plan_scroll = self.ascii_plan_scroll.saturating_sub(1);
                } else {
                    // Up: Scroll query text
                    self.query_scroll = self.query_scroll.saturating_sub(1);
                }
                StateChange::Keep
            }
            KeyCode::Down => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Down: Scroll ASCII plan graph
                    self.ascii_plan_scroll += 1;
                } else {
                    // Down: Scroll query text
                    self.query_scroll += 1;
                }
                StateChange::Keep
            }
            KeyCode::PageUp => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+PageUp: Scroll ASCII plan graph
                    self.ascii_plan_scroll = self.ascii_plan_scroll.saturating_sub(5);
                } else {
                    // PageUp: Scroll query text
                    self.query_scroll = self.query_scroll.saturating_sub(5);
                }
                StateChange::Keep
            }
            KeyCode::PageDown => {
                if key_event.modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+PageDown: Scroll ASCII plan graph
                    self.ascii_plan_scroll += 5;
                } else {
                    // PageDown: Scroll query text
                    self.query_scroll += 5;
                }
                StateChange::Keep
            }
            KeyCode::Left => {
                // Left: Scroll raw plan text horizontally
                self.plan_horizontal_scroll = self.plan_horizontal_scroll.saturating_sub(1);
                StateChange::Keep
            }
            KeyCode::Right => {
                // Right: Scroll raw plan text horizontally
                self.plan_horizontal_scroll += 1;
                StateChange::Keep
            }
            KeyCode::Null => StateChange::Keep,
            _ => StateChange::Keep,
        }
    }

    fn is_noninteractive(&self) -> bool {
        // Return true during analysis to ensure UI updates
        matches!(
            self.analysis_status,
            AnalysisStatus::Delayed(_) | AnalysisStatus::Running
        )
    }
}
