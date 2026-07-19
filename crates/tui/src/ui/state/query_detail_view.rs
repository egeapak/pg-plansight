use std::collections::HashMap;
use std::time::Instant;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use tokio::sync::oneshot;

use crate::plan_renderer::PlanRenderer;
use pg_plansight_core::analysis::{
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
        // The canonical analyzer set: hand-assembled lists here had already
        // diverged from the list view (this one was missing BufferWalAnalyzer),
        // so the two views reported different findings for the same query.
        let analysis_config = AnalysisConfiguration::default();
        let analysis_engine = AnalysisEngineBuilder::new()
            .with_default_analyzers()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_status_is_not_started() {
        let view = QueryDetailView::new();
        assert_eq!(view.analysis_status, AnalysisStatus::NotStarted);
    }

    #[test]
    fn test_initial_scroll_positions_are_zero() {
        let view = QueryDetailView::new();
        assert_eq!(view.query_scroll, 0);
        assert_eq!(view.ascii_plan_scroll, 0);
        assert_eq!(view.analysis_scroll, 0);
    }

    #[test]
    fn test_initial_analysis_result_is_none() {
        let view = QueryDetailView::new();
        assert!(view.analysis_result.is_none());
    }

    #[test]
    fn test_initial_tab_is_statistics() {
        let view = QueryDetailView::new();
        assert_eq!(view.selected_tab, AnalysisTab::Statistics);
    }

    #[test]
    fn test_start_analysis_delay_transitions_to_delayed() {
        let mut view = QueryDetailView::new();
        assert_eq!(view.analysis_status, AnalysisStatus::NotStarted);
        view.start_analysis_delay();
        assert!(
            matches!(view.analysis_status, AnalysisStatus::Delayed(_)),
            "Expected Delayed status after start_analysis_delay()"
        );
    }

    #[test]
    fn test_start_analysis_delay_sets_timer() {
        let mut view = QueryDetailView::new();
        assert!(view.analysis_delay_timer.is_none());
        view.start_analysis_delay();
        assert!(view.analysis_delay_timer.is_some());
    }

    #[test]
    fn test_reset_scroll_positions_clears_all_scrolls() {
        let mut view = QueryDetailView::new();
        view.query_scroll = 10;
        view.ascii_plan_scroll = 5;
        view.analysis_scroll = 7;
        view.reset_scroll_positions();
        assert_eq!(view.query_scroll, 0);
        assert_eq!(view.ascii_plan_scroll, 0);
        assert_eq!(view.analysis_scroll, 0);
    }
}
