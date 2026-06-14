# Production Features Roadmap

## Overview
This document outlines missing features and enhancements needed for production-grade deployment of pg-plansight.

**Last Updated:** 2025-11-07
**Total Identified Gaps:** ~79 features across 15 categories

---

## Priority Matrix

### CRITICAL (Must-Have for Production)

#### 1. Monitoring & Observability
- [ ] Application logging for TUI mode (currently silent)
  - Structured logging with rotation
  - Debug mode for troubleshooting
  - Error tracking and reporting
- [ ] Enhanced health checks for exporter
  - Liveness and readiness probes
  - Dependency health (filesystem, SQLite)
- [ ] Metrics for TUI operations
  - File parse times
  - Errors encountered

#### 2. Security Enhancements
- [ ] Authentication/Authorization for Prometheus exporter
  - Basic auth or token-based authentication
  - TLS/HTTPS support
- [ ] Terminal escape sequence sanitization in TUI
- [ ] State database encryption at rest
- [ ] Rate limiting on metrics endpoint
- [ ] State DB size limits and rotation policies

#### 3. Error Recovery & Resilience
- [ ] Graceful degradation when log files are corrupted
- [ ] Circuit breakers for file access failures
- [ ] Retry logic with backoff for transient failures
- [ ] Watchdog for exporter process health

---

### HIGH PRIORITY (Important for Production)

#### 4. Configuration Management
- [ ] TUI config file support (`~/.config/pg-plansight/config.toml`)
  - Default sort order, color schemes, key bindings
  - Performance tuning (batch sizes, parallel workers)
  - Custom highlighting rules
- [ ] Environment variable support for all CLI flags
- [ ] Config validation on startup
- [x] Hot reload for exporter configuration *(IN PROGRESS)*

#### 5. Data Export & Integration
- [ ] CSV export from TUI (filtered query results)
- [ ] JSON export with full statistics
- [ ] Report generation (HTML/PDF summaries)
- [ ] Integration hooks (webhooks for slow queries)
- [ ] Alerting documentation (Prometheus Alertmanager)

#### 6. Testing Infrastructure
- [x] Integration tests for end-to-end workflows *(IN PROGRESS)*
  - Core parsing integration tests
  - TUI integration tests
  - Exporter integration tests
- [ ] TUI testing framework (snapshot tests for UI)
- [ ] Property-based tests for parser (proptest/quickcheck)
- [ ] Load testing for exporter under high cardinality
- [ ] CI coverage reporting (codecov integration)
- [ ] Mutation testing (cargo-mutants)

#### 7. Documentation
- [ ] README.md with quick start guide
- [ ] User manual (keyboard shortcuts, features, examples)
- [ ] API documentation (`cargo doc` for library usage)
- [ ] Troubleshooting guide (common issues, FAQ)
- [ ] Architecture documentation (ADRs, design decisions)
- [ ] Sample Grafana dashboards (JSON templates)

---

### MEDIUM PRIORITY (Nice-to-Have)

#### 8. Performance & Scalability
- [ ] Streaming parser for very large files (>10GB)
- [ ] Compression-aware indexing
- [ ] Query result pagination in TUI (millions of queries)
- [ ] Background parsing with progress indicator
- [ ] Memory limits and backpressure handling
- [ ] Query plan caching across runs

#### 9. User Experience
- [ ] Search/filter in TUI (fuzzy search for queries)
- [ ] Query comparison mode (side-by-side)
- [ ] Plan diff viewer (compare query plan changes)
- [ ] Color scheme customization (theme support)
- [ ] Mouse support (clickable UI elements)
- [ ] Bookmark/favorite queries
- [ ] Query history navigation

#### 10. Data Management
- [ ] Query result persistence in TUI
- [ ] Historical trending (track query performance)
- [ ] Data retention policies (automatic cleanup)
- [ ] Backup/restore for state database
- [ ] Migration tools for schema changes
- [ ] Deduplication strategies for high-volume logs

#### 11. Advanced Analytics
- [ ] Anomaly detection (sudden performance changes)
- [ ] Query pattern clustering (group similar queries)
- [ ] Index recommendations based on query patterns
- [ ] Cost trend analysis (plan cost changes)
- [ ] Correlation analysis (identify related slow queries)
- [ ] Regression detection (compare baseline vs current)

---

### LOW PRIORITY (Future Enhancements)

#### 12. Multi-Database Support
- [ ] MySQL slow query log parsing
- [ ] Oracle trace file parsing
- [ ] Generic SQL log format support
- [ ] Cloud provider log formats (AWS RDS, GCP Cloud SQL)

