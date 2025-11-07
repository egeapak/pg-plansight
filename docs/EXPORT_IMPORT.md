# Export/Import Analysis Data

pg-loganalyze supports exporting and importing analysis results as JSON files. This allows you to:

- Save analysis results for later review
- Share analysis data with team members
- Archive historical performance data
- Load pre-analyzed data without re-parsing logs

## Features

- **Full Data Preservation**: Exports all query statistics, percentiles, execution counts, and timestamps
- **Metadata Tracking**: Includes source files, hostname, user, and analysis period
- **Sorted Output**: Queries sorted by total duration for quick identification of hotspots
- **Human-Readable**: JSON format is easy to read and can be processed by other tools
- **Roundtrip Support**: Exported data can be imported back with full fidelity

## Export Format

The export file contains:

```json
{
  "version": "0.1.0",
  "exported_at": "2025-11-07T15:30:45Z",
  "analysis_period": {
    "start": "2025-06-12T00:00:00Z",
    "end": "2025-06-12T23:59:59Z"
  },
  "query_count": 10,
  "execution_count": 1500,
  "queries": [
    {
      "query_hash": "000000000000abcd",
      "original_query": "SELECT * FROM users WHERE id = $1",
      "normalized_query": "SELECT * FROM users WHERE id = ?",
      "formatted_query": "SELECT * FROM users WHERE id = ?",
      "plan": "Index Scan using users_pkey...",
      "statistics": {
        "count": 150,
        "total_duration_ms": 1500.0,
        "min_duration_ms": 5.0,
        "max_duration_ms": 25.0,
        "mean_duration_ms": 10.0,
        "std_dev_ms": 3.5,
        "min_timestamp": "2025-06-12T00:15:00Z",
        "max_timestamp": "2025-06-12T23:45:00Z",
        "percentiles": {
          "p25": 8.0,
          "p50": 10.0,
          "p90": 15.0,
          "p95": 18.0,
          "p99": 22.0
        },
        "hourly_histogram": {
          "2025-06-12T14:00:00Z": {
            "count": 25,
            "total_duration_ms": 250.0,
            "min_duration_ms": 7.0,
            "max_duration_ms": 15.0,
            "mean_duration_ms": 10.0
          }
        },
        "sample_execution_times": [10.5, 9.8, 11.2, ...]
      }
    }
  ],
  "metadata": {
    "source_files": [
      "/var/log/postgresql/postgresql-2025-06-12.log"
    ],
    "hostname": "prod-db-01",
    "user": "dbadmin",
    "tags": {}
  }
}
```

## Usage

### Exporting Analysis Results

**From the TUI:**

1. Run your analysis: `pg-loganalyze logs/postgresql-*.log`
2. Wait for parsing to complete
3. Press **Ctrl+X** to export
4. The export will be saved to `pg_analysis_YYYYMMDD_HHMMSS.json` in the current directory

**Example:**
```bash
pg-loganalyze /var/log/postgresql/postgresql-2025-06-12.log
# Press Ctrl+X after analysis completes
# File saved: pg_analysis_20251107_153045.json
```

### Importing Analysis Results

**Load a previously exported analysis:**

```bash
pg-loganalyze --import pg_analysis_20251107_153045.json
```

This will:
- Skip log parsing entirely
- Load the saved analysis data
- Open the TUI with all queries, statistics, and plans available
- Preserve all query details and performance metrics

**Benefits:**
- Instant loading (no parsing time)
- Review historical data
- Compare analyses from different time periods
- Work offline with archived data

## Use Cases

### 1. Daily Performance Reports

Export analysis results at the end of each day for tracking:

```bash
# Parse today's logs
pg-loganalyze /var/log/postgresql/postgresql-$(date +%Y-%m-%d).log

# Export in TUI (Ctrl+X)
# Archive the JSON file
mv pg_analysis_*.json ~/performance_archive/$(date +%Y-%m-%d).json
```

### 2. Sharing Analysis with Team

```bash
# Analyze logs
pg-loganalyze production.log

# Export (Ctrl+X) and share
scp pg_analysis_*.json teammate@remote:/tmp/

# Teammate reviews
ssh teammate@remote
pg-loganalyze --import /tmp/pg_analysis_*.json
```

### 3. Before/After Comparisons

