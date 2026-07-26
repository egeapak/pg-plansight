use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pg_plansight_exporter::{Config, ConfigReloader, LogCollector, Scheduler, StateManager};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};

/// Environment override for the state database path.
///
/// Named as a constant so the shipped systemd unit and this binary cannot
/// drift apart; the unit previously set `PG_PLANSIGHT_STATE_DB`, which nothing
/// read, making the override silently inert.
const STATE_DB_ENV_VAR: &str = "PG_EXPORTER_STATE_DB";

/// Build the tracing filter, defaulting to `info` when `RUST_LOG` is unset.
///
/// `EnvFilter::from_default_env()` with no `RUST_LOG` yields zero directives,
/// whose effective level is ERROR — so a hand-run daemon printed nothing at
/// all, not even "Starting". The shipped systemd unit sets `RUST_LOG=info`, so
/// only non-systemd runs (containers, manual troubleshooting) were affected —
/// which is exactly when you need the output. `from_env_lossy` additionally
/// keeps a typo in `RUST_LOG` from silently zeroing the filter.
fn log_filter() -> tracing_subscriber::EnvFilter {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::filter::LevelFilter;

    EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
}

#[derive(Parser)]
#[command(name = "pg-plansight-exporter")]
#[command(about = "Prometheus exporter for PostgreSQL auto_explain logs")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    // `global = true` so these are accepted on either side of the subcommand.
    // The shipped systemd unit and config/example.toml use
    // `daemon --config <path>`, while the crate README documents
    // `--config <path> daemon`; without this, only the latter parsed and every
    // packaged install failed to start with a clap usage error.
    #[arg(long, short, global = true, help = "Configuration file path")]
    config: Option<PathBuf>,

    #[arg(long, global = true, help = "Override state database path")]
    state_db: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the exporter daemon
    Daemon,
    /// Initialize or reset the state database
    State {
        #[command(subcommand)]
        action: StateAction,
    },
    /// Process log files once (useful for testing/backfill)
    Process {
        #[arg(long, help = "Log file paths or patterns")]
        logs: Vec<String>,
    },
    /// Load and validate the configuration file, then exit.
    ///
    /// Exits non-zero with a diagnostic if the config is unparseable or
    /// contains values that would fail later on the collection path. Intended
    /// as the pre-flight check before `systemctl restart`.
    CheckConfig,
    /// Process remaining unread content from last checkpoint to end of files
    ProcessRest {
        #[arg(
            long,
            help = "Log file paths or patterns (optional, uses config if not specified)"
        )]
        logs: Option<Vec<String>>,
    },
}

#[derive(Subcommand)]
enum StateAction {
    /// Initialize the state database
    Init,
    /// Show current state information
    Show,
    /// Reset all state (clear database)
    Reset,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(log_filter())
        .init();

    let cli = Cli::parse();

    // Load configuration
    let config_path = cli.config.clone();
    let config = if let Some(ref path) = config_path {
        Config::load_from_file(path)
            .with_context(|| format!("Failed to load config from {}", path.display()))?
    } else {
        info!("No config file specified, using defaults");
        Config::default()
    };

    // Override state database path if provided
    let mut config = config;
    if let Some(state_db_path) = cli.state_db {
        config.state.database_path = state_db_path;
    } else if let Ok(env_path) = std::env::var(STATE_DB_ENV_VAR) {
        config.state.database_path = env_path;
    }

    let state_manager = StateManager::new(&config.state.database_path);

    match cli.command {
        Commands::CheckConfig => {
            // Reaching here means load_from_file (and Config::validate) already
            // succeeded, or we fell back to the built-in defaults.
            match &config_path {
                Some(path) => println!("Configuration at {} is valid.", path.display()),
                None => println!(
                    "No --config given; built-in defaults are valid. \
                     Pass --config <path> to check a specific file."
                ),
            }
            println!("  bind_address:  {}", config.server.bind_address);
            println!("  metrics_path:  {}", config.server.metrics_path);
            println!("  log_paths:     {:?}", config.log_parsing.log_paths);
            println!("  poll_interval: {}", config.log_parsing.poll_interval);
            println!("  backends:      {:?}", config.metrics.backends);
            println!("  state db:      {}", config.state.database_path);
            Ok(())
        }
        Commands::Daemon => run_daemon(config, state_manager, config_path).await,
        Commands::State { action } => run_state_command(action, state_manager).await,
        Commands::Process { logs } => run_process_command(config, state_manager, logs).await,
        Commands::ProcessRest { logs } => {
            run_process_rest_command(config, state_manager, logs).await
        }
    }
}

