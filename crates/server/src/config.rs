use std::net::SocketAddr;

use clap::{Parser, builder::NonEmptyStringValueParser};
use reqwest::Url;

const MIN_JWT_SECRET_LEN: usize = 32;

/// Each setting can come from a command-line flag or an env var. Docker uses the env vars.
/// This type has no `Debug`, so a log line cannot print the secrets by mistake.
#[derive(Parser, Clone)]
#[command(name = "toolport-server", version, about = "ToolPort backend")]
pub struct Config {
    /// Postgres connection URL.
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// Address and port to listen on.
    #[arg(long, env = "BIND_ADDR", default_value = "0.0.0.0:8080")]
    pub bind_addr: SocketAddr,

    /// Maximum number of open database connections.
    #[arg(long, env = "DATABASE_MAX_CONNECTIONS", default_value_t = 10)]
    pub database_max_connections: u32,

    /// Time limit in milliseconds for one upstream API call of an MCP tool.
    #[arg(long, env = "UPSTREAM_TIMEOUT_MS", default_value_t = 10_000)]
    pub upstream_timeout_ms: u64,

    /// The URL that users open in the browser. GitHub sends users back to this URL after sign-in.
    #[arg(long, env = "PUBLIC_URL")]
    pub public_url: Url,

    /// Secret that signs the JWTs. Use at least 32 random characters.
    #[arg(long, env = "JWT_SECRET", hide_env_values = true, value_parser = parse_jwt_secret)]
    pub jwt_secret: String,

    /// Client ID of the GitHub OAuth app.
    #[arg(long, env = "GITHUB_CLIENT_ID", value_parser = NonEmptyStringValueParser::new())]
    pub github_client_id: String,

    /// Client secret of the GitHub OAuth app.
    #[arg(
        long,
        env = "GITHUB_CLIENT_SECRET",
        hide_env_values = true,
        value_parser = NonEmptyStringValueParser::new()
    )]
    pub github_client_secret: String,

    /// Base URL for the GitHub OAuth pages. Tests point it to a fake GitHub.
    #[arg(
        long,
        env = "GITHUB_OAUTH_URL",
        default_value = "https://github.com",
        hide = true
    )]
    pub github_oauth_url: Url,

    /// Base URL for the GitHub REST API. Tests point it to a fake GitHub.
    #[arg(
        long,
        env = "GITHUB_API_URL",
        default_value = "https://api.github.com",
        hide = true
    )]
    pub github_api_url: Url,
}

#[derive(Debug, thiserror::Error)]
#[error("the JWT secret must have at least {MIN_JWT_SECRET_LEN} characters")]
pub struct JwtSecretTooShort;

fn parse_jwt_secret(value: &str) -> Result<String, JwtSecretTooShort> {
    if value.len() < MIN_JWT_SECRET_LEN {
        return Err(JwtSecretTooShort);
    }
    Ok(value.to_owned())
}
