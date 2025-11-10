use std::collections::HashMap;
use std::time::Instant;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use tokio::sync::oneshot;

use crate::plan_renderer::PlanRenderer;
use pg_loganalyze_core::analysis::{
    analyzers::{CostAnalyzer, JoinAnalyzer, MemoryAnalyzer, RowEstimationAnalyzer, ScanAnalyzer},
    consolidated_config::AnalysisConfiguration,
    engine::{AnalysisEngine, AnalysisEngineBuilder, EngineResult},
};

#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisStatus {
    NotStarted,
    Delayed(Instant), // Waiting for delay period
    #[allow(dead_code)]
    Running, // Analysis in progress
    Completed,        // Analysis finished
    Failed(String),   // Analysis failed with error
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnalysisTab {
    Statistics,
    Complexity,
    Metadata,
    Regression,
    AnalysisInsights,
}

/// View-specific state for query detail display
/// This struct only contains UI state, not the actual query data
pub struct QueryDetailView {
    // UI state
    pub query_scroll: u16,
    #[allow(dead_code)]
    pub plan_scroll: u16,
    #[allow(dead_code)]
    pub plan_horizontal_scroll: u16,
    pub ascii_plan_scroll: u16,
    pub analysis_scroll: u16,
    pub selected_tab: AnalysisTab,

    // Analysis state
    pub analysis_status: AnalysisStatus,
    pub analysis_result: Option<EngineResult>,
    pub analysis_receiver: Option<oneshot::Receiver<Result<EngineResult, String>>>,
    pub analysis_delay_timer: Option<Instant>,

    // Cached rendering data
    #[allow(dead_code)]
    pub highlighted_sql_cache: HashMap<String, ratatui::text::Text<'static>>,

    // Rendering helpers (shared with parent)
    #[allow(dead_code)]
    pub syntax_set: SyntaxSet,
    #[allow(dead_code)]
    pub theme_set: ThemeSet,
    #[allow(dead_code)]
    pub analysis_engine: AnalysisEngine,
    #[allow(dead_code)]
    pub analysis_config: AnalysisConfiguration,
    pub plan_renderer: PlanRenderer,
}

impl QueryDetailView {
    pub fn new() -> Self {
        // Build analysis engine with enhanced unified configuration
        let analysis_config = AnalysisConfiguration::default();
        let analysis_engine = AnalysisEngineBuilder::new()
            .add_analyzer(RowEstimationAnalyzer::new())
            .add_analyzer(ScanAnalyzer::new())
            .add_analyzer(JoinAnalyzer::new())
            .add_analyzer(CostAnalyzer::new())
            .add_analyzer(MemoryAnalyzer::new())
            .build();

        Self {
            query_scroll: 0,
            plan_scroll: 0,
            plan_horizontal_scroll: 0,
            ascii_plan_scroll: 0,
            analysis_scroll: 0,
            selected_tab: AnalysisTab::Statistics,
            analysis_status: AnalysisStatus::NotStarted,
            analysis_result: None,
            analysis_receiver: None,
            analysis_delay_timer: None,
            highlighted_sql_cache: HashMap::new(),
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
            analysis_engine,
            analysis_config,
            plan_renderer: PlanRenderer::new(),
        }
    }

    #[allow(dead_code)]
    pub fn reset_scroll_positions(&mut self) {
        self.query_scroll = 0;
        self.plan_scroll = 0;
        self.plan_horizontal_scroll = 0;
        self.ascii_plan_scroll = 0;
        self.analysis_scroll = 0;
    }

    pub fn start_analysis_delay(&mut self) {
        self.analysis_delay_timer = Some(Instant::now());
        self.analysis_status = AnalysisStatus::Delayed(Instant::now());
    }
}