#### 13. Deployment & Distribution
- [ ] Docker images (official)
- [ ] Kubernetes manifests (Helm charts)
- [ ] Homebrew formula (macOS)
- [ ] Snap/Flatpak packages
- [ ] Windows builds (MSI installer)
- [ ] Pre-built binaries for more targets

#### 14. Advanced Features
- [ ] Live tail mode (watch logs in real-time)
- [ ] Query replay capability (re-execute with EXPLAIN)
- [ ] Plan visualization (graphical tree view)
- [ ] Multi-file comparison (across environments)
- [ ] Plugin system for custom parsers/exporters
- [ ] REST API for programmatic access

#### 15. Compliance & Audit
- [ ] Audit logging (who accessed what)
- [ ] Data sanitization (PII removal from queries)
- [ ] Compliance reporting (GDPR, SOC 2)
- [ ] Access control (RBAC for different user roles)

---

## Implementation Roadmap

### Phase 1: MVP → Production
**Target:** Q1 2026
**Focus:** Core production readiness

1. Application logging (observability)
2. Authentication for exporter
3. Terminal sanitization
4. Integration tests
5. README + user docs

**Deliverables:**
- Production-grade logging with structured output
- Secure exporter deployment
- Comprehensive test suite
- User-facing documentation

---

### Phase 2: Hardening
**Target:** Q2 2026
**Focus:** Operational excellence

1. TUI config file
2. CSV/JSON export
3. Error recovery improvements
4. Health check enhancements
5. Comprehensive documentation

**Deliverables:**
- Configurable TUI experience
- Data export capabilities
- Enhanced reliability
- Complete documentation set

---

### Phase 3: Scale & Polish
**Target:** Q3 2026
**Focus:** Enterprise features

1. Advanced search/filter in TUI
2. Performance optimizations
3. Alerting integration
4. Historical trending
5. Docker/Kubernetes packaging

**Deliverables:**
- Enhanced user experience
- Better performance at scale
- Container deployment options
- Trend analysis features

---

### Phase 4: Innovation
**Target:** Q4 2026
**Focus:** Advanced capabilities

1. Anomaly detection
2. Index recommendations
3. Multi-database support
4. Plugin system

**Deliverables:**
- AI-powered insights
- Proactive optimization suggestions
- Broader database support
- Extensibility framework

---

## Current Status

### Completed Features ✅
- Core parsing engine with parallel processing
- Interactive TUI with syntax highlighting
- Prometheus exporter with comprehensive metrics
- Multi-architecture packaging (DEB/RPM)
- Systemd integration with security hardening
- Incremental state tracking
- Compressed file support

### In Progress 🚧
- Hot reload for exporter configuration
- Integration test suite

### Blocked ⛔
- None currently

---

## Success Metrics

### Phase 1 Success Criteria
- [ ] 80%+ code coverage with integration tests
- [ ] Zero production security vulnerabilities
- [ ] <100ms TUI response time
- [ ] Complete user documentation

### Phase 2 Success Criteria
- [ ] 95%+ code coverage
- [ ] 99.9% uptime for exporter
- [ ] Support for 100k+ queries per day
- [ ] <5 minute MTTR for common issues

### Phase 3 Success Criteria
- [ ] Support for 1M+ queries per day
- [ ] <50MB memory usage for TUI
- [ ] <200MB memory usage for exporter
- [ ] Container deployment in production

### Phase 4 Success Criteria
- [ ] Active plugin ecosystem
- [ ] Support for 3+ database types
- [ ] AI-powered recommendations in production
- [ ] Community contributions

---

## Contributing

To contribute to this roadmap:

1. Pick an unchecked item from the appropriate priority category
2. Create a feature branch: `claude/feature-name-<session-id>`
3. Implement with tests and documentation
4. Submit PR with reference to this document
5. Update checkbox when merged

---

## Notes

### Design Principles
- **Security first:** All features must meet security standards
- **Performance matters:** Sub-100ms user interactions
- **Reliability:** Graceful degradation over crashes
- **Usability:** Features should be intuitive
- **Extensibility:** Design for future enhancements

### Technical Debt
- None identified currently (well-architected codebase)

### Dependencies
- Rust 1.82+ (edition 2024)
- PostgreSQL 12+ for log format compatibility
- Linux systemd for service deployment

---

## References

- [Installation Guide](INSTALLATION.md)
- [Development Guide](DEVELOPMENT.md)
- [PostgreSQL Cost Model](../POSTGRESQL_COST_EXPLANATION.md)
- [Project Overview](../CLAUDE.md)
