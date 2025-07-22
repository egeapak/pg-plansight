// Example showing the improved generic parsing API
use pg_loganalyze_core::PostgreSQLLogParser;
use std::io::{BufRead, Cursor};

fn main() -> anyhow::Result<()> {
    let sample_log = r#"2024-01-01 10:00:00.123 UTC [1234] LOG:  duration: 123.456 ms  plan:
2024-01-01 10:00:00.124 UTC [1234] LOG:  Query Text: SELECT * FROM users WHERE id = $1
2024-01-01 10:00:00.125 UTC [1234] LOG:  Seq Scan on users  (cost=0.00..100.00 rows=1 width=100)"#;

    let mut parser = PostgreSQLLogParser::new();
    
    println!("Demonstrating generic parse_with_progress API:");
    
    // 1. Parse from string using Cursor (most flexible)
    let cursor = Cursor::new(sample_log.as_bytes());
    let queries = parser.parse_with_progress(cursor, sample_log.len() as u64, |progress, delta| {
        if delta > 0 {
            println!("  Found {} queries (progress: {:.1}%)", delta, progress * 100.0);
        }
    })?;
    println!("  Total: {} query plans from cursor\n", queries.len());
    
    // 2. Parse from any BufRead implementor
    let reader = std::io::BufReader::new(Cursor::new(sample_log.as_bytes()));
    let queries = parser.parse_with_progress(reader, sample_log.len() as u64, |_, delta| {
        if delta > 0 {
            println!("  Processed {} more queries", delta);
        }
    })?;
    println!("  Total: {} query plans from BufReader\n", queries.len());
    
    // 3. The convenience methods still work and use the generic API internally
    let queries = parser.parse_string_with_progress(sample_log, |progress, _| {
        println!("  Convenience method progress: {:.1}%", progress * 100.0);
    })?;
    println!("  Total: {} query plans from convenience method", queries.len());
    
    // 4. Could also work with stdin, network streams, etc.
    let stdin = std::io::stdin();
    if false {  // Don't actually read stdin in this example
        let _queries = parser.parse_with_progress(stdin.lock(), 0, |_, _| {})?;
    }
    
    println!("\n✅ Generic parsing API works with any BufRead source!");
    Ok(())
}