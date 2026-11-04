//! Creates, lists, and revokes MCP client tokens from the command line.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use server::{
    auth::jwt::JwtKeys,
    config::parse_jwt_secret,
    error::error_chain,
    mcp_tokens::{self, DEFAULT_DAYS, MAX_DAYS, TokenError},
};
use sqlx::postgres::PgPoolOptions;

#[derive(Parser)]
#[command(name = "mcp-token", about = "Manage the tokens that MCP clients use")]
struct Args {
    /// Postgres connection URL.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: String,

    /// Secret that signs the JWTs. It must be the same secret that the server uses.
    #[arg(long, env = "JWT_SECRET", hide_env_values = true, value_parser = parse_jwt_secret)]
    jwt_secret: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a token. The token value is shown only this one time.
    Create {
        /// The server that the token opens.
        #[arg(long)]
        server: String,
        /// A name that tells you which client uses the token, for example `claude-desktop`.
        #[arg(long)]
        name: String,
        /// Days until the token expires.
        #[arg(long, default_value_t = DEFAULT_DAYS, value_parser = clap::value_parser!(i64).range(1..=MAX_DAYS))]
        days: i64,
        /// The token never expires. Only a revoke stops it.
        #[arg(long, conflicts_with = "days")]
        never: bool,
    },
    /// List the tokens of a server. The token values are never shown again.
    List {
        #[arg(long)]
        server: String,
    },
    /// Revoke a token. This deletes the token, and MCP clients that use it get 401 at once.
    Revoke {
        /// The token id from `list`.
        #[arg(long)]
        id: i64,
    },
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("cannot connect to the database")]
    DatabaseConnect(#[source] sqlx::Error),
    #[error("cannot run the database migrations")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("the token command failed")]
    Token(#[from] TokenError),
    #[error("no token has the id {0}")]
    NoSuchToken(i64),
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
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&args.database_url)
        .await
        .map_err(CliError::DatabaseConnect)?;
    sqlx::migrate!("../../migrations").run(&pool).await?;

    match args.command {
        Command::Create {
            server,
            name,
            days,
            never,
        } => {
            let keys = JwtKeys::new(args.jwt_secret.as_bytes());
            let days = (!never).then_some(days);
            let ttl = mcp_tokens::lifetime(days)?;
            let token = mcp_tokens::create(&pool, &keys, &server, &name, ttl).await?;
            println!("token id: {}", token.id);
            match days {
                Some(days) => println!("expires:  in {days} days"),
                None => println!("expires:  never, only a revoke stops the token"),
            }
            println!();
            println!("{}", token.value);
            println!();
            println!("Copy the token now. ToolPort does not store it and cannot show it again.");
            println!("MCP clients send it as `Authorization: Bearer <token>` to /mcp/{server}.");
        }
        Command::List { server } => {
            let tokens = mcp_tokens::list(&pool, &server).await?;
            println!(
                "{:>5}  {:<20}  {:<16}  {:<16}  {:<16}  STATE",
                "ID", "NAME", "CREATED (UTC)", "EXPIRES (UTC)", "LAST USED (UTC)"
            );
            for token in tokens {
                let state = if token.expired { "expired" } else { "active" };
                println!(
                    "{:>5}  {:<20}  {:<16}  {:<16}  {:<16}  {state}",
                    token.id,
                    token.name,
                    token.created_at,
                    token.expires_at.as_deref().unwrap_or("never"),
                    token.last_used_at.as_deref().unwrap_or("never"),
                );
            }
        }
        Command::Revoke { id } => {
            if !mcp_tokens::revoke(&pool, id).await? {
                return Err(CliError::NoSuchToken(id));
            }
            println!("token {id} is revoked and deleted");
        }
    }
    Ok(())
}