```bash
# Export baseline before optimization
pg-loganalyze logs/before.log  # Ctrl+X to save
mv pg_analysis_*.json baseline.json

# Make database changes...

# Export after optimization
pg-loganalyze logs/after.log  # Ctrl+X to save
mv pg_analysis_*.json optimized.json

# Compare side by side
pg-loganalyze --import baseline.json
pg-loganalyze --import optimized.json
```

### 4. Long-Term Archival

```bash
#!/bin/bash
# weekly_archive.sh - Archive weekly analysis

DATE=$(date +%Y-W%U)
pg-loganalyze /var/log/postgresql/postgresql-*.log
# Press Ctrl+X in TUI
mv pg_analysis_*.json "/archive/weekly/$DATE.json"
```

## File Management

### Automatic Naming

Export files are automatically named with timestamps:
- Format: `pg_analysis_YYYYMMDD_HHMMSS.json`
- Example: `pg_analysis_20251107_153045.json`
- Created in current working directory

### Manual File Naming

To organize exports by purpose:

```bash
# Export in TUI (Ctrl+X)
mv pg_analysis_*.json prod_db_weekly_2025_W45.json
```

## Integration with Other Tools

### jq for Analysis

Query the JSON with jq:

```bash
# Find slowest queries
jq '.queries | sort_by(-.statistics.mean_duration_ms) | .[0:5]' export.json

# Count queries by table
jq '.queries[].normalized_query' export.json | grep -o 'FROM [a-z_]*' | sort | uniq -c

# Extract execution counts
jq '.queries[].statistics.count' export.json | awk '{sum+=$1} END {print sum}'
```

### Python Processing

```python
import json

with open('pg_analysis_20251107_153045.json') as f:
    data = json.load(f)

# Find queries over 100ms average
slow_queries = [
    q for q in data['queries']
    if q['statistics']['mean_duration_ms'] > 100
]

print(f"Found {len(slow_queries)} slow queries")
```

### Monitoring Integration

Export files can be processed by monitoring systems:

```bash
# Extract metrics for Prometheus/Grafana
jq -r '.queries[] | "\(.statistics.count) \(.statistics.mean_duration_ms)"' export.json
```

## Data Privacy

### Sensitive Data

Exported files contain:
- **Original SQL queries** (may include sensitive data in comments or literals)
- **Query parameters** (anonymized as `?` in normalized queries)
- **Execution plans** (table and column names visible)
- **System metadata** (hostname, username)

**Recommendations:**
- Store exports securely
- Sanitize before sharing externally
- Add to `.gitignore` if in version control
- Consider encryption for sensitive environments

### Gitignore Pattern

Add to `.gitignore`:
```
pg_analysis_*.json
*.pg-analysis.json
```

## Troubleshooting

### Import Fails with "Invalid Data"

**Cause:** Corrupted or incompatible JSON file

**Solution:**
```bash
# Validate JSON
jq . export.json > /dev/null

# Check version compatibility
jq '.version' export.json
```

### Export File Too Large

**Cause:** Very large log files with many unique queries

**Solutions:**
- Compress the JSON: `gzip pg_analysis_*.json`
- Filter logs by date before analysis
- Archive old exports

### Import Shows No Data

**Cause:** Empty queries array in export

**Solution:**
Check the export file:
```bash
jq '.query_count, .execution_count' export.json
```

## API Reference

### Command-Line Flags

```
pg-loganalyze [OPTIONS] [LOG_FILES]...

OPTIONS:
    --import <FILE>     Import analysis from JSON file instead of parsing logs
    --help              Print help information
```

### Keyboard Shortcuts

In the TUI:

| Key | Action |
|-----|--------|
| `Ctrl+X` | Export current analysis to JSON |
| `q` | Quit application |

### Export File Schema

See the [Export Format](#export-format) section above for the complete JSON schema.

## Performance

### Export Speed

- Typical export: < 1 second for 1000 queries
- File size: ~500KB for 1000 queries with full statistics

### Import Speed

- Typical import: < 100ms for 1000 queries
- Much faster than re-parsing logs (seconds vs minutes)

## Future Enhancements

Planned features for export/import:

- [ ] Export to multiple formats (CSV, SQLite)
- [ ] Incremental exports (delta updates)
- [ ] Export filtering (by time range, duration threshold)
- [ ] Compression support (automatic .gz handling)
- [ ] Custom export locations via CLI flag
- [ ] Export templates for specific use cases

## See Also

- [Installation Guide](INSTALLATION.md)
- [Development Guide](DEVELOPMENT.md)
- [Production Features Roadmap](PRODUCTION_FEATURES.md)
