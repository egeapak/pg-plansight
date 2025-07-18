mod app;
mod state {
    pub mod log_parsing_state;
    pub mod results_state;
    pub mod query_detail_state;
}

pub use app::App;
