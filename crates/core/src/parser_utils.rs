use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, Timelike, Utc};
use regex::Regex;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
// Removed unused std::borrow::Cow import
use std::collections::HashMap;
use std::fmt::Write as _;
#[cfg(feature = "file-io")]
use std::fs;
#[cfg(feature = "file-io")]
use std::path::PathBuf;

use crate::PlanLine;
use crate::models::{HourlyMetrics, PerformancePercentiles};

#[derive(Debug)]
pub struct RegexPatterns {
    pub log_line_regex: Regex,
    pub duration_regex: Regex,
    pub plan_regex: Regex,
    pub placeholder_regex: Regex,
}

/// Matches the timestamp a `%m` or `%t` log_line_prefix puts first on the
/// line: date, time with optional fractional seconds (%t has none), and an
/// optional timezone token (abbreviation like "UTC"/"PDT" or numeric offset
/// like "+02"/"-05:30"). The timezone stays inside capture group 1 so group 2
/// remains the message for all existing callers; parse_timestamp() consumes
/// the zone.
pub(crate) const LOG_LINE_PATTERN: &str = r"^(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:\.\d{1,6})?(?: (?:[A-Z]{2,5}|[+-]\d{2}(?::?\d{2})?))?)(.*)";

/// Cheap byte-level check for a `YYYY-MM-DD HH:MM:SS` line prefix — the shape
/// every `%m`/`%t`-prefixed PostgreSQL log line starts with. This is the
/// single definition shared by the exporter's checkpoint boundary scan and
/// the pg extension's ingest offset logic; keep it consistent with
/// [`LOG_LINE_PATTERN`].
pub fn is_log_line_start(line: &[u8]) -> bool {
    if line.len() < 19 {
        return false;
    }
    let digit = |i: usize| line[i].is_ascii_digit();
    digit(0)
        && digit(1)
        && digit(2)
        && digit(3)
        && line[4] == b'-'
        && digit(5)
        && digit(6)
        && line[7] == b'-'
        && digit(8)
        && digit(9)
        && line[10] == b' '
        && digit(11)
        && digit(12)
        && line[13] == b':'
        && digit(14)
        && digit(15)
        && line[16] == b':'
        && digit(17)
        && digit(18)
}

impl RegexPatterns {
    pub fn new() -> Self {
        Self {
            log_line_regex: Regex::new(LOG_LINE_PATTERN).unwrap(),
            duration_regex: Regex::new(r"duration: ([\d.]+) ms\s+plan:\s*$").unwrap(),
            plan_regex: Regex::new(r"\(cost=[\d.]+\.\.[\d.]+\s+rows=\d+\s+width=\d+\)").unwrap(),
            placeholder_regex: Regex::new(r"\$\d+").unwrap(),
        }
    }
}

impl Default for RegexPatterns {
    fn default() -> Self {
        Self::new()
    }
}

// These functions have been removed. Use the new SQL analysis module instead:
// - normalize_query_enhanced() for query normalization
// - calculate_query_fingerprint() for query fingerprinting

/// Resolves a PostgreSQL log timezone *abbreviation* ("CST", "IST", ...) to an
/// offset east of UTC. Abbreviations are inherently ambiguous — "CST" is US
/// Central (-6) in PostgreSQL's *Default* tznames file but China Standard Time
/// (+8) elsewhere; "IST" is Indian/Israeli/Irish — so a server whose
/// `log_timezone` prints a conflicting abbreviation needs to override the
/// built-in interpretation, otherwise every timestamp is silently shifted by
/// hours. Numeric offsets in the log (+02, -05:30) are unambiguous and never
/// consult a resolver. The default resolver reproduces the built-in
/// Default-tznames table, so the zero-config behavior is unchanged.
#[derive(Debug, Clone, Default)]
pub struct TimezoneResolver {
    /// Applied to *any* abbreviation the override map does not name — the
    /// server's known single `log_timezone` offset. `None` falls through to the
    /// built-in Default-tznames table.
    fixed_offset_seconds: Option<i32>,
    /// Per-abbreviation overrides, e.g. `{"CST": 8 * 3600}` for China Standard
    /// Time. Consulted before the fixed offset and the built-in table.
    overrides: HashMap<String, i32>,
}

impl TimezoneResolver {
    /// A resolver that only consults the built-in Default-tznames table — the
    /// same as [`TimezoneResolver::default`] and what [`parse_timestamp`] uses.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interpret *every* non-numeric zone token as this fixed offset east of
    /// UTC (seconds), for a server whose single `log_timezone` an abbreviation
    /// would otherwise misresolve. Per-token entries added via
    /// [`Self::with_override`] still take precedence.
    pub fn with_fixed_offset_seconds(mut self, offset_seconds: i32) -> Self {
        self.fixed_offset_seconds = Some(offset_seconds);
        self
    }

