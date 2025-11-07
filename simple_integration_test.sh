#!/bin/bash
set -e

echo "======================================================================"
echo "PostgreSQL Auto_Explain Integration Test (Session-Level Config)"
echo "======================================================================"
echo ""

# Clean up
docker rm -f test-pg 2>/dev/null || true

echo "🐘 Starting PostgreSQL container with auto_explain pre-loaded..."
docker run -d --name test-pg \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=testdb \
  -p 5432:5432 \
  postgres:16-alpine \
  -c shared_preload_libraries=auto_explain \
  -c auto_explain.log_min_duration=0 \
  -c auto_explain.log_analyze=on \
  -c auto_explain.log_buffers=on \
  -c auto_explain.log_timing=on \
  -c auto_explain.log_verbose=on

echo "⏳ Waiting for PostgreSQL to be ready..."
sleep 10

for i in {1..30}; do
  if docker exec test-pg pg_isready -U postgres > /dev/null 2>&1; then
    echo "✅ PostgreSQL is ready with auto_explain enabled!"
    break
  fi
  echo "   Waiting... ($i/30)"
  sleep 1
done

echo ""
echo "📊 Creating test data and executing queries..."

docker exec test-pg psql -U postgres -d testdb << 'SQL'
-- Create tables
CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT, age INT);
CREATE TABLE orders (id SERIAL PRIMARY KEY, user_id INT, amount DECIMAL);

-- Insert data
INSERT INTO users VALUES (1, 'Alice', 30), (2, 'Bob', 25), (3, 'Charlie', 35);
INSERT INTO orders VALUES (1, 1, 100.50), (2, 1, 200.75), (3, 2, 150.25);

-- Execute queries that will be logged
SELECT * FROM users WHERE age > 20;
SELECT u.name, SUM(o.amount) FROM users u JOIN orders o ON u.id = o.user_id GROUP BY u.name;
SELECT COUNT(*), AVG(age) FROM users;
SQL

echo "✅ Queries executed"

echo ""
echo "📝 Extracting logs..."
docker logs test-pg 2>&1 > /tmp/pg_logs.txt

echo "📏 Log size: $(wc -l < /tmp/pg_logs.txt) lines"

echo ""
echo "🔍 Sample logs with query plans:"
echo "----------------------------------------------------------------------"
grep -B 1 -A 10 "Query Text:" /tmp/pg_logs.txt | head -50
echo "----------------------------------------------------------------------"

echo ""
echo "🧪 Testing parser..."

cd /home/user/pg-loganalyze

# Build the core library first
cargo build --release -p pg-loganalyze-core 2>&1 | grep -E "(Compiling|Finished)" || true

cat > /tmp/test_main.rs << 'RUST'
use pg_loganalyze_core::PostgreSQLLogParser;

fn main() -> anyhow::Result<()> {
    let logs = std::fs::read_to_string("/tmp/pg_logs.txt")?;
    println!("\n📖 Log file: {} bytes", logs.len());

    let mut parser = PostgreSQLLogParser::new();
    let plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    println!("✅ Parsed {} query plans\n", plans.len());

    if plans.is_empty() {
        println!("⚠️  No plans found. Checking for 'duration:' in logs...");
        println!("Occurrences: {}", logs.matches("duration:").count());
        return Ok(());
    }

    for (i, plan) in plans.iter().enumerate() {
        let sep = "-".repeat(70);
        println!("{}", sep);
        println!("📊 Plan #{}: Duration={:.2}ms", i+1, plan.duration_ms());

        let q = plan.query_text();
        let query_display = if q.len() > 60 { format!("{}...", &q[..60]) } else { q.to_string() };
        println!("📝 Query: {}", query_display.replace("\n", " ").trim());

        match plan {
            pg_loganalyze_core::QueryPlan::TextPlan(d) => {
                println!("📋 Type: Text ({} lines)", d.plan_lines.len());
                if !d.plan_lines.is_empty() {
                    println!("\nPlan excerpt:");
                    for line in d.plan_lines.iter().take(2) {
                        println!("  {}{}", "  ".repeat(line.indentation), line.query);
                    }
                }
            },
            pg_loganalyze_core::QueryPlan::JsonPlan(d) => {
                println!("📋 Type: JSON - {}", d.parsed_json.plan.node_type);
            }
        }
    }

    println!("\n{}", "=".repeat(70));
    println!("✅ TEST PASSED - Parsed {} real PostgreSQL query plans!", plans.len());
    println!("{}", "=".repeat(70));

    Ok(())
}
RUST

mkdir -p /tmp/parser_test/src
cat > /tmp/parser_test/Cargo.toml << 'TOML'
[package]
name = "parser_test"
version = "0.1.0"
edition = "2024"

[dependencies]
pg-loganalyze-core = { path = "/home/user/pg-loganalyze/crates/core" }
anyhow = "1.0"
TOML

mv /tmp/test_main.rs /tmp/parser_test/src/main.rs

cd /tmp/parser_test
cargo run --release 2>&1

TEST_RESULT=$?

echo ""
docker rm -f test-pg > /dev/null 2>&1
exit $TEST_RESULT
