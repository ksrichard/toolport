//! Dashboard routes for servers, their tools, and API imports.
//! The onboarding uses them to go from an empty account to a working MCP server.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::{get, post, put},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use toolport_core::server_name::is_valid_server_name;
use ts_rs::TS;

use crate::upstream_guard::check_url;
use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson},
    import::{ImportReport, ImportRequest, Owner, import_spec},
    plan,
    state::AppState,
    templates::{self, TEMPLATES, Template},
};

/// Large public specs (for example Stripe or GitHub) are several MiB.
pub(crate) const MAX_SPEC_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Serialize, TS, sqlx::FromRow)]
#[ts(export)]
pub struct ServerSummary {
    #[serde(skip)]
    #[ts(skip)]
    pub id: i64,
    pub name: String,
    #[sqlx(skip)]
    pub mcp_url: String,
    #[ts(type = "number")]
    pub apis: i64,
    #[ts(type = "number")]
    pub tools: i64,
    #[ts(type = "number")]
    pub enabled_tools: i64,
    /// Tokens that are not expired.
    #[ts(type = "number")]
    pub active_tokens: i64,
    /// Milliseconds since the Unix epoch. `None` when the server has no saved call.
    #[ts(type = "number | null")]
    pub last_call_ms: Option<i64>,
    /// Clients with an MCP request in the last 2 minutes. The count is in memory only.
    #[sqlx(skip)]
    #[ts(type = "number")]
    pub connected_clients: i64,
    /// True when the server is over the plan limit. It does not serve MCP requests then.
    pub paused: bool,
    /// Enabled tools that the plan does not serve.
    #[ts(type = "number")]
    pub paused_tools: i64,
}