    /// Map one abbreviation to an explicit offset east of UTC (seconds), e.g.
    /// `.with_override("CST", 8 * 3600)` to read "CST" as China Standard Time.
    ///
    /// The key is upper-cased on insert: PostgreSQL always prints zone
    /// abbreviations in upper case (the token matched at parse time), so this
    /// lets callers pass `"cst"` or `"Cst"` without a silent no-match, while
    /// keeping the parse-time lookup allocation-free.
    pub fn with_override(mut self, abbrev: impl Into<String>, offset_seconds: i32) -> Self {
        self.overrides
            .insert(abbrev.into().to_ascii_uppercase(), offset_seconds);
        self
    }

    /// Resolve a non-numeric abbreviation to seconds east of UTC: overrides win
    /// over the fixed offset, which wins over the built-in Default-tznames
    /// table. `None` means the token is unrecognized everywhere.
    fn resolve_abbreviation(&self, token: &str) -> Option<i32> {
        if let Some(&seconds) = self.overrides.get(token) {
            return Some(seconds);
        }
        if let Some(seconds) = self.fixed_offset_seconds {
            return Some(seconds);
        }
        default_abbreviation_offset_seconds(token)
    }
}

/// Parse a PostgreSQL log timestamp, honoring the timezone token `%m`/`%t`
/// append ("UTC", "PDT", "+02", "-05:30", ...). The wall-clock time is
/// converted to UTC using that zone's offset; timestamps without a
/// recognizable zone are assumed to already be UTC (with a warning for
/// unknown abbreviations, since silently shifting data is worse). Uses the
/// default resolver — see [`parse_timestamp_with_tz`] to override how ambiguous
/// abbreviations are interpreted.
pub fn parse_timestamp(timestamp_str: &str) -> anyhow::Result<DateTime<Utc>> {
    parse_timestamp_with_tz(timestamp_str, &TimezoneResolver::default())
}

/// Like [`parse_timestamp`], but resolves timezone abbreviations through the
/// supplied [`TimezoneResolver`] so callers on a server with an ambiguous
/// `log_timezone` can override the interpretation. Numeric offsets in the log
/// always win over any configured override.
pub fn parse_timestamp_with_tz(
    timestamp_str: &str,
    tz: &TimezoneResolver,
) -> anyhow::Result<DateTime<Utc>> {
    fn parse_naive(s: &str) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
            .ok()
    }

    let trimmed = timestamp_str.trim();
    if let Some(naive_dt) = parse_naive(trimmed) {
        return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
    }
    if let Some((datetime_part, tz_token)) = trimmed.rsplit_once(' ')
        && let Some(naive_dt) = parse_naive(datetime_part)
    {
        // Numeric offsets in the log are unambiguous and always win over any
        // configured resolver; only abbreviations consult the resolver.
        let offset_seconds = numeric_token_offset_seconds(tz_token)
            .or_else(|| tz.resolve_abbreviation(tz_token))
            .unwrap_or_else(|| {
                warn_unknown_timezone_once(tz_token);
                0
            });
        // The naive value is wall-clock time at `offset` east of UTC.
        let utc_naive = naive_dt - Duration::seconds(offset_seconds as i64);
        return Ok(DateTime::from_naive_utc_and_offset(utc_naive, Utc));
    }
    anyhow::bail!("Unrecognized timestamp format: '{}'", timestamp_str)
}

/// Warn about an unrecognized timezone abbreviation only the first time it is
/// seen: parse_timestamp runs once per log line, and a large file with an
/// unknown zone would otherwise emit millions of identical warnings.
fn warn_unknown_timezone_once(token: &str) {
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    if let Ok(mut seen) = seen.lock()
        && seen.insert(token.to_string())
    {
        tracing::warn!(
            timezone = token,
            "Unknown log timezone abbreviation; assuming UTC for all timestamps carrying it"
        );
    }
}

/// Seconds east of UTC for an *unambiguous* numeric offset token ("+02",
/// "-0530", "+05:30"). Returns None for non-numeric tokens (abbreviations,
/// which resolve through a [`TimezoneResolver`] instead) — these offsets are
/// exact and always win over any configured override.
fn numeric_token_offset_seconds(token: &str) -> Option<i32> {
    if let Some(rest) = token.strip_prefix('+') {
        return numeric_offset_seconds(rest);
    }
    if let Some(rest) = token.strip_prefix('-') {
        return numeric_offset_seconds(rest).map(|s| -s);
    }
    None
}

