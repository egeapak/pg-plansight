//! The `[postgres]` source: polls the pg_plansight extension's `plansight.metrics*`
//! views and serves them as Prometheus metrics.
//!
//! The extension already aggregates, so the values are absolute and are exposed
//! as they are by a custom collector at scrape time. Nothing is incremented
//! here, which is why an exporter restart does not reset a counter.

use crate::config::PostgresConfig;
use anyhow::{Context, Result};
use pg_plansight_core::checks::{Finding, Severity};
use prometheus::core::{Collector, Desc};
use prometheus::proto::{
    Bucket, Counter, Gauge, Histogram, LabelPair, Metric, MetricFamily, MetricType,
};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_postgres::{Client, Row};
use tracing::{error, info, warn};

const SERIES_SQL: &str = "\
    SELECT database, fingerprint, calls, total_seconds \
    FROM plansight.metrics \
    ORDER BY total_seconds DESC \
    LIMIT $1";

const BUCKETS_SQL: &str = "\
    WITH top AS ( \
        SELECT database, fingerprint FROM plansight.metrics \
        ORDER BY total_seconds DESC LIMIT $1 \
    ) \
    SELECT b.database, b.fingerprint, b.le, b.count \
    FROM plansight.metrics_latency_buckets b \
    JOIN top USING (database, fingerprint) \
    ORDER BY b.database, b.fingerprint, b.le";

const RECENT_SQL: &str = "\
    SELECT database, fingerprint, calls_per_second, p95_le_seconds, p99_le_seconds \
    FROM plansight.metrics_recent";

const CAPTURE_SQL: &str = "\
    SELECT captured_total, dropped_total, ring_pending FROM plansight.metrics_capture";

#[derive(Debug, Clone, Default)]
struct Series {
    database: String,
    hash: String,
    calls: i64,
    total_seconds: f64,
    /// `(le, cumulative count)`; empty for rows captured before 0.2.0.
    buckets: Vec<(f64, i64)>,
    recent: Option<Recent>,
}

#[derive(Debug, Clone)]
struct Recent {
    calls_per_second: f64,
    p95_le_seconds: f64,
    p99_le_seconds: f64,
}

#[derive(Debug, Clone, Default)]
struct Capture {
    captured_total: i64,
    dropped_total: i64,
    ring_pending: i64,
}

#[derive(Debug, Clone, Default)]
struct Snapshot {
    up: bool,
    polled_at_seconds: f64,
    series: Vec<Series>,
    capture: Option<Capture>,
}

pub struct PostgresSource;

impl PostgresSource {
    /// Register the collector on `registry` and start polling. The task lives
    /// until the process exits.
    pub fn spawn(
        config: PostgresConfig,
        namespace: &str,
        registry: &prometheus::Registry,
    ) -> Result<tokio::task::JoinHandle<()>> {
        let interval = crate::config::parse_duration(&config.poll_interval)
            .context("invalid postgres.poll_interval")?;
        let state = Arc::new(RwLock::new(Snapshot::default()));
        registry
            .register(Box::new(ExtensionCollector {
                namespace: namespace.to_string(),
                state: state.clone(),
            }))
            .context("registering the pg_plansight extension collector")?;

        Ok(tokio::spawn(async move {
            poll_loop(config, interval, state).await;
        }))
    }
}

async fn poll_loop(config: PostgresConfig, interval: Duration, state: Arc<RwLock<Snapshot>>) {
    let mut client: Option<Client> = None;
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if client.as_ref().is_none_or(Client::is_closed) {
            client = match connect(&config.dsn).await {
                Ok(connected) => {
                    info!("Connected to PostgreSQL for the pg_plansight extension source");
                    Some(connected)
                }
                Err(e) => {
                    warn!("pg_plansight extension source: {e:#}");
                    mark_down(&state);
                    continue;
                }
            };
        }
        let Some(connected) = client.as_ref() else {
            continue;
        };
        match fetch(connected, config.max_series).await {
            Ok(snapshot) => {
                *state.write().unwrap_or_else(|e| e.into_inner()) = snapshot;
            }
            Err(e) => {
                error!("pg_plansight extension source: poll failed: {e:#}");
                mark_down(&state);
                client = None;
            }
        }
    }
}

fn mark_down(state: &RwLock<Snapshot>) {
    state.write().unwrap_or_else(|e| e.into_inner()).up = false;
}

async fn connect(dsn: &str) -> Result<Client> {
    let config = connection_config(dsn)?;
    let (client, connection) = config
        .connect(tls_connector()?)
        .await
        .context("cannot connect to PostgreSQL")?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            warn!("pg_plansight extension source: connection ended: {e}");
        }
    });
    Ok(client)
}

