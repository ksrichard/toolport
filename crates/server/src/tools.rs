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
use sqlx::{PgPool, Postgres, Transaction, types::Json as Db};
use toolport_core::server_name::is_valid_server_name;
use toolport_core::{
    BODY_ARGUMENT, HttpMethod, InputRow, InputType, TemplateError, ToolDef, apply_rows,
    parse_query_template, print_query_template, rows_from_schema, schema_from_rows,
    tool::{BodyDef, ParamDef, ParamLocation, RequestTemplate},
    tool_json,
};
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson},
    plan,
    secrets::seal,
    servers::{ServerSummary, mcp_url, name_error, owned_server_id},
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
    /// Set when the spec came from a URL.
    pub spec_url: Option<String>,
    pub auto_check: bool,
    #[ts(type = "number | null")]
    pub last_check_ms: Option<i64>,
    /// `unchanged`, `updated`, or `failed`.
    pub last_check_result: Option<String>,
    #[ts(type = "number")]
    pub tool_count: i64,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct SaveApi {
    pub name: String,
    pub base_url: String,
    pub auth: AuthInput,
}

/// A header row. A value of exactly `{input}` sends that input. Other text is sent as it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HeaderRow {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
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
    /// The name of a saved spec tool of this server. The test then uses its stored request.
    #[ts(optional)]
    pub spec_tool: Option<String>,
    /// The name of a saved manual tool of this server. Its complex inputs then keep their
    /// stored schema.
    #[ts(optional)]
    pub manual_tool: Option<String>,
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
    #[error("the input name body is reserved for the request body")]
    ReservedInputName,
    #[error(
        "the input name {0} must start with a letter or `_` and have 1 to 64 letters, digits, `_`, or `-`"
    )]
    InvalidInputName(String),
    #[error("put credentials in the API auth, not in a fixed header")]
    SecretInFixedHeader(String),
}

impl DraftError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidToolName(_) => "invalid_tool_name",
            Self::UnknownPlaceholder(_) => "unknown_placeholder",
            Self::PathInputOptional(_) => "path_input_optional",
            Self::BadQueryTemplate(_) => "bad_query_template",
            Self::ComplexInput(_) => "complex_input_on_manual_tool",
            Self::ReservedInputName => "reserved_input_name",
            Self::InvalidInputName(_) => "invalid_input_name",
            Self::SecretInFixedHeader(_) => "secret_in_fixed_header",
        }
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/servers/{name}/apis", get(list_apis))
        .route("/servers/{name}/apis/manual", post(create_api))
        .route(
            "/servers/{name}/apis/{id}",
            put(update_api).delete(delete_api),
        )
        .route("/servers/{name}/tool-test", post(test_tool))
        .route("/tool-test", post(test_draft_tool))
        .route("/servers/{name}/tools", post(create_tool))
        .route(
            "/servers/{name}/tools/{tool}",
            get(get_tool).put(update_tool).delete(delete_tool),
        )
        .route("/servers/{name}/tools/{tool}/reset", post(reset_tool))
        .route("/servers/{name}/tools/{tool}/detach", post(detach_tool))
        .route(
            "/servers/{name}/tools/{tool}/enabled",
            put(set_tool_enabled),
        )
        .route("/servers/with-tool", post(create_server_with_tool))
}