/// Offset east of UTC in seconds for a timezone *abbreviation*, resolved the
/// way PostgreSQL's *Default* timezone_abbreviations file does for the
/// ambiguous ones (CST = US Central, BST = British Summer, IST = Indian). This
/// is the built-in fallback a [`TimezoneResolver`] consults after its
/// overrides; returns None for unknown tokens.
fn default_abbreviation_offset_seconds(token: &str) -> Option<i32> {
    // Offsets in half-hours to allow :30 zones in an integer table.
    let half_hours: i32 = match token {
        "UTC" | "GMT" | "UT" | "Z" | "ZULU" | "WET" => 0,
        // North America
        "HST" => -20,
        "AKST" => -18,
        "PST" | "AKDT" => -16,
        "PDT" | "MST" => -14,
        "MDT" | "CST" => -12,
        "CDT" | "EST" => -10,
        "EDT" | "AST" | "CLT" => -8,
        "NST" => -7,
        // South America
        "BRT" | "ART" => -6,
        // Europe / Africa
        "CET" | "BST" | "WEST" | "WAT" => 2,
        "CEST" | "EET" | "SAST" | "CAT" => 4,
        "EEST" | "MSK" | "EAT" => 6,
        // Asia / Pacific
        "PKT" => 10,
        "IST" => 11, // Indian Standard Time (PostgreSQL Default tznames)
        "ICT" | "WIB" => 14,
        "HKT" | "SGT" | "AWST" => 16,
        "JST" | "KST" => 18,
        "ACST" => 19,
        "AEST" => 20,
        "AEDT" => 22,
        "NZST" => 24,
        "NZDT" => 26,
        _ => return None,
    };
    Some(half_hours * 1800)
}

/// "02", "0530", or "05:30" → seconds.
fn numeric_offset_seconds(s: &str) -> Option<i32> {
    let (hours, minutes): (i32, i32) = match s.len() {
        2 => (s.parse().ok()?, 0),
        4 => (s[..2].parse().ok()?, s[2..].parse().ok()?),
        5 if s.as_bytes()[2] == b':' => (s[..2].parse().ok()?, s[3..].parse().ok()?),
        _ => return None,
    };
    if hours > 15 || minutes > 59 {
        return None;
    }
    Some(hours * 3600 + minutes * 60)
}

pub fn parse_relative_date(date_str: &str) -> anyhow::Result<DateTime<Utc>> {
    let now = Utc::now();

    // Try parsing as absolute timestamp first
    if let Ok(dt) = parse_absolute_timestamp(date_str) {
        return Ok(dt);
    }

    // Parse relative time format (e.g., "2h", "3d", "1w")
    let regex = Regex::new(r"^(\d+)([smhdw])$")?;
    if let Some(caps) = regex.captures(date_str) {
        let amount: i64 = caps.get(1).unwrap().as_str().parse()?;
        let unit = caps.get(2).unwrap().as_str();

        // Use the checked `try_*` builders: the unchecked ones panic on
        // overflow, which a huge user-supplied number (e.g. "99999999999w")
        // would trigger.
        let duration = match unit {
            "s" => Duration::try_seconds(amount),
            "m" => Duration::try_minutes(amount),
            "h" => Duration::try_hours(amount),
            "d" => Duration::try_days(amount),
            "w" => Duration::try_weeks(amount),
            _ => return Err(anyhow::anyhow!("Invalid time unit: {}", unit)),
        }
        .ok_or_else(|| anyhow::anyhow!("Relative time '{}' is out of range", date_str))?;

        return now
            .checked_sub_signed(duration)
            .ok_or_else(|| anyhow::anyhow!("Relative time '{}' is out of range", date_str));
    }

    Err(anyhow::anyhow!("Invalid date format: {}", date_str))
}

fn parse_absolute_timestamp(date_str: &str) -> anyhow::Result<DateTime<Utc>> {
    // Try various timestamp formats
    let formats = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
    ];

    for format in &formats {
        if let Ok(naive_dt) = NaiveDateTime::parse_from_str(date_str, format) {
            return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
        }
    }

    // Try date-only format
    if let Ok(naive_date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
        let naive_dt = naive_date.and_hms_opt(0, 0, 0).unwrap();
        return Ok(DateTime::from_naive_utc_and_offset(naive_dt, Utc));
    }

    Err(anyhow::anyhow!(
        "Could not parse absolute timestamp: {}",
        date_str
    ))
}

