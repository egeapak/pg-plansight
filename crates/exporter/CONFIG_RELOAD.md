# Configuration Hot Reload

The pg-loganalyze-exporter supports reloading configuration without restart using the standard Unix SIGHUP signal.

## How It Works

The exporter listens for SIGHUP signals and reloads the configuration file when received. This approach:

- ✅ Gives operators explicit control over when to reload
- ✅ Allows validation of config before applying
- ✅ Avoids issues with partial file writes during editing
- ✅ Follows Unix daemon conventions (like nginx, postgres, etc.)
- ✅ Integrates seamlessly with systemd

## Usage

### Manual Reload

Find the process ID and send SIGHUP:

```bash
# Find PID
ps aux | grep pg-loganalyze-exporter

# Send SIGHUP to reload config
kill -HUP <pid>
```

### Using systemd

If running as a systemd service:

```bash
# Reload configuration (sends SIGHUP)
systemctl reload pg-loganalyze-exporter

# Check logs to verify reload
journalctl -u pg-loganalyze-exporter -f
```

### Verification

After sending SIGHUP, check the logs:

```
INFO Config reload enabled via SIGHUP signal
INFO Config file: /etc/pg-loganalyze-exporter/config.toml
INFO To reload: kill -HUP 12345 or systemctl reload pg-loganalyze-exporter
...
INFO SIGHUP received, reloading configuration...
INFO Configuration reloaded successfully
```

## What Can Be Reloaded

All configuration settings can be hot-reloaded:

### Server Settings
- `server.bind_address` - **Note**: Requires restart to rebind
- `server.metrics_path` - Updates endpoint path

### Log Parsing
- `log_parsing.log_paths` - Watch new files
- `log_parsing.poll_interval` - Change collection frequency
- `log_parsing.batch_size` - Tune batch processing

### Metrics
- `metrics.namespace` - **Note**: Changes metric names
- `metrics.histogram_buckets` - **Note**: Affects existing metrics
- `metrics.slow_query_thresholds` - Update threshold tracking
- `metrics.retain_days` - Change retention policy

### Filters
- `filters.include_databases` - Filter by database
- `filters.exclude_query_patterns` - Exclude query patterns
- `filters.min_duration_ms` - Filter by duration

## Validation

The exporter validates configuration before applying:

```bash
# Edit config
vim /etc/pg-loganalyze-exporter/config.toml

# Test configuration (will fail if invalid)
pg-loganalyze-exporter daemon --config /etc/pg-loganalyze-exporter/config.toml --dry-run

# If valid, reload
systemctl reload pg-loganalyze-exporter
```

## Error Handling

If the new configuration is invalid:

1. Error is logged
2. Previous configuration remains active
3. Service continues running

Example error:

```
ERROR SIGHUP received, reloading configuration...
ERROR Failed to reload configuration: Invalid poll_interval in config
ERROR Keeping previous configuration
```

## Best Practices

### 1. Validate Before Reload

Always test configuration before reloading in production:

```bash
# Copy current config
cp /etc/pg-loganalyze-exporter/config.toml /tmp/config.toml.new

# Edit new config
vim /tmp/config.toml.new

# Test new config (--dry-run would be nice to add)
pg-loganalyze-exporter daemon --config /tmp/config.toml.new &
PID=$!
sleep 2
kill $PID

# If OK, replace and reload
mv /tmp/config.toml.new /etc/pg-loganalyze-exporter/config.toml
systemctl reload pg-loganalyze-exporter
```

### 2. Monitor After Reload

```bash
# Watch logs during reload
journalctl -u pg-loganalyze-exporter -f

# Check metrics are still being collected
curl http://localhost:9090/metrics | grep pg_loganalyze
```

### 3. Use Version Control

```bash
# Keep config in git
cd /etc/pg-loganalyze-exporter
git add config.toml
git commit -m "Update poll interval to 60s"
systemctl reload pg-loganalyze-exporter
```

## Systemd Integration

The service file supports reload:

```ini
[Service]
ExecStart=/usr/bin/pg-loganalyze-exporter daemon --config /etc/pg-loganalyze-exporter/config.toml
ExecReload=/bin/kill -HUP $MAINPID
```

This allows:

```bash
systemctl reload pg-loganalyze-exporter
```

## Limitations

Some settings require a full restart:

### Requires Restart
- `state.database_path` - State DB path is fixed at startup
- `server.bind_address` - Socket binding happens once

For these changes:

```bash
systemctl restart pg-loganalyze-exporter
```

## Automation

### Reload on Config Change (with validation)

```bash
#!/bin/bash
# /usr/local/bin/reload-pg-exporter

set -e

CONFIG="/etc/pg-loganalyze-exporter/config.toml"

# Validate config (would need --config-test flag)
if pg-loganalyze-exporter daemon --config "$CONFIG" --check-config 2>&1 | grep -q "valid"; then
    echo "Config valid, reloading..."
    systemctl reload pg-loganalyze-exporter
    echo "Reloaded successfully"
else
    echo "Config validation failed, not reloading"
    exit 1
fi
```

### Monitoring Integration

```bash
# Prometheus alert for failed reloads
ALERT ConfigReloadFailed
  IF increase(pg_loganalyze_config_reload_errors_total[5m]) > 0
  FOR 5m
  LABELS { severity = "warning" }
  ANNOTATIONS {
    summary = "pg-loganalyze-exporter config reload failed",
    description = "Config reload failed in the last 5 minutes"
  }
```

## Troubleshooting

### SIGHUP Not Working

1. Check process is running:
   ```bash
   systemctl status pg-loganalyze-exporter
   ```

2. Check config file path:
   ```bash
   ps aux | grep pg-loganalyze-exporter
   # Should show --config flag
   ```

3. Check permissions:
   ```bash
   ls -l /etc/pg-loganalyze-exporter/config.toml
   # Should be readable by pg-loganalyze user
   ```

### Config Not Reloading

1. Check logs for errors:
   ```bash
   journalctl -u pg-loganalyze-exporter -n 100
   ```

2. Verify config syntax:
   ```bash
   toml-lint /etc/pg-loganalyze-exporter/config.toml
   ```

3. Send signal manually:
   ```bash
   kill -HUP $(pgrep pg-loganalyze-exporter)
   ```

## Implementation Notes

For developers:

- Uses `tokio::signal::unix::signal(SignalKind::hangup())`
- Config changes propagated via `tokio::sync::watch` channel
- Scheduler and Collector updated atomically
- Invalid configs rejected, previous config retained
- No file watching - explicit operator control

See `crates/exporter/src/config_watcher.rs` for implementation details.