async fn run_daemon(
    config: Config,
    state_manager: StateManager,
    config_path: Option<PathBuf>,
) -> Result<()> {
    info!("Starting pg-plansight-exporter daemon");

    // Initialize state database
    state_manager
        .initialize()
        .context("Failed to initialize state database")?;

    // Initialize metrics backends based on configuration
    let metrics = {
        use pg_plansight_exporter::metrics::{
            CompositeBackend, MetricsBackendType, create_metrics_backend,
        };

        let mut backends = Vec::new();

        for backend_name in &config.metrics.backends {
            match backend_name.as_str() {
                #[cfg(feature = "prometheus")]
                "prometheus" => {
                    let backend = create_metrics_backend(MetricsBackendType::Prometheus {
                        namespace: config.metrics.namespace.clone(),
                        histogram_buckets: config.metrics.histogram_buckets.clone(),
                        max_query_cardinality: config.metrics.max_query_cardinality,
                    })
                    .context("Failed to initialize Prometheus metrics backend")?;
                    backends.push(backend);
                }
                #[cfg(feature = "opentelemetry")]
                "opentelemetry" => {
                    let otel_config =
                        config.metrics.opentelemetry.as_ref().context(
                            "OpenTelemetry backend selected but no configuration provided",
                        )?;
                    let backend = create_metrics_backend(MetricsBackendType::OpenTelemetry {
                        endpoint: otel_config.endpoint.clone(),
                        namespace: config.metrics.namespace.clone(),
                    })
                    .context("Failed to initialize OpenTelemetry metrics backend")?;
                    backends.push(backend);
                }
                backend => {
                    anyhow::bail!(
                        "Unsupported metrics backend: {}. Available: prometheus, opentelemetry",
                        backend
                    );
                }
            }
        }

        if backends.is_empty() {
            anyhow::bail!("No metrics backends configured");
        }

        if backends.len() == 1 {
            backends.into_iter().next().unwrap()
        } else {
            Arc::new(CompositeBackend::new(backends))
                as Arc<dyn pg_plansight_exporter::metrics::MetricsBackend>
        }
    };

    // Start metrics server (Prometheus only)
    #[cfg(feature = "prometheus")]
    let server_handle = if config.metrics.backends.contains(&"prometheus".to_string()) {
        use pg_plansight_exporter::metrics::{CompositeBackend, PrometheusBackend};
        let server_config = config.clone();
        let metrics_clone = metrics.clone();
        Some(tokio::spawn(async move {
            // Try to get Prometheus backend (either directly or from composite)
            let prometheus_backend =
                if let Some(prom) = metrics_clone.as_any().downcast_ref::<PrometheusBackend>() {
                    Some(prom)
                } else if let Some(composite) =
                    metrics_clone.as_any().downcast_ref::<CompositeBackend>()
                {
                    composite
                        .backends()
                        .iter()
                        .find_map(|b| b.as_any().downcast_ref::<PrometheusBackend>())
                } else {
                    None
                };

            let Some(prometheus_backend) = prometheus_backend else {
                anyhow::bail!(
                    "Prometheus backend not found in metrics registry. \
                     This is a configuration error - prometheus is listed in backends \
                     but could not be initialized."
                );
            };

            pg_plansight_exporter::server::start_metrics_server(
                server_config.server.bind_address,
                server_config.server.metrics_path,
                Arc::new(prometheus_backend.registry.clone()),
            )
            .await
        }))
    } else {
        None
    };

    // Set up config reloader if config file is provided
    let config_rx = if let Some(ref path) = config_path {
        info!("Config reload enabled via SIGHUP signal");
        info!("Config file: {}", path.display());
        info!(
            "To reload: kill -HUP {} or systemctl reload pg-plansight-exporter",
            std::process::id()
        );

        let (reloader, rx) = ConfigReloader::new(path.clone(), config.clone());

        // Start SIGHUP handler in background
        tokio::spawn(async move {
            if let Err(e) = reloader.run().await {
                error!("Config reloader failed: {}", e);
            }
        });

        Some(rx)
    } else {
        info!("Config reload disabled (no config file specified)");
        None
    };

    // Keep a handle for the shutdown flush below; the other clone moves into
    // the collector.
    let metrics_for_shutdown = metrics.clone();

    // Create collector and scheduler
    let collector = LogCollector::new(config.clone(), state_manager, metrics)?;
    let poll_interval = config.poll_interval_duration()?;

    let mut scheduler = if let Some(rx) = config_rx {
        Scheduler::with_hot_reload(collector, poll_interval, rx)
    } else {
        Scheduler::new(collector, poll_interval)
    };

    // Start scheduler
    let mut scheduler_handle = tokio::spawn(async move { scheduler.start().await });

    // Wait for a shutdown signal or for a supervised task to die.
    #[cfg(feature = "prometheus")]
    let exit = if let Some(mut server) = server_handle {
        tokio::select! {
            sig = shutdown_signal() => ExitReason::Signalled(sig),
            res = &mut server => ExitReason::from_task("metrics server", res),
            res = &mut scheduler_handle => ExitReason::from_task("scheduler", res),
        }
    } else {
        tokio::select! {
            sig = shutdown_signal() => ExitReason::Signalled(sig),
            res = &mut scheduler_handle => ExitReason::from_task("scheduler", res),
        }
    };

    #[cfg(not(feature = "prometheus"))]
    let exit = tokio::select! {
        sig = shutdown_signal() => ExitReason::Signalled(sig),
        res = &mut scheduler_handle => ExitReason::from_task("scheduler", res),
    };

    info!("Shutting down");
    // Flush metric pipelines: the OTel periodic reader buffers up to a full
    // export interval of samples that are lost unless shutdown() is called.
    // This runs on every exit path, including the failure ones below.
    if let Err(e) = metrics_for_shutdown.shutdown() {
        error!("Failed to shut down metrics backends cleanly: {}", e);
    }

    match exit {
        ExitReason::Signalled(sig) => {
            info!("Exited cleanly after {sig}");
            Ok(())
        }
        // A supervised task ending on its own is always a failure: the daemon
        // is supposed to run until signalled. Returning Ok(()) here (the old
        // behaviour) meant an unusable exporter — e.g. one whose metrics port
        // was already bound — exited 0, which every supervisor reads as an
        // intentional stop.
        ExitReason::TaskDied { what, source } => Err(source.context(format!("{what} exited"))),
    }
}

/// Why `run_daemon` stopped.
enum ExitReason {
    Signalled(&'static str),
    TaskDied {
        what: &'static str,
        source: anyhow::Error,
    },
}

impl ExitReason {
    /// Collapse a joined task's outcome into a failure reason. A task that
    /// returned `Ok(())` still counts as a failure — the daemon's tasks are
    /// not supposed to finish on their own.
    fn from_task(
        what: &'static str,
        res: Result<anyhow::Result<()>, tokio::task::JoinError>,
    ) -> Self {
        let source = match res {
            Err(join_err) if join_err.is_panic() => anyhow::anyhow!("task panicked: {join_err}"),
            Err(join_err) => anyhow::anyhow!("task failed to join: {join_err}"),
            Ok(Err(e)) => e,
            Ok(Ok(())) => anyhow::anyhow!("task returned unexpectedly"),
        };
        ExitReason::TaskDied { what, source }
    }
}

/// Resolve when the process is asked to stop, returning the signal name.
///
/// `tokio::signal::ctrl_c()` is SIGINT only. systemd's default `KillSignal` is
/// SIGTERM, for which no handler was installed — so `systemctl stop` killed the
/// process outright and the metric flush below never ran, silently dropping up
/// to a full OTel export interval on every restart.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to register SIGTERM handler: {e}");
                let _ = signal::ctrl_c().await;
                return "SIGINT";
            }
        };

        tokio::select! {
            _ = signal::ctrl_c() => "SIGINT",
            _ = sigterm.recv() => "SIGTERM",
        }
    }

    #[cfg(not(unix))]
    {
        let _ = signal::ctrl_c().await;
        "ctrl-c"
    }
}

async fn run_state_command(action: StateAction, state_manager: StateManager) -> Result<()> {
    match action {
        StateAction::Init => {
            info!("Initializing state database");
            state_manager.initialize()?;
            info!("State database initialized successfully");
        }
        StateAction::Show => {
            state_manager.initialize()?;
            let states = state_manager.get_all_file_states()?;

            if states.is_empty() {
                println!("No processed files in state database");
            } else {
                println!("Processed files:");
                for (path, state) in states {
                    println!(
                        "  {}: position={}, size={}, last_processed={}",
                        path.display(),
                        state.last_position,
                        state.file_size,
                        state.last_processed_at
                    );
                }
            }
        }
        StateAction::Reset => {
            info!("Resetting state database");
            state_manager.initialize()?;
            state_manager.reset_state()?;
            info!("State database reset successfully");
        }
    }

    Ok(())
}

