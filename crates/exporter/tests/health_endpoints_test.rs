//! `/health` and `/ready` must be served even with no Prometheus backend.
//!
//! The HTTP listener used to spawn only when "prometheus" was in
//! `metrics.backends`, so an OpenTelemetry-only deployment had no HTTP surface
//! at all — no health endpoint, nothing for a Kubernetes probe to hit.

#![cfg(feature = "prometheus")]

use pg_plansight_exporter::server::{HealthState, bind_metrics_listener, serve_metrics};
use std::sync::Arc;
use std::time::Duration;

async fn spawn_server(health: Arc<HealthState>) -> String {
    let listener = bind_metrics_listener("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // No registry: this is the OTel-only shape.
        let _ = serve_metrics(listener, "/metrics".to_string(), None, health).await;
    });

    // Wait for the listener to accept.
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    format!("http://{addr}")
}

async fn status(url: &str) -> u16 {
    let stream = tokio::net::TcpStream::connect(url.trim_start_matches("http://"))
        .await
        .unwrap();
    let (mut reader, mut writer) = stream.into_split();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    writer
        .write_all(b"GET /ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    text.split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

#[tokio::test]
async fn ready_is_served_without_a_prometheus_backend() {
    // Generous window: the grace period from process start applies.
    let health = Arc::new(HealthState::new(3600));
    let url = spawn_server(health).await;

    assert_eq!(
        status(&url).await,
        200,
        "/ready must be served even when no Prometheus registry exists"
    );
}

#[tokio::test]
async fn ready_reports_503_once_the_staleness_window_has_passed() {
    // A zero-second window means "already stale", including the grace period.
    let health = Arc::new(HealthState::new(0));
    let url = spawn_server(health.clone()).await;

    assert_eq!(
        status(&url).await,
        503,
        "/ready must fail when no collection has succeeded within the window"
    );

    // A fresh success does not help with a zero-second window either — the
    // signal is staleness, not "ever succeeded".
    health.mark_success();
    assert_eq!(status(&url).await, 503);
}
