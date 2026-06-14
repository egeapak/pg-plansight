# pg-plansight-exporter

A daemon service that continuously monitors PostgreSQL `auto_explain` logs and exports metrics to Prometheus and/or OpenTelemetry.

## Features

- **Continuous Monitoring**: Runs as a background daemon, polling log files at configurable intervals
- **Multiple Backends**: Support for both Prometheus and OpenTelemetry metrics exporters
- **State Management**: Tracks file positions and processing history using SQLite
- **Query Normalization**: Groups similar queries for meaningful aggregations
- **Comprehensive Metrics**: Tracks query performance, execution patterns, plan analysis, and more
- **Compressed Logs**: Handles gzip and bzip2 compressed log files
- **Production Ready**: Includes systemd integration and packaging for Debian/RPM

## Installation

### From Source

```bash
# Build with Prometheus support (default)
cargo build --release

# Build with OpenTelemetry support
cargo build --release --features opentelemetry

# Build with both
cargo build --release --features "prometheus,opentelemetry"
```

### System Packages

```bash
# Debian/Ubuntu
dpkg -i pg-plansight-exporter_*.deb

# RHEL/CentOS/Fedora
rpm -i pg-plansight-exporter-*.rpm
```

## Configuration

Create a configuration file at `/etc/pg-plansight-exporter/config.toml`:

```toml
[server]
bind_address = "0.0.0.0:9090"
metrics_path = "/metrics"

[log_parsing]
log_paths = ["/var/log/postgresql/*.log", "/var/log/postgresql/*.log.gz"]
poll_interval = "30s"
batch_size = 1000

[metrics]
namespace = "pg_plansight"
backends = ["prometheus"]  # Can use multiple: ["prometheus", "opentelemetry"]

# Optional: OpenTelemetry configuration (required if using opentelemetry backend)
# [metrics.opentelemetry]
# endpoint = "http://localhost:4317"

histogram_buckets = [0.001, 0.01, 0.1, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0]
slow_query_thresholds = ["1s", "5s", "10s", "30s"]
retain_days = 7

[state]
database_path = "/var/lib/pg-plansight-exporter/state.db"

# Optional: Filtering
[filters]
include_databases = ["production", "analytics"]
exclude_query_patterns = ["^EXPLAIN", "^SET"]
min_duration_ms = 100.0
```

## Usage

### Daemon Mode

```bash
# Start the daemon
pg-plansight-exporter --config /etc/pg-plansight-exporter/config.toml daemon

# With systemd
systemctl start pg-plansight-exporter
systemctl enable pg-plansight-exporter
```

### One-Time Processing

```bash
# Process specific log files once
pg-plansight-exporter --config config.toml process --logs /var/log/postgresql/postgresql-*.log

# Process remaining unread content
pg-plansight-exporter --config config.toml process-rest
```

### State Management

```bash
# Initialize state database
pg-plansight-exporter --config config.toml state init

# Show current state
pg-plansight-exporter --config config.toml state show

# Reset state (clear all tracking)
pg-plansight-exporter --config config.toml state reset
```

## Metrics

### Query Performance
- `pg_plansight_query_duration_seconds` - Query execution duration histogram
- `pg_plansight_query_executions_total` - Total query executions counter
- `pg_plansight_slow_queries_total` - Slow queries counter by threshold

### Query Complexity
- `pg_plansight_query_plan_cost` - Query plan estimated cost histogram
- `pg_plansight_query_rows_examined` - Rows examined histogram

### Database Aggregates
- `pg_plansight_database_avg_query_duration_seconds` - Average query duration per database
- `pg_plansight_database_queries_per_second` - QPS per database
- `pg_plansight_database_unique_queries_total` - Unique queries per database

### Plan Analysis
- `pg_plansight_query_plan_node_types_total` - Plan node type counters
- `pg_plansight_query_scan_types_total` - Scan type counters (seq scan, index scan, etc.)
- `pg_plansight_query_join_types_total` - Join type counters (hash join, nested loop, etc.)

### Exporter Health
- `pg_plansight_exporter_up` - Whether the exporter is running
- `pg_plansight_logs_parsed_total` - Total log entries parsed
- `pg_plansight_parse_errors_total` - Parse errors counter
- `pg_plansight_export_duration_seconds` - Time spent exporting metrics
- `pg_plansight_memory_usage_bytes` - Current memory usage
- `pg_plansight_last_successful_parse_timestamp` - Last successful parse timestamp

## Metrics Backends

The exporter supports exporting metrics to multiple backends simultaneously!

### Using Multiple Backends

You can configure the exporter to send metrics to both Prometheus and OpenTelemetry at the same time:

```toml
[metrics]
backends = ["prometheus", "opentelemetry"]

[metrics.opentelemetry]
endpoint = "http://otel-collector:4317"
```

### Prometheus

When Prometheus is enabled, metrics are exposed via HTTP at the configured endpoint (default: `http://0.0.0.0:9090/metrics`).

Add to your Prometheus configuration:

```yaml
scrape_configs:
  - job_name: 'pg-plansight'
    static_configs:
      - targets: ['localhost:9090']
```

**Prometheus-only configuration:**
```toml
[metrics]
backends = ["prometheus"]
```

### OpenTelemetry

When OpenTelemetry is enabled, metrics are pushed to an OTLP endpoint via gRPC.

**OpenTelemetry-only configuration:**
```toml
[metrics]
backends = ["opentelemetry"]

[metrics.opentelemetry]
endpoint = "http://otel-collector:4317"
```

The exporter will push metrics to the configured OTLP endpoint at regular intervals.

### Dual Backend Configuration

For maximum observability, use both backends simultaneously:

```toml
[metrics]
backends = ["prometheus", "opentelemetry"]

[metrics.opentelemetry]
endpoint = "http://otel-collector:4317"
```

This allows you to:
- Scrape metrics with Prometheus for local monitoring and alerting
- Push metrics to OpenTelemetry for centralized observability platforms
- Maintain compatibility with existing Prometheus setups while migrating to OpenTelemetry

## PostgreSQL Setup

Ensure `auto_explain` is configured in your PostgreSQL:

```sql
-- postgresql.conf
shared_preload_libraries = 'auto_explain'
auto_explain.log_min_duration = 1000  # Log queries taking > 1s
auto_explain.log_analyze = true
auto_explain.log_buffers = true
auto_explain.log_timing = true
auto_explain.log_format = 'json'  # Recommended for better parsing
```

## Architecture

```
PostgreSQL Logs → Log Parser → State Manager → Metrics Backend → Prometheus/OpenTelemetry
                       ↓              ↓
                  Query Plans    SQLite DB
```

- **Log Parser**: Parses PostgreSQL logs with auto_explain output
- **State Manager**: Tracks file positions, checksums, and processing history
- **Metrics Backend**: Abstraction layer supporting multiple exporters
- **Scheduler**: Coordinates periodic collection runs

## Development

```bash
# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run -- --config config.toml daemon

# Build all targets
just build
```

## Troubleshooting

### No metrics appearing

1. Check that PostgreSQL auto_explain is enabled and logging
2. Verify log paths in configuration match actual log locations
3. Check exporter logs: `journalctl -u pg-plansight-exporter -f`
4. Verify state database is writable: `ls -la /var/lib/pg-plansight-exporter/`

### High memory usage

- Reduce `batch_size` in configuration
- Increase `poll_interval` to reduce frequency
- Enable log rotation and compression

### Parse errors

- Ensure `auto_explain.log_format = 'json'` in PostgreSQL
- Check log format matches expected auto_explain output
- Review parse errors metric for error types

## License

MIT