/// A stored tool, as the dashboard edits it.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct ToolView {
    /// `spec` or `manual`.
    pub source: String,
    /// True when a spec tool differs from the last import.
    pub edited: bool,
    pub spec_changed: bool,
    #[ts(type = "number")]
    pub api_id: i64,
    pub draft: ToolDraft,
    /// The tool as `tools/list` sends it.
    #[ts(type = "unknown")]
    pub preview: Value,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct SaveTool {
    #[ts(type = "number")]
    pub api_id: i64,
    pub draft: ToolDraft,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct SetToolEnabled {
    pub enabled: bool,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateServerWithTool {
    pub server: String,
    pub api: SaveApi,
    pub draft: ToolDraft,
}

/// Turns a draft into a tool.
/// - The schema comes from the input rows, plus `body` when there is a body schema.
/// - Each `{x}` in the path is a required path input.
/// - Each `key={x}` in the query is a query input. `key` is sent when it differs from `x`.
/// - A header value of exactly `{x}` is a header input. Other values are fixed headers.
pub fn draft_to_tool(draft: &ToolDraft) -> Result<ToolDef, DraftError> {
    manual_tool_def(draft, None)
}

/// `stored` is the input schema of the saved tool. A tool detached from a spec can have
/// complex inputs. The table cannot rebuild them, so they keep their stored schema.
fn manual_tool_def(
    draft: &ToolDraft,
    stored: Option<&Map<String, Value>>,
) -> Result<ToolDef, DraftError> {
    let stored_property = |name: &str| {
        stored
            .and_then(|schema| schema.get("properties"))
            .and_then(|properties| properties.get(name))
    };
    let name = valid_tool_name(&draft.name)?;
    if let Some(row) = draft
        .inputs
        .iter()
        .find(|r| r.input_type == InputType::Complex && stored_property(&r.name).is_none())
    {
        return Err(DraftError::ComplexInput(row.name.clone()));
    }
    if let Some(row) = draft.inputs.iter().find(|r| !valid_input_name(&r.name)) {
        return Err(DraftError::InvalidInputName(row.name.clone()));
    }
    if draft.body_schema.is_some() && draft.inputs.iter().any(|r| r.name == BODY_ARGUMENT) {
        return Err(DraftError::ReservedInputName);
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
            _ if is_secret_header(&header.name) => {
                return Err(DraftError::SecretInFixedHeader(header.name.clone()));
            }
            _ => fixed_headers.push((header.name.clone(), header.value.clone())),
        }
    }

    let mut input_schema = schema_from_rows(&draft.inputs, draft.body_schema.as_ref());
    if let Some(Value::Object(properties)) = input_schema.get_mut("properties") {
        let complex = draft
            .inputs
            .iter()
            .filter(|r| r.input_type == InputType::Complex);
        for row in complex {
            let mut property = stored_property(&row.name).cloned().unwrap_or_default();
            if let Value::Object(fields) = &mut property {
                if row.description.is_empty() {
                    fields.remove("description");
                } else {
                    fields.insert("description".into(), row.description.clone().into());
                }
            }
            properties.insert(row.name.clone(), property);
        }
    }

    Ok(ToolDef {
        name: name.to_owned(),
        description: draft.description.clone(),
        input_schema,
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

/// The same rule is in `web/src/Builder.tsx`. `__proto__` would break the page preview.
fn valid_input_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && name != "__proto__"
}

/// A fixed header is stored and shown as plain text, so it must not carry credentials.
fn is_secret_header(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    ["key", "token", "secret", "auth", "cookie"]
        .iter()
        .any(|word| name.contains(word))
}

fn valid_tool_name(text: &str) -> Result<&str, DraftError> {
    let name = text.trim();
    let valid = (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if valid {
        Ok(name)
    } else {
        Err(DraftError::InvalidToolName(text.to_owned()))
    }
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

#[derive(sqlx::FromRow)]
struct ApiRow {
    id: i64,
    name: String,
    kind: String,
    base_url: String,
    auth_type: String,
    auth_header: Option<String>,
    secret_set: bool,
    spec_url: Option<String>,
    auto_check: bool,
    last_check_ms: Option<i64>,
    last_check_result: Option<String>,
    tool_count: i64,
}

/// `concat!` takes only literals.
macro_rules! api_select {
    () => {
        "SELECT a.id, a.name, a.kind, a.base_url, a.auth_type, a.auth_header,
                (a.auth_secret IS NOT NULL) AS secret_set, a.spec_url, a.auto_check,
                (extract(epoch FROM a.last_check_at) * 1000)::bigint AS last_check_ms,
                a.last_check_result,
                (SELECT count(*) FROM tools t WHERE t.api_id = a.id) AS tool_count
         FROM apis a "
    };
}

fn view(row: ApiRow) -> ApiView {
    let secret_set = row.secret_set;
    let auth = match (row.auth_type.as_str(), row.auth_header) {
        ("api_key", Some(header)) => AuthView::ApiKey { header, secret_set },
        ("bearer", _) => AuthView::Bearer { secret_set },
        _ => AuthView::None,
    };
    ApiView {
        id: row.id,
        name: row.name,
        kind: row.kind,
        base_url: row.base_url,
        auth,
        spec_url: row.spec_url,
        auto_check: row.auto_check,
        last_check_ms: row.last_check_ms,
        last_check_result: row.last_check_result,
        tool_count: row.tool_count,
    }
}

async fn list_apis(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<Vec<ApiView>>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let rows: Vec<ApiRow> = sqlx::query_as(concat!(
        api_select!(),
        "WHERE a.server_id = $1 ORDER BY a.id"
    ))
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
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    let id = insert_manual_api(&state, &mut tx, server_id, &body).await?;
    tx.commit().await.map_err(ApiError::Database)?;
    let view = api_view(&state.pool, id).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn insert_manual_api(
    state: &AppState,
    tx: &mut Transaction<'_, Postgres>,
    server_id: i64,
    body: &SaveApi,
) -> Result<i64, ApiError> {
    let api_name = clean_name(&body.name)?;
    let base_url = check_base_url(&body.base_url)?;
    let resolved = resolve_auth(&body.auth, None)?;

    // The sealed secret is bound to the API id, so the id must exist before we seal.
    // The auth check needs a value in the column, so the insert uses an empty placeholder.
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
    .fetch_one(&mut **tx)
    .await
    .map_err(name_taken(api_name))?;
    if let Some(secret) = &resolved.new_secret {
        sqlx::query("UPDATE apis SET auth_secret = $1 WHERE id = $2")
            .bind(seal(&state.secrets, id, secret)?)
            .bind(id)
            .execute(&mut **tx)
            .await
            .map_err(ApiError::Database)?;
    }
    Ok(id)
}

pub(crate) async fn api_view(pool: &PgPool, id: i64) -> Result<ApiView, ApiError> {
    let row: ApiRow = sqlx::query_as(concat!(api_select!(), "WHERE a.id = $1"))
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

/// The result of an API removal.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct RemovedApi {
    #[ts(type = "number")]
    pub removed_tools: i64,
}

/// Removes the API and its tools. The call rows stay, because they store the tool name only.
/// The server must keep at least one enabled tool, as for `delete_tool`.
async fn delete_api(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, id)): Path<(String, i64)>,
) -> Result<Json<RemovedApi>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    sqlx::query_scalar::<_, i64>("SELECT id FROM apis WHERE id = $1 AND server_id = $2")
        .bind(id)
        .bind(server_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::NotFound)?;
    // The lock keeps two removals from removing the last enabled tools together.
    let enabled_apis: Vec<i64> =
        sqlx::query_scalar("SELECT api_id FROM tools WHERE server_id = $1 AND enabled FOR UPDATE")
            .bind(server_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(ApiError::Database)?;
    if enabled_apis.iter().all(|api_id| *api_id == id) {
        return Err(ApiError::LastEnabledTool);
    }
    let removed_tools: i64 = sqlx::query_scalar("SELECT count(*) FROM tools WHERE api_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ApiError::Database)?;
    let deleted = sqlx::query("DELETE FROM apis WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Database)?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    tx.commit().await.map_err(ApiError::Database)?;
    state.peers.notify_tools_changed(server_id).await;
    Ok(Json(RemovedApi { removed_tools }))
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
    check_test_plan(&state, auth.user_id, server_id, &body).await?;
    run_test(&state, Some(server_id), body).await.map(Json)
}

/// A test sends a real upstream request, so it follows the same plan rules as an MCP call:
/// no test on a paused server, and no test of a stored tool that the plan pauses. A test of
/// a new, unsaved tool is allowed on a served server.
async fn check_test_plan(
    state: &AppState,
    user_id: i64,
    server_id: i64,
    body: &TestRequest,
) -> Result<(), ApiError> {
    let account = plan::account_plan(&state.pool, user_id)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::Unauthorized)?;
    let served = plan::server_is_served(&state.pool, server_id, &account.plan)
        .await
        .map_err(ApiError::Database)?;
    if !served {
        let message = plan::paused_server_message(&account.plan);
        return Err(plan::LimitError::PausedServer(message).into());
    }
    let Some(name) = body.spec_tool.as_deref().or(body.manual_tool.as_deref()) else {
        return Ok(());
    };
    let tool_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM tools WHERE server_id = $1 AND name = $2")
            .bind(server_id)
            .bind(name)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::Database)?;
    let Some(tool_id) = tool_id else {
        return Ok(());
    };
    let paused = plan::tool_is_paused(&state.pool, server_id, tool_id, &account.plan)
        .await
        .map_err(ApiError::Database)?;
    if paused {
        let message = plan::paused_tool_message(&account.plan, name);
        return Err(plan::LimitError::PausedTool(message).into());
    }
    Ok(())
}

/// The test for a user without a server yet (the onboarding wizard). The API must be a draft,
/// because there is no saved API to read.
async fn test_draft_tool(
    State(state): State<AppState>,
    _auth: AuthUser,
    ApiJson(body): ApiJson<TestRequest>,
) -> Result<Json<TestResult>, ApiError> {
    if matches!(body.api, TestApi::Saved { .. }) {
        return Err(ApiError::TestNeedsDraft);
    }
    run_test(&state, None, body).await.map(Json)
}

async fn run_test(
    state: &AppState,
    server_id: Option<i64>,
    body: TestRequest,
) -> Result<TestResult, ApiError> {
    let tool = match (&body.spec_tool, server_id) {
        (Some(name), Some(server_id)) => {
            let row = fetch_tool(&state.pool, server_id, name).await?;
            let (_, _, _, _, Db(schema), Db(request), source, ..) = row;
            if source != "spec" {
                return Err(ApiError::NotASpecTool);
            }
            spec_tool_def(&schema, &request, &body.draft)?
        }
        (Some(_), None) => return Err(ApiError::NotFound),
        (None, _) => manual_def(state, server_id, body.manual_tool.as_deref(), &body.draft).await?,
    };

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
            .bind(server_id.ok_or(ApiError::NotFound)?)
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
    Ok(TestResult {
        mcp,
        http: output.trace,
    })
}

/// The tool of a manual draft. With `saved`, the stored schema of that manual tool keeps
/// the complex inputs, as a save of the tool would.
async fn manual_def(
    state: &AppState,
    server_id: Option<i64>,
    saved: Option<&str>,
    draft: &ToolDraft,
) -> Result<ToolDef, ApiError> {
    let Some(name) = saved else {
        return Ok(draft_to_tool(draft)?);
    };
    let server_id = server_id.ok_or(ApiError::NotFound)?;
    let (_, _, _, _, Db(schema), _, source, ..) = fetch_tool(&state.pool, server_id, name).await?;
    if source != "manual" {
        return Err(ApiError::NotAManualTool);
    }
    Ok(manual_tool_def(draft, Some(&schema))?)
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

type ToolRow = (
    i64,
    i64,
    String,
    String,
    Db<Map<String, Value>>,
    Db<RequestTemplate>,
    String,
    bool,
    Option<Db<Value>>,
    bool,
);

fn tool_name_taken(name: &str) -> impl FnOnce(sqlx::Error) -> ApiError + '_ {
    move |error| match &error {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::ToolNameTaken(name.to_owned())
        }
        _ => ApiError::Database(error),
    }
}

/// The inverse of `draft_to_tool`, for a stored tool.
fn stored_draft(
    name: &str,
    description: &str,
    schema: &Map<String, Value>,
    request: &RequestTemplate,
) -> ToolDraft {
    let has_body = request.body.is_some();
    let mut inputs = rows_from_schema(schema);
    if has_body {
        inputs.retain(|row| row.name != BODY_ARGUMENT);
    }
    let key = |param: &ParamDef| param.target.clone().unwrap_or_else(|| param.name.clone());
    let query: Vec<(String, String)> = request
        .params
        .iter()
        .filter(|p| p.location == ParamLocation::Query)
        .map(|p| (key(p), p.name.clone()))
        .collect();
    let mut headers: Vec<HeaderRow> = request
        .params
        .iter()
        .filter(|p| p.location == ParamLocation::Header)
        .map(|p| HeaderRow {
            name: key(p),
            value: format!("{{{}}}", p.name),
        })
        .collect();
    headers.extend(request.fixed_headers.iter().map(|(name, value)| HeaderRow {
        name: name.clone(),
        value: value.clone(),
    }));
    let body_schema = if has_body {
        schema
            .get("properties")
            .and_then(|p| p.get(BODY_ARGUMENT))
            .cloned()
    } else {
        None
    };
    ToolDraft {
        name: name.to_owned(),
        description: description.to_owned(),
        method: request.method,
        path: request.path.clone(),
        query: print_query_template(&query),
        headers,
        inputs,
        body_schema,
    }
}

fn row_view(row: ToolRow) -> ToolView {
    let (_, api_id, name, description, Db(schema), Db(request), source, edited, _, spec_changed) =
        row;
    ToolView {
        draft: stored_draft(&name, &description, &schema, &request),
        preview: tool_json(&name, &description, &schema),
        source,
        edited,
        spec_changed,
        api_id,
    }
}

async fn fetch_tool(pool: &PgPool, server_id: i64, name: &str) -> Result<ToolRow, ApiError> {
    sqlx::query_as(
        "SELECT id, api_id, name, description, input_schema, request, source, edited, spec_tool,
                (edited AND edited_hash IS NOT NULL AND spec_hash <> edited_hash)
         FROM tools WHERE server_id = $1 AND name = $2",
    )
    .bind(server_id)
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(ApiError::Database)?
    .ok_or(ApiError::NotFound)
}

async fn api_in_server(pool: &PgPool, server_id: i64, api_id: i64) -> Result<(), ApiError> {
    sqlx::query_scalar::<_, i64>("SELECT id FROM apis WHERE id = $1 AND server_id = $2")
        .bind(api_id)
        .bind(server_id)
        .fetch_optional(pool)
        .await
        .map_err(ApiError::Database)?
        .map(|_| ())
        .ok_or(ApiError::NotFound)
}

async fn insert_manual_tool(
    tx: &mut Transaction<'_, Postgres>,
    server_id: i64,
    api_id: i64,
    tool: &ToolDef,
    enabled: bool,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO tools (api_id, server_id, name, description, input_schema, request, source, enabled)
         VALUES ($1, $2, $3, $4, $5, $6, 'manual', $7)",
    )
    .bind(api_id)
    .bind(server_id)
    .bind(&tool.name)
    .bind(&tool.description)
    .bind(Db(&tool.input_schema))
    .bind(Db(&tool.request))
    .bind(enabled)
    .execute(&mut **tx)
    .await
    .map_err(tool_name_taken(&tool.name))?;
    Ok(())
}

async fn get_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, tool)): Path<(String, String)>,
) -> Result<Json<ToolView>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let row = fetch_tool(&state.pool, server_id, &tool).await?;
    Ok(Json(row_view(row)))
}