async fn run_process_command(
    config: Config,
    state_manager: StateManager,
    log_patterns: Vec<String>,
) -> Result<()> {
    info!("Processing log files: {:?}", log_patterns);

    state_manager.initialize()?;

    // Initialize metrics backends
    let metrics = {
        use pg_plansight_exporter::metrics::{
            CompositeBackend, MetricsBackendType, create_metrics_backend,
        };

        let mut backends = Vec::new();

        for backend_name in &config.metrics.backends {
            match backend_name.as_str() {
                #[cfg(feature = "prometheus")]
                "prometheus" => {
                    backends.push(create_metrics_backend(MetricsBackendType::Prometheus {
                        namespace: config.metrics.namespace.clone(),
                        histogram_buckets: config.metrics.histogram_buckets.clone(),
                        max_query_cardinality: config.metrics.max_query_cardinality,
                    })?);
                }
                #[cfg(feature = "opentelemetry")]
                "opentelemetry" => {
                    let otel_config =
                        config.metrics.opentelemetry.as_ref().context(
                            "OpenTelemetry backend selected but no configuration provided",
                        )?;
                    backends.push(create_metrics_backend(MetricsBackendType::OpenTelemetry {
                        endpoint: otel_config.endpoint.clone(),
                        namespace: config.metrics.namespace.clone(),
                    })?);
                }
                backend => {
                    anyhow::bail!("Unsupported metrics backend: {}", backend);
                }
            }
        }

        if backends.len() == 1 {
            backends.into_iter().next().unwrap()
        } else {
            Arc::new(CompositeBackend::new(backends))
                as Arc<dyn pg_plansight_exporter::metrics::MetricsBackend>
        }
    };

    // Override log paths with provided patterns
    let mut process_config = config;
    process_config.log_parsing.log_paths = log_patterns;

    let mut collector = LogCollector::new(process_config, state_manager, metrics)?;
    collector.collect_metrics().await?;

    info!("Processing completed successfully");

    Ok(())
}

