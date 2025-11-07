use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pg_loganalyze_exporter::{Config, ConfigWatcher, LogCollector, MetricsRegistry, Scheduler, StateManager};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::signal;
use tracing::{error, info};

#[derive(Parser)]
#[command(name = "pg-loganalyze-exporter")]
#[command(about = "Prometheus exporter for PostgreSQL auto_explain logs")]
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

async fn run_daemon(config: Config, state_manager: StateManager, config_path: Option<PathBuf>) -> Result<()> {
    info!("Starting pg-loganalyze-exporter daemon");

    // Initialize state database
    state_manager
        .initialize()
        .context("Failed to initialize state database")?;

    #[cfg(feature = "prometheus")]
    {
        // Initialize metrics registry
        let metrics = Arc::new(
            MetricsRegistry::new(
                &config.metrics.namespace,
                config.metrics.histogram_buckets.clone(),
            )
            .context("Failed to initialize metrics registry")?,
        );

        // Start metrics server
        let server_config = config.clone();
        let server_registry = metrics.registry.clone();
        let server_handle = tokio::spawn(async move {
            if let Err(e) = pg_loganalyze_exporter::server::start_metrics_server(
                server_config.server.bind_address,
                server_config.server.metrics_path,
                Arc::new(server_registry),
            )
            .await
            {
                error!("Metrics server failed: {}", e);
            }
        });

        // Set up hot reload if config file is provided
        let config_rx = if let Some(ref path) = config_path {
            info!("Hot reload enabled for config file: {}", path.display());
            let (_watcher, rx) = ConfigWatcher::new(path.clone(), config.clone())
                .context("Failed to set up config watcher")?;
            // Keep watcher alive by storing it
            tokio::spawn(async move {
                // Watcher needs to stay alive for the duration of the program
                let _keep_alive = _watcher;
                tokio::signal::ctrl_c().await.ok();
            });
            Some(rx)
        } else {
            info!("Hot reload disabled (no config file specified)");
            None
        };

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
        tokio::select! {
            _ = signal::ctrl_c() => {
                info!("Received shutdown signal");
            }
            _ = server_handle => {
                error!("Metrics server exited unexpectedly");
            }
            _ = scheduler_handle => {
                error!("Scheduler exited unexpectedly");
            }
        }
    }

    #[cfg(not(feature = "prometheus"))]
    {
        anyhow::bail!("Prometheus feature not enabled");
    }

    info!("Shutting down");
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

    #[cfg(feature = "prometheus")]
    {
        let metrics = Arc::new(MetricsRegistry::new(
            &config.metrics.namespace,
            config.metrics.histogram_buckets.clone(),
        )?);

        // Override log paths with provided patterns
        let mut process_config = config;
        process_config.log_parsing.log_paths = log_patterns;

        let mut collector = LogCollector::new(process_config, state_manager, metrics)?;
        collector.collect_metrics().await?;

        info!("Processing completed successfully");
    }

    #[cfg(not(feature = "prometheus"))]
    {
        anyhow::bail!("Prometheus feature not enabled");
    }

    Ok(())
}

async fn run_process_rest_command(
    config: Config,
    state_manager: StateManager,
    log_patterns: Option<Vec<String>>,
) -> Result<()> {
    info!("Processing remaining unread content from files");

    state_manager.initialize()?;

    #[cfg(feature = "prometheus")]
    {
        let metrics = Arc::new(MetricsRegistry::new(
            &config.metrics.namespace,
            config.metrics.histogram_buckets.clone(),
        )?);

        // Use provided patterns or fall back to config
        let mut process_config = config;
        if let Some(patterns) = log_patterns {
            process_config.log_parsing.log_paths = patterns;
        }

        let mut collector = LogCollector::new(process_config, state_manager, metrics)?;

        // Process only the remaining content (from last checkpoint to end)
        collector.collect_remaining_metrics().await?;

        info!("Processing remaining content completed successfully");
    }

    #[cfg(not(feature = "prometheus"))]
    {
        anyhow::bail!("Prometheus feature not enabled");
    }

    Ok(())
}
