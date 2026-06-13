//! Adversarial / malformed-input regression tests.
//!
//! These lock in the hardening fixes for crash and resource-exhaustion bugs
//! that were reachable from untrusted PostgreSQL log files: deeply-nested plan
//! trees (stack overflow), lone-quote slicing panics, corrupt compressed
//! streams, and degenerate numeric values. The bar for most of these is simply
//! "parses without panicking / aborting".

use flate2::Compression;
use flate2::write::GzEncoder;
use pg_loganalyze_core::PostgreSQLLogParser;
use std::io::Write;
use tempfile::NamedTempFile;

fn parse_str(content: &str) -> Vec<pg_loganalyze_core::QueryPlan> {
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(content.as_bytes()).unwrap();
    temp.flush().unwrap();
    let mut parser = PostgreSQLLogParser::new();
    parser
        .parse_file_with_progress(temp.path(), |_, _| {})
        .expect("parser must not error on adversarial input")
}

/// A pathologically deep text plan must not overflow the stack. Before the
/// depth cap this recursion was unbounded; the parser should now bail
/// gracefully (yielding no plan) rather than aborting the process.
#[test]
fn deeply_nested_text_plan_does_not_stack_overflow() {
    let mut content = String::from(
        "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 10.0 ms  plan:\n\
         Query Text: SELECT 1\n",
    );
    // ~4000 logical levels of nesting (2 spaces per level), each a node line
    // carrying a cost so it is treated as a plan node.
    for depth in 0..4000 {
        let indent = " ".repeat(depth * 2);
        content.push_str(&format!(
            "{indent}Nested Loop  (cost=0.42..851.21 rows=822 width=16)\n"
        ));
    }

    // The assertion is implicit: returning at all means we did not overflow.
    let _plans = parse_str(&content);
}

/// A quoted identifier that is a single lone quote character previously sliced
/// `[1..0]` and panicked. It must now parse cleanly.
#[test]
fn lone_quote_identifier_does_not_panic() {
    let content = "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 5.0 ms  plan:\n\
         Query Text: SELECT 1\n\
         Index Scan using \" on \"  (cost=0.42..851.21 rows=1 width=16)\n";
    let _plans = parse_str(content);
}

/// A truncated/corrupt gzip stream must surface as a normal error path, never a
/// panic, and the decompression-bomb guard must keep us from reading without
/// bound.
#[test]
fn corrupt_gzip_stream_does_not_panic() {
    // Valid gzip magic bytes followed by garbage so decoding fails mid-stream.
    let mut temp = NamedTempFile::new().unwrap();
    temp.write_all(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00])
        .unwrap();
    temp.write_all(b"this is not a valid deflate body").unwrap();
    temp.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    // Either Ok(empty) or Err is acceptable; the contract is "no panic".
    let _ = parser.parse_file_with_progress(temp.path(), |_, _| {});
}

/// A well-formed gzip log round-trips through the size-capped decoder.
#[test]
fn valid_gzip_log_still_parses_after_bomb_guard() {
    let log = "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 12.5 ms  plan:\n\
         Query Text: SELECT * FROM t WHERE id = $1\n\
         Seq Scan on t  (cost=0.00..1.10 rows=10 width=4)\n";
    let mut temp = NamedTempFile::new().unwrap();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(log.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    temp.write_all(&compressed).unwrap();
    temp.flush().unwrap();

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser
        .parse_file_with_progress(temp.path(), |_, _| {})
        .expect("valid gzip should parse");
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].duration_ms(), 12.5);
}

/// JSON plans whose string literals contain `]`/`}` must not trip the
/// incremental bracket-depth completion scanner into closing early.
#[test]
fn json_plan_with_brackets_in_strings_parses() {
    let content = "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 7.0 ms  plan:\n\
         Query Text: SELECT 1\n\
         [\n\
         {\n\
         \"Plan\": {\n\
         \"Node Type\": \"Seq Scan\",\n\
         \"Relation Name\": \"weird]}name\",\n\
         \"Filter\": \"(col = '}]['::text)\",\n\
         \"Startup Cost\": 0.00,\n\
         \"Total Cost\": 1.50,\n\
         \"Plan Rows\": 1,\n\
         \"Plan Width\": 4\n\
         }\n\
         }\n\
         ]\n";
    let plans = parse_str(content);
    assert_eq!(plans.len(), 1, "plan with brackets-in-strings should parse");
    assert!(plans[0].is_json_plan());
}

/// A degenerate duration value must not panic the numeric paths.
#[test]
fn huge_duration_value_does_not_panic() {
    let content = "2025-06-25 00:03:51.601 UTC [1] LOG:  duration: 99999999999999.999 ms  plan:\n\
         Query Text: SELECT 1\n\
         Seq Scan on t  (cost=0.00..1.10 rows=10 width=4)\n";
    let _plans = parse_str(content);
}
