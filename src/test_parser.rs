use crate::log_parser::PostgreSQLLogParser;

async fn test_parsing_async() {
    let parser = PostgreSQLLogParser::new();
    
    // Test file size detection
    let file_metadata = std::fs::metadata("postgresql-Wed.log").unwrap();
    println!("File size: {} bytes", file_metadata.len());
    
    println!("\n=== Testing EXACT TUI simulation ===");
    let start_tui = std::time::Instant::now();
    
    // Simulate exactly what the TUI does
    let file_path = std::path::PathBuf::from("postgresql-Wed.log");
    let (progress_sender, mut progress_receiver) = tokio::sync::mpsc::unbounded_channel();
    
    let task = tokio::spawn(async move {
        let parser = PostgreSQLLogParser::new();
        match parser.parse_file_with_progress(&file_path, move |progress| {
            let _ = progress_sender.send(progress);
        }) {
            Ok(queries) => {
                // Calculate statistics in the async task to avoid blocking UI
                let statistics = parser.get_query_statistics(&queries);
                Ok((queries, statistics))
            },
            Err(e) => Err(format!("Failed to parse log file: {}", e)),
        }
    });
    
    // Simulate progress checking like TUI does
    let mut progress_updates = 0;
    loop {
        // Check for progress updates
        while let Ok(_progress) = progress_receiver.try_recv() {
            progress_updates += 1;
        }
        
        if task.is_finished() {
            match task.await {
                Ok(Ok((queries, statistics))) => {
                    let tui_duration = start_tui.elapsed();
                    println!("TUI SIMULATION: Successfully parsed {} queries in {:.2?}", queries.len(), tui_duration);
                    println!("TUI SIMULATION: Progress updates received: {}", progress_updates);
                    println!("Statistics: {} unique queries", statistics.unique_queries);
                    break;
                }
                Ok(Err(err)) => {
                    println!("TUI SIMULATION: Error: {}", err);
                    break;
                }
                Err(join_err) => {
                    println!("TUI SIMULATION: Task error: {}", join_err);
                    break;
                }
            }
        }
        
        // Small delay to simulate TUI update frequency
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

pub fn test_parsing() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(test_parsing_async());
}

pub fn test_simple_parsing() {
    let parser = PostgreSQLLogParser::new();
    
    // Create a test log content matching the actual format
    let test_content = r#"2025-05-28 00:03:45.571 UTC [1769930] LOG:  duration: 6184.126 ms  plan:
        Query Text: SELECT v."Id", v."Comment", v."DeviceId", v."MeasurementTypeId", v."Value", v0."Id", v0."AcceptanceId", v0."CreatedDate", v0."DeviceName", v0."IsValidated", v0."MeasuredDate", v0."ValidatedById", v0."ValidationDate", v0."VentilationMode"
        FROM "Shared"."VentilatorMeasurements" AS v
        INNER JOIN "Shared"."Ventilators" AS v0 ON v."DeviceId" = v0."Id"
        WHERE v0."Id" = ANY ($1) AND v."MeasurementTypeId" = ANY ($2)
        ORDER BY v0."Id"
        Gather Merge  (cost=644222.66..644375.38 rows=1328 width=109)
          ->  Sort  (cost=644222.66..644224.32 rows=664 width=109)
                Sort Key: v0."Id"
                ->  Hash Join  (cost=1.27..644189.41 rows=664 width=109)
                      Hash Cond: (v."DeviceId" = v0."Id")
                      ->  Seq Scan on "VentilatorMeasurements" v  (cost=0.00..644162.50 rows=3982 width=41)
                            Filter: ("MeasurementTypeId" = ANY ($2))
                      ->  Hash  (cost=1.25..1.25 rows=2 width=76)
                            ->  Seq Scan on "Ventilators" v0  (cost=0.00..1.25 rows=2 width=76)
                                  Filter: ("Id" = ANY ($1))
2025-05-28 00:03:45.572 UTC [1769930] LOG:  some other log entry"#;
    
    // Write to a temporary file
    std::fs::write("debug_test_log.txt", test_content).unwrap();
    
    match parser.parse_file("debug_test_log.txt") {
        Ok(queries) => {
            println!("Successfully parsed {} queries", queries.len());
            for (i, query) in queries.iter().enumerate() {
                println!("Query {}: ", i + 1);
                println!("  Duration: {} ms", query.duration_ms);
                println!("  Query Text: '{}'", query.query_text);
                println!("  Plan: '{}'", query.plan);
                println!("  Parameters: {:?}", query.parameters);
                println!();
            }
        }
        Err(e) => {
            println!("Error parsing file: {}", e);
        }
    }
    
    // Clean up
    let _ = std::fs::remove_file("debug_test_log.txt");
}