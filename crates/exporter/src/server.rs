#[cfg(feature = "prometheus")]
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
#[cfg(feature = "prometheus")]
use prometheus::{Encoder, Registry, TextEncoder};
#[cfg(feature = "prometheus")]
use std::sync::Arc;
#[cfg(feature = "prometheus")]
use tower_http::cors::CorsLayer;

#[cfg(feature = "prometheus")]
pub async fn start_metrics_server(
    bind_address: String,
    metrics_path: String,
    registry: Arc<Registry>,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route(&metrics_path, get(metrics_handler))
        .route("/health", get(health_handler))
        .layer(CorsLayer::permissive())
        .with_state(registry);

    tracing::info!("Starting metrics server on {}", bind_address);

    let listener = tokio::net::TcpListener::bind(&bind_address).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(feature = "prometheus")]
async fn metrics_handler(State(registry): State<Arc<Registry>>) -> Response {
    let encoder = TextEncoder::new();
    let metric_families = registry.gather();

    match encoder.encode_to_string(&metric_families) {
        Ok(output) => (
            StatusCode::OK,
            [("content-type", encoder.format_type())],
            output,
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to encode metrics: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to encode metrics".to_string(),
            )
                .into_response()
        }
    }
}

#[cfg(feature = "prometheus")]
async fn health_handler() -> Response {
    (StatusCode::OK, "OK").into_response()
}

#[cfg(not(feature = "prometheus"))]
pub async fn start_metrics_server(
    _bind_address: String,
    _metrics_path: String,
    _registry: std::sync::Arc<()>,
) -> anyhow::Result<()> {
    anyhow::bail!("Prometheus feature not enabled");
}
