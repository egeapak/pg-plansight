//! Configuration findings shared by the CLI, the exporter and the extension.
//!
//! Each tool runs its own checks as required and may fold in another tool's
//! findings capped at [`Severity::Warn`], so a foreign problem never blocks it.

use crate::log_line_prefix::LogLinePrefix;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

impl Severity {
    /// PostgreSQL's vocabulary, as reported by `plansight_check()`.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Warn => "warning",
            Severity::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// Stable machine-readable id, e.g. `log_line_prefix.no_database`.
    pub code: &'static str,
    pub category: &'static str,
    pub message: String,
}

impl Finding {
    pub fn new(
        severity: Severity,
        code: &'static str,
        category: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code,
            category,
            message: message.into(),
        }
    }

    /// Lower the severity to at most `max`.
    pub fn capped(mut self, max: Severity) -> Self {
        self.severity = self.severity.min(max);
        self
    }
}

pub fn highest(findings: &[Finding]) -> Option<Severity> {
    findings.iter().map(|finding| finding.severity).max()
}

/// A `log_line_prefix` that cannot be parsed is an error; one without `%d` only
/// loses the per-database split, so it is a warning.
pub fn check_log_line_prefix(format: &str) -> Vec<Finding> {
    match LogLinePrefix::parse(format) {
        Err(error) => vec![Finding::new(
            Severity::Error,
            "log_line_prefix.invalid",
            "log_line_prefix",
            error.to_string(),
        )],
        Ok(prefix) if !prefix.has_database() => vec![Finding::new(
            Severity::Warn,
            "log_line_prefix.no_database",
            "log_line_prefix",
            format!(
                "log_line_prefix {format:?} has no %d, so every query is attributed to the \
                 database \"unknown\"; add %d (for example '%m [%p] %q%u@%d ')."
            ),
        )],
        Ok(_) => Vec::new(),
    }
}

/// The `auto_explain` settings plansight depends on when reading plans from the log.
#[derive(Debug, Clone, Default)]
pub struct AutoExplainSettings {
    pub log_min_duration: Option<String>,
    pub log_analyze: Option<String>,
    pub log_format: Option<String>,
}

pub fn check_auto_explain(settings: &AutoExplainSettings) -> Vec<Finding> {
    let mut findings = Vec::new();
    if settings.log_min_duration.as_deref() == Some("-1") {
        findings.push(Finding::new(
            Severity::Warn,
            "auto_explain.disabled",
            "log",
            "auto_explain.log_min_duration=-1 — no plans are logged; set it to 0 \
             (or a duration threshold).",
        ));
    }
    if settings.log_analyze.as_deref() == Some("off") {
        findings.push(Finding::new(
            Severity::Warn,
            "auto_explain.no_analyze",
            "log",
            "auto_explain.log_analyze=off — logged plans lack actual times, which \
             plansight needs.",
        ));
    }
    if let Some(format) = settings.log_format.as_deref()
        && format != "text"
    {
        findings.push(Finding::new(
            Severity::Warn,
            "auto_explain.not_text",
            "log",
            format!(
                "auto_explain.log_format={format} — plansight parses text plans; \
                 set auto_explain.log_format=text."
            ),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_order_and_cap() {
        assert!(Severity::Info < Severity::Warn && Severity::Warn < Severity::Error);
        let finding = Finding::new(Severity::Error, "x", "c", "m").capped(Severity::Warn);
        assert_eq!(finding.severity, Severity::Warn);
        let low = Finding::new(Severity::Info, "x", "c", "m").capped(Severity::Warn);
        assert_eq!(low.severity, Severity::Info);
    }

    #[test]
    fn severity_text_is_postgresql_vocabulary() {
        assert_eq!(Severity::Warn.as_str(), "warning");
    }

    #[test]
    fn prefix_without_database_warns() {
        let findings = check_log_line_prefix("%m [%p] ");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "log_line_prefix.no_database");
        assert_eq!(highest(&findings), Some(Severity::Warn));
    }

    #[test]
    fn prefix_with_database_is_clean() {
        assert!(check_log_line_prefix("%m [%p] %q%u@%d ").is_empty());
    }

    #[test]
    fn unparseable_prefix_is_an_error() {
        let findings = check_log_line_prefix("%p %m ");
        assert_eq!(findings[0].code, "log_line_prefix.invalid");
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn auto_explain_settings_are_checked_independently() {
        let findings = check_auto_explain(&AutoExplainSettings {
            log_min_duration: Some("-1".into()),
            log_analyze: Some("off".into()),
            log_format: Some("json".into()),
        });
        let codes: Vec<_> = findings.iter().map(|f| f.code).collect();
        assert_eq!(
            codes,
            [
                "auto_explain.disabled",
                "auto_explain.no_analyze",
                "auto_explain.not_text"
            ]
        );
        assert!(check_auto_explain(&AutoExplainSettings::default()).is_empty());
    }
}