async fn create_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<SaveTool>,
) -> Result<(StatusCode, Json<ToolView>), ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let tool = draft_to_tool(&body.draft)?;
    api_in_server(&state.pool, server_id, body.api_id).await?;
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    // Over the plan limit, the new tool starts off, as a spec import does.
    let enabled = plan::tool_room(&mut tx, server_id).await?.take();
    insert_manual_tool(&mut tx, server_id, body.api_id, &tool, enabled).await?;
    tx.commit().await.map_err(ApiError::Database)?;
    state.peers.notify_tools_changed(server_id).await;
    let row = fetch_tool(&state.pool, server_id, &tool.name).await?;
    Ok((StatusCode::CREATED, Json(row_view(row))))
}

/// The stored spec tool with the name, description, and input rows of the draft.
/// Only these can change. The rows also set `required` on the query and header inputs.
fn spec_tool_def(
    schema: &Map<String, Value>,
    request: &RequestTemplate,
    draft: &ToolDraft,
) -> Result<ToolDef, ApiError> {
    let stored = stored_draft("", "", schema, request);
    let same = draft.method == stored.method
        && draft.path == stored.path
        && draft.query == stored.query
        && draft.headers == stored.headers
        && draft.body_schema == stored.body_schema;
    if !same {
        return Err(ApiError::SpecToolFixed);
    }
    let name = valid_tool_name(&draft.name)?;
    if request.body.is_some() && draft.inputs.iter().any(|r| r.name == BODY_ARGUMENT) {
        return Err(DraftError::ReservedInputName.into());
    }
    // The request template maps inputs by name, so the names cannot change.
    let mut old_names: Vec<&str> = stored.inputs.iter().map(|r| r.name.as_str()).collect();
    let mut new_names: Vec<&str> = draft.inputs.iter().map(|r| r.name.as_str()).collect();
    old_names.sort_unstable();
    new_names.sort_unstable();
    if old_names != new_names {
        return Err(ApiError::SpecToolFixed);
    }
    let mut request = request.clone();
    for param in &mut request.params {
        let Some(row) = draft.inputs.iter().find(|r| r.name == param.name) else {
            continue;
        };
        if param.location == ParamLocation::Path {
            if !row.required {
                return Err(DraftError::PathInputOptional(param.name.clone()).into());
            }
        } else {
            param.required = row.required;
        }
    }
    Ok(ToolDef {
        name: name.to_owned(),
        description: draft.description.clone(),
        input_schema: apply_rows(schema, &draft.inputs),
        request,
    })
}

