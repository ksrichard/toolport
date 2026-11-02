//! The MCP endpoint at `/mcp/<server>`. Each tool call becomes one HTTP request to the upstream API.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, Request, State},
    http::{
        HeaderName, HeaderValue, StatusCode,
        header::{ACCEPT, CONTENT_TYPE},
        request::Parts,
    },
    middleware::Next,
    response::Response,
};
use reqwest::{Method, Url};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
        Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use sqlx::{PgPool, types::Json};
use toolport_core::{ArgumentError, HttpMethod, RequestTemplate, UpstreamRequest, build_request};

use crate::{
    error::{ApiError, error_chain},
    state::AppState,
};

/// A larger upstream response is cut, so one call cannot fill the memory or the client context.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

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

/// The server in the request path. `resolve_server` adds it to each HTTP request.
/// The handler reads it per request, because one MCP session can send many HTTP requests.
#[derive(Debug, Clone)]
pub struct CalledServer {
    pub id: i64,
    pub name: String,
}

/// Route layer for `/mcp/{server}`. An unknown server gets a JSON 404 before rmcp sees the request.
pub async fn resolve_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let id: i64 = sqlx::query_scalar("SELECT id FROM servers WHERE name = $1")
        .bind(&name)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::McpServerNotFound)?;
    request.extensions_mut().insert(CalledServer { id, name });
    Ok(next.run(request).await)
}

#[derive(Clone)]
pub struct ToolportMcp {
    pool: PgPool,
    http: reqwest::Client,
    upstream_timeout: Duration,
}

/// Errors that stop a tool call before ToolPort sends the upstream request.
/// Upstream failures are not errors here: the client gets them as a tool result with `isError`.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("the request has no MCP server")]
    MissingServer,
    #[error("the tool {0} does not exist")]
    UnknownTool(String),
    #[error(transparent)]
    Argument(#[from] ArgumentError),
    #[error("the header parameter {0} has a value that is not valid")]
    InvalidHeader(String),
    #[error("the stored base URL {0} is not valid")]
    BaseUrl(String),
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
            CallError::MissingServer | CallError::BaseUrl(_) | CallError::Database(_) => {
                tracing::error!(error = %error_chain(&error), "MCP request failed");
                McpError::internal_error("an internal error happened", None)
            }
        }
    }
}

impl ToolportMcp {
    pub fn new(pool: PgPool, http: reqwest::Client, upstream_timeout: Duration) -> Self {
        Self {
            pool,
            http,
            upstream_timeout,
        }
    }

    async fn tools(&self, server: &CalledServer) -> Result<Vec<Tool>, CallError> {
        let rows: Vec<(String, String, Json<JsonObject>)> = sqlx::query_as(
            "SELECT name, description, input_schema FROM tools
             WHERE server_id = $1 AND enabled ORDER BY id",
        )
        .bind(server.id)
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
        server: &CalledServer,
        tool: &str,
        args: &JsonObject,
    ) -> Result<CallToolResult, CallError> {
        let row: Option<(Json<RequestTemplate>, String)> = sqlx::query_as(
            "SELECT t.request, a.base_url FROM tools t JOIN apis a ON a.id = t.api_id
             WHERE t.server_id = $1 AND t.name = $2 AND t.enabled",
        )
        .bind(server.id)
        .bind(tool)
        .fetch_optional(&self.pool)
        .await
        .map_err(CallError::Database)?;
        let (Json(template), base_url) =
            row.ok_or_else(|| CallError::UnknownTool(tool.to_owned()))?;

        let upstream = build_request(&template, args)?;
        let url = upstream_url(&base_url, &upstream)?;
        let mut builder = self
            .http
            .request(method(upstream.method), url)
            .timeout(self.upstream_timeout)
            .header(ACCEPT, "application/json");
        for (name, value) in &upstream.headers {
            let invalid = || CallError::InvalidHeader(name.clone());
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
            let header_value = HeaderValue::from_str(value).map_err(|_| invalid())?;
            builder = builder.header(header_name, header_value);
        }
        if let Some((content_type, body)) = &upstream.body {
            builder = builder
                .header(CONTENT_TYPE, content_type)
                .body(body.to_string());
        }

        let started = Instant::now();
        let outcome = send(builder).await;
        // The log has no arguments and no response text, because they can hold secrets.
        tracing::info!(
            server = %server.name,
            tool,
            result = outcome.label(),
            status = outcome.status().map(|s| s.as_u16()),
            latency_ms = started.elapsed().as_millis() as u64,
            "MCP tool call",
        );
        Ok(outcome.into_result(self.upstream_timeout))
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
        let server = called_server(&context)?;
        Ok(ListToolsResult::with_all_items(self.tools(&server).await?))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let server = called_server(&context)?;
        let args = request.arguments.unwrap_or_default();
        Ok(self.call(&server, &request.name, &args).await?.into())
    }
}

