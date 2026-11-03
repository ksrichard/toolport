//! Dashboard routes for manual APIs and the tool Test route.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderName, StatusCode},
    routing::{get, post, put},
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgPool;
use toolport_core::{
    HttpMethod, InputRow, InputType, TemplateError, ToolDef, parse_query_template,
    schema_from_rows,
    tool::{BodyDef, ParamDef, ParamLocation, RequestTemplate},
};
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson},
    secrets::seal,
    servers::owned_server_id,
    state::AppState,
    upstream::{ApiTarget, DraftSecret, HttpTrace, UpstreamAuth, UpstreamCtx, call_upstream},
};

#[derive(Debug, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum AuthInput {
    None,
    ApiKey {
        header: String,
        #[ts(optional)]
        secret: Option<String>,
    },
    Bearer {
        #[ts(optional)]
        secret: Option<String>,
    },
}

#[derive(Debug, Serialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum AuthView {
    None,
    ApiKey { header: String, secret_set: bool },
    Bearer { secret_set: bool },
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ApiView {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub auth: AuthView,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct SaveApi {
    pub name: String,
    pub base_url: String,
    pub auth: AuthInput,
}

/// A header row. A value of exactly `{input}` sends that input. Other text is sent as it is.
#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct HeaderRow {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct ToolDraft {
    pub name: String,
    pub description: String,
    pub method: HttpMethod,
    pub path: String,
    /// For example `latitude={lat}&longitude={lon}`.
    pub query: String,
    pub headers: Vec<HeaderRow>,
    pub inputs: Vec<InputRow>,
    /// The schema of the JSON request body. Without it the tool sends no body.
    #[ts(type = "unknown", optional)]
    pub body_schema: Option<Value>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum TestApi {
    /// A saved API. `auth` overrides its auth. Without a new secret, the saved one is used.
    Saved {
        #[ts(type = "number")]
        id: i64,
        #[ts(optional)]
        auth: Option<AuthInput>,
    },
    Draft {
        base_url: String,
        auth: AuthInput,
    },
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct TestRequest {
    pub draft: ToolDraft,
    pub api: TestApi,
    #[ts(type = "Record<string, unknown>")]
    pub args: Map<String, Value>,
}

#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct TestResult {
    /// The tool result, as an MCP client would get it.
    #[ts(type = "unknown")]
    pub mcp: Value,
    pub http: HttpTrace,
}

#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("the tool name must have 1 to 64 letters, digits, `_`, or `-`")]
    InvalidToolName(String),
    #[error("the placeholder {{{0}}} has no input row")]
    UnknownPlaceholder(String),
    #[error("the input {0} is part of the path, so it must be required")]
    PathInputOptional(String),
    #[error("{0}")]
    BadQueryTemplate(#[from] TemplateError),
    #[error("the input {0} has a complex type, which a manual tool cannot use")]
    ComplexInput(String),
}

impl DraftError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidToolName(_) => "invalid_tool_name",
            Self::UnknownPlaceholder(_) => "unknown_placeholder",
            Self::PathInputOptional(_) => "path_input_optional",
            Self::BadQueryTemplate(_) => "bad_query_template",
            Self::ComplexInput(_) => "complex_input_on_manual_tool",
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/servers/{name}/apis", get(list_apis))
        .route("/servers/{name}/apis/manual", post(create_api))
        .route("/servers/{name}/apis/{id}", put(update_api))
        .route("/servers/{name}/tools/test", post(test_tool))
}

/// Turns a draft into a tool.
/// - The schema comes from the input rows, plus `body` when there is a body schema.
/// - Each `{x}` in the path is a required path input.
/// - Each `key={x}` in the query is a query input. `key` is sent when it differs from `x`.
/// - A header value of exactly `{x}` is a header input. Other values are fixed headers.
pub fn draft_to_tool(draft: &ToolDraft) -> Result<ToolDef, DraftError> {
    let name = draft.name.trim();
    let valid = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return Err(DraftError::InvalidToolName(draft.name.clone()));
    }
    if let Some(row) = draft
        .inputs
        .iter()
        .find(|r| r.input_type == InputType::Complex)
    {
        return Err(DraftError::ComplexInput(row.name.clone()));
    }
    let row_of = |input: &str| {
        draft
            .inputs
            .iter()
            .find(|row| row.name == input)
            .ok_or_else(|| DraftError::UnknownPlaceholder(input.to_owned()))
    };

    let mut params = Vec::new();
    for input in path_placeholders(&draft.path) {
        if !row_of(&input)?.required {
            return Err(DraftError::PathInputOptional(input));
        }
        params.push(ParamDef {
            name: input,
            location: ParamLocation::Path,
            required: true,
            target: None,
        });
    }
    for (key, input) in parse_query_template(&draft.query)? {
        params.push(ParamDef {
            required: row_of(&input)?.required,
            target: (key != input).then_some(key),
            name: input,
            location: ParamLocation::Query,
        });
    }
    let mut fixed_headers = Vec::new();
    for header in &draft.headers {
        match header
            .value
            .strip_prefix('{')
            .and_then(|v| v.strip_suffix('}'))
        {
            Some(input) if !input.contains(['{', '}']) => params.push(ParamDef {
                required: row_of(input)?.required,
                target: (header.name != input).then(|| header.name.clone()),
                name: input.to_owned(),
                location: ParamLocation::Header,
            }),
            _ => fixed_headers.push((header.name.clone(), header.value.clone())),
        }
    }

    Ok(ToolDef {
        name: name.to_owned(),
        description: draft.description.clone(),
        input_schema: schema_from_rows(&draft.inputs, draft.body_schema.as_ref()),
        request: RequestTemplate {
            method: draft.method,
            path: draft.path.clone(),
            params,
            body: draft.body_schema.as_ref().map(|_| BodyDef {
                content_type: "application/json".to_owned(),
                required: false,
            }),
            fixed_headers,
        },
    })
}

