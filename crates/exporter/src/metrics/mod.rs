mod composite_backend;
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
        } => {
            let backend = PrometheusBackend::new(&namespace, histogram_buckets)?;
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

            let meter_provider = SdkMeterProvider::builder()
                .with_reader(
                    opentelemetry_sdk::metrics::PeriodicReader::builder(
                        exporter,
                        opentelemetry_sdk::runtime::Tokio,
                    )
                    .build(),
                )
                .build();

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
                    && let Ok(kb) = kb_str.trim().parse::<i64>() {
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