fn called_server(context: &RequestContext<RoleServer>) -> Result<CalledServer, CallError> {
    context
        .extensions
        .get::<Parts>()
        .and_then(|parts| parts.extensions.get::<CalledServer>())
        .cloned()
        .ok_or(CallError::MissingServer)
}

fn method(method: HttpMethod) -> Method {
    match method {
        HttpMethod::Get => Method::GET,
        HttpMethod::Put => Method::PUT,
        HttpMethod::Post => Method::POST,
        HttpMethod::Delete => Method::DELETE,
        HttpMethod::Options => Method::OPTIONS,
        HttpMethod::Head => Method::HEAD,
        HttpMethod::Patch => Method::PATCH,
        HttpMethod::Trace => Method::TRACE,
    }
}

/// Adds the path segments to the base URL path, for example `/api/v3` + `pet/7`.
/// The URL encoder escapes each segment, so a `/` inside a value stays inside its segment.
fn upstream_url(base: &str, request: &UpstreamRequest) -> Result<Url, CallError> {
    let invalid = || CallError::BaseUrl(base.to_owned());
    let mut url = Url::parse(base).map_err(|_| invalid())?;
    url.path_segments_mut()
        .map_err(|()| invalid())?
        .pop_if_empty()
        .extend(&request.path_segments);
    if !request.query.is_empty() {
        url.query_pairs_mut().extend_pairs(&request.query);
    }
    Ok(url)
}

enum Outcome {
    Answered {
        status: StatusCode,
        text: String,
        cut: bool,
    },
    TimedOut,
    Failed,
}

impl Outcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Answered { status, .. } if status.is_success() => "ok",
            Self::Answered { .. } => "upstream_error",
            Self::TimedOut => "timed_out",
            Self::Failed => "request_failed",
        }
    }

    fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Answered { status, .. } => Some(*status),
            Self::TimedOut | Self::Failed => None,
        }
    }

    fn into_result(self, timeout: Duration) -> CallToolResult {
        match self {
            Self::Answered { status, text, cut } => {
                let mut message = if status.is_success() {
                    text
                } else {
                    format!("the upstream API answered HTTP {status}\n\n{text}")
                };
                if message.is_empty() {
                    message = format!("the upstream API answered HTTP {status} with an empty body");
                }
                if cut {
                    message.push_str("\n\n[ToolPort cut this response at 1 MiB]");
                }
                let content = vec![ContentBlock::text(message)];
                if status.is_success() {
                    CallToolResult::success(content)
                } else {
                    CallToolResult::error(content)
                }
            }
            Self::TimedOut => CallToolResult::error(vec![ContentBlock::text(format!(
                "the upstream API did not answer within {} ms",
                timeout.as_millis()
            ))]),
            Self::Failed => CallToolResult::error(vec![ContentBlock::text(
                "the upstream API cannot be reached",
            )]),
        }
    }
}

async fn send(builder: reqwest::RequestBuilder) -> Outcome {
    let result = match builder.send().await {
        Ok(response) => {
            let status = response.status();
            read_body(response)
                .await
                .map(|(text, cut)| Outcome::Answered { status, text, cut })
        }
        Err(error) => Err(error),
    };
    result.unwrap_or_else(|error| {
        if error.is_timeout() {
            Outcome::TimedOut
        } else {
            tracing::warn!(error = %error_chain(&error), "upstream request failed");
            Outcome::Failed
        }
    })
}

/// Reads at most `MAX_RESPONSE_BYTES`. The bool is true when the body was longer.
async fn read_body(mut response: reqwest::Response) -> Result<(String, bool), reqwest::Error> {
    let mut bytes = Vec::new();
    let mut cut = false;
    while let Some(chunk) = response.chunk().await? {
        let room = MAX_RESPONSE_BYTES - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            cut = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), cut))
}
