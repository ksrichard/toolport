use std::{process::ExitCode, time::Duration};

use clap::Parser;
use server::{
    calls,
    config::Config,
    error::{StartupError, error_chain},
    spec_check,
    state::AppState,
};
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const CALL_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);

#[tokio::main]
async fn main() -> ExitCode {
    let config = Config::parse();

    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    match run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error_chain(&error), "server stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run(config: Config) -> Result<(), StartupError> {
    let pool = PgPoolOptions::new()
        .max_connections(config.database_max_connections)
        .connect(&config.database_url)
        .await
        .map_err(StartupError::DatabaseConnect)?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .map_err(|source| StartupError::Bind {
            addr: config.bind_addr,
            source,
        })?;
    tracing::info!(bind_addr = %config.bind_addr, "server listening");

    let state = AppState::new(pool, &config)?;
    spec_check::spawn_daily_check(
        state.clone(),
        Duration::from_secs(config.spec_check_interval_secs),
    );
    calls::spawn_cleanup(state.pool.clone(), CALL_CLEANUP_INTERVAL);
    // A manual edit of the `plans` table sends no webhook, so the tool limits apply here.
    server::plan::spawn_reconcile(state.clone(), CALL_CLEANUP_INTERVAL);
    axum::serve(listener, server::app(state))
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