pub fn get_indent_level(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

pub fn format_plan_lines(plan_lines: &[PlanLine]) -> String {
    let mut plan = String::new();

    plan_lines.iter().for_each(|pl| {
        writeln!(
            &mut plan,
            "{:indent$}{content}",
            "",
            content = pl.query,
            indent = pl.indentation
        )
        .unwrap();
    });

    if !plan.is_empty() {
        plan.pop(); // Remove trailing newline
    }

    plan
}

pub fn format_sql_query(sql: &str) -> String {
    let dialect = PostgreSqlDialect {};

    match Parser::parse_sql(&dialect, sql) {
        Ok(statements) => {
            let mut formatted = String::new();
            for (i, statement) in statements.iter().enumerate() {
                if i > 0 {
                    formatted.push('\n');
                }
                // Use built-in pretty-printing with {:#} format specifier
                formatted.push_str(&format!("{:#}", statement));
            }
            formatted
        }
        Err(_) => sql.to_string(), // Return original if parsing fails
    }
}

pub struct QueryStatisticsCalculator;

impl QueryStatisticsCalculator {
    pub fn calculate_mean_and_std_dev(durations: &[f64]) -> (f64, f64) {
        if durations.is_empty() {
            return (0.0, 0.0);
        }

        // Serial: these duration slices are per query-group (typically a
        // handful to a few hundred values) and this runs *inside* the already
        // parallel group loop, so rayon's split/join overhead and nested-pool
        // contention dwarf the actual work.
        let mean = durations.iter().sum::<f64>() / durations.len() as f64;
        // Sample variance (N-1): these durations are a sample of the query's
        // executions, matching StatisticalCalculator::sample_variance.
        let variance = if durations.len() < 2 {
            0.0
        } else {
            durations.iter().map(|&d| (d - mean).powi(2)).sum::<f64>()
                / (durations.len() - 1) as f64
        };
        let std_dev = variance.sqrt();

        (mean, std_dev)
    }

    pub fn find_min_max(durations: &[f64]) -> (f64, f64) {
        if durations.is_empty() {
            return (0.0, 0.0);
        }

        let min = durations.iter().min_by(|a, b| a.total_cmp(b)).unwrap();
        let max = durations.iter().max_by(|a, b| a.total_cmp(b)).unwrap();

        (*min, *max)
    }

    pub fn calculate_percentiles(durations: &[f64]) -> PerformancePercentiles {
        if durations.is_empty() {
            return PerformancePercentiles {
                p25: 0.0,
                p50: 0.0,
                p90: 0.0,
                p95: 0.0,
                p99: 0.0,
            };
        }

        let mut sorted_durations = durations.to_vec();
        sorted_durations.sort_by(|a, b| a.total_cmp(b));

        PerformancePercentiles {
            p25: Self::percentile(&sorted_durations, 25.0),
            p50: Self::percentile(&sorted_durations, 50.0),
            p90: Self::percentile(&sorted_durations, 90.0),
            p95: Self::percentile(&sorted_durations, 95.0),
            p99: Self::percentile(&sorted_durations, 99.0),
        }
    }

    fn percentile(sorted_data: &[f64], percentile: f64) -> f64 {
        if sorted_data.is_empty() {
            return 0.0;
        }

        let index = (percentile / 100.0) * (sorted_data.len() - 1) as f64;
        let lower_index = index.floor() as usize;
        let upper_index = index.ceil() as usize;

        if lower_index == upper_index {
            sorted_data[lower_index]
        } else {
            let weight = index - lower_index as f64;
            sorted_data[lower_index] * (1.0 - weight) + sorted_data[upper_index] * weight
        }
    }

    pub fn generate_hourly_histogram(
        executions: &[crate::models::ExecutionRecord],
    ) -> HashMap<DateTime<Utc>, HourlyMetrics> {
        let mut histogram = HashMap::new();

        for execution in executions {
            // Truncate to hour precision (set minutes, seconds, nanoseconds to 0)
            let hour_key = execution
                .timestamp
                .with_minute(0)
                .unwrap()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap();

            let entry = histogram.entry(hour_key).or_insert(HourlyMetrics {
                count: 0,
                total_duration_ms: 0.0,
                min_duration_ms: execution.duration_ms,
                max_duration_ms: execution.duration_ms,
                mean_duration_ms: 0.0,
            });

            entry.count += 1;
            entry.total_duration_ms += execution.duration_ms;
            entry.min_duration_ms = entry.min_duration_ms.min(execution.duration_ms);
            entry.max_duration_ms = entry.max_duration_ms.max(execution.duration_ms);
        }

        // Calculate mean for each hour
        for metrics in histogram.values_mut() {
            metrics.mean_duration_ms = metrics.total_duration_ms / metrics.count as f64;
        }

        histogram
    }
}

pub fn parse_duration_from_line(line: &str, duration_regex: &Regex) -> Option<f64> {
    duration_regex
        .captures(line)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok())
}

#[cfg(feature = "file-io")]
fn expand_path(folder_path: &PathBuf) -> Vec<PathBuf> {
    if !folder_path.exists() {
        return vec![];
    }
    if folder_path.is_dir() {
        // Don't panic if the directory becomes unreadable mid-walk; treat it as
        // empty instead.
        match fs::read_dir(folder_path) {
            Ok(entries) => entries
                .flatten()
                .flat_map(|entry| expand_path(&entry.path()))
                .collect(),
            Err(_) => vec![],
        }
    } else {
        vec![folder_path.clone()]
    }
}