async fn update_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, tool)): Path<(String, String)>,
    ApiJson(body): ApiJson<SaveTool>,
) -> Result<Json<ToolView>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let row = fetch_tool(&state.pool, server_id, &tool).await?;
    let (id, api_id, _, _, Db(schema), Db(request), source, _, _, _) = row;
    let new_name = if source == "spec" {
        if body.api_id != api_id {
            return Err(ApiError::SpecToolFixed);
        }
        let def = spec_tool_def(&schema, &request, &body.draft)?;
        sqlx::query(
            "UPDATE tools SET name = $1, description = $2, input_schema = $3, request = $4,
                    edited = true, edited_hash = spec_hash
             WHERE id = $5",
        )
        .bind(&def.name)
        .bind(&def.description)
        .bind(Db(&def.input_schema))
        .bind(Db(&def.request))
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(tool_name_taken(&def.name))?;
        def.name
    } else {
        let def = manual_tool_def(&body.draft, Some(&schema))?;
        api_in_server(&state.pool, server_id, body.api_id).await?;
        sqlx::query(
            "UPDATE tools SET api_id = $1, name = $2, description = $3, input_schema = $4,
                    request = $5 WHERE id = $6",
        )
        .bind(body.api_id)
        .bind(&def.name)
        .bind(&def.description)
        .bind(Db(&def.input_schema))
        .bind(Db(&def.request))
        .bind(id)
        .execute(&state.pool)
        .await
        .map_err(tool_name_taken(&def.name))?;
        def.name
    };
    state.peers.notify_tools_changed(server_id).await;
    let row = fetch_tool(&state.pool, server_id, &new_name).await?;
    Ok(Json(row_view(row)))
}

