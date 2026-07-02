//! Parses a committed fixture shaped like real PostgreSQL auto_explain output
//! (text format with ANALYZE actuals, a JSON-object-format entry, a multi-line
//! query containing a JSON literal, non-UTC timezones, and interleaved noise
//! lines) so parser changes are exercised against the real format in CI, not
//! only hand-written inline strings.

use pg_plansight_core::log_parser::PostgreSQLLogParser;

const FIXTURE: &str = include_str!("fixtures/auto_explain_real.log");

#[test]
fn fixture_parses_all_entries() {
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(FIXTURE, |_, _| {})
        .expect("fixture must parse");

    assert_eq!(plans.len(), 3, "all three auto_explain entries must parse");
}

#[test]
fn fixture_text_entry_has_actuals_and_query() {
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(FIXTURE, |_, _| {})
        .unwrap();

    let text_plan = &plans[0];
    assert!(text_plan.is_text_plan());
    assert_eq!(text_plan.duration_ms(), 1242.373);
    assert!(text_plan.query_text().contains("VitalAlarms"));
    let actuals = text_plan
        .parsed
        .root
        .actuals
        .as_ref()
        .expect("ANALYZE actuals must be extracted from text plans");
    assert_eq!(actuals.actual_rows, Some(1000));
}

#[test]
fn fixture_json_object_entry_parses_with_embedded_query() {
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(FIXTURE, |_, _| {})
        .unwrap();

    let json_plan = &plans[1];
    assert!(json_plan.is_json_plan(), "log_format=json entry must parse");
    assert_eq!(json_plan.query_text(), "SELECT * FROM users WHERE id = 42");
    assert_eq!(json_plan.duration_ms(), 150.5);
}

#[test]
fn fixture_non_utc_timezone_converts() {
    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_string_with_progress(FIXTURE, |_, _| {})
        .unwrap();

    // 02:10 CEST == 00:10 UTC.
    let cest_entry = &plans[2];
    assert_eq!(
        cest_entry.timestamp().to_rfc3339(),
        "2025-06-12T00:10:00.123+00:00"
    );
    assert!(
        cest_entry.query_text().contains("{\"status\": \"active\"}"),
        "JSON literal must stay in the query text"
    );
}
