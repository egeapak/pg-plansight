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
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio_postgres::{Client, NoTls};

/// Helper struct to manage PostgreSQL container with auto_explain enabled
struct PostgresContainer {
    container: testcontainers::ContainerAsync<Postgres>,
    client: Client,
}

impl PostgresContainer {
    /// Start a PostgreSQL container with auto_explain extension enabled
    async fn start() -> anyhow::Result<Self> {
        // Create PostgreSQL container
        let container = Postgres::default().start().await?;

        // Get container port
        let host_port = container.get_host_port_ipv4(5432).await?;

        // Connect to the database
        let connection_string = format!(
            "host=127.0.0.1 port={} user=postgres password=postgres dbname=postgres",
            host_port
        );
        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;

        // Spawn connection handler
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("PostgreSQL connection error: {}", e);
            }
        });

        Ok(Self {
            container,
            client,
        })
    }

    /// Configure PostgreSQL for auto_explain logging
    async fn configure_auto_explain(&self) -> anyhow::Result<()> {
        // Load auto_explain extension
        self.client
            .execute("LOAD 'auto_explain';", &[])
            .await?;

        // Configure auto_explain settings
        self.client
            .execute("SET auto_explain.log_min_duration = 0;", &[]) // Log all queries
            .await?;

        self.client
            .execute("SET auto_explain.log_analyze = true;", &[])
            .await?;

        self.client
            .execute("SET auto_explain.log_buffers = true;", &[])
            .await?;

        self.client
            .execute("SET auto_explain.log_timing = true;", &[])
            .await?;

        self.client
            .execute("SET auto_explain.log_verbose = true;", &[])
            .await?;

        self.client
            .execute("SET auto_explain.log_nested_statements = true;", &[])
            .await?;

        // Set log format to include timestamps
        self.client
            .execute("SET log_line_prefix = '%t [%p]: ';", &[])
            .await?;

        self.client
            .execute("SET client_min_messages = 'log';", &[])
            .await?;

        Ok(())
    }

    /// Execute a query (for testing purposes)
    async fn execute(&self, query: &str) -> anyhow::Result<()> {
        self.client.execute(query, &[]).await?;
        Ok(())
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
    // Start PostgreSQL container
    let pg = PostgresContainer::start().await?;

    // Configure auto_explain
    pg.configure_auto_explain().await?;

    // Create a test table
    pg.execute("CREATE TABLE test_users (id SERIAL PRIMARY KEY, name TEXT, age INT);")
        .await?;

    // Insert test data
    pg.execute("INSERT INTO test_users (name, age) VALUES ('Alice', 30), ('Bob', 25), ('Charlie', 35);")
        .await?;

    // Run a query that will be explained
    pg.execute("SELECT * FROM test_users WHERE age > 20;")
        .await?;

    // Wait a moment for logs to be written
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get logs
    let logs = pg.get_logs().await?;

    // Verify logs contain auto_explain output
    assert!(
        logs.contains("Query Text:") || logs.contains("duration:"),
        "Logs should contain auto_explain output. Logs:\n{}",
        logs
    );

    // Parse logs with our parser
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    // Verify we parsed at least one query plan
    assert!(
        !parsed_plans.is_empty(),
        "Should parse at least one query plan from logs"
    );

    // Verify the parsed plan contains expected information
    let first_plan = &parsed_plans[0];
    println!("Parsed plan: {:?}", first_plan);

    // Check that we have query text
    assert!(
        !first_plan.query_text().is_empty(),
        "Query text should not be empty"
    );

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_auto_explain_join_query() -> anyhow::Result<()> {
    // Start PostgreSQL container
    let pg = PostgresContainer::start().await?;

    // Configure auto_explain
    pg.configure_auto_explain().await?;

    // Create test tables
    pg.execute(
        "CREATE TABLE orders (id SERIAL PRIMARY KEY, user_id INT, total DECIMAL(10,2));",
    )
    .await?;

    pg.execute(
        "CREATE TABLE customers (id SERIAL PRIMARY KEY, name TEXT, email TEXT);",
    )
    .await?;

    // Insert test data
    pg.execute(
        "INSERT INTO customers (name, email) VALUES
         ('John', 'john@example.com'),
         ('Jane', 'jane@example.com');",
    )
    .await?;

    pg.execute(
        "INSERT INTO orders (user_id, total) VALUES
         (1, 100.50),
         (1, 200.75),
         (2, 150.25);",
    )
    .await?;

    // Run a JOIN query
    pg.execute(
        "SELECT c.name, SUM(o.total) as total_spent
         FROM customers c
         JOIN orders o ON c.id = o.user_id
         GROUP BY c.name;",
    )
    .await?;

    // Wait for logs
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get and parse logs
    let logs = pg.get_logs().await?;
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    // Verify we have query plans
    assert!(
        !parsed_plans.is_empty(),
        "Should parse query plans from JOIN query"
    );

    // Find the JOIN query in parsed plans
    let join_plan = parsed_plans
        .iter()
        .find(|p| p.query_text().to_lowercase().contains("join"));

    assert!(
        join_plan.is_some(),
        "Should find the JOIN query in parsed plans"
    );

    if let Some(plan) = join_plan {
        println!("Parsed JOIN plan: {:?}", plan);
        assert!(
            plan.query_text().to_lowercase().contains("customers")
                || plan.query_text().to_lowercase().contains("orders"),
            "Query should reference our test tables"
        );
    }

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_auto_explain_aggregate_query() -> anyhow::Result<()> {
    // Start PostgreSQL container
    let pg = PostgresContainer::start().await?;

    // Configure auto_explain
    pg.configure_auto_explain().await?;

    // Create test table with more data
    pg.execute(
        "CREATE TABLE sales (
            id SERIAL PRIMARY KEY,
            product TEXT,
            amount DECIMAL(10,2),
            sale_date DATE
        );",
    )
    .await?;

    // Insert test data
    pg.execute(
        "INSERT INTO sales (product, amount, sale_date) VALUES
         ('Widget', 50.00, '2024-01-01'),
         ('Widget', 75.00, '2024-01-02'),
         ('Gadget', 100.00, '2024-01-01'),
         ('Gadget', 125.00, '2024-01-03'),
         ('Widget', 60.00, '2024-01-03');",
    )
    .await?;

    // Run aggregate query
    pg.execute(
        "SELECT product, COUNT(*) as count, SUM(amount) as total, AVG(amount) as average
         FROM sales
         GROUP BY product
         ORDER BY total DESC;",
    )
    .await?;

    // Wait for logs
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get and parse logs
    let logs = pg.get_logs().await?;
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    // Verify we parsed plans
    assert!(
        !parsed_plans.is_empty(),
        "Should parse query plans from aggregate query"
    );

    // Print all parsed plans for debugging
    for (i, plan) in parsed_plans.iter().enumerate() {
        println!("Plan {}: {:?}", i, plan);
    }

    Ok(())
}

#[tokio::test]
#[ignore = "Requires Docker to be available"]
async fn test_parser_handles_multiple_queries() -> anyhow::Result<()> {
    // Start PostgreSQL container
    let pg = PostgresContainer::start().await?;

    // Configure auto_explain
    pg.configure_auto_explain().await?;

    // Create test table
    pg.execute("CREATE TABLE items (id SERIAL PRIMARY KEY, value INT);")
        .await?;

    // Run multiple different queries
    pg.execute("INSERT INTO items (value) VALUES (1), (2), (3);")
        .await?;
    pg.execute("SELECT * FROM items WHERE value > 1;").await?;
    pg.execute("SELECT COUNT(*) FROM items;").await?;
    pg.execute("SELECT AVG(value) FROM items;").await?;

    // Wait for logs
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Get and parse logs
    let logs = pg.get_logs().await?;
    let mut parser = PostgreSQLLogParser::new();
    let parsed_plans = parser.parse_string_with_progress(&logs, |_, _| {})?;

    // Verify we parsed multiple plans
    println!("Parsed {} query plans", parsed_plans.len());
    assert!(
        !parsed_plans.is_empty(),
        "Should parse at least one query plan"
    );

    // Verify each plan has basic required fields
    for (i, plan) in parsed_plans.iter().enumerate() {
        assert!(
            !plan.query_text().is_empty(),
            "Plan {} should have non-empty query text",
            i
        );
        assert!(plan.duration_ms() >= 0.0, "Plan {} should have valid duration", i);
    }

    Ok(())
}