async fn run_process_rest_command(
    config: Config,
    state_manager: StateManager,
    log_patterns: Option<Vec<String>>,
) -> Result<()> {
    info!("Processing remaining unread content from files");

    state_manager.initialize()?;

    // Initialize metrics backends (same as other commands)
    let metrics = {
        use pg_plansight_exporter::metrics::{
            CompositeBackend, MetricsBackendType, create_metrics_backend,
        };

        let mut backends = Vec::new();

        for backend_name in &config.metrics.backends {
            match backend_name.as_str() {
                #[cfg(feature = "prometheus")]
                "prometheus" => {
                    backends.push(create_metrics_backend(MetricsBackendType::Prometheus {
                        namespace: config.metrics.namespace.clone(),
                        histogram_buckets: config.metrics.histogram_buckets.clone(),
                        max_query_cardinality: config.metrics.max_query_cardinality,
                    })?);
                }
                #[cfg(feature = "opentelemetry")]
                "opentelemetry" => {
                    let otel_config =
                        config.metrics.opentelemetry.as_ref().context(
                            "OpenTelemetry backend selected but no configuration provided",
                        )?;
                    backends.push(create_metrics_backend(MetricsBackendType::OpenTelemetry {
                        endpoint: otel_config.endpoint.clone(),
                        namespace: config.metrics.namespace.clone(),
                    })?);
                }
                backend => {
                    anyhow::bail!("Unsupported metrics backend: {}", backend);
                }
            }
        }

        if backends.len() == 1 {
            backends.into_iter().next().unwrap()
        } else {
            Arc::new(CompositeBackend::new(backends))
                as Arc<dyn pg_plansight_exporter::metrics::MetricsBackend>
        }
    };

    // Use provided patterns or fall back to config
    let mut process_config = config;
    if let Some(patterns) = log_patterns {
        process_config.log_parsing.log_paths = patterns;
    }

    let mut collector = LogCollector::new(process_config, state_manager, metrics)?;

    // Process only the remaining content (from last checkpoint to end)
    collector.collect_remaining_metrics().await?;

    info!("Processing remaining content completed successfully");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Extract the argv of the `ExecStart=` line from the shipped systemd unit.
    fn shipped_execstart_argv() -> Vec<String> {
        let unit = include_str!("../systemd/pg-plansight-exporter.service");
        let line = unit
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("ExecStart="))
            .expect("shipped unit has no ExecStart= line");
        line.trim_start_matches("ExecStart=")
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// The unit file is executed by systemd, never by `cargo`, so nothing else
    /// in the test suite would catch a `clap` usage error in it. Regression
    /// test for `daemon --config X` being rejected because `--config` was
    /// declared on the parent command without `global = true`.
    #[test]
    fn shipped_systemd_execstart_parses() {
        let argv = shipped_execstart_argv();
        assert!(
            argv.iter().any(|a| a == "--config"),
            "unit's ExecStart no longer passes --config; update this test"
        );

        let parsed = Cli::try_parse_from(&argv);
        assert!(
            parsed.is_ok(),
            "shipped systemd ExecStart is not parseable by the CLI: {}\nargv: {argv:?}",
            parsed.err().unwrap()
        );
    }

    /// `--config` must be accepted on *both* sides of the subcommand: the unit
    /// file and `config/example.toml` document one order, the crate README the
    /// other. Both must keep working.
    #[test]
    fn config_flag_accepted_before_and_after_subcommand() {
        let after = Cli::try_parse_from(["pg-plansight-exporter", "daemon", "--config", "/tmp/c"]);
        assert!(
            after.is_ok(),
            "--config rejected after subcommand: {}",
            after.err().unwrap()
        );

        let before = Cli::try_parse_from(["pg-plansight-exporter", "--config", "/tmp/c", "daemon"]);
        assert!(
            before.is_ok(),
            "--config rejected before subcommand: {}",
            before.err().unwrap()
        );
    }

    /// Parse the shipped unit into (section, key, value) triples.
    fn shipped_unit_entries() -> Vec<(String, String, String)> {
        let unit = include_str!("../systemd/pg-plansight-exporter.service");
        let mut section = String::new();
        let mut entries = Vec::new();

        for line in unit.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.to_string();
            } else if let Some((key, value)) = line.split_once('=') {
                entries.push((
                    section.clone(),
                    key.trim().to_string(),
                    value.trim().to_string(),
                ));
            }
        }
        entries
    }

    /// `StartLimitIntervalSec`/`StartLimitBurst` moved to `[Unit]` in systemd
    /// v230. Left in `[Service]`, systemd logs "Unknown key name ... ignoring"
    /// and the crash-loop brake silently does not apply — so a daemon that
    /// fails at startup restarts forever under `Restart=always`.
    #[test]
    fn start_limit_directives_are_in_the_unit_section() {
        for (section, key, _) in shipped_unit_entries() {
            if key.starts_with("StartLimit") {
                assert_eq!(
                    section, "Unit",
                    "{key} must be in [Unit], not [{section}]; systemd ignores it there"
                );
            }
        }
    }

    /// Resolving an OTLP or pushgateway hostname goes through a local AF_UNIX
    /// socket on hosts using systemd-resolved/nscd, and glibc's resolver uses
    /// AF_NETLINK to enumerate interfaces.
    #[test]
    fn restrict_address_families_allows_local_resolution() {
        let families = shipped_unit_entries()
            .into_iter()
            .find(|(_, k, _)| k == "RestrictAddressFamilies")
            .map(|(_, _, v)| v)
            .expect("unit does not set RestrictAddressFamilies");

        for required in ["AF_UNIX", "AF_NETLINK", "AF_INET"] {
            assert!(
                families.split_whitespace().any(|f| f == required),
                "RestrictAddressFamilies is missing {required}: {families:?}"
            );
        }
    }

    /// PostgreSQL logs live elsewhere on RHEL-family distros. Without a leading
    /// `-`, systemd fails namespace setup on a nonexistent path and the unit
    /// refuses to start at all.
    #[test]
    fn optional_read_only_paths_tolerate_absence() {
        for (_, key, value) in shipped_unit_entries() {
            if key == "ReadOnlyPaths" && value.contains("/var/log/postgresql") {
                assert!(
                    value.starts_with('-'),
                    "ReadOnlyPaths={value} must be prefixed with `-`; the path does \
                     not exist on RHEL/Rocky/Alma and the unit would fail to start"
                );
            }
        }
    }

    /// The unit's `Environment=` must name the variable `main()` actually reads,
    /// otherwise the override is silently inert.
    #[test]
    fn state_db_env_var_matches_the_one_the_binary_reads() {
        let env_vars: Vec<String> = shipped_unit_entries()
            .into_iter()
            .filter(|(_, k, _)| k == "Environment")
            .map(|(_, _, v)| v)
            .collect();

        let state_db_vars: Vec<&String> = env_vars
            .iter()
            .filter(|v| v.contains("STATE_DB") || v.contains("state_db"))
            .collect();

        for var in state_db_vars {
            assert!(
                var.starts_with(STATE_DB_ENV_VAR),
                "unit sets {var}, but the binary reads {STATE_DB_ENV_VAR}"
            );
        }
    }

    #[test]
    fn state_db_flag_accepted_after_subcommand() {
        let parsed =
            Cli::try_parse_from(["pg-plansight-exporter", "daemon", "--state-db", "/tmp/s.db"]);
        assert!(
            parsed.is_ok(),
            "--state-db rejected after subcommand: {}",
            parsed.err().unwrap()
        );
    }
}