fn path_placeholders(path: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = path;
    while let Some((_, after)) = rest.split_once('{') {
        let Some((name, tail)) = after.split_once('}') else {
            break;
        };
        found.push(name.to_owned());
        rest = tail;
    }
    found
}

/// The auth of a request, with the secret source resolved.
struct ResolvedAuth {
    auth_type: &'static str,
    header: Option<String>,
    /// A new secret from the request.
    new_secret: Option<String>,
    /// The saved sealed secret, when the request keeps it.
    kept: Option<Vec<u8>>,
}

/// `saved` is the sealed secret of the API, if it has one. An empty new secret counts as none.
fn resolve_auth(input: &AuthInput, saved: Option<Vec<u8>>) -> Result<ResolvedAuth, ApiError> {
    let (auth_type, header, secret) = match input {
        AuthInput::None => {
            return Ok(ResolvedAuth {
                auth_type: "none",
                header: None,
                new_secret: None,
                kept: None,
            });
        }
        AuthInput::ApiKey { header, secret } => {
            let header = header.trim();
            HeaderName::from_bytes(header.as_bytes()).map_err(|_| ApiError::InvalidAuthHeader)?;
            ("api_key", Some(header.to_owned()), secret)
        }
        AuthInput::Bearer { secret } => ("bearer", None, secret),
    };
    let new_secret = secret.clone().filter(|s| !s.is_empty());
    if new_secret.is_none() && saved.is_none() {
        return Err(ApiError::MissingSecret);
    }
    let kept = if new_secret.is_none() { saved } else { None };
    Ok(ResolvedAuth {
        auth_type,
        header,
        new_secret,
        kept,
    })
}

fn check_base_url(text: &str) -> Result<String, ApiError> {
    let url = Url::parse(text.trim()).map_err(|_| ApiError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ApiError::InvalidBaseUrl);
    }
    Ok(url.to_string())
}

type ApiRow = (i64, String, String, String, String, Option<String>, bool);

fn view(row: ApiRow) -> ApiView {
    let (id, name, kind, base_url, auth_type, header, secret_set) = row;
    let auth = match (auth_type.as_str(), header) {
        ("api_key", Some(header)) => AuthView::ApiKey { header, secret_set },
        ("bearer", _) => AuthView::Bearer { secret_set },
        _ => AuthView::None,
    };
    ApiView {
        id,
        name,
        kind,
        base_url,
        auth,
    }
}

async fn list_apis(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<Vec<ApiView>>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let rows: Vec<ApiRow> = sqlx::query_as(
        "SELECT id, name, kind, base_url, auth_type, auth_header, (auth_secret IS NOT NULL)
         FROM apis WHERE server_id = $1 ORDER BY id",
    )
    .bind(server_id)
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::Database)?;
    Ok(Json(rows.into_iter().map(view).collect()))
}

fn name_taken(name: &str) -> impl FnOnce(sqlx::Error) -> ApiError + '_ {
    move |error| match &error {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::ApiNameTaken(name.to_owned())
        }
        _ => ApiError::Database(error),
    }
}

fn clean_name(name: &str) -> Result<&str, ApiError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::InvalidApiName);
    }
    Ok(name)
}