/// Copies the last import back over the tool.
async fn reset_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, tool)): Path<(String, String)>,
) -> Result<Json<ToolView>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let row = fetch_tool(&state.pool, server_id, &tool).await?;
    let (id, _, _, _, _, _, source, _, spec_tool, _) = row;
    let Some(Db(original)) = spec_tool.filter(|_| source == "spec") else {
        return Err(ApiError::NotASpecTool);
    };
    let original: ToolDef = serde_json::from_value(original).map_err(|_| ApiError::StoredTool)?;
    sqlx::query(
        "UPDATE tools SET name = $1, description = $2, input_schema = $3, request = $4,
                edited = false, edited_hash = NULL WHERE id = $5",
    )
    .bind(&original.name)
    .bind(&original.description)
    .bind(Db(&original.input_schema))
    .bind(Db(&original.request))
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(tool_name_taken(&original.name))?;
    state.peers.notify_tools_changed(server_id).await;
    let row = fetch_tool(&state.pool, server_id, &original.name).await?;
    Ok(Json(row_view(row)))
}

/// Turns a spec tool into a manual tool. A re-import then no longer updates it.
async fn detach_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, tool)): Path<(String, String)>,
) -> Result<Json<ToolView>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let detached = sqlx::query(
        "UPDATE tools SET source = 'manual', edited = false, spec_tool = NULL, spec_hash = NULL,
                edited_hash = NULL, operation = NULL
         WHERE server_id = $1 AND name = $2 AND source = 'spec'",
    )
    .bind(server_id)
    .bind(&tool)
    .execute(&state.pool)
    .await
    .map_err(ApiError::Database)?;
    if detached.rows_affected() == 0 {
        // Gives 404 for an unknown tool.
        fetch_tool(&state.pool, server_id, &tool).await?;
        return Err(ApiError::NotASpecTool);
    }
    // The tools/list entry does not change, so the open sessions need no notice.
    let row = fetch_tool(&state.pool, server_id, &tool).await?;
    Ok(Json(row_view(row)))
}

