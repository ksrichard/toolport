//! Imports an OpenAPI 3.0 JSON spec into a ToolPort server. This is a dev tool until the APIs page exists.

use std::{path::PathBuf, process::ExitCode, time::Duration};

use clap::Parser;
use reqwest::Url;
use server::{
    error::error_chain,
    import::{ImportError, ImportRequest, Owner, import_spec},
};
use sqlx::postgres::PgPoolOptions;

#[derive(Parser)]
#[command(
    name = "import-spec",
    about = "Import an OpenAPI 3.0 JSON spec into a ToolPort server"
)]
struct Args {
    /// Postgres connection URL.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,

    /// GitHub login of the server owner. The user must have signed in to the dashboard once.
    #[arg(long)]
    owner: String,

    /// Server name. ToolPort serves the tools at /mcp/<server>. A new name creates the server.
    #[arg(long)]
    server: String,

    /// A file path, or an http:// or https:// URL of the spec.
    #[arg(long)]
    spec: String,

    /// The base URL of the upstream API. The default is the first `servers` URL in the spec.
    #[arg(long)]
    base_url: Option<Url>,

    /// The API name. The default is the spec title. An API with the same name gets replaced.
    #[arg(long)]
    api_name: Option<String>,
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("cannot read the spec file {path}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot download the spec")]
    Download(#[source] reqwest::Error),
    #[error("cannot connect to the database")]
    DatabaseConnect(#[source] sqlx::Error),
    #[error("cannot run the database migrations")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("the import failed")]
    Import(#[from] ImportError),
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {}", error_chain(&error));
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<(), CliError> {
    let spec_text = read_spec(&args.spec).await?;

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&args.database_url)
        .await
        .map_err(CliError::DatabaseConnect)?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    let report = import_spec(
        &pool,
        ImportRequest {
            owner: Owner::Login(&args.owner),
            server: &args.server,
            api_name: args.api_name.as_deref(),
            spec_text: &spec_text,
            base_url: args.base_url,
        },
    )
    .await?;

    let server_state = if report.server_created {
        "new"
    } else {
        "existing"
    };
    let api_state = if report.replaced { "replaced" } else { "added" };
    println!("server:   {} ({server_state})", args.server);
    println!("api:      {} ({api_state})", report.api_name);
    println!("base URL: {}", report.base_url);
    println!("tools:    {}", report.tools.len());
    for name in &report.tools {
        println!("  + {name}");
    }
    if !report.skipped.is_empty() {
        println!("skipped:  {}", report.skipped.len());
        for skipped in &report.skipped {
            let method = skipped.method.map_or("*", |m| m.as_str());
            println!("  - {method} {}: {}", skipped.path, skipped.reason);
        }
    }
    Ok(())
}

async fn read_spec(source: &str) -> Result<String, CliError> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let client = reqwest::Client::builder()
            .user_agent("toolport")
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(CliError::Download)?;
        let response = client
            .get(source)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(CliError::Download)?;
        return response.text().await.map_err(CliError::Download);
    }
    let path = PathBuf::from(source);
    std::fs::read_to_string(&path).map_err(|source| CliError::ReadFile { path, source })
}