/// Parse the DSN. A Unix-socket-only DSN never uses TLS: tokio-postgres would
/// otherwise try it under the default `sslmode=prefer` with the socket directory
/// as the server name, which cannot be a certificate name.
fn connection_config(dsn: &str) -> Result<tokio_postgres::Config> {
    use tokio_postgres::config::{Host, SslMode};
    let mut config: tokio_postgres::Config = dsn.parse().context("invalid postgres.dsn")?;
    if !config.get_hosts().is_empty()
        && config
            .get_hosts()
            .iter()
            .all(|host| matches!(host, Host::Unix(_)))
    {
        config.ssl_mode(SslMode::Disable);
    }
    Ok(config)
}

fn tls_connector() -> Result<tokio_postgres_rustls::MakeRustlsConnect> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_native_certs::load_native_certs().certs {
        // A malformed system certificate must not stop the connection.
        let _ = roots.add(certificate);
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("selecting TLS protocol versions")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(tls))
}

async fn fetch(client: &Client, max_series: usize) -> Result<Snapshot> {
    let limit: i64 = if max_series == 0 {
        i64::MAX
    } else {
        max_series as i64
    };
    let mut series: Vec<Series> = client
        .query(SERIES_SQL, &[&limit])
        .await
        .context("reading plansight.metrics")?
        .iter()
        .map(series_from_row)
        .collect();

    let mut bucket_rows = client
        .query(BUCKETS_SQL, &[&limit])
        .await
        .context("reading plansight.metrics_latency_buckets")?
        .into_iter();
    let mut pending = bucket_rows.next();
    for entry in &mut series {
        while let Some(row) = &pending {
            let (database, hash): (String, String) = (row.get(0), row.get(1));
            if (database.as_str(), hash.as_str()) != (entry.database.as_str(), entry.hash.as_str())
            {
                break;
            }
            entry.buckets.push((row.get(2), row.get(3)));
            pending = bucket_rows.next();
        }
    }

    for row in client
        .query(RECENT_SQL, &[])
        .await
        .context("reading plansight.metrics_recent")?
    {
        let (database, hash): (String, String) = (row.get(0), row.get(1));
        if let Some(entry) = series
            .iter_mut()
            .find(|entry| entry.database == database && entry.hash == hash)
        {
            entry.recent = Some(Recent {
                calls_per_second: row.get(2),
                p95_le_seconds: row.get(3),
                p99_le_seconds: row.get(4),
            });
        }
    }

    let capture = client
        .query_opt(CAPTURE_SQL, &[])
        .await
        .context("reading plansight.metrics_capture")?
        .map(|row| Capture {
            captured_total: row.get(0),
            dropped_total: row.get(1),
            ring_pending: row.get(2),
        });

    Ok(Snapshot {
        up: true,
        polled_at_seconds: now_seconds(),
        series,
        capture,
    })
}

fn series_from_row(row: &Row) -> Series {
    Series {
        database: row.get(0),
        hash: row.get(1),
        calls: row.get(2),
        total_seconds: row.get(3),
        buckets: Vec::new(),
        recent: None,
    }
}

fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// Connect once and report what `plansight_check()` finds, for `check-config`.
/// A connection failure is an error; the extension's own findings are capped at
/// a warning, since the exporter can still run against a misconfigured server.
pub async fn check(config: &PostgresConfig) -> Vec<Finding> {
    let client = match connect(&config.dsn).await {
        Ok(client) => client,
        Err(e) => {
            return vec![Finding::new(
                Severity::Error,
                "postgres.connect",
                "postgres",
                format!("{e:#}"),
            )];
        }
    };
    let rows = match client
        .query("SELECT code, category, message FROM plansight_check()", &[])
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return vec![Finding::new(
                Severity::Error,
                "postgres.no_extension",
                "postgres",
                format!(
                    "plansight_check() failed ({e}); is the pg_plansight extension 0.2.0 installed in this database?"
                ),
            )];
        }
    };
    rows.iter()
        .filter_map(|row| {
            let code: String = row.get(0);
            (code != "config.ok").then(|| {
                Finding::new(
                    Severity::Warn,
                    "postgres.extension_check",
                    "postgres",
                    format!(
                        "pg_plansight {code} ({}): {}",
                        row.get::<_, String>(1),
                        row.get::<_, String>(2)
                    ),
                )
            })
        })
        .collect()
}

struct ExtensionCollector {
    namespace: String,
    state: Arc<RwLock<Snapshot>>,
}

impl std::fmt::Debug for ExtensionCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionCollector")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl Collector for ExtensionCollector {
    // Unchecked collector: the families are built per scrape.
    fn desc(&self) -> Vec<&Desc> {
        Vec::new()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let snapshot = self.state.read().unwrap_or_else(|e| e.into_inner()).clone();
        families(&self.namespace, &snapshot)
    }
}