#[cfg(feature = "file-io")]
pub fn expand_files(file_paths: &[PathBuf]) -> Vec<PathBuf> {
    file_paths
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).as_ref().map(expand_path).ok())
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_query_new_approach() {
        use crate::sql_analysis::normalize_query_enhanced;
        let query = "SELECT * FROM users WHERE id = 123 AND status = 'active'";
        let result = normalize_query_enhanced(query).expect("Normalization should succeed");
        assert!(result.successful);
        assert_eq!(result.parameter_count, 2);
        assert!(result.normalized_sql.contains("$1"));
        assert!(result.normalized_sql.contains("$2"));
    }

    #[test]
    fn test_get_indent_level() {
        assert_eq!(get_indent_level("    test"), 4);
        assert_eq!(get_indent_level("\t\ttest"), 2);
        assert_eq!(get_indent_level("test"), 0);
    }

    #[test]
    fn test_indentation_preservation() {
        // Test with original log file line that has tab + 2 spaces indentation
        let original_line = "\t  ->  Index Scan Backward using \"IX_VitalAlarms_EndDate\" on \"Shared\".\"VitalAlarms\" v  (cost=0.43..95610.13 rows=159718 width=56)";

        // Create PlanLine with the original line
        let plan_line = PlanLine::new(original_line);

        // The indentation should be counted correctly (1 tab + 2 spaces = 3)
        assert_eq!(plan_line.indentation, 3);

        // The query field stores trimmed content (normalized)
        let trimmed_content = "->  Index Scan Backward using \"IX_VitalAlarms_EndDate\" on \"Shared\".\"VitalAlarms\" v  (cost=0.43..95610.13 rows=159718 width=56)";
        assert_eq!(plan_line.query, trimmed_content);

        // Test format_plan_lines - reconstructs with normalized spaces (not tabs)
        let plan_lines = vec![plan_line];
        let formatted = format_plan_lines(&plan_lines);

        // Should reconstruct with 3 spaces (normalized from tab + 2 spaces)
        let expected = "   ->  Index Scan Backward using \"IX_VitalAlarms_EndDate\" on \"Shared\".\"VitalAlarms\" v  (cost=0.43..95610.13 rows=159718 width=56)";
        assert_eq!(formatted.trim_end(), expected);
    }

    #[test]
    fn test_parse_timestamp() {
        let timestamp_str = "2024-01-01 10:30:45.123";
        let result = parse_timestamp(timestamp_str);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_timestamp_honors_timezone_token() {
        // %m prints a zone after the time; the instant must convert to UTC.
        let utc = parse_timestamp("2025-01-15 10:30:00.123 UTC").unwrap();
        assert_eq!(utc.to_rfc3339(), "2025-01-15T10:30:00.123+00:00");

        let pdt = parse_timestamp("2025-06-15 10:30:00.123 PDT").unwrap();
        assert_eq!(pdt.to_rfc3339(), "2025-06-15T17:30:00.123+00:00");

        let cest = parse_timestamp("2025-06-15 10:30:00.123 CEST").unwrap();
        assert_eq!(cest.to_rfc3339(), "2025-06-15T08:30:00.123+00:00");

        let plus2 = parse_timestamp("2025-06-15 10:30:00.123 +02").unwrap();
        assert_eq!(plus2.to_rfc3339(), "2025-06-15T08:30:00.123+00:00");

        let ist = parse_timestamp("2025-06-15 10:30:00.123 +05:30").unwrap();
        assert_eq!(ist.to_rfc3339(), "2025-06-15T05:00:00.123+00:00");

        let minus0530 = parse_timestamp("2025-06-15 10:30:00.123 -0530").unwrap();
        assert_eq!(minus0530.to_rfc3339(), "2025-06-15T16:00:00.123+00:00");

        // Unknown abbreviation: assume UTC rather than failing.
        let unknown = parse_timestamp("2025-06-15 10:30:00.123 XKCD").unwrap();
        assert_eq!(unknown.to_rfc3339(), "2025-06-15T10:30:00.123+00:00");
    }

    #[test]
    fn test_timezone_override_remaps_abbreviation() {
        // A server whose log_timezone is China Standard Time prints "CST"; the
        // built-in table reads that as US Central (-6), so the user overrides it
        // to +8. 10:30 CST(+8) is 02:30 UTC.
        let tz = TimezoneResolver::new().with_override("CST", 8 * 3600);
        let china = parse_timestamp_with_tz("2025-06-15 10:30:00.123 CST", &tz).unwrap();
        assert_eq!(china.to_rfc3339(), "2025-06-15T02:30:00.123+00:00");

        // The default resolver still reads "CST" as US Central (-6): 10:30
        // becomes 16:30 UTC. Confirms the override is scoped to the resolver.
        let default = parse_timestamp("2025-06-15 10:30:00.123 CST").unwrap();
        assert_eq!(default.to_rfc3339(), "2025-06-15T16:30:00.123+00:00");
    }

    #[test]
    fn test_numeric_offset_wins_over_override() {
        // A numeric offset in the log is unambiguous and must win regardless of
        // config — even an override keyed on the same digits is irrelevant.
        let tz = TimezoneResolver::new()
            .with_fixed_offset_seconds(8 * 3600)
            .with_override("+02", 99 * 3600);
        let plus2 = parse_timestamp_with_tz("2025-06-15 10:30:00.123 +02", &tz).unwrap();
        assert_eq!(plus2.to_rfc3339(), "2025-06-15T08:30:00.123+00:00");

        let minus0530 = parse_timestamp_with_tz("2025-06-15 10:30:00.123 -0530", &tz).unwrap();
        assert_eq!(minus0530.to_rfc3339(), "2025-06-15T16:00:00.123+00:00");
    }

    #[test]
    fn test_fixed_offset_applies_to_any_abbreviation() {
        // A fixed offset stands in for the built-in table for every
        // abbreviation the override map does not name.
        let tz = TimezoneResolver::new().with_fixed_offset_seconds(3 * 3600);
        // "MSK" would be +3 in the built-in table anyway; use a zone the fixed
        // offset actually changes: "PDT" is -7 by default.
        let pdt = parse_timestamp_with_tz("2025-06-15 10:30:00.123 PDT", &tz).unwrap();
        assert_eq!(pdt.to_rfc3339(), "2025-06-15T07:30:00.123+00:00");
        // Per-token override still beats the fixed offset.
        let tz = tz.with_override("PDT", -7 * 3600);
        let pdt = parse_timestamp_with_tz("2025-06-15 10:30:00.123 PDT", &tz).unwrap();
        assert_eq!(pdt.to_rfc3339(), "2025-06-15T17:30:00.123+00:00");
    }

    #[test]
    fn test_default_resolver_matches_builtin_table() {
        // The default (no override) must be identical to the built-in behavior:
        // every branch of parse_timestamp still resolves exactly as before.
        let tz = TimezoneResolver::default();
        for (input, expected) in [
            (
                "2025-06-15 10:30:00.123 UTC",
                "2025-06-15T10:30:00.123+00:00",
            ),
            (
                "2025-06-15 10:30:00.123 PDT",
                "2025-06-15T17:30:00.123+00:00",
            ),
            (
                "2025-06-15 10:30:00.123 CEST",
                "2025-06-15T08:30:00.123+00:00",
            ),
            (
                "2025-06-15 10:30:00.123 +02",
                "2025-06-15T08:30:00.123+00:00",
            ),
            (
                "2025-06-15 10:30:00.123 +05:30",
                "2025-06-15T05:00:00.123+00:00",
            ),
        ] {
            assert_eq!(
                parse_timestamp_with_tz(input, &tz).unwrap().to_rfc3339(),
                expected
            );
            // The convenience wrapper must agree with the explicit default.
            assert_eq!(parse_timestamp(input).unwrap().to_rfc3339(), expected);
        }
    }

    #[test]
    fn test_unknown_zone_still_assumed_utc_with_override() {
        // A token neither overridden, covered by a fixed offset, nor in the
        // built-in table is still assumed UTC rather than failing.
        let tz = TimezoneResolver::new().with_override("CST", 8 * 3600);
        let unknown = parse_timestamp_with_tz("2025-06-15 10:30:00.123 XKCD", &tz).unwrap();
        assert_eq!(unknown.to_rfc3339(), "2025-06-15T10:30:00.123+00:00");
    }

    #[test]
    fn test_parse_timestamp_second_precision() {
        // %t prints second precision without a fractional part.
        let t = parse_timestamp("2025-06-15 10:30:00 UTC").unwrap();
        assert_eq!(t.to_rfc3339(), "2025-06-15T10:30:00+00:00");
    }

    #[test]
    fn test_dst_abbreviations_resolve_distinctly() {
        // PostgreSQL prints the DST-aware abbreviation (CET in winter, CEST in
        // summer), so daylight time is carried by the token itself — the two
        // must resolve to different offsets rather than one "Central Europe".
        let cet = parse_timestamp("2025-01-15 10:30:00 CET").unwrap(); // +1
        assert_eq!(cet.to_rfc3339(), "2025-01-15T09:30:00+00:00");
        let cest = parse_timestamp("2025-06-15 10:30:00 CEST").unwrap(); // +2
        assert_eq!(cest.to_rfc3339(), "2025-06-15T08:30:00+00:00");
        // Likewise US Eastern: EST (-5) vs EDT (-4).
        let est = parse_timestamp("2025-01-15 10:30:00 EST").unwrap();
        assert_eq!(est.to_rfc3339(), "2025-01-15T15:30:00+00:00");
        let edt = parse_timestamp("2025-06-15 10:30:00 EDT").unwrap();
        assert_eq!(edt.to_rfc3339(), "2025-06-15T14:30:00+00:00");
    }

    #[test]
    fn test_fractional_hour_offsets() {
        // Half-hour zones from the built-in table: IST is +05:30, NST is -03:30.
        let ist = parse_timestamp("2025-06-15 10:30:00 IST").unwrap();
        assert_eq!(ist.to_rfc3339(), "2025-06-15T05:00:00+00:00");
        let nst = parse_timestamp("2025-06-15 10:30:00 NST").unwrap();
        assert_eq!(nst.to_rfc3339(), "2025-06-15T14:00:00+00:00");
        // 45-minute zones aren't in the half-hour table, but an unambiguous
        // numeric offset in the log carries them exactly (Nepal, +05:45).
        let npt = parse_timestamp("2025-06-15 10:30:00 +05:45").unwrap();
        assert_eq!(npt.to_rfc3339(), "2025-06-15T04:45:00+00:00");
        // ...and a caller can name the abbreviation via an override (seconds).
        let tz = TimezoneResolver::new().with_override("NPT", 5 * 3600 + 45 * 60);
        let npt = parse_timestamp_with_tz("2025-06-15 10:30:00 NPT", &tz).unwrap();
        assert_eq!(npt.to_rfc3339(), "2025-06-15T04:45:00+00:00");
    }

    #[test]
    fn test_override_key_is_case_insensitive() {
        // PostgreSQL emits upper-case tokens; a lower/mixed-case override key
        // must still match rather than silently falling through to the table.
        let tz = TimezoneResolver::new().with_override("cSt", 8 * 3600);
        let china = parse_timestamp_with_tz("2025-06-15 10:30:00 CST", &tz).unwrap();
        assert_eq!(china.to_rfc3339(), "2025-06-15T02:30:00+00:00");
    }

    #[test]
    fn test_parser_honors_timezone_override_end_to_end() {
        // Behavioral check through the real parser hot loop: a log written by a
        // China-Standard-Time server (token "CST") must land at +8, not the
        // built-in US-Central -6, once the parser carries the override.
        use crate::log_parser::PostgreSQLLogParser;
        let log = "2025-06-15 10:30:00.000 CST [1] LOG:  duration: 5.0 ms  plan:\n\
                   \tQuery Text: SELECT 1\n\
                   \tResult  (cost=0.00..0.01 rows=1 width=4)\n\
                   2025-06-15 10:30:01.000 CST [1] LOG:  done\n";

        let tz = TimezoneResolver::new().with_override("CST", 8 * 3600);
        let plans = PostgreSQLLogParser::new()
            .with_timezone_override(tz)
            .parse_string_with_progress(log, |_, _| {})
            .unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(
            plans[0].timestamp().to_rfc3339(),
            "2025-06-15T02:30:00+00:00"
        );

        // Default parser reads the same token as US Central (-6): 16:30 UTC.
        let plans = PostgreSQLLogParser::new()
            .parse_string_with_progress(log, |_, _| {})
            .unwrap();
        assert_eq!(
            plans[0].timestamp().to_rfc3339(),
            "2025-06-15T16:30:00+00:00"
        );
    }

    #[test]
    fn test_log_line_regex_matches_zone_and_second_precision() {
        let patterns = RegexPatterns::new();
        for line in [
            "2025-06-12 00:00:16.915 UTC [3416548] LOG:  duration: 1242.373 ms  plan:",
            "2025-06-12 00:00:16.915 PDT [1] LOG:  x",
            "2025-06-12 00:00:16 CEST [1] LOG:  x", // %t precision
            "2025-06-12 00:00:16.915 +02 [1] LOG:  x",
            "2025-06-12 00:00:16.915234 UTC [1] LOG:  x", // microseconds
        ] {
            let caps = patterns
                .log_line_regex
                .captures(line)
                .unwrap_or_else(|| panic!("regex must match: {}", line));
            assert!(
                parse_timestamp(caps.get(1).unwrap().as_str()).is_ok(),
                "timestamp must parse: {}",
                caps.get(1).unwrap().as_str()
            );
            assert!(
                caps.get(2).unwrap().as_str().contains("[1]")
                    || caps.get(2).unwrap().as_str().contains("[3416548]"),
                "message must contain the pid part: {:?}",
                caps.get(2).unwrap().as_str()
            );
        }
    }

    #[test]
    fn test_statistics_calculation() {
        let durations = vec![100.0, 200.0, 300.0, 400.0, 500.0];
        let (mean, std_dev) = QueryStatisticsCalculator::calculate_mean_and_std_dev(&durations);
        assert_eq!(mean, 300.0);
        // Sample std dev (N-1): sqrt(100000/4) ≈ 158.11
        assert!((std_dev - 158.11).abs() < 0.1);

        let (min, max) = QueryStatisticsCalculator::find_min_max(&durations);
        assert_eq!(min, 100.0);
        assert_eq!(max, 500.0);
    }

    #[test]
    fn test_parse_relative_date() {
        // Test relative dates
        assert!(parse_relative_date("2h").is_ok());
        assert!(parse_relative_date("3d").is_ok());
        assert!(parse_relative_date("1w").is_ok());
        assert!(parse_relative_date("30m").is_ok());
        assert!(parse_relative_date("45s").is_ok());

        // Test invalid formats
        assert!(parse_relative_date("2x").is_err());
        assert!(parse_relative_date("invalid").is_err());
        assert!(parse_relative_date("").is_err());

        // Test absolute dates
        assert!(parse_relative_date("2024-01-01T10:30:00").is_ok());
        assert!(parse_relative_date("2024-01-01 10:30:00").is_ok());
        assert!(parse_relative_date("2024-01-01").is_ok());
    }

    #[test]
    fn test_calculate_percentiles() {
        let durations = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&durations);

        assert_eq!(percentiles.p25, 3.25); // 25th percentile
        assert_eq!(percentiles.p50, 5.5); // Median
        assert!((percentiles.p90 - 9.1).abs() < 0.001);
        assert!((percentiles.p95 - 9.55).abs() < 0.001);
        assert!((percentiles.p99 - 9.91).abs() < 0.001);

        // Test empty case
        let empty_percentiles = QueryStatisticsCalculator::calculate_percentiles(&[]);
        assert_eq!(empty_percentiles.p25, 0.0);
        assert_eq!(empty_percentiles.p50, 0.0);
        assert_eq!(empty_percentiles.p90, 0.0);
        assert_eq!(empty_percentiles.p95, 0.0);
        assert_eq!(empty_percentiles.p99, 0.0);
    }

    #[test]
    fn test_percentile_calculation() {
        // Test with single value
        let single = vec![42.0];
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&single);
        assert_eq!(percentiles.p25, 42.0);
        assert_eq!(percentiles.p50, 42.0);
        assert_eq!(percentiles.p90, 42.0);
        assert_eq!(percentiles.p95, 42.0);
        assert_eq!(percentiles.p99, 42.0);

        // Test with two values
        let two = vec![10.0, 20.0];
        let percentiles = QueryStatisticsCalculator::calculate_percentiles(&two);
        assert_eq!(percentiles.p50, 15.0); // Average of 10 and 20
    }

    #[test]
    fn test_generate_hourly_histogram() {
        use crate::models::ExecutionRecord;
        use chrono::TimeZone;

        let executions = vec![
            ExecutionRecord {
                timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 10, 30, 0).unwrap(),
                duration_ms: 100.0,
            },
            ExecutionRecord {
                timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 10, 45, 0).unwrap(),
                duration_ms: 200.0,
            },
            ExecutionRecord {
                timestamp: Utc.with_ymd_and_hms(2024, 1, 1, 11, 15, 0).unwrap(),
                duration_ms: 300.0,
            },
        ];

        let histogram = QueryStatisticsCalculator::generate_hourly_histogram(&executions);

        // Check 10th hour
        let hour_10_key = Utc.with_ymd_and_hms(2024, 1, 1, 10, 0, 0).unwrap();
        let hour_10 = histogram.get(&hour_10_key).unwrap();
        assert_eq!(hour_10.count, 2);
        assert_eq!(hour_10.total_duration_ms, 300.0);
        assert_eq!(hour_10.min_duration_ms, 100.0);
        assert_eq!(hour_10.max_duration_ms, 200.0);
        assert_eq!(hour_10.mean_duration_ms, 150.0);

        // Check 11th hour
        let hour_11_key = Utc.with_ymd_and_hms(2024, 1, 1, 11, 0, 0).unwrap();
        let hour_11 = histogram.get(&hour_11_key).unwrap();
        assert_eq!(hour_11.count, 1);
        assert_eq!(hour_11.total_duration_ms, 300.0);
        assert_eq!(hour_11.min_duration_ms, 300.0);
        assert_eq!(hour_11.max_duration_ms, 300.0);
        assert_eq!(hour_11.mean_duration_ms, 300.0);

        // Should have exactly 2 hours
        assert_eq!(histogram.len(), 2);

        // Verify keys are properly truncated to hour precision
        for (hour_key, _) in histogram.iter() {
            assert_eq!(hour_key.minute(), 0);
            assert_eq!(hour_key.second(), 0);
            assert_eq!(hour_key.nanosecond(), 0);
        }
    }
}
