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

/// Liveness/readiness signals, written by the collector and read by the HTTP
/// handlers.
///
/// Plain atomics so a scrape never contends with the collection path.
#[derive(Debug)]
pub struct HealthState {
    /// Unix seconds of the last collection cycle that completed with no file
    /// errors. 0 = none yet.
    last_success_unix: std::sync::atomic::AtomicI64,
    /// Process start, so readiness has a grace period before the first cycle.
    started_unix: i64,
    /// A last success older than this makes `/ready` return 503.
    max_staleness_secs: i64,
}

impl HealthState {
    pub fn new(max_staleness_secs: u64) -> Self {
        Self {
            last_success_unix: std::sync::atomic::AtomicI64::new(0),
            started_unix: chrono::Utc::now().timestamp(),
            max_staleness_secs: max_staleness_secs as i64,
        }
    }

    /// Called after a collection cycle that had no per-file errors.
    pub fn mark_success(&self) {
        self.last_success_unix.store(
            chrono::Utc::now().timestamp(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    /// True while the exporter is actually keeping up.
    ///
    /// Before the first successful cycle this is a grace period rather than an
    /// immediate failure, so a slow first scrape does not flap a rollout.
    pub fn is_ready(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        let last = self
            .last_success_unix
            .load(std::sync::atomic::Ordering::Relaxed);
        let reference = if last == 0 { self.started_unix } else { last };
        now - reference < self.max_staleness_secs
    }
}

/// Shared state for the HTTP handlers.
///
/// `registry` is `None` when no Prometheus backend is configured — the health
/// and readiness endpoints are still served, which they were not before: the
/// whole listener only spawned when "prometheus" was in `metrics.backends`, so
/// an OpenTelemetry-only deployment had no HTTP surface at all and nothing for
/// a probe to hit.
#[cfg(feature = "prometheus")]
#[derive(Clone)]
pub struct AppState {
    registry: Option<Arc<Registry>>,
    health: Arc<HealthState>,
}

#[cfg(feature = "prometheus")]
pub async fn start_metrics_server(
    bind_address: String,
    metrics_path: String,
    registry: Option<Arc<Registry>>,
    health: Arc<HealthState>,
) -> anyhow::Result<()> {
    let listener = bind_metrics_listener(&bind_address).await?;
    serve_metrics(listener, metrics_path, registry, health).await
}

/// Bind the listener up front so a port conflict surfaces to the caller
/// synchronously instead of inside a spawned task.
#[cfg(feature = "prometheus")]
pub async fn bind_metrics_listener(bind_address: &str) -> anyhow::Result<tokio::net::TcpListener> {
    tracing::info!("Starting metrics server on {}", bind_address);
    if bind_address.starts_with("0.0.0.0") || bind_address.starts_with("[::]") {
        tracing::warn!(
            "Metrics server is bound to all interfaces ({}); it has no authentication. \
             Restrict it to a trusted network or place it behind a TLS reverse proxy.",
            bind_address
        );
    }
    Ok(tokio::net::TcpListener::bind(bind_address).await?)
}

#[cfg(feature = "prometheus")]
pub async fn serve_metrics(
    listener: tokio::net::TcpListener,
    metrics_path: String,
    registry: Option<Arc<Registry>>,
    health: Arc<HealthState>,
) -> anyhow::Result<()> {
    // No CORS layer: the metrics endpoint is scraped server-to-server (e.g. by
    // Prometheus), never from a browser, so a permissive CORS policy would only
    // widen the attack surface. Add timeout + body-size limits to bound the
    // resources a single client can consume.
    let has_registry = registry.is_some();
    let mut app = Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler));

    if has_registry {
        app = app.route(&metrics_path, get(metrics_handler));
    } else {
        tracing::info!("No Prometheus backend configured; serving /health and /ready only");
    }

    let app = app
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .with_state(AppState { registry, health });

    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(feature = "prometheus")]
async fn metrics_handler(State(state): State<AppState>) -> Response {
    let Some(registry) = state.registry.as_ref() else {
        return (StatusCode::NOT_FOUND, "no Prometheus backend configured").into_response();
    };
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
    // Liveness: the process is up and the event loop responds. Deliberately
    // unconditional — readiness is what carries the staleness signal.
    (StatusCode::OK, "OK").into_response()
}

/// Readiness: has a collection cycle succeeded recently?
#[cfg(feature = "prometheus")]
async fn ready_handler(State(state): State<AppState>) -> Response {
    if state.health.is_ready() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "no successful collection within the staleness window",
        )
            .into_response()
    }
}

#[cfg(not(feature = "prometheus"))]
pub async fn start_metrics_server(
    _bind_address: String,
    _metrics_path: String,
    _registry: Option<std::sync::Arc<()>>,
    _health: std::sync::Arc<HealthState>,
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

        let response = metrics_handler(State(AppState {
            registry: Some(Arc::clone(&registry)),
            health: Arc::new(HealthState::new(60)),
        }))
        .await;
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
        let response = metrics_handler(State(AppState {
            registry: Some(registry),
            health: Arc::new(HealthState::new(60)),
        }))
        .await;
        let response = response.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
