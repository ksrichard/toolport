//! Re-import of spec APIs, from the dashboard and from the daily check.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    routing::{post, put},
};
use reqwest::Url;
use serde::Deserialize;
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson, error_chain},
    import::{ImportError, ImportReport, ImportRequest, Owner, import_spec},
    servers::{
        DownloadError, ImportedApi, MAX_SPEC_BYTES, download_spec, owned_server_id, parse_spec_url,
    },
    state::AppState,
    tools::{ApiView, api_view},
};

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct ReimportApi {
    /// A new spec text. It is needed for an API without a spec URL.
    /// With a text, the API no longer has a spec URL.
    #[ts(optional)]
    pub spec: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct SetAutoCheck {
    pub enabled: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SpecCheckError {
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
    #[error("cannot download the spec")]
    Download(#[from] DownloadError),
    #[error("the stored URL is not valid")]
    StoredUrl,
    #[error("cannot import the spec")]
    Import(#[from] ImportError),
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/servers/{name}/apis/{id}/reimport",
            post(reimport).layer(DefaultBodyLimit::max(MAX_SPEC_BYTES)),
        )
        .route("/servers/{name}/apis/{id}/auto-check", put(set_auto_check))
}

/// The fields that a re-import of one API needs.
#[derive(sqlx::FromRow)]
struct SpecApi {
    id: i64,
    server_id: i64,
    server: String,
    user_id: i64,
    name: String,
    kind: String,
    base_url: String,
    spec_url: Option<String>,
    spec: Option<String>,
}

/// `concat!` takes only literals.
macro_rules! spec_api_select {
    () => {
        "SELECT a.id, a.server_id, s.name AS server, s.user_id, a.name, a.kind, a.base_url,
                a.spec_url, a.spec
         FROM apis a JOIN servers s ON s.id = a.server_id "
    };
}

async fn owned_api(
    state: &AppState,
    user_id: i64,
    server: &str,
    id: i64,
) -> Result<SpecApi, ApiError> {
    let server_id = owned_server_id(&state.pool, user_id, server).await?;
    sqlx::query_as(concat!(
        spec_api_select!(),
        "WHERE a.id = $1 AND a.server_id = $2"
    ))
    .bind(id)
    .bind(server_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::Database)?
    .ok_or(ApiError::NotFound)
}

/// Runs the import for the API under its own name and with its current base URL,
/// so a base URL that the user changed stays.
async fn import_again(
    state: &AppState,
    api: &SpecApi,
    spec_text: &str,
    spec_url: Option<&str>,
) -> Result<ImportReport, SpecCheckError> {
    let base_url = Url::parse(&api.base_url).map_err(|_| SpecCheckError::StoredUrl)?;
    let report = import_spec(
        &state.pool,
        ImportRequest {
            owner: Owner::UserId(api.user_id),
            server: &api.server,
            api_name: Some(&api.name),
            spec_text,
            base_url: Some(base_url),
            spec_url,
        },
    )
    .await?;
    if !(report.added.is_empty() && report.changed.is_empty() && report.removed.is_empty()) {
        state.peers.notify_tools_changed(api.server_id).await;
    }
    Ok(report)
}

async fn reimport(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, id)): Path<(String, i64)>,
    ApiJson(body): ApiJson<ReimportApi>,
) -> Result<Json<ImportedApi>, ApiError> {
    let api = owned_api(&state, auth.user_id, &name, id).await?;
    if api.kind != "spec" {
        return Err(ApiError::NotASpecApi);
    }
    let (text, spec_url) = match (body.spec, &api.spec_url) {
        (Some(text), _) => (text, None),
        (None, Some(url)) => {
            let url = parse_spec_url(url)?;
            (download_spec(&state, &url).await?, Some(url.to_string()))
        }
        (None, None) => return Err(ApiError::SpecTextNeeded),
    };
    let report = import_again(&state, &api, &text, spec_url.as_deref())
        .await
        .map_err(|error| match error {
            SpecCheckError::Database(error) => ApiError::Database(error),
            SpecCheckError::Download(error) => error.into(),
            SpecCheckError::StoredUrl => ApiError::InvalidBaseUrl,
            SpecCheckError::Import(error) => error.into(),
        })?;
    Ok(Json(report.into()))
}

async fn set_auto_check(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, id)): Path<(String, i64)>,
    ApiJson(body): ApiJson<SetAutoCheck>,
) -> Result<Json<ApiView>, ApiError> {
    let api = owned_api(&state, auth.user_id, &name, id).await?;
    if api.spec_url.is_none() {
        return Err(ApiError::NoSpecUrl);
    }
    let updated =
        sqlx::query("UPDATE apis SET auto_check = $1 WHERE id = $2 AND spec_url IS NOT NULL")
            .bind(body.enabled)
            .bind(api.id)
            .execute(&state.pool)
            .await
            .map_err(ApiError::Database)?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::NoSpecUrl);
    }
    Ok(Json(api_view(&state.pool, api.id).await?))
}

/// Checks each API that has the daily check on and no check in the last 24 hours.
/// A failed check of one API is logged and recorded, and the next API still runs.
/// Returns the number of checked APIs.
pub async fn check_due_apis(state: &AppState) -> Result<usize, SpecCheckError> {
    let due: Vec<SpecApi> = sqlx::query_as(concat!(
        spec_api_select!(),
        "WHERE a.auto_check AND a.kind = 'spec' AND a.spec_url IS NOT NULL
           AND (a.last_check_at IS NULL OR a.last_check_at < now() - interval '24 hours')
         ORDER BY a.id"
    ))
    .fetch_all(&state.pool)
    .await?;
    for api in &due {
        let result = match check_one(state, api).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(api_id = api.id, error = %error_chain(&error), "spec check failed");
                "failed"
            }
        };
        let recorded = sqlx::query(
            "UPDATE apis SET last_check_at = now(), last_check_result = $1 WHERE id = $2",
        )
        .bind(result)
        .bind(api.id)
        .execute(&state.pool)
        .await;
        if let Err(error) = recorded {
            tracing::error!(api_id = api.id, error = %error_chain(&error), "cannot record the spec check");
        }
    }
    Ok(due.len())
}

async fn check_one(state: &AppState, api: &SpecApi) -> Result<&'static str, SpecCheckError> {
    let url = parse_spec_url(api.spec_url.as_deref().unwrap_or_default())?;
    let text = download_spec(state, &url).await?;
    if api.spec.as_deref() == Some(text.as_str()) {
        return Ok("unchanged");
    }
    let report = import_again(state, api, &text, Some(url.as_str())).await?;
    tracing::info!(
        api_id = api.id,
        added = report.added.len(),
        changed = report.changed.len(),
        removed = report.removed.len(),
        "spec check updated the API"
    );
    Ok("updated")
}

/// Runs `check_due_apis` at each interval until the process stops.
pub fn spawn_daily_check(state: AppState, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            if let Err(error) = check_due_apis(&state).await {
                tracing::error!(error = %error_chain(&error), "spec check run failed");
            }
        }
    });
}
