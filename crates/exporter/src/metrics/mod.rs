mod composite_backend;
pub(crate) mod derived;
#[cfg(feature = "opentelemetry")]
mod otel_backend;
#[cfg(feature = "prometheus")]
mod prometheus_backend;
#[cfg(test)]
mod tests;
mod traits;

pub use traits::MetricsBackend;

pub use composite_backend::CompositeBackend;
#[cfg(feature = "opentelemetry")]
pub use otel_backend::OpenTelemetryBackend;
#[cfg(feature = "prometheus")]
pub use prometheus_backend::PrometheusBackend;

use anyhow::Result;
use std::sync::Arc;

// Type alias for backward compatibility with merged branches
pub type MetricsRegistry = Arc<dyn MetricsBackend>;

/// Factory for creating metrics backends
pub enum MetricsBackendType {
    #[cfg(feature = "prometheus")]
    Prometheus {
        namespace: String,
        histogram_buckets: Vec<f64>,
        /// Max distinct `normalized_query_hash` series to keep (0 = unlimited).
        max_query_cardinality: usize,
    },
    #[cfg(feature = "opentelemetry")]
    OpenTelemetry { endpoint: String, namespace: String },
}

pub fn create_metrics_backend(backend_type: MetricsBackendType) -> Result<Arc<dyn MetricsBackend>> {
    match backend_type {
        #[cfg(feature = "prometheus")]
        MetricsBackendType::Prometheus {
            namespace,
            histogram_buckets,
            max_query_cardinality,
        } => {
            let backend =
                PrometheusBackend::new(&namespace, histogram_buckets, max_query_cardinality)?;
            Ok(Arc::new(backend))
        }
        #[cfg(feature = "opentelemetry")]
        MetricsBackendType::OpenTelemetry {
            endpoint,
            namespace,
        } => {
            use opentelemetry_otlp::WithExportConfig;
            use opentelemetry_sdk::metrics::SdkMeterProvider;

            let exporter = opentelemetry_otlp::MetricExporter::builder()
                .with_tonic()
                .with_endpoint(endpoint)
                .build()?;

            // Use the async-runtime reader so periodic export runs as a tokio
            // task (the OTLP gRPC/tonic exporter needs the reactor). The default
            // PeriodicReader runs export on its own non-tokio thread via
            // block_on, which panics on the tonic future and drops all metrics.
            let reader = opentelemetry_sdk::metrics::periodic_reader_with_async_runtime::PeriodicReader::builder(
                exporter,
                opentelemetry_sdk::runtime::Tokio,
            )
            .build();
            let meter_provider = SdkMeterProvider::builder().with_reader(reader).build();

            let backend = OpenTelemetryBackend::new(meter_provider, &namespace)?;
            Ok(Arc::new(backend))
        }
    }
}

/// Helper functions for common metrics operations
pub fn update_memory_usage(backend: &dyn MetricsBackend) {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(vm_rss) = line.strip_prefix("VmRSS:")
                && let Some(kb_str) = vm_rss.trim().strip_suffix(" kB")
                && let Ok(kb) = kb_str.trim().parse::<i64>()
            {
                backend.set_memory_usage(kb * 1024); // Convert to bytes
                break;
            }
        }
    }
}

pub fn record_successful_parse(backend: &dyn MetricsBackend) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    backend.set_last_successful_parse(timestamp);
}

/// Backend names this build actually supports.
fn available_backend_names() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut names: Vec<&'static str> = Vec::new();
    #[cfg(feature = "prometheus")]
    {
        names.push("prometheus");
    }
    #[cfg(feature = "opentelemetry")]
    {
        names.push("opentelemetry");
    }
    names
}

/// Build the configured metrics backend(s) from `metrics.backends`.
///
/// Shared by `daemon`, `process` and `process-rest`. The two batch commands
/// previously inlined this without the empty-list guard `daemon` had, so
/// `backends = []` built an empty `CompositeBackend` whose every method is a
/// no-op loop — a backfill then "succeeded", advancing every checkpoint to EOF
/// while exporting nothing, with no way to reprocess short of `state reset`.
pub fn from_config(cfg: &crate::config::MetricsConfig) -> anyhow::Result<Arc<dyn MetricsBackend>> {
    use anyhow::Context as _;

    let mut backends: Vec<Arc<dyn MetricsBackend>> = Vec::new();

    for name in &cfg.backends {
        match name.as_str() {
            #[cfg(feature = "prometheus")]
            "prometheus" => {
                backends.push(
                    create_metrics_backend(MetricsBackendType::Prometheus {
                        namespace: cfg.namespace.clone(),
                        histogram_buckets: cfg.histogram_buckets.clone(),
                        max_query_cardinality: cfg.max_query_cardinality,
                    })
                    .context("Failed to initialize Prometheus metrics backend")?,
                );
            }
            #[cfg(feature = "opentelemetry")]
            "opentelemetry" => {
                let otel = cfg
                    .opentelemetry
                    .as_ref()
                    .context("OpenTelemetry backend selected but no configuration provided")?;
                backends.push(
                    create_metrics_backend(MetricsBackendType::OpenTelemetry {
                        endpoint: otel.endpoint.clone(),
                        namespace: cfg.namespace.clone(),
                    })
                    .context("Failed to initialize OpenTelemetry metrics backend")?,
                );
            }
            other => anyhow::bail!(
                "Unsupported metrics backend: {other}. Available in this build: {}",
                available_backend_names().join(", ")
            ),
        }
    }

    match backends.len() {
        0 => anyhow::bail!(
            "No metrics backends configured (metrics.backends is empty). Nothing would be \
             exported, while checkpoints would still advance."
        ),
        1 => Ok(backends.pop().expect("len checked")),
        _ => Ok(Arc::new(CompositeBackend::new(backends))),
    }
}
