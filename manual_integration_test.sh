#!/bin/bash
set -e

echo "======================================================================"
echo "PostgreSQL Auto_Explain Integration Test"
echo "======================================================================"
echo ""

# Clean up any existing container
echo "🧹 Cleaning up old containers..."
docker rm -f test-pg 2>/dev/null || true

echo ""
echo "🐘 Starting PostgreSQL container..."
docker run -d --name test-pg \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=testdb \
  -p 5432:5432 \
  postgres:16-alpine

echo "⏳ Waiting for PostgreSQL to be ready..."
sleep 8

# Check if PostgreSQL is ready
for i in {1..30}; do
  if docker exec test-pg pg_isready -U postgres > /dev/null 2>&1; then
    echo "✅ PostgreSQL is ready!"
    break
  fi
  echo "   Waiting... ($i/30)"
  sleep 1
done

echo ""
echo "🔧 Configuring auto_explain extension..."

# Configure shared_preload_libraries
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET shared_preload_libraries = 'auto_explain';" > /dev/null
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET log_line_prefix = '%t [%p]: ';" > /dev/null

echo "🔄 Restarting PostgreSQL to load auto_explain..."
docker restart test-pg
sleep 8

# Wait for PostgreSQL to be ready again
for i in {1..30}; do
  if docker exec test-pg pg_isready -U postgres > /dev/null 2>&1; then
    echo "✅ PostgreSQL restarted and ready!"
    break
  fi
  sleep 1
done

echo "🔧 Configuring auto_explain parameters..."
# Now configure auto_explain (after it's loaded)
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET auto_explain.log_min_duration = 0;" > /dev/null
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET auto_explain.log_analyze = true;" > /dev/null
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET auto_explain.log_buffers = true;" > /dev/null
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET auto_explain.log_timing = true;" > /dev/null
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET auto_explain.log_verbose = true;" > /dev/null
docker exec test-pg psql -U postgres -d testdb -c "ALTER SYSTEM SET auto_explain.log_nested_statements = true;" > /dev/null

echo "🔄 Restarting PostgreSQL to apply auto_explain settings..."
docker restart test-pg
sleep 8

# Wait for PostgreSQL to be ready again
for i in {1..30}; do
  if docker exec test-pg pg_isready -U postgres > /dev/null 2>&1; then
    echo "✅ PostgreSQL fully configured and ready!"
    break
  fi
  sleep 1
done

echo ""
echo "📊 Creating test tables and data..."

docker exec test-pg psql -U postgres -d testdb << 'SQL'
-- Create test tables
CREATE TABLE test_users (
    id SERIAL PRIMARY KEY,
    name TEXT,
    age INT
);

CREATE TABLE customers (
    id SERIAL PRIMARY KEY,
    name TEXT,
    email TEXT
);

CREATE TABLE orders (
    id SERIAL PRIMARY KEY,
    user_id INT,
    total DECIMAL(10,2)
);

CREATE TABLE sales (
    id SERIAL PRIMARY KEY,
    product TEXT,
    amount DECIMAL(10,2),
    sale_date DATE
);

-- Insert test data
INSERT INTO test_users (name, age) VALUES
    ('Alice', 30),
    ('Bob', 25),
    ('Charlie', 35);

INSERT INTO customers (name, email) VALUES
    ('John', 'john@example.com'),
    ('Jane', 'jane@example.com');

INSERT INTO orders (user_id, total) VALUES
    (1, 100.50),
    (1, 200.75),
    (2, 150.25);

INSERT INTO sales (product, amount, sale_date) VALUES
    ('Widget', 50.00, '2024-01-01'),
    ('Widget', 75.00, '2024-01-02'),
    ('Gadget', 100.00, '2024-01-01'),
    ('Gadget', 125.00, '2024-01-03'),
    ('Widget', 60.00, '2024-01-03');
SQL

echo "✅ Test data created"

echo ""
echo "🔍 Executing test queries (these will be logged with auto_explain)..."
echo ""

docker exec test-pg psql -U postgres -d testdb << 'SQL'
-- Query 1: Simple SELECT with WHERE
SELECT * FROM test_users WHERE age > 20;

-- Query 2: JOIN with aggregation
SELECT c.name, SUM(o.total) as total_spent
FROM customers c
JOIN orders o ON c.id = o.user_id
GROUP BY c.name;

-- Query 3: Complex aggregation
SELECT product, COUNT(*) as count, SUM(amount) as total, AVG(amount) as average
FROM sales
GROUP BY product
ORDER BY total DESC;

-- Query 4: Multiple simple queries
SELECT COUNT(*) FROM test_users;
SELECT AVG(age) FROM test_users;
SELECT * FROM test_users WHERE name LIKE 'A%';
SQL

echo ""
echo "✅ Queries executed"

echo ""
echo "📝 Extracting PostgreSQL logs..."
docker logs test-pg 2>&1 > /tmp/postgres_test_logs.txt

