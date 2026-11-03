//! The MCP endpoint at `/mcp/<server>`. Each tool call becomes one HTTP request to the upstream API.

use std::{sync::Arc, time::Duration};

use axum::{
    extract::{Path, Request, State},
    http::request::Parts,
    middleware::Next,
    response::Response,
};
use reqwest::Url;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, JsonObject,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use sqlx::{PgPool, types::Json};
use toolport_core::{ArgumentError, RequestTemplate};

use crate::{
    auth::bearer_token,
    calls::{self, NewCall},
    error::{ApiError, error_chain},
    live::{CallEvent, LiveHub},
    mcp_tokens::{self, AuthorizeError, McpCaller},
    secrets::{SecretsError, SecretsKey},
    state::AppState,
    upstream::{
        self, ApiTarget, CallOutput, DraftSecret, Outcome, UpstreamAuth, UpstreamCtx,
        blocked_result,
    },
    upstream_guard::BlockedAddress,
};

pub type McpService = StreamableHttpService<ToolportMcp, LocalSessionManager>;

/// rmcp accepts only loopback `Host` headers by default, to block DNS rebinding.
/// Behind Caddy the `Host` header is the public domain, so we allow that host too.
pub fn service(handler: ToolportMcp, public_url: &Url) -> McpService {
    let mut hosts = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if let Some(host) = public_url.host_str() {
        hosts.push(host.to_owned());
        if let Some(port) = public_url.port() {
            hosts.push(format!("{host}:{port}"));
        }
    }
    let config = StreamableHttpServerConfig::default().with_allowed_hosts(hosts);
    StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

/// Route layer for `/mcp/{server}`. It checks the token and adds the `McpCaller` to the request.
/// The handler reads the caller per HTTP request, because one MCP session sends many requests,
/// and a revoked token must stop working at once.
pub async fn authorize(
    State(state): State<AppState>,
    Path(server): Path<String>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let bearer = bearer_token(request.headers());
    let caller = mcp_tokens::authorize(&state.pool, &state.auth.jwt, &server, bearer)
        .await
        .map_err(|error| match error {
            AuthorizeError::Database(source) => ApiError::Database(source),
            other => {
                tracing::debug!(error = %error_chain(&other), "MCP token rejected");
                ApiError::Unauthorized
            }
        })?;
    request.extensions_mut().insert(caller);
    Ok(next.run(request).await)
}

#[derive(Clone)]
pub struct ToolportMcp {
    pool: PgPool,
    http: reqwest::Client,
    secrets: Arc<SecretsKey>,
    allow_local_upstream: bool,
    upstream_timeout: Duration,
    live: Arc<LiveHub>,
}

/// Errors that stop a tool call before ToolPort sends the upstream request.
/// Upstream failures are not errors here: the client gets them as a tool result with `isError`.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("the request has no MCP caller")]
    MissingCaller,
    #[error("the tool {0} does not exist")]
    UnknownTool(String),
    #[error(transparent)]
    Argument(#[from] ArgumentError),
    #[error("the header parameter {0} has a value that is not valid")]
    InvalidHeader(String),
    #[error("the stored base URL {0} is not valid")]
    BaseUrl(String),
    #[error(transparent)]
    Blocked(#[from] BlockedAddress),
    #[error(transparent)]
    Secret(#[from] SecretsError),
    #[error("the stored upstream auth of the API is not valid")]
    AuthSetup,
    #[error("the database query failed")]
    Database(#[source] sqlx::Error),
}

impl From<CallError> for McpError {
    fn from(error: CallError) -> Self {
        match error {
            CallError::UnknownTool(_) | CallError::Argument(_) | CallError::InvalidHeader(_) => {
                tracing::debug!(error = %error_chain(&error), "MCP tool call rejected");
                McpError::invalid_params(error.to_string(), None)
            }
            CallError::MissingCaller
            | CallError::BaseUrl(_)
            | CallError::Blocked(_)
            | CallError::Secret(_)
            | CallError::AuthSetup
            | CallError::Database(_) => {
                tracing::error!(error = %error_chain(&error), "MCP request failed");
                McpError::internal_error("an internal error happened", None)
            }
        }
    }
}

impl ToolportMcp {
    pub fn new(
        pool: PgPool,
        http: reqwest::Client,
        secrets: Arc<SecretsKey>,
        allow_local_upstream: bool,
        upstream_timeout: Duration,
        live: Arc<LiveHub>,
    ) -> Self {
        Self {
            pool,
            http,
            secrets,
            allow_local_upstream,
            upstream_timeout,
            live,
        }
    }

    async fn tools(&self, caller: &McpCaller) -> Result<Vec<Tool>, CallError> {
        let rows: Vec<(String, String, Json<JsonObject>)> = sqlx::query_as(
            "SELECT name, description, input_schema FROM tools
             WHERE server_id = $1 AND enabled ORDER BY id",
        )
        .bind(caller.server_id)
        .fetch_all(&self.pool)
        .await
        .map_err(CallError::Database)?;
        Ok(rows
            .into_iter()
            .map(|(name, description, Json(schema))| Tool::new(name, description, schema))
            .collect())
    }

    async fn call(
        &self,
        caller: &McpCaller,
        client_name: Option<&str>,
        tool: &str,
        args: &JsonObject,
    ) -> Result<CallToolResult, CallError> {
        type Row = (
            Json<RequestTemplate>,
            i64,
            String,
            String,
            Option<String>,
            Option<Vec<u8>>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT t.request, a.id, a.base_url, a.auth_type, a.auth_header, a.auth_secret
             FROM tools t JOIN apis a ON a.id = t.api_id
             WHERE t.server_id = $1 AND t.name = $2 AND t.enabled",
        )
        .bind(caller.server_id)
        .bind(tool)
        .fetch_optional(&self.pool)
        .await
        .map_err(CallError::Database)?;
        let (Json(template), id, base_url, auth_type, auth_header, auth_secret) =
            row.ok_or_else(|| CallError::UnknownTool(tool.to_owned()))?;
        let auth = UpstreamAuth::from_columns(&auth_type, auth_header, auth_secret)
            .ok_or(CallError::AuthSetup)?;
        let api = ApiTarget { id, base_url, auth };
        let ctx = UpstreamCtx {
            http: &self.http,
            secrets: &self.secrets,
            timeout: self.upstream_timeout,
            allow_loopback: self.allow_local_upstream,
        };

        // A blocked address is not an MCP error. The client gets a tool error and the call is saved.
        let (result, outcome, latency_ms) =
            match upstream::call_upstream(&ctx, &api, DraftSecret::Saved, &template, args).await {
                Ok(CallOutput {
                    result,
                    outcome,
                    trace,
                }) => (
                    result,
                    outcome,
                    u32::try_from(trace.latency_ms).unwrap_or(u32::MAX),
                ),
                Err(CallError::Blocked(_)) => (blocked_result(), Outcome::Failed, 0),
                Err(other) => return Err(other),
            };
        // The log has no arguments and no response text, because they can hold secrets.
        tracing::info!(
            server = %caller.server_name,
            token_id = caller.token_id,
            tool,
            result = outcome.label(),
            status = outcome.status().map(|s| s.as_u16()),
            latency_ms,
            "MCP tool call",
        );
        let call = NewCall {
            server_id: caller.server_id,
            token_id: caller.token_id,
            client_name,
            tool,
            result: outcome.call_result(),
            http_status: outcome.status().map(|s| s.as_u16()),
            latency_ms,
        };
        // A failed save must not fail the tool call. The client already has its answer.
        match calls::record(&self.pool, call).await {
            Ok(record) => self.live.publish(CallEvent {
                server_id: caller.server_id,
                record,
            }),
            Err(error) => tracing::error!(error = %error_chain(&error), "cannot save the call"),
        }
        Ok(result)
    }
}

impl ServerHandler for ToolportMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("toolport", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let caller = mcp_caller(&context)?;
        self.live.seen(&caller, client_name(&context).as_deref());
        Ok(ListToolsResult::with_all_items(self.tools(&caller).await?))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let caller = mcp_caller(&context)?;
        let client = client_name(&context);
        self.live.seen(&caller, client.as_deref());
        let args = request.arguments.unwrap_or_default();
        let result = self
            .call(&caller, client.as_deref(), &request.name, &args)
            .await?;
        Ok(result.into())
    }
}

fn mcp_caller(context: &RequestContext<RoleServer>) -> Result<McpCaller, CallError> {
    context
        .extensions
        .get::<Parts>()
        .and_then(|parts| parts.extensions.get::<McpCaller>())
        .cloned()
        .ok_or(CallError::MissingCaller)
}

/// The name that the client sent in `initialize`, for example `claude-ai`.
fn client_name(context: &RequestContext<RoleServer>) -> Option<String> {
    context
        .peer
        .peer_info()
        .map(|info| info.client_info.name.clone())
}
