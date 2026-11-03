//! Dashboard routes for servers, their tools, API imports, and new tokens.
//! The onboarding uses them to go from an empty account to a working MCP server.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    routing::{get, post},
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
    import::{ImportRequest, Owner, import_spec},
    mcp_tokens::{self, DEFAULT_TTL},
    state::AppState,
    templates::{self, TEMPLATES, Template},
};

/// Large public specs (for example Stripe or GitHub) are several MiB.
const MAX_SPEC_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Serialize, TS, sqlx::FromRow)]
#[ts(export)]
pub struct ServerSummary {
    pub name: String,
    #[sqlx(skip)]
    pub mcp_url: String,
    #[ts(type = "number")]
    pub apis: i64,
    #[ts(type = "number")]
    pub tools: i64,
    #[ts(type = "number")]
    pub enabled_tools: i64,
    /// Tokens that are not revoked and not expired.
    #[ts(type = "number")]
    pub active_tokens: i64,
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
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateServer {
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
    /// True when an API with the same name existed and its tools were replaced.
    pub replaced: bool,
    pub tools: Vec<String>,
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

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateToken {
    pub name: String,
}

/// The only response that contains the token value.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct NewToken {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub token: String,
    #[ts(type = "number")]
    pub expires_in_days: u64,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/templates", get(list_templates))
        .route("/servers", get(list_servers).post(create_server))
        .route("/servers/{name}/tools", get(list_tools))
        .route(
            "/servers/{name}/apis",
            post(import_api).layer(DefaultBodyLimit::max(MAX_SPEC_BYTES)),
        )
        .route("/servers/{name}/tokens", post(create_token))
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

fn mcp_url(state: &AppState, name: &str) -> String {
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
    let mut servers: Vec<ServerSummary> = sqlx::query_as(
        "SELECT s.name,
                (SELECT count(*) FROM apis a WHERE a.server_id = s.id) AS apis,
                (SELECT count(*) FROM tools t WHERE t.server_id = s.id) AS tools,
                (SELECT count(*) FROM tools t WHERE t.server_id = s.id AND t.enabled) AS enabled_tools,
                (SELECT count(*) FROM mcp_tokens k WHERE k.server_id = s.id
                    AND k.revoked_at IS NULL AND k.expires_at > now()) AS active_tokens
         FROM servers s WHERE s.user_id = $1 ORDER BY s.created_at, s.id",
    )
    .bind(auth.user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::Database)?;
    for server in &mut servers {
        server.mcp_url = mcp_url(&state, &server.name);
    }
    Ok(Json(servers))
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
    sqlx::query("INSERT INTO servers (user_id, name) VALUES ($1, $2)")
        .bind(auth.user_id)
        .bind(name)
        .execute(&state.pool)
        .await
        .map_err(|error| match &error {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                ApiError::ServerNameTaken(name.to_owned())
            }
            _ => ApiError::Database(error),
        })?;
    let summary = ServerSummary {
        name: name.to_owned(),
        mcp_url: mcp_url(&state, name),
        apis: 0,
        tools: 0,
        enabled_tools: 0,
        active_tokens: 0,
    };
    Ok((StatusCode::CREATED, Json(summary)))
}

async fn list_tools(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<Vec<ToolSummary>>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let tools = sqlx::query_as(
        "SELECT t.name, t.description, t.request->>'method' AS method,
                t.request->>'path' AS path, a.name AS api, t.enabled
         FROM tools t JOIN apis a ON a.id = t.api_id
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
/// the same name gets replaced.
async fn import_api(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<ImportApi>,
) -> Result<Json<Vec<ImportedApi>>, ApiError> {
    owned_server_id(&state.pool, auth.user_id, &name).await?;

    let (specs, base_url): (Vec<String>, Option<String>) = match body {
        ImportApi::Template { template } => {
            let template = templates::find(&template).ok_or(ApiError::NotFound)?;
            let specs = template.specs.iter().map(|s| (*s).to_owned()).collect();
            (specs, None)
        }
        ImportApi::Text { spec, base_url } => (vec![spec], base_url),
        ImportApi::Url { spec_url, base_url } => {
            (vec![download_spec(&state, &spec_url).await?], base_url)
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
            },
        )
        .await?;
        imported.push(ImportedApi {
            name: report.api_name,
            base_url: report.base_url.to_string(),
            replaced: report.replaced,
            tools: report.tools,
            skipped: report
                .skipped
                .into_iter()
                .map(|skipped| SkippedTool {
                    method: skipped.method.map(|m| m.as_str().to_owned()),
                    path: skipped.path,
                    reason: skipped.reason.to_string(),
                })
                .collect(),
        });
    }
    Ok(Json(imported))
}

/// The browser cannot fetch most spec URLs itself, because the hosts do not send CORS headers.
async fn download_spec(state: &AppState, spec_url: &str) -> Result<String, ApiError> {
    let url = Url::parse(spec_url.trim()).map_err(|_| ApiError::InvalidSpecUrl)?;
    check_url(&url, state.allow_local_upstream).map_err(|_| ApiError::InvalidSpecUrl)?;
    let mut response = state
        .upstream
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(ApiError::SpecDownload)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(ApiError::SpecDownload)? {
        if bytes.len() + chunk.len() > MAX_SPEC_BYTES {
            return Err(ApiError::SpecTooLarge(MAX_SPEC_BYTES));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| ApiError::SpecNotText)
}

async fn create_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<CreateToken>,
) -> Result<(StatusCode, Json<NewToken>), ApiError> {
    owned_server_id(&state.pool, auth.user_id, &name).await?;
    let created =
        mcp_tokens::create(&state.pool, &state.auth.jwt, &name, &body.name, DEFAULT_TTL).await?;
    let token = NewToken {
        id: created.id,
        name: body.name.trim().to_owned(),
        token: created.value,
        expires_in_days: DEFAULT_TTL.as_secs() / 86_400,
    };
    Ok((StatusCode::CREATED, Json(token)))
}
