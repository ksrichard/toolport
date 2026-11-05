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

    /// Seconds between two runs of the spec check. Each run checks the APIs whose daily
    /// check is on and whose last check is older than 24 hours.
    #[arg(long, env = "SPEC_CHECK_INTERVAL_SECS", default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
    pub spec_check_interval_secs: u64,

    /// Tool calls per minute for each MCP token. A token can send a burst of the calls of
    /// 10 seconds. 0 turns the limit off.
    #[arg(long, env = "RATE_LIMIT_PER_MINUTE", default_value_t = 60)]
    pub rate_limit_per_minute: u32,

    /// Tool calls per minute for all MCP tokens of one user together, on all servers of the
    /// user. The burst is the calls of 10 seconds. 0 turns the limit off.
    #[arg(long, env = "USER_RATE_LIMIT_PER_MINUTE", default_value_t = 300)]
    pub user_rate_limit_per_minute: u32,

    /// The URL that users open in the browser. GitHub sends users back to this URL after sign-in.
    #[arg(long, env = "PUBLIC_URL")]
    pub public_url: Url,

    /// Secret that signs the JWTs. Use at least 32 random characters.
    #[arg(long, env = "JWT_SECRET", hide_env_values = true, value_parser = parse_jwt_secret)]
    pub jwt_secret: String,

    /// Key that encrypts the upstream API secrets. 64 hex characters. Make one with: openssl rand -hex 32
    #[arg(long, env = "SECRETS_KEY", hide_env_values = true)]
    pub secrets_key: String,

    /// Lets upstream calls reach loopback addresses. Use it only for local dev and tests.
    #[arg(long, env = "ALLOW_LOCAL_UPSTREAM", default_value_t = false)]
    pub allow_local_upstream: bool,

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

pub fn parse_jwt_secret(value: &str) -> Result<String, JwtSecretTooShort> {
    if value.len() < MIN_JWT_SECRET_LEN {
        return Err(JwtSecretTooShort);
    }
    Ok(value.to_owned())
}
