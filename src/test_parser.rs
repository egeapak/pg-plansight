use crate::log_parser::PostgreSQLLogParser;

pub fn test_parsing() {
    let parser = PostgreSQLLogParser::new();
    
    // Test file size detection
    let file_metadata = std::fs::metadata("postgresql-Wed.log").unwrap();
    println!("File size: {} bytes", file_metadata.len());
    
    println!("Testing parser performance with real progress...");
    let start = std::time::Instant::now();
    
    match parser.parse_file("postgresql-Wed.log") {
        Ok(queries) => {
            let parse_duration = start.elapsed();
            println!("Successfully parsed {} queries in {:.2?}", queries.len(), parse_duration);
            
            let stats_start = std::time::Instant::now();
            let statistics = parser.get_query_statistics(&queries);
            let stats_duration = stats_start.elapsed();
            println!("Statistics calculated in {:.2?}", stats_duration);
            
            println!("Total duration: {:.2} ms", statistics.total_duration_ms);
            println!("Average duration: {:.2} ms", statistics.average_duration_ms);
            println!("Unique queries: {}", statistics.unique_queries);
            
            for (i, query) in queries.iter().take(2).enumerate() {
                println!("Query {}: {:.2}ms", i + 1, query.duration_ms);
                println!("Text: '{}'", query.query_text.chars().take(100).collect::<String>());
                println!("---");
            }
        }
        Err(e) => {
            println!("Error parsing: {}", e);
        }
    }
}