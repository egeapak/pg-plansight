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
use std::time::Duration;
#[cfg(feature = "prometheus")]
use tower_http::limit::RequestBodyLimitLayer;
#[cfg(feature = "prometheus")]
use tower_http::timeout::TimeoutLayer;

/// Maximum time a single request handler may run before the server aborts it.
#[cfg(feature = "prometheus")]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum accepted request body. The metrics/health endpoints are GET-only and
/// take no body, so this is deliberately tiny — it just stops a client from
/// streaming an unbounded body at us.
#[cfg(feature = "prometheus")]
const MAX_BODY_BYTES: usize = 64 * 1024;

#[cfg(feature = "prometheus")]
pub async fn start_metrics_server(
    bind_address: String,
    metrics_path: String,
    registry: Arc<Registry>,
) -> anyhow::Result<()> {
    // No CORS layer: the metrics endpoint is scraped server-to-server (e.g. by
    // Prometheus), never from a browser, so a permissive CORS policy would only
    // widen the attack surface. Add timeout + body-size limits to bound the
    // resources a single client can consume.
    let app = Router::new()
        .route(&metrics_path, get(metrics_handler))
        .route("/health", get(health_handler))
        .layer(TimeoutLayer::new(REQUEST_TIMEOUT))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .with_state(registry);

    tracing::info!("Starting metrics server on {}", bind_address);
    if bind_address.starts_with("0.0.0.0") || bind_address.starts_with("[::]") {
        tracing::warn!(
            "Metrics server is bound to all interfaces ({}); it has no authentication. \
             Restrict it to a trusted network or place it behind a TLS reverse proxy.",
            bind_address
        );
    }

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

#[cfg(all(test, feature = "prometheus"))]
mod tests {
    use super::*;
    use axum::{extract::State, http::StatusCode, response::IntoResponse};
    use prometheus::Registry;

    #[tokio::test]
    async fn test_health_handler_returns_200_ok() {
        let response = health_handler().await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"OK");
    }

    #[tokio::test]
    async fn test_metrics_handler_returns_200_with_prometheus_format() {
        use prometheus::{Counter, Opts};

        let registry = Arc::new(Registry::new());

        let counter_opts = Opts::new("test_requests_total", "A test counter");
        let counter = Counter::with_opts(counter_opts).unwrap();
        registry.register(Box::new(counter.clone())).unwrap();
        counter.inc();

        let response = metrics_handler(State(Arc::clone(&registry))).await;
        let response = response.into_response();

        assert_eq!(response.status(), StatusCode::OK);

        let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = std::str::from_utf8(&body_bytes).unwrap();

        assert!(
            body_str.contains("# HELP test_requests_total"),
            "body should contain HELP comment: {}",
            body_str
        );
        assert!(
            body_str.contains("# TYPE test_requests_total counter"),
            "body should contain TYPE comment: {}",
            body_str
        );
        assert!(
            body_str.contains("test_requests_total 1"),
            "counter value should be 1: {}",
            body_str
        );
    }

    #[tokio::test]
    async fn test_metrics_handler_empty_registry_returns_200() {
        let registry = Arc::new(Registry::new());
        let response = metrics_handler(State(registry)).await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