fn label(name: &str, value: &str) -> LabelPair {
    let mut pair = LabelPair::default();
    pair.set_name(name.to_string());
    pair.set_value(value.to_string());
    pair
}

fn series_labels(series: &Series) -> Vec<LabelPair> {
    vec![
        label("database", &series.database),
        label("normalized_query_hash", &series.hash),
    ]
}

fn family(
    namespace: &str,
    name: &str,
    help: &str,
    kind: MetricType,
    metrics: Vec<Metric>,
) -> MetricFamily {
    let mut family = MetricFamily::default();
    family.set_name(format!("{namespace}_ext_{name}"));
    family.set_help(help.to_string());
    family.set_field_type(kind);
    family.set_metric(metrics);
    family
}

fn gauge_metric(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut gauge = Gauge::default();
    gauge.set_value(value);
    let mut metric = Metric::default();
    metric.set_label(labels);
    metric.set_gauge(gauge);
    metric
}

fn counter_metric(labels: Vec<LabelPair>, value: f64) -> Metric {
    let mut counter = Counter::default();
    counter.set_value(value);
    let mut metric = Metric::default();
    metric.set_label(labels);
    metric.set_counter(counter);
    metric
}

fn histogram_metric(series: &Series) -> Metric {
    let mut histogram = Histogram::default();
    histogram.set_sample_count(series.calls.max(0) as u64);
    histogram.set_sample_sum(series.total_seconds);
    let buckets = series
        .buckets
        .iter()
        .filter(|(le, _)| le.is_finite())
        .map(|(le, count)| {
            let mut bucket = Bucket::default();
            bucket.set_upper_bound(*le);
            bucket.set_cumulative_count((*count).max(0) as u64);
            bucket
        })
        .collect();
    histogram.set_bucket(buckets);
    let mut metric = Metric::default();
    metric.set_label(series_labels(series));
    metric.set_histogram(histogram);
    metric
}