async fn delete_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, tool)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let row = fetch_tool(&state.pool, server_id, &tool).await?;
    if row.6 == "spec" {
        return Err(ApiError::SpecToolCannotBeDeleted);
    }
    // The lock keeps two deletes from removing the last two enabled tools together.
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    let enabled: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM tools WHERE server_id = $1 AND enabled FOR UPDATE")
            .bind(server_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(ApiError::Database)?;
    if enabled == [row.0] {
        return Err(ApiError::LastEnabledTool);
    }
    sqlx::query("DELETE FROM tools WHERE id = $1")
        .bind(row.0)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Database)?;
    tx.commit().await.map_err(ApiError::Database)?;
    state.peers.notify_tools_changed(server_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A server must keep at least one tool on, so a client never sees an empty tool list.
async fn set_tool_enabled(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, tool)): Path<(String, String)>,
    ApiJson(body): ApiJson<SetToolEnabled>,
) -> Result<StatusCode, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let tool_id: i64 =
        sqlx::query_scalar("SELECT id FROM tools WHERE server_id = $1 AND name = $2")
            .bind(server_id)
            .bind(&tool)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::Database)?
            .ok_or(ApiError::NotFound)?;
    // The lock keeps two requests from turning off the last two enabled tools together.
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    if body.enabled {
        let mut room = plan::tool_room(&mut tx, server_id).await?;
        let on: bool = sqlx::query_scalar("SELECT enabled FROM tools WHERE id = $1")
            .bind(tool_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(ApiError::Database)?;
        if !on && !room.take() {
            return Err(room.error().into());
        }
    } else {
        let enabled: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM tools WHERE server_id = $1 AND enabled FOR UPDATE")
                .bind(server_id)
                .fetch_all(&mut *tx)
                .await
                .map_err(ApiError::Database)?;
        if enabled == [tool_id] {
            return Err(ApiError::LastEnabledTool);
        }
    }
    let updated = sqlx::query("UPDATE tools SET enabled = $2 WHERE id = $1")
        .bind(tool_id)
        .bind(body.enabled)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::Database)?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    tx.commit().await.map_err(ApiError::Database)?;
    state.peers.notify_tools_changed(server_id).await;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates the server, a manual API, and the first tool in one transaction.