async fn create_api(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<SaveApi>,
) -> Result<(StatusCode, Json<ApiView>), ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let api_name = clean_name(&body.name)?;
    let base_url = check_base_url(&body.base_url)?;
    let resolved = resolve_auth(&body.auth, None)?;

    // The sealed secret is bound to the API id, so the id must exist before we seal.
    // The auth check needs a value in the column, so the insert uses an empty placeholder.
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO apis (server_id, name, base_url, kind, auth_type, auth_header, auth_secret)
         VALUES ($1, $2, $3, 'manual', $4, $5, CASE WHEN $4 = 'none' THEN NULL ELSE ''::bytea END)
         RETURNING id",
    )
    .bind(server_id)
    .bind(api_name)
    .bind(&base_url)
    .bind(resolved.auth_type)
    .bind(&resolved.header)
    .fetch_one(&mut *tx)
    .await
    .map_err(name_taken(api_name))?;
    if let Some(secret) = &resolved.new_secret {
        sqlx::query("UPDATE apis SET auth_secret = $1 WHERE id = $2")
            .bind(seal(&state.secrets, id, secret)?)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(ApiError::Database)?;
    }
    tx.commit().await.map_err(ApiError::Database)?;
    let view = api_view(&state.pool, id).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn api_view(pool: &PgPool, id: i64) -> Result<ApiView, ApiError> {
    let row: ApiRow = sqlx::query_as(
        "SELECT id, name, kind, base_url, auth_type, auth_header, (auth_secret IS NOT NULL)
         FROM apis WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .map_err(ApiError::Database)?;
    Ok(view(row))
}

async fn update_api(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, id)): Path<(String, i64)>,
    ApiJson(body): ApiJson<SaveApi>,
) -> Result<Json<ApiView>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let (old_name, kind, saved): (String, String, Option<Vec<u8>>) =
        sqlx::query_as("SELECT name, kind, auth_secret FROM apis WHERE id = $1 AND server_id = $2")
            .bind(id)
            .bind(server_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::Database)?
            .ok_or(ApiError::NotFound)?;
    let api_name = clean_name(&body.name)?;
    if kind == "spec" && api_name != old_name {
        return Err(ApiError::SpecApiFixed);
    }
    let base_url = check_base_url(&body.base_url)?;
    let resolved = resolve_auth(&body.auth, saved)?;
    let secret = match (&resolved.new_secret, resolved.kept) {
        (Some(plain), _) => Some(seal(&state.secrets, id, plain)?),
        (None, kept) => kept,
    };
    sqlx::query(
        "UPDATE apis SET name = $1, base_url = $2, auth_type = $3, auth_header = $4,
                auth_secret = $5, updated_at = now() WHERE id = $6",
    )
    .bind(api_name)
    .bind(&base_url)
    .bind(resolved.auth_type)
    .bind(&resolved.header)
    .bind(secret)
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(name_taken(api_name))?;
    Ok(Json(api_view(&state.pool, id).await?))
}

/// Runs the draft through the same upstream call as a real MCP call. It saves no call row
/// and publishes no live event.
async fn test_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<TestRequest>,
) -> Result<Json<TestResult>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let tool = draft_to_tool(&body.draft)?;

    let (target, new_secret) = match body.api {
        TestApi::Draft { base_url, auth } => {
            let resolved = resolve_auth(&auth, None)?;
            (
                target_of(0, base_url, resolved.auth_type, resolved.header, Vec::new())?,
                resolved.new_secret,
            )
        }
        TestApi::Saved { id, auth } => {
            let (base_url, auth_type, header, sealed): (
                String,
                String,
                Option<String>,
                Option<Vec<u8>>,
            ) = sqlx::query_as(
                "SELECT base_url, auth_type, auth_header, auth_secret FROM apis
                     WHERE id = $1 AND server_id = $2",
            )
            .bind(id)
            .bind(server_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::Database)?
            .ok_or(ApiError::NotFound)?;
            match auth {
                None => {
                    let auth = UpstreamAuth::from_columns(&auth_type, header, sealed)
                        .ok_or(ApiError::StoredAuth)?;
                    (ApiTarget { id, base_url, auth }, None)
                }
                Some(input) => {
                    let resolved = resolve_auth(&input, sealed)?;
                    let sealed = resolved.kept.unwrap_or_default();
                    (
                        target_of(id, base_url, resolved.auth_type, resolved.header, sealed)?,
                        resolved.new_secret,
                    )
                }
            }
        }
    };
    let secret = match &new_secret {
        Some(plain) => DraftSecret::New(plain),
        None => DraftSecret::Saved,
    };
    let ctx = UpstreamCtx {
        http: &state.upstream,
        secrets: &state.secrets,
        timeout: state.upstream_timeout,
        allow_loopback: state.allow_local_upstream,
    };
    let output = call_upstream(&ctx, &target, secret, &tool.request, &body.args)
        .await
        .map_err(ApiError::TestCall)?;
    let mcp = serde_json::to_value(&output.result).map_err(|_| ApiError::StoredAuth)?;
    Ok(Json(TestResult {
        mcp,
        http: output.trace,
    }))
}

fn target_of(
    id: i64,
    base_url: String,
    auth_type: &str,
    header: Option<String>,
    sealed: Vec<u8>,
) -> Result<ApiTarget, ApiError> {
    let base_url = check_base_url(&base_url)?;
    let auth =
        UpstreamAuth::from_columns(auth_type, header, Some(sealed)).ok_or(ApiError::StoredAuth)?;
    Ok(ApiTarget { id, base_url, auth })
}