#[derive(Debug, Serialize, TS, sqlx::FromRow)]
#[ts(export)]
pub struct ToolSummary {
    pub name: String,
    pub description: String,
    pub method: String,
    pub path: String,
    pub api: String,
    pub enabled: bool,
    /// `spec` or `manual`.
    pub source: String,
    pub edited: bool,
    pub spec_changed: bool,
    /// True when the tool is on, but over the plan limit, so MCP clients do not get it.
    pub paused: bool,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateServer {
    pub name: String,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct RenameServer {
    pub name: String,
}

/// Where the spec of a new API comes from.
#[derive(Debug, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case")]
#[ts(export)]
pub enum ImportApi {
    Template {
        template: String,
    },
    Text {
        spec: String,
        #[ts(optional)]
        base_url: Option<String>,
    },
    Url {
        spec_url: String,
        #[ts(optional)]
        base_url: Option<String>,
    },
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ImportedApi {
    pub name: String,
    pub base_url: String,
    /// True when an API with the same name existed. It was updated in place.
    pub replaced: bool,
    pub tools: Vec<String>,
    /// Spec tools that this import created.
    pub added: Vec<String>,
    /// New tools that start off, because the plan allows no more enabled tools.
    pub disabled: Vec<String>,
    /// Unedited spec tools that the new spec changed.
    pub changed: Vec<String>,
    /// Edited tools that stayed.
    pub kept: Vec<String>,
    /// Spec tools deleted because the spec no longer has their operation.
    pub removed: Vec<String>,
    pub skipped: Vec<SkippedTool>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct SkippedTool {
    /// `None` when the whole path was skipped.
    pub method: Option<String>,
    pub path: String,
    pub reason: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/templates", get(list_templates))
        .route("/servers", get(list_servers).post(create_server))
        .route("/servers/{name}", put(rename_server).delete(delete_server))
        .route("/servers/{name}/tools", get(list_tools))
        .route(
            "/servers/{name}/apis",
            post(import_api).layer(DefaultBodyLimit::max(MAX_SPEC_BYTES)),
        )
}

/// Returns the id of a server that the user owns. Another user's server gives 404, not 403,
/// so a user cannot find out which names exist.
pub async fn owned_server_id(pool: &PgPool, user_id: i64, name: &str) -> Result<i64, ApiError> {
    sqlx::query_scalar("SELECT id FROM servers WHERE name = $1 AND user_id = $2")
        .bind(name)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::NotFound)
}

pub(crate) fn mcp_url(state: &AppState, name: &str) -> String {
    let base = state.auth.public_url.as_str().trim_end_matches('/');
    format!("{base}/mcp/{name}")
}

async fn list_templates() -> Json<&'static [Template]> {
    Json(TEMPLATES)
}

async fn list_servers(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<ServerSummary>>, ApiError> {
    Ok(Json(summaries(&state, auth.user_id, None).await?))
}

/// The servers of a user with their status. `only` limits the list to one server id.
/// Each count is a subquery on an indexed `server_id` column, so one query serves all servers.
async fn summaries(
    state: &AppState,
    user_id: i64,
    only: Option<i64>,
) -> Result<Vec<ServerSummary>, ApiError> {
    let mut servers: Vec<ServerSummary> = sqlx::query_as(
        "SELECT s.id, s.name,
                (p.max_servers IS NOT NULL AND s.rank > p.max_servers) AS paused,
                (SELECT greatest(count(*) - coalesce(p.max_tools_per_server, count(*)), 0)
                    FROM tools t WHERE t.server_id = s.id AND t.enabled) AS paused_tools,
                (SELECT count(*) FROM apis a WHERE a.server_id = s.id) AS apis,
                (SELECT count(*) FROM tools t WHERE t.server_id = s.id) AS tools,
                (SELECT count(*) FROM tools t WHERE t.server_id = s.id AND t.enabled) AS enabled_tools,
                (SELECT count(*) FROM mcp_tokens k WHERE k.server_id = s.id
                    AND (k.expires_at IS NULL OR k.expires_at > now())) AS active_tokens,
                (SELECT (extract(epoch FROM max(c.created_at)) * 1000)::bigint
                    FROM calls c WHERE c.server_id = s.id) AS last_call_ms
         FROM (SELECT *, row_number() OVER (ORDER BY created_at, id) AS rank
               FROM servers WHERE user_id = $1) s
         JOIN users u ON u.id = s.user_id JOIN plans p ON p.id = u.plan_id
         WHERE $2::bigint IS NULL OR s.id = $2
         ORDER BY s.created_at, s.id",
    )
    .bind(user_id)
    .bind(only)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::Database)?;
    let clients = state.live.client_counts();
    for server in &mut servers {
        server.mcp_url = mcp_url(state, &server.name);
        server.connected_clients = clients.get(&server.id).copied().unwrap_or(0);
    }
    Ok(servers)
}

async fn create_server(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(body): ApiJson<CreateServer>,
) -> Result<(StatusCode, Json<ServerSummary>), ApiError> {
    let name = body.name.trim();
    if !is_valid_server_name(name) {
        return Err(ApiError::InvalidServerName);
    }
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    plan::check_server_limit(&mut tx, auth.user_id, 1).await?;
    let id = sqlx::query_scalar("INSERT INTO servers (user_id, name) VALUES ($1, $2) RETURNING id")
        .bind(auth.user_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| name_error(error, name))?;
    tx.commit().await.map_err(ApiError::Database)?;
    let summary = ServerSummary {
        id,
        name: name.to_owned(),
        mcp_url: mcp_url(&state, name),
        apis: 0,
        tools: 0,
        enabled_tools: 0,
        active_tokens: 0,
        last_call_ms: None,
        connected_clients: 0,
        paused: false,
        paused_tools: 0,
    };
    Ok((StatusCode::CREATED, Json(summary)))
}

/// Server names are unique for all users, because the MCP URL has no user part.
pub(crate) fn name_error(error: sqlx::Error, name: &str) -> ApiError {
    match &error {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::ServerNameTaken(name.to_owned())
        }
        _ => ApiError::Database(error),
    }
}

/// A rename changes the MCP URL. The open MCP sessions close, and clients must use the new URL.
/// The tokens stay valid, because they hold the server id and not the name.
async fn rename_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<RenameServer>,
) -> Result<Json<ServerSummary>, ApiError> {
    let new_name = body.name.trim();
    if !is_valid_server_name(new_name) {
        return Err(ApiError::InvalidServerName);
    }
    let id: i64 = sqlx::query_scalar(
        "UPDATE servers SET name = $1, updated_at = now()
         WHERE name = $2 AND user_id = $3 RETURNING id",
    )
    .bind(new_name)
    .bind(&name)
    .bind(auth.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| name_error(error, new_name))?
    .ok_or(ApiError::NotFound)?;
    if new_name != name {
        state.peers.close_server(id, &state.mcp_sessions).await;
    }
    summaries(&state, auth.user_id, Some(id))
        .await?
        .pop()
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// The database deletes the APIs, tools, tokens, and calls of the server with it.
async fn delete_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let id: i64 =
        sqlx::query_scalar("DELETE FROM servers WHERE name = $1 AND user_id = $2 RETURNING id")
            .bind(&name)
            .bind(auth.user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::Database)?
            .ok_or(ApiError::NotFound)?;
    state.peers.close_server(id, &state.mcp_sessions).await;
    state.live.forget_server(id);
    plan::plan_changed(&state, auth.user_id)
        .await
        .map_err(ApiError::Database)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_tools(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<Vec<ToolSummary>>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let tools = sqlx::query_as(
        "SELECT t.name, t.description, t.request->>'method' AS method,
                t.request->>'path' AS path, a.name AS api, t.enabled,
                t.source, t.edited,
                (t.edited AND t.edited_hash IS NOT NULL AND t.spec_hash <> t.edited_hash)
                    AS spec_changed,
                (t.enabled AND p.max_tools_per_server IS NOT NULL
                    AND (SELECT count(*) FROM tools e
                         WHERE e.server_id = t.server_id AND e.enabled AND e.id < t.id)
                        >= p.max_tools_per_server) AS paused
         FROM tools t JOIN apis a ON a.id = t.api_id
         JOIN servers s ON s.id = t.server_id JOIN users u ON u.id = s.user_id
         JOIN plans p ON p.id = u.plan_id
         WHERE t.server_id = $1 ORDER BY t.id",
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::Database)?;
    Ok(Json(tools))
}

/// Imports one API, or every API of a template. Each API has its own transaction, so a
/// failure keeps the APIs before it. The same request can run again, because an API with
/// the same name gets updated.
async fn import_api(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<ImportApi>,
) -> Result<Json<Vec<ImportedApi>>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;

    let (specs, base_url, spec_url): (Vec<String>, Option<String>, Option<String>) = match body {
        ImportApi::Template { template } => {
            let template = templates::find(&template).ok_or(ApiError::NotFound)?;
            let specs = template.specs.iter().map(|s| (*s).to_owned()).collect();
            (specs, None, None)
        }
        ImportApi::Text { spec, base_url } => (vec![spec], base_url, None),
        ImportApi::Url { spec_url, base_url } => {
            let url = parse_spec_url(&spec_url)?;
            let text = download_spec(&state, &url).await?;
            (vec![text], base_url, Some(url.to_string()))
        }
    };
    let base_url = match base_url.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(text) => Some(Url::parse(text).map_err(|_| ApiError::InvalidBaseUrl)?),
    };

    let mut imported = Vec::with_capacity(specs.len());
    for spec_text in &specs {
        let report = import_spec(
            &state.pool,
            ImportRequest {
                owner: Owner::UserId(auth.user_id),
                server: &name,
                api_name: None,
                spec_text,
                base_url: base_url.clone(),
                spec_url: spec_url.as_deref(),
            },
        )
        .await?;
        state.peers.notify_tools_changed(server_id).await;
        imported.push(report.into());
    }
    Ok(Json(imported))
}

