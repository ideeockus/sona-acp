//! `control-plane`: one process with every component (D20, D44).
//!
//! Logs go to stderr; whoever runs the process collects and rotates them.

mod config;

use clap::{Parser, Subcommand};
use config::Config;
use control_plane_agent_journal::{Clock, FileJournal};
use control_plane_agent_registry::SqliteRegistry;
use control_plane_agent_runtime::launcher::{ContainerUser, DockerLauncher};
use control_plane_agent_runtime::{Runtime, RuntimeConfig};
use control_plane_api::{Services, router};
use control_plane_common::Timestamp;
use control_plane_common::files::FileStorage;
use control_plane_common::registry::Credentials;
use control_plane_file_storage::{NoFileStorage, S3Config, S3Store, SqliteFileStorage};
use control_plane_message_broker::SqliteBroker;
use control_plane_scheduler::SqliteScheduler;
use control_plane_task_manager::SqliteTaskManager;
use control_plane_tool_gateway::Gateway;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about = "Agent Control Plane")]
struct Cli {
    /// The configuration file.
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates the operator's credential and prints it once (D71).
    Init,
    /// Runs the control plane.
    Serve,
}

type Failure = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let result = match Config::load(&cli.config) {
        Ok(config) => match cli.command {
            Command::Init => init(config).await,
            Command::Serve => serve(config).await,
        },
        Err(e) => Err(e.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("control-plane: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Goes straight to Registry: the API needs a credential, and this makes the first one.
async fn init(config: Config) -> Result<(), Failure> {
    std::fs::create_dir_all(&config.data_dir)?;
    let registry = SqliteRegistry::new(database(&config.data_dir, "registry").await?).await?;
    let credential = registry.create_operator(&config.operator).await?;
    // Only its hash is stored, so this is the one time it can be shown (D68).
    println!("{}", credential.expose());
    Ok(())
}

async fn serve(config: Config) -> Result<(), Failure> {
    std::fs::create_dir_all(&config.data_dir)?;
    let clock: Clock = Arc::new(Timestamp::now);
    let journal = Arc::new(
        FileJournal::open(
            config.data_dir.join("journal"),
            config.journal_retention_days,
            clock,
        )
        .await?,
    );
    let registry =
        Arc::new(SqliteRegistry::new(database(&config.data_dir, "registry").await?).await?);
    let files: Arc<dyn FileStorage> = match &config.storage {
        Some(storage) => {
            let objects = S3Store::connect(S3Config {
                endpoint: storage.endpoint.clone(),
                bucket: storage.bucket.clone(),
                access_key: required_env("SONA_S3_ACCESS_KEY")?,
                secret_key: required_env("SONA_S3_SECRET_KEY")?,
            })
            .await?;
            Arc::new(
                SqliteFileStorage::new(
                    database(&config.data_dir, "files").await?,
                    Arc::new(objects),
                    registry.clone(),
                    journal.clone(),
                )
                .await?,
            )
        }
        None => Arc::new(NoFileStorage),
    };
    let broker = Arc::new(
        SqliteBroker::new(
            database(&config.data_dir, "broker").await?,
            registry.clone(),
            registry.clone(),
            files.clone(),
            journal.clone(),
        )
        .await?,
    );
    let tasks = Arc::new(
        SqliteTaskManager::new(
            database(&config.data_dir, "tasks").await?,
            registry.clone(),
            registry.clone(),
            journal.clone(),
        )
        .await?,
    );
    let gateway = Arc::new(Gateway::new(registry.clone(), journal.clone()));

    let runtime = Arc::new(
        Runtime::new(
            RuntimeConfig {
                data_dir: config.data_dir.clone(),
                extension: config.extension,
                api_url: config.api_url,
                models_json: config.models_json,
                provider_env: provider_env(&config.provider_env)?,
                // Numbers to be tuned on the first real runs (D91).
                retry_delays: vec![Duration::from_secs(30), Duration::from_secs(120)],
                limits: config.limits.to_runtime(),
            },
            database(&config.data_dir, "runtime").await?,
            registry.clone(),
            registry.clone(),
            broker.clone(),
            journal.clone(),
            Arc::new(DockerLauncher::new(
                config.agent_image,
                config.agent_network,
                ContainerUser::owner_of(&config.data_dir)?,
            )),
        )
        .await?,
    );
    runtime.start().await?;

    let schedules = Arc::new(
        SqliteScheduler::new(
            database(&config.data_dir, "scheduler").await?,
            broker.clone(),
            broker.clone(),
            registry.clone(),
            journal.clone(),
            Arc::new(Timestamp::now),
        )
        .await?,
    );
    schedules.start().await?;

    let app = router(Services {
        credentials: registry.clone(),
        registry,
        broker: broker.clone(),
        delivery: broker,
        runs: runtime,
        tasks,
        gateway,
        journal,
        files,
        schedules,
    });
    let listener = TcpListener::bind(config.listen).await?;
    tracing::info!(address = %listener.local_addr()?, "listening");

    // Active Runs are not drained on shutdown: the next start revokes their tokens
    // (D24) and delivers their messages again (D49).
    let mut terminate = signal(SignalKind::terminate())?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        })
        .await?;
    tracing::info!("stopped");
    Ok(())
}

/// One SQLite file per component (D33).
async fn database(data_dir: &Path, name: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(data_dir.join(format!("{name}.db")))
        .create_if_missing(true)
        // Readers do not block the writer.
        .journal_mode(SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(options).await
}

/// Takes the named variables from our environment. A missing one fails the start.
fn provider_env(names: &[String]) -> Result<Vec<(String, String)>, String> {
    names
        .iter()
        .map(|name| Ok((name.clone(), required_env(name)?)))
        .collect()
}

fn required_env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set"))
}
