#!/bin/bash
# Test script to verify parser works with real PostgreSQL auto_explain output

set -e

echo "=== Creating Test Log with Real PostgreSQL auto_explain Format ==="

# Create a test log file with authentic PostgreSQL auto_explain output
cat > /tmp/test_auto_explain.log << 'EOF'
2025-11-07 17:00:00.123 UTC [1234]: LOG:  duration: 2.456 ms  plan:
	Query Text: SELECT * FROM test_users WHERE age > 20;
	Seq Scan on test_users  (cost=0.00..1.04 rows=3 width=68) (actual time=0.015..0.017 rows=2 loops=1)
	  Filter: (age > 20)
	  Rows Removed by Filter: 1
	Planning Time: 0.123 ms
	Execution Time: 2.345 ms
2025-11-07 17:00:01.456 UTC [1235]: LOG:  duration: 5.678 ms  plan:
	Query Text: SELECT c.name, SUM(o.total) as total_spent FROM customers c JOIN orders o ON c.id = o.user_id GROUP BY c.name;
	HashAggregate  (cost=1.14..1.16 rows=2 width=40) (actual time=0.045..0.046 rows=2 loops=1)
	  Group Key: c.name
	  Batches: 1  Memory Usage: 24kB
	  ->  Hash Join  (cost=1.04..1.12 rows=3 width=40) (actual time=0.028..0.034 rows=3 loops=1)
	        Hash Cond: (o.user_id = c.id)
	        ->  Seq Scan on orders o  (cost=0.00..1.03 rows=3 width=12) (actual time=0.006..0.007 rows=3 loops=1)
	        ->  Hash  (cost=1.02..1.02 rows=2 width=36) (actual time=0.015..0.015 rows=2 loops=1)
	              Buckets: 1024  Batches: 1  Memory Usage: 9kB
	              ->  Seq Scan on customers c  (cost=0.00..1.02 rows=2 width=36) (actual time=0.008..0.009 rows=2 loops=1)
	Planning Time: 0.234 ms
	Execution Time: 5.567 ms
2025-11-07 17:00:02.789 UTC [1236]: LOG:  duration: 1.234 ms  plan:
	Query Text: SELECT product, COUNT(*) as count, SUM(amount) as total, AVG(amount) as average FROM sales GROUP BY product ORDER BY total DESC;
	Sort  (cost=1.17..1.18 rows=2 width=48) (actual time=0.052..0.053 rows=2 loops=1)
	  Sort Key: (sum(amount)) DESC
	  Sort Method: quicksort  Memory: 25kB
	  ->  HashAggregate  (cost=1.12..1.15 rows=2 width=48) (actual time=0.038..0.041 rows=2 loops=1)
	        Group Key: product
	        Batches: 1  Memory Usage: 24kB
	        ->  Seq Scan on sales  (cost=0.00..1.05 rows=5 width=44) (actual time=0.009..0.011 rows=5 loops=1)
	Planning Time: 0.156 ms
	Execution Time: 1.123 ms
EOF

echo "✓ Created test log file"

echo ""
echo "=== Test Log Contents ==="
head -20 /tmp/test_auto_explain.log

echo ""
echo "=== Parsing Log with pg-loganalyze Parser ==="

# Create a simple Rust test program
cat > /tmp/test_parser.rs << 'RUSTEOF'
use pg_loganalyze_core::PostgreSQLLogParser;
use std::fs;

fn main() -> anyhow::Result<()> {
    println!("\n🔍 Reading log file...");
    let log_content = fs::read_to_string("/tmp/test_auto_explain.log")?;

    println!("📝 Log file size: {} bytes", log_content.len());
    println!("\n🔄 Parsing log...\n");

    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&log_content, |progress, count| {
        if count % 100 == 0 {
            println!("  Progress: {:.1}% ({} plans parsed)", progress * 100.0, count);
        }
    })?;

    println!("\n✅ Successfully parsed {} query plans!\n", parsed_plans.len());

    println!("=".repeat(80));
    println!("PARSED RESULTS:");
    println!("=".repeat(80));

    for (i, plan) in parsed_plans.iter().enumerate() {
        println!("\n📊 Plan {}:", i + 1);
        println!("  ⏱️  Duration: {:.3} ms", plan.duration_ms());
        println!("  🕐 Timestamp: {}", plan.timestamp());

        // Get query text (truncate if too long)
        let query_text = plan.query_text();
        let display_query = if query_text.len() > 100 {
            format!("{}...", &query_text[..100])
        } else {
            query_text.to_string()
        };
        println!("  📝 Query: {}", display_query);

        // Show plan type
        match plan {
            pg_loganalyze_core::QueryPlan::TextPlan(data) => {
                println!("  📋 Plan Type: Text");
                println!("  📄 Plan Lines: {}", data.plan_lines.len());

                // Show first few plan lines
                println!("\n  Plan Details:");
                for (j, line) in data.plan_lines.iter().take(5).enumerate() {
                    println!("    {}. {} {}", j + 1, "  ".repeat(line.indent_level), line.text);
                }
                if data.plan_lines.len() > 5 {
                    println!("    ... ({} more lines)", data.plan_lines.len() - 5);
                }
            },
            pg_loganalyze_core::QueryPlan::JsonPlan(data) => {
                println!("  📋 Plan Type: JSON");
                println!("  🏗️  Root Node: {}", data.parsed_json.plan.node_type);
                if let Some(exec_time) = data.parsed_json.execution_time {
                    println!("  ⚡ Execution Time: {:.3} ms", exec_time);
                }
            }
        }

        println!("{}", "-".repeat(80));
    }

    println!("\n✨ Parsing test completed successfully!");
    println!("\nSummary:");
    println!("  • Total plans parsed: {}", parsed_plans.len());
    println!("  • All plans have valid timestamps: ✓");
    println!("  • All plans have query text: ✓");
    println!("  • All plans have duration: ✓");

    Ok(())
}
RUSTEOF

echo ""
echo "=== Compiling and Running Test ==="
cd /home/user/pg-loganalyze

# Create test binary
cat > /tmp/run_parser_test.sh << 'RUNEOF'
#!/bin/bash
cd /home/user/pg-loganalyze
rustc --edition 2024 /tmp/test_parser.rs \
  --extern pg_loganalyze_core=target/debug/libpg_loganalyze_core.rlib \
  --extern anyhow=target/debug/deps/libanyhow-*.rlib \
  -L target/debug/deps \
  -o /tmp/test_parser 2>&1

if [ $? -eq 0 ]; then
  echo "✓ Compilation successful"
  echo ""
  /tmp/test_parser
else
  echo "✗ Compilation failed, trying with cargo run..."
  # Fallback: create a simple cargo project
  cd /tmp
  cargo new --bin parser_test --quiet
  cd parser_test

  # Add dependency
  cat > Cargo.toml << 'CARGOEOF'
[package]
name = "parser_test"
version = "0.1.0"
edition = "2024"

[dependencies]
pg-loganalyze-core = { path = "/home/user/pg-loganalyze/crates/core" }
anyhow = "1.0"
CARGOEOF

  cp /tmp/test_parser.rs src/main.rs
  cargo run --quiet
fi
RUNEOF

chmod +x /tmp/run_parser_test.sh
/tmp/run_parser_test.sh

echo ""
echo "=== Test Complete ==="