impl From<ImportReport> for ImportedApi {
    fn from(report: ImportReport) -> Self {
        Self {
            name: report.api_name,
            base_url: report.base_url.to_string(),
            replaced: report.replaced,
            tools: report.tools,
            added: report.added,
            disabled: report.disabled,
            changed: report.changed,
            kept: report.kept,
            removed: report.removed,
            skipped: report
                .skipped
                .into_iter()
                .map(|skipped| SkippedTool {
                    method: skipped.method.map(|m| m.as_str().to_owned()),
                    path: skipped.path,
                    reason: skipped.reason.to_string(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("the spec URL must be an http:// or https:// URL")]
    InvalidUrl,
    #[error("cannot download the spec from the URL")]
    Request(#[source] reqwest::Error),
    #[error("the spec must be smaller than {} MiB", .0 / (1024 * 1024))]
    TooLarge(usize),
    #[error("the spec is not UTF-8 text")]
    NotText,
}

impl From<DownloadError> for ApiError {
    fn from(error: DownloadError) -> Self {
        match error {
            DownloadError::InvalidUrl => Self::InvalidSpecUrl,
            DownloadError::Request(source) => Self::SpecDownload(source),
            DownloadError::TooLarge(limit) => Self::SpecTooLarge(limit),
            DownloadError::NotText => Self::SpecNotText,
        }
    }
}

pub(crate) fn parse_spec_url(text: &str) -> Result<Url, DownloadError> {
    Url::parse(text.trim()).map_err(|_| DownloadError::InvalidUrl)
}

/// The browser cannot fetch most spec URLs itself, because the hosts do not send CORS headers.
pub(crate) async fn download_spec(state: &AppState, url: &Url) -> Result<String, DownloadError> {
    check_url(url, state.allow_local_upstream).map_err(|_| DownloadError::InvalidUrl)?;
    let mut response = state
        .upstream
        .get(url.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(DownloadError::Request)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(DownloadError::Request)? {
        if bytes.len() + chunk.len() > MAX_SPEC_BYTES {
            return Err(DownloadError::TooLarge(MAX_SPEC_BYTES));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| DownloadError::NotText)
}
