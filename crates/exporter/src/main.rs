use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pg_plansight_exporter::{Config, ConfigReloader, LogCollector, Scheduler, StateManager};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "pg-plansight-exporter")]
#[command(about = "Prometheus exporter for PostgreSQL auto_explain logs")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, short, help = "Configuration file path")]
    config: Option<PathBuf>,

    #[arg(long, help = "Override state database path")]
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
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
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
    } else if let Ok(env_path) = std::env::var("PG_EXPORTER_STATE_DB") {
        config.state.database_path = env_path;
    }

    let state_manager = StateManager::new(&config.state.database_path);

    match cli.command {
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
                error!(
                    "Prometheus backend not found in metrics registry. \
                     This is a configuration error - prometheus is listed in backends \
                     but could not be initialized."
                );
                return;
            };

            if let Err(e) = pg_plansight_exporter::server::start_metrics_server(
                server_config.server.bind_address,
                server_config.server.metrics_path,
                Arc::new(prometheus_backend.registry.clone()),
            )
            .await
            {
                error!("Metrics server failed: {}", e);
            }
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
    let scheduler_handle = tokio::spawn(async move {
        if let Err(e) = scheduler.start().await {
            error!("Scheduler failed: {}", e);
        }
    });

    // Wait for shutdown signal
    #[cfg(feature = "prometheus")]
    if let Some(server) = server_handle {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("Received shutdown signal");
            }
            _ = server => {
                error!("Metrics server exited unexpectedly");
            }
            _ = scheduler_handle => {
                error!("Scheduler exited unexpectedly");
            }
        }
    } else {
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("Received shutdown signal");
            }
            _ = scheduler_handle => {
                error!("Scheduler exited unexpectedly");
            }
        }
    }

    #[cfg(not(feature = "prometheus"))]
    tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Received shutdown signal");
        }
        _ = scheduler_handle => {
            error!("Scheduler exited unexpectedly");
        }
    }

    info!("Shutting down");
    // Flush metric pipelines: the OTel periodic reader buffers up to a full
    // export interval of samples that are lost unless shutdown() is called.
    if let Err(e) = metrics_for_shutdown.shutdown() {
        error!("Failed to shut down metrics backends cleanly: {}", e);
    }
    Ok(())
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
