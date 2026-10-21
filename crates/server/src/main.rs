use std::{net::SocketAddr, process::ExitCode};

use clap::Parser;
use server::error::{StartupError, error_chain};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

/// Each setting can come from a command-line flag or an env var. Docker uses the env vars.
#[derive(Parser)]
#[command(name = "toolport-server", version, about = "ToolPort backend")]
struct Args {
    /// Postgres connection URL.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,

    /// Address and port to listen on.
    #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
    bind_addr: SocketAddr,

    /// Maximum number of open database connections.
    #[arg(long, env = "DATABASE_MAX_CONNECTIONS", default_value_t = 10)]
    database_max_connections: u32,
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error_chain(&error), "server stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), StartupError> {
    let pool = PgPoolOptions::new()
        .max_connections(args.database_max_connections)
        .connect(&args.database_url)
        .await
        .map_err(StartupError::DatabaseConnect)?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    let listener =
        TcpListener::bind(args.bind_addr)
            .await
            .map_err(|source| StartupError::Bind {
                addr: args.bind_addr,
                source,
            })?;
    tracing::info!(bind_addr = %args.bind_addr, "server listening");

    axum::serve(listener, server::app(pool))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(StartupError::Serve)
}

/// Docker stops containers with SIGTERM. A local run stops with Ctrl+C.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received");
}