async fn create_server_with_tool(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(body): ApiJson<CreateServerWithTool>,
) -> Result<(StatusCode, Json<ServerSummary>), ApiError> {
    let server_name = body.server.trim();
    if !is_valid_server_name(server_name) {
        return Err(ApiError::InvalidServerName);
    }
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    plan::check_server_limit(&mut tx, auth.user_id, 1).await?;
    let server_id: i64 =
        sqlx::query_scalar("INSERT INTO servers (user_id, name) VALUES ($1, $2) RETURNING id")
            .bind(auth.user_id)
            .bind(server_name)
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| name_error(error, server_name))?;
    let api_id = insert_manual_api(&state, &mut tx, server_id, &body.api).await?;
    let tool = draft_to_tool(&body.draft)?;
    insert_manual_tool(&mut tx, server_id, api_id, &tool, true).await?;
    tx.commit().await.map_err(ApiError::Database)?;
    state.peers.notify_tools_changed(server_id).await;
    let summary = ServerSummary {
        id: server_id,
        name: server_name.to_owned(),
        mcp_url: mcp_url(&state, server_name),
        apis: 1,
        tools: 1,
        enabled_tools: 1,
        active_tokens: 0,
        last_call_ms: None,
        connected_clients: 0,
        paused: false,
        paused_tools: 0,
    };
    Ok((StatusCode::CREATED, Json(summary)))
}