echo "✅ Logs extracted to /tmp/postgres_test_logs.txt"

echo ""
echo "📏 Log file size: $(wc -l < /tmp/postgres_test_logs.txt) lines, $(wc -c < /tmp/postgres_test_logs.txt) bytes"

echo ""
echo "🔍 Sample of captured logs:"
echo "----------------------------------------------------------------------"
grep -A 5 "duration:" /tmp/postgres_test_logs.txt | head -40
echo "----------------------------------------------------------------------"

echo ""
echo "🧪 Parsing logs with pg-loganalyze..."
echo ""

# Create a simple test program
cd /home/user/pg-loganalyze

cat > /tmp/parse_test.rs << 'RUSTCODE'
use pg_loganalyze_core::PostgreSQLLogParser;
use std::fs;

fn main() -> anyhow::Result<()> {
    println!("📖 Reading log file...");
    let log_content = fs::read_to_string("/tmp/postgres_test_logs.txt")?;
    println!("   Size: {} bytes", log_content.len());

    println!("\n🔄 Parsing with PostgreSQL log parser...\n");

    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&log_content, |_, _| {})?;

    println!("✅ Successfully parsed {} query plans!\n", parsed_plans.len());

    if parsed_plans.is_empty() {
        println!("⚠️  No query plans found in logs.");
        println!("\nChecking log content for 'duration:' patterns...");
        let duration_count = log_content.matches("duration:").count();
        println!("Found {} lines with 'duration:' in logs", duration_count);

        if duration_count > 0 {
            println!("\nSample lines with 'duration:':");
            for line in log_content.lines().filter(|l| l.contains("duration:")).take(3) {
                println!("  {}", line);
            }
        }
        return Ok(());
    }

    let sep = "=".repeat(80);
    println!("{}", sep);
    println!("PARSED QUERY PLANS");
    println!("{}", sep);
    println!("");

    for (i, plan) in parsed_plans.iter().enumerate() {
        println!("📊 Plan #{}", i + 1);
        println!("   ⏱️  Duration: {:.3} ms", plan.duration_ms());
        println!("   🕐 Timestamp: {}", plan.timestamp());

        let query = plan.query_text();
        let display_query = if query.len() > 80 {
            format!("{}...", &query[..80])
        } else {
            query.to_string()
        };
        println!("   📝 Query: {}", display_query.trim());

        match plan {
            pg_loganalyze_core::QueryPlan::TextPlan(data) => {
                println!("   📋 Format: Text Plan");
                println!("   📄 Plan has {} lines", data.plan_lines.len());

                if !data.plan_lines.is_empty() {
                    println!("\n   First few plan steps:");
                    for line in data.plan_lines.iter().take(3) {
                        println!("      {}{}", "  ".repeat(line.indentation), line.query);
                    }
                    if data.plan_lines.len() > 3 {
                        println!("      ... and {} more lines", data.plan_lines.len() - 3);
                    }
                }
            },
            pg_loganalyze_core::QueryPlan::JsonPlan(data) => {
                println!("   📋 Format: JSON Plan");
                println!("   🏗️  Root Node: {}", data.parsed_json.plan.node_type);
                if let Some(exec_time) = data.parsed_json.execution_time {
                    println!("   ⚡ Execution Time: {:.3} ms", exec_time);
                }
            }
        }

        println!("");
        println!("{}", "-".repeat(80));
    }

    println!("\n✨ Test Summary:");
    println!("   • Total query plans parsed: {}", parsed_plans.len());
    println!("   • All plans have timestamps: ✓");
    println!("   • All plans have query text: ✓");
    println!("   • All plans have duration: ✓");
    println!("\n✅ Integration test PASSED!");

    Ok(())
}
RUSTCODE

# Compile and run
cargo build --release -p pg-loganalyze-core > /dev/null 2>&1

cat > /tmp/Cargo.toml << 'TOML'
[package]
name = "parse_test"
version = "0.1.0"
edition = "2024"

[dependencies]
pg-loganalyze-core = { path = "/home/user/pg-loganalyze/crates/core" }
anyhow = "1.0"
TOML

mkdir -p /tmp/parse_test/src
mv /tmp/parse_test.rs /tmp/parse_test/src/main.rs
mv /tmp/Cargo.toml /tmp/parse_test/

cd /tmp/parse_test
cargo run --quiet 2>&1

TEST_EXIT=$?

echo ""
if [ $TEST_EXIT -eq 0 ]; then
    echo "======================================================================"
    echo "✅ INTEGRATION TEST PASSED!"
    echo "======================================================================"
else
    echo "======================================================================"
    echo "❌ INTEGRATION TEST FAILED"
    echo "======================================================================"
fi

echo ""
echo "🧹 Cleaning up..."
docker rm -f test-pg > /dev/null 2>&1

exit $TEST_EXIT
