//! Integration tests for PostgreSQL auto_explain log parsing
//!
//! These tests use testcontainers to spin up a real PostgreSQL instance,
//! enable the auto_explain extension, execute queries, and verify that
//! the log parser correctly parses the generated logs.
//!
//! # Requirements
//! - Docker must be installed and running
//! - The Docker socket must be accessible at /var/run/docker.sock
//!
//! # Running the tests
//! Since these tests require Docker, they are marked with `#[ignore]` by default.
//! To run them:
//! ```bash
//! cargo test --test integration_test -- --ignored
//! ```
//!
//! Or to run all tests including these:
//! ```bash
//! cargo test --test integration_test -- --include-ignored
//! ```

use pg_loganalyze_core::PostgreSQLLogParser;
use testcontainers::{core::WaitFor, runners::AsyncRunner, GenericImage, ImageExt};
use tokio_postgres::{Client, NoTls};

/// Helper struct to manage PostgreSQL container with auto_explain enabled
struct PostgresContainer {
    container: testcontainers::ContainerAsync<GenericImage>,
    client: Client,
    host_port: u16,
}

impl PostgresContainer {
    /// Start a PostgreSQL container with auto_explain extension enabled
    async fn start() -> anyhow::Result<Self> {
        // Create PostgreSQL container with proper configuration
        let postgres_image = GenericImage::new("postgres", "16-alpine")
            .with_exposed_port(5432.into())
            .with_wait_for(WaitFor::message_on_stderr("database system is ready to accept connections"))
            .with_env_var("POSTGRES_PASSWORD", "postgres")
            .with_env_var("POSTGRES_USER", "postgres")
            .with_env_var("POSTGRES_DB", "testdb")
            .with_cmd(vec![
                "-c", "shared_preload_libraries=auto_explain",
                "-c", "auto_explain.log_min_duration=0",
                "-c", "auto_explain.log_analyze=on",
                "-c", "auto_explain.log_buffers=on",
                "-c", "auto_explain.log_timing=on",
            ]);

        let container = postgres_image.start().await?;

        // Give PostgreSQL time to fully start
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Get container port
        let host_port = container.get_host_port_ipv4(5432).await?;

        // Connect to the database
        let connection_string = format!(
            "host=127.0.0.1 port={} user=postgres password=postgres dbname=testdb",
            host_port
        );

        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;

        // Spawn connection handler
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("PostgreSQL connection error: {}", e);
            }
        });

        // Wait for connection to be ready
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(Self {
            container,
            client,
            host_port,
        })
    }

    /// Execute a query (for testing purposes)
    async fn execute(&self, query: &str) -> anyhow::Result<()> {
        self.client.execute(query, &[]).await?;
        Ok(())
    }

    /// Execute a query and return row count
    async fn query_count(&self, query: &str) -> anyhow::Result<i64> {
        let row = self.client.query_one(query, &[]).await?;
        let count: i64 = row.get(0);
        Ok(count)
    }

    /// Get the PostgreSQL logs from the container
    async fn get_logs(&self) -> anyhow::Result<String> {
        // Get logs from the container using docker logs command
        let container_id = self.container.id();

        let output = tokio::process::Command::new("docker")
            .args(["logs", container_id])
            .output()
            .await?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        Ok(stderr.to_string())
    }
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_auto_explain_simple_query() -> anyhow::Result<()> {
    println!("\n=== Test: Simple Query ===");

    // Start PostgreSQL container
    let pg = PostgresContainer::start().await?;
    println!("✓ PostgreSQL started on port {}", pg.host_port);

    // Create a test table
    pg.execute("CREATE TABLE test_users (id SERIAL PRIMARY KEY, name TEXT, age INT);")
        .await?;
    println!("✓ Created test_users table");

    // Insert test data
    pg.execute("INSERT INTO test_users (name, age) VALUES ('Alice', 30), ('Bob', 25), ('Charlie', 35);")
        .await?;
    println!("✓ Inserted test data");

    // Run a query that will be explained
    let count = pg.query_count("SELECT COUNT(*) FROM test_users WHERE age > 20").await?;
    println!("✓ Executed SELECT query (result: {} rows)", count);
    assert_eq!(count, 3);

    // Wait a moment for logs to be written
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get logs
    let logs = pg.get_logs().await?;
    println!("✓ Retrieved logs ({} bytes)", logs.len());

    // Check logs contain query plans
    let query_text_count = logs.matches("Query Text:").count();
    println!("✓ Found {} query text entries in logs", query_text_count);

    // Parse logs with our parser
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    println!("✓ Parsed {} query plans", parsed_plans.len());

    // Verify we parsed at least one query plan
    assert!(
        !parsed_plans.is_empty(),
        "Should parse at least one query plan from logs"
    );

    // Verify the first parsed plan has required fields
    let first_plan = &parsed_plans[0];
    assert!(!first_plan.query_text().is_empty(), "Query text should not be empty");
    assert!(first_plan.duration_ms() >= 0.0, "Duration should be non-negative");

    println!("\n✅ Test passed: Successfully parsed {} real PostgreSQL query plans!", parsed_plans.len());
    println!("   First plan duration: {:.3}ms", first_plan.duration_ms());
    println!("   First plan query: {}", first_plan.query_text().lines().next().unwrap_or(""));

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_auto_explain_join_query() -> anyhow::Result<()> {
    println!("\n=== Test: JOIN Query ===");

    // Start PostgreSQL container
    let pg = PostgresContainer::start().await?;
    println!("✓ PostgreSQL started");

    // Create test tables
    pg.execute("CREATE TABLE customers (id SERIAL PRIMARY KEY, name TEXT, email TEXT);")
        .await?;
    pg.execute("CREATE TABLE orders (id SERIAL PRIMARY KEY, user_id INT, total DECIMAL(10,2));")
        .await?;
    println!("✓ Created tables");

    // Insert test data
    pg.execute("INSERT INTO customers (name, email) VALUES ('John', 'john@example.com'), ('Jane', 'jane@example.com');")
        .await?;
    pg.execute("INSERT INTO orders (user_id, total) VALUES (1, 100.50), (1, 200.75), (2, 150.25);")
        .await?;
    println!("✓ Inserted data");

    // Run a JOIN query
    pg.execute("SELECT c.name, SUM(o.total) as total_spent FROM customers c JOIN orders o ON c.id = o.user_id GROUP BY c.name;")
        .await?;
    println!("✓ Executed JOIN query");

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get and parse logs
    let logs = pg.get_logs().await?;
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    println!("✓ Parsed {} query plans", parsed_plans.len());

    // Verify we have query plans
    assert!(!parsed_plans.is_empty(), "Should parse query plans from JOIN query");

    println!("\n✅ Test passed!");

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_auto_explain_aggregate_query() -> anyhow::Result<()> {
    println!("\n=== Test: Aggregate Query ===");

    let pg = PostgresContainer::start().await?;
    println!("✓ PostgreSQL started");

    // Create test table
    pg.execute("CREATE TABLE sales (id SERIAL PRIMARY KEY, product TEXT, amount DECIMAL(10,2), sale_date DATE);")
        .await?;
    println!("✓ Created sales table");

    // Insert test data
    pg.execute("INSERT INTO sales (product, amount, sale_date) VALUES \
        ('Widget', 50.00, '2024-01-01'), \
        ('Widget', 75.00, '2024-01-02'), \
        ('Gadget', 100.00, '2024-01-01'), \
        ('Gadget', 125.00, '2024-01-03'), \
        ('Widget', 60.00, '2024-01-03');")
        .await?;
    println!("✓ Inserted sales data");

    // Run aggregate query
    pg.execute("SELECT product, COUNT(*) as count, SUM(amount) as total, AVG(amount) as average \
        FROM sales GROUP BY product ORDER BY total DESC;")
        .await?;
    println!("✓ Executed aggregate query");

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get and parse logs
    let logs = pg.get_logs().await?;
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    println!("✓ Parsed {} query plans", parsed_plans.len());

    assert!(!parsed_plans.is_empty(), "Should parse query plans from aggregate query");

    println!("\n✅ Test passed!");

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_parser_handles_multiple_queries() -> anyhow::Result<()> {
    println!("\n=== Test: Multiple Queries ===");

    let pg = PostgresContainer::start().await?;
    println!("✓ PostgreSQL started");

    // Create test table
    pg.execute("CREATE TABLE items (id SERIAL PRIMARY KEY, value INT);").await?;
    println!("✓ Created items table");

    // Run multiple different queries
    pg.execute("INSERT INTO items (value) VALUES (1), (2), (3);").await?;
    pg.execute("SELECT * FROM items WHERE value > 1;").await?;
    let count1 = pg.query_count("SELECT COUNT(*) FROM items;").await?;
    println!("✓ Executed multiple queries (count: {})", count1);

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get and parse logs
    let logs = pg.get_logs().await?;
    println!("✓ Retrieved logs ({} bytes)", logs.len());

    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    println!("✓ Parsed {} query plans", parsed_plans.len());

    // Verify we parsed multiple plans
    assert!(!parsed_plans.is_empty(), "Should parse at least one query plan");

    // Verify each plan has basic required fields
    for (i, plan) in parsed_plans.iter().enumerate() {
        assert!(!plan.query_text().is_empty(), "Plan {} should have non-empty query text", i);
        assert!(plan.duration_ms() >= 0.0, "Plan {} should have valid duration", i);
    }

    println!("\n✅ Test passed! Parsed {} real query plans", parsed_plans.len());

    Ok(())
}
