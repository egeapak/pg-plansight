//! Cross-version throughput / overhead harness for the `pg_plansight` extension.
//!
//! Starts a postgres container (built with the extension by
//! `crates/pg_extension/docker/Dockerfile.bench`) via testcontainers, then for
//! `capture_mode` = `off` (baseline) and `hook` measures:
//!   - point-query latency (OLTP),
//!   - aggregate-query latency (OLAP),
//!   - high-throughput point-query TPS over a fixed window,
//!
//! and prints a one-line `RESULT` row plus the per-phase capture profile.
//!
//! Usage (needs Docker):
//!   PG_BENCH_IMAGE=pg_plansight_bench:pg16 cargo run -p pg-plansight-bench-harness
//!
//! `scripts/bench_versions.sh` builds the image per major (13–18) and runs this.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use postgres::{Client, NoTls};
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::SyncRunner,
    GenericImage, ImageExt,
};

/// Rows in the synthetic `orders` table.
const ROWS: i64 = 50_000;
/// Latency samples per query type.
const LATENCY_ITERS: u32 = 2_000;
/// High-throughput measurement window.
const THROUGHPUT_SECS: u64 = 3;

fn main() -> Result<()> {
    let image_ref =
        std::env::var("PG_BENCH_IMAGE").unwrap_or_else(|_| "pg_plansight_bench:pg16".to_string());
    let (name, tag) = image_ref
        .split_once(':')
        .unwrap_or(("pg_plansight_bench", "pg16"));

    eprintln!("[harness] starting container {image_ref} …");
    let container = GenericImage::new(name, tag)
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
        .start()
        .context("failed to start container (is Docker running and the image built?)")?;

    let port = container.get_host_port_ipv4(5432.tcp())?;
    let conn = format!("host=127.0.0.1 port={port} user=postgres dbname=postgres");
    let mut client = connect_retry(&conn, Duration::from_secs(60))?;

    client
        .batch_execute("CREATE EXTENSION pg_plansight;")
        .context("CREATE EXTENSION failed")?;
    let version: String = client.query_one("SHOW server_version", &[])?.get(0);
    eprintln!("[harness] connected to PostgreSQL {version}; loading {ROWS} rows …");
    setup_data(&mut client)?;

    let base = run_suite(&mut client, "off")?;
    let hook = run_suite(&mut client, "hook")?;

    // Per-phase capture profile (hook mode, profiling on).
    let profile = capture_profile(&mut client)?;

    println!(
        "RESULT pg={version} \
         | point off={:.4}ms hook={:.4}ms (+{:.0}%) \
         | olap off={:.3}ms hook={:.3}ms (+{:.0}%) \
         | tps off={:.0} hook={:.0} ({:.0}% of baseline) \
         | render≈{}ns consume≈{}ns",
        base.point_ms,
        hook.point_ms,
        pct(base.point_ms, hook.point_ms),
        base.olap_ms,
        hook.olap_ms,
        pct(base.olap_ms, hook.olap_ms),
        base.tps,
        hook.tps,
        hook.tps / base.tps * 100.0,
        profile.render_ns,
        profile.consume_ns,
    );
    Ok(())
}

struct Suite {
    point_ms: f64,
    olap_ms: f64,
    tps: f64,
}

fn run_suite(client: &mut Client, mode: &str) -> Result<Suite> {
    client.batch_execute(&format!(
        "SET plansight.capture_mode = '{mode}'; \
         SET plansight.synchronous = off; \
         SET plansight.sample_rate = 1.0; \
         SET plansight.min_duration_ms = 0;"
    ))?;

    let point = client.prepare("SELECT * FROM orders WHERE id = $1")?;
    let olap = client
        .prepare("SELECT customer_id, count(*), avg(amount) FROM orders GROUP BY customer_id")?;

    // Warm up both plans.
    for i in 0..200i32 {
        let _ = client.query(&point, &[&((i % ROWS as i32) + 1)])?;
    }
    let _ = client.query(&olap, &[])?;

    // Point-query latency.
    let t = Instant::now();
    for i in 0..LATENCY_ITERS as i32 {
        let _ = client.query(&point, &[&((i % ROWS as i32) + 1)])?;
    }
    let point_ms = t.elapsed().as_secs_f64() * 1000.0 / LATENCY_ITERS as f64;

    // OLAP latency (fewer iters; each is much heavier).
    let olap_iters = 200u32;
    let t = Instant::now();
    for _ in 0..olap_iters {
        let _ = client.query(&olap, &[])?;
    }
    let olap_ms = t.elapsed().as_secs_f64() * 1000.0 / olap_iters as f64;

    // High-throughput point queries over a fixed window.
    let deadline = Instant::now() + Duration::from_secs(THROUGHPUT_SECS);
    let mut n = 0i32;
    while Instant::now() < deadline {
        let _ = client.query(&point, &[&((n % ROWS as i32) + 1)])?;
        n += 1;
    }
    let tps = n as f64 / THROUGHPUT_SECS as f64;

    eprintln!("[harness] mode={mode}: point={point_ms:.4}ms olap={olap_ms:.3}ms tps={tps:.0}");
    Ok(Suite {
        point_ms,
        olap_ms,
        tps,
    })
}

struct Profile {
    render_ns: i64,
    consume_ns: i64,
}

fn capture_profile(client: &mut Client) -> Result<Profile> {
    client.batch_execute(
        "SET plansight.capture_mode='hook'; SET plansight.synchronous=off; \
         SET plansight.min_duration_ms=0; SET plansight.profile=on;",
    )?;
    let _ = client.query("SELECT * FROM plansight_capture_timings()", &[])?; // reset
    for _ in 0..2000 {
        let _ = client.query(
            "SELECT customer_id, count(*) FROM orders GROUP BY customer_id",
            &[],
        )?;
    }
    let row = client.query_one(
        "SELECT render_ns, consume_ns FROM plansight_capture_timings()",
        &[],
    )?;
    client.batch_execute("SET plansight.profile=off; SET plansight.capture_mode='off';")?;
    Ok(Profile {
        render_ns: row.get(0),
        consume_ns: row.get(1),
    })
}

fn setup_data(client: &mut Client) -> Result<()> {
    client.batch_execute(&format!(
        "CREATE TABLE orders (id serial PRIMARY KEY, customer_id int, amount numeric); \
         INSERT INTO orders (customer_id, amount) \
            SELECT (g % 5000) + 1, (random() * 1000)::numeric(10,2) \
            FROM generate_series(1, {ROWS}) g; \
         ANALYZE orders;"
    ))?;
    Ok(())
}

fn connect_retry(conn: &str, timeout: Duration) -> Result<Client> {
    let deadline = Instant::now() + timeout;
    let mut last_err = None;
    while Instant::now() < deadline {
        match Client::connect(conn, NoTls) {
            Ok(c) => return Ok(c),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not connect within {timeout:?}: {last_err:?}"
    ))
}

fn pct(base: f64, other: f64) -> f64 {
    if base <= 0.0 {
        0.0
    } else {
        (other - base) / base * 100.0
    }
}