fn families(namespace: &str, snapshot: &Snapshot) -> Vec<MetricFamily> {
    let mut out = vec![
        family(
            namespace,
            "up",
            "1 if the last poll of the pg_plansight extension succeeded.",
            MetricType::GAUGE,
            vec![gauge_metric(
                Vec::new(),
                if snapshot.up { 1.0 } else { 0.0 },
            )],
        ),
        family(
            namespace,
            "last_poll_timestamp_seconds",
            "Unix time of the last successful poll of the pg_plansight extension.",
            MetricType::GAUGE,
            vec![gauge_metric(Vec::new(), snapshot.polled_at_seconds)],
        ),
    ];

    if !snapshot.series.is_empty() {
        out.push(family(
            namespace,
            "query_executions_total",
            "Captured executions per query and database, as counted by the extension.",
            MetricType::COUNTER,
            snapshot
                .series
                .iter()
                .map(|s| counter_metric(series_labels(s), s.calls as f64))
                .collect(),
        ));
        out.push(family(
            namespace,
            "query_duration_seconds",
            "Execution time per query and database, from the extension's cumulative buckets.",
            MetricType::HISTOGRAM,
            snapshot
                .series
                .iter()
                .filter(|s| !s.buckets.is_empty())
                .map(histogram_metric)
                .collect(),
        ));
        let recent: Vec<_> = snapshot
            .series
            .iter()
            .filter_map(|s| s.recent.as_ref().map(|r| (s, r)))
            .collect();
        if !recent.is_empty() {
            out.push(family(
                namespace,
                "query_recent_calls_per_second",
                "Executions per second over the extension's recent window.",
                MetricType::GAUGE,
                recent
                    .iter()
                    .map(|(s, r)| gauge_metric(series_labels(s), r.calls_per_second))
                    .collect(),
            ));
            for (name, pick) in [
                (
                    "query_recent_p95_seconds",
                    (|r: &Recent| r.p95_le_seconds) as fn(&Recent) -> f64,
                ),
                ("query_recent_p99_seconds", |r: &Recent| r.p99_le_seconds),
            ] {
                out.push(family(
                    namespace,
                    name,
                    "Upper bound of the latency bucket holding the percentile over the recent window.",
                    MetricType::GAUGE,
                    recent
                        .iter()
                        .map(|(s, r)| gauge_metric(series_labels(s), pick(r)))
                        .collect(),
                ));
            }
        }
    }

    if let Some(capture) = &snapshot.capture {
        out.push(family(
            namespace,
            "capture_captured_total",
            "Executions the extension captured.",
            MetricType::COUNTER,
            vec![counter_metric(Vec::new(), capture.captured_total as f64)],
        ));
        out.push(family(
            namespace,
            "capture_dropped_total",
            "Executions the extension dropped because its ring was full.",
            MetricType::COUNTER,
            vec![counter_metric(Vec::new(), capture.dropped_total as f64)],
        ));
        out.push(family(
            namespace,
            "capture_ring_pending",
            "Captures waiting in the extension's ring for the background worker.",
            MetricType::GAUGE,
            vec![gauge_metric(Vec::new(), capture.ring_pending as f64)],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(hash: &str, database: &str) -> Series {
        Series {
            database: database.to_string(),
            hash: hash.to_string(),
            calls: 7,
            total_seconds: 0.35,
            buckets: vec![(0.001, 0), (0.01, 2), (0.1, 7), (f64::INFINITY, 7)],
            recent: Some(Recent {
                calls_per_second: 0.5,
                p95_le_seconds: 0.1,
                p99_le_seconds: 0.1,
            }),
        }
    }

    fn names(families: &[MetricFamily]) -> Vec<String> {
        families.iter().map(|f| f.name().to_string()).collect()
    }

    #[test]
    fn a_down_source_exports_only_up_and_the_last_poll_time() {
        let out = families("pg_plansight", &Snapshot::default());
        assert_eq!(
            names(&out),
            [
                "pg_plansight_ext_up",
                "pg_plansight_ext_last_poll_timestamp_seconds"
            ]
        );
        assert_eq!(out[0].get_metric()[0].get_gauge().value(), 0.0);
    }

    #[test]
    fn series_carry_database_and_hash_labels_and_a_consistent_histogram() {
        let snapshot = Snapshot {
            up: true,
            polled_at_seconds: 1.0,
            series: vec![series("abc", "shop"), series("abc", "crm")],
            capture: Some(Capture {
                captured_total: 14,
                dropped_total: 1,
                ring_pending: 0,
            }),
        };
        let out = families("pg_plansight", &snapshot);
        let executions = out
            .iter()
            .find(|f| f.name() == "pg_plansight_ext_query_executions_total")
            .unwrap();
        assert_eq!(executions.get_metric().len(), 2);
        let labels: Vec<_> = executions.get_metric()[0]
            .get_label()
            .iter()
            .map(|l| (l.name().to_string(), l.value().to_string()))
            .collect();
        assert_eq!(
            labels,
            [
                ("database".to_string(), "shop".to_string()),
                ("normalized_query_hash".to_string(), "abc".to_string())
            ]
        );

        let histogram = out
            .iter()
            .find(|f| f.name() == "pg_plansight_ext_query_duration_seconds")
            .unwrap()
            .get_metric()[0]
            .get_histogram();
        assert_eq!(histogram.get_sample_count(), 7);
        let counts: Vec<u64> = histogram
            .get_bucket()
            .iter()
            .map(|b| b.cumulative_count())
            .collect();
        assert!(
            counts.windows(2).all(|w| w[0] <= w[1]),
            "buckets must be cumulative"
        );
        assert_eq!(histogram.get_bucket().len(), 3, "+Inf is implicit");
        assert!(names(&out).contains(&"pg_plansight_ext_capture_dropped_total".to_string()));
    }

    #[test]
    fn rows_without_buckets_still_export_a_counter_but_no_histogram() {
        let mut legacy = series("old", "shop");
        legacy.buckets.clear();
        let snapshot = Snapshot {
            up: true,
            series: vec![legacy],
            ..Snapshot::default()
        };
        let out = families("ns", &snapshot);
        let histogram = out
            .iter()
            .find(|f| f.name() == "ns_ext_query_duration_seconds")
            .unwrap();
        assert!(histogram.get_metric().is_empty());
        assert!(names(&out).contains(&"ns_ext_query_executions_total".to_string()));
    }

    #[test]
    fn unix_socket_dsns_disable_tls_and_tcp_dsns_keep_the_default() {
        use tokio_postgres::config::SslMode;
        let socket = connection_config("host=/var/run/postgresql user=monitor").unwrap();
        assert_eq!(socket.get_ssl_mode(), SslMode::Disable);
        let tcp = connection_config("host=db.example user=monitor").unwrap();
        assert_eq!(tcp.get_ssl_mode(), SslMode::Prefer);
        let required = connection_config("host=db.example sslmode=require").unwrap();
        assert_eq!(required.get_ssl_mode(), SslMode::Require);
    }

    #[test]
    fn the_collector_registers_and_gathers() {
        let registry = prometheus::Registry::new();
        let state = Arc::new(RwLock::new(Snapshot {
            up: true,
            series: vec![series("abc", "shop")],
            ..Snapshot::default()
        }));
        registry
            .register(Box::new(ExtensionCollector {
                namespace: "t".to_string(),
                state,
            }))
            .unwrap();
        let gathered = registry.gather();
        assert!(
            gathered
                .iter()
                .any(|f| f.name() == "t_ext_query_duration_seconds")
        );
    }
}
