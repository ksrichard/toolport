use std::{sync::Arc, time::Duration};

use reqwest::Url;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use sqlx::PgPool;

use crate::{
    auth::jwt::JwtKeys,
    config::Config,
    error::StartupError,
    live::LiveHub,
    mcp::{self, McpService, ToolportMcp},
    rate_limit::RateLimiter,
    secrets::SecretsKey,
    sessions::SessionPeers,
    upstream_guard,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub auth: Arc<AuthSettings>,
    /// Guarded client for user-chosen URLs. `http` is for GitHub only.
    pub upstream: reqwest::Client,
    pub allow_local_upstream: bool,
    pub http: reqwest::Client,
    pub mcp: McpService,
    pub live: Arc<LiveHub>,
    pub secrets: Arc<SecretsKey>,
    pub peers: Arc<SessionPeers>,
    pub mcp_sessions: Arc<LocalSessionManager>,
    pub upstream_timeout: Duration,
    pub rate_limit: Arc<RateLimiter>,
}

pub struct AuthSettings {
    pub public_url: Url,
    /// Browsers drop `Secure` cookies on plain http, so local http runs turn this off.
    pub secure_cookies: bool,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub github_oauth_url: Url,
    pub github_api_url: Url,
    pub jwt: JwtKeys,
}

impl AppState {
    pub fn new(pool: PgPool, config: &Config) -> Result<Self, StartupError> {
        let http = reqwest::Client::builder()
            .user_agent("toolport")
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(StartupError::HttpClient)?;
        let upstream = upstream_guard::upstream_client(config.allow_local_upstream)
            .map_err(StartupError::HttpClient)?;
        let auth = AuthSettings {
            secure_cookies: config.public_url.scheme() == "https",
            public_url: config.public_url.clone(),
            github_client_id: config.github_client_id.clone(),
            github_client_secret: config.github_client_secret.clone(),
            github_oauth_url: config.github_oauth_url.clone(),
            github_api_url: config.github_api_url.clone(),
            jwt: JwtKeys::new(config.jwt_secret.as_bytes()),
        };
        let secrets =
            Arc::new(SecretsKey::from_hex(&config.secrets_key).map_err(StartupError::SecretsKey)?);
        let live = Arc::new(LiveHub::default());
        let peers = Arc::new(SessionPeers::default());
        let upstream_timeout = Duration::from_millis(config.upstream_timeout_ms);
        let rate_limit = Arc::new(RateLimiter::new(
            config.rate_limit_per_minute,
            config.user_rate_limit_per_minute,
        ));
        let handler = ToolportMcp {
            pool: pool.clone(),
            http: upstream.clone(),
            secrets: secrets.clone(),
            allow_local_upstream: config.allow_local_upstream,
            upstream_timeout,
            live: live.clone(),
            peers: peers.clone(),
            rate_limit: rate_limit.clone(),
        };
        let mcp_sessions = Arc::new(LocalSessionManager::default());
        Ok(Self {
            live,
            mcp: mcp::service(handler, mcp_sessions.clone(), &config.public_url),
            mcp_sessions,
            pool,
            auth: Arc::new(auth),
            upstream,
            allow_local_upstream: config.allow_local_upstream,
            http,
            secrets,
            peers,
            upstream_timeout,
            rate_limit,
        })
    }
}
