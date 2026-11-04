//! Dashboard routes for the MCP client tokens of one server: list, create, and revoke (delete).

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson},
    mcp_tokens::{self, DEFAULT_DAYS},
    servers::owned_server_id,
    state::AppState,
};

#[derive(Debug, Deserialize, TS)]
#[ts(export)]
pub struct CreateToken {
    pub name: String,
    /// 1 to 365 days. `null` means that the token never expires. A missing field means 90 days.
    #[serde(default = "default_days")]
    #[ts(optional, type = "number | null")]
    pub expires_in_days: Option<i64>,
}

fn default_days() -> Option<i64> {
    Some(DEFAULT_DAYS)
}

/// One token row. It never contains the token value.
#[derive(Debug, Serialize, TS, sqlx::FromRow)]
#[ts(export)]
pub struct TokenView {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    #[ts(type = "number")]
    pub created_ms: i64,
    /// `null` for a token that never expires.
    #[ts(type = "number | null")]
    pub expires_ms: Option<i64>,
    /// The server updates it at most once a minute.
    #[ts(type = "number | null")]
    pub last_used_ms: Option<i64>,
}

/// The only response that contains the token value.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct NewToken {
    pub token: String,
    /// `null` for a token that never expires.
    #[ts(type = "number | null")]
    pub expires_in_days: Option<i64>,
    pub row: TokenView,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/servers/{name}/tokens",
            get(list_tokens).post(create_token),
        )
        .route("/servers/{name}/tokens/{id}/revoke", post(revoke_token))
}

/// The tokens of a server, the newest first. With an id, only that token.
async fn token_views(
    pool: &PgPool,
    server_id: i64,
    id: Option<i64>,
) -> Result<Vec<TokenView>, ApiError> {
    sqlx::query_as(
        "SELECT id, name,
                (extract(epoch FROM created_at) * 1000)::bigint AS created_ms,
                (extract(epoch FROM expires_at) * 1000)::bigint AS expires_ms,
                (extract(epoch FROM last_used_at) * 1000)::bigint AS last_used_ms
         FROM mcp_tokens WHERE server_id = $1 AND ($2::bigint IS NULL OR id = $2)
         ORDER BY id DESC",
    )
    .bind(server_id)
    .bind(id)
    .fetch_all(pool)
    .await
    .map_err(ApiError::Database)
}

async fn token_view(pool: &PgPool, server_id: i64, id: i64) -> Result<TokenView, ApiError> {
    token_views(pool, server_id, Some(id))
        .await?
        .pop()
        .ok_or(ApiError::NotFound)
}

async fn list_tokens(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
) -> Result<Json<Vec<TokenView>>, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    Ok(Json(token_views(&state.pool, server_id, None).await?))
}

async fn create_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(name): Path<String>,
    ApiJson(body): ApiJson<CreateToken>,
) -> Result<(StatusCode, Json<NewToken>), ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    let ttl = mcp_tokens::lifetime(body.expires_in_days)?;
    let created = mcp_tokens::create(&state.pool, &state.auth.jwt, &name, &body.name, ttl).await?;
    let token = NewToken {
        token: created.value,
        expires_in_days: body.expires_in_days,
        row: token_view(&state.pool, server_id, created.id).await?,
    };
    Ok((StatusCode::CREATED, Json(token)))
}

/// Deletes the token row. The token stops working at once, and the list no longer shows it.
/// Old calls keep the token name in their own column.
async fn revoke_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((name, id)): Path<(String, i64)>,
) -> Result<StatusCode, ApiError> {
    let server_id = owned_server_id(&state.pool, auth.user_id, &name).await?;
    // A token of another server gives NotFound, the same as an unknown id.
    let deleted = sqlx::query("DELETE FROM mcp_tokens WHERE id = $1 AND server_id = $2")
        .bind(id)
        .bind(server_id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::Database)?
        .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    state.live.forget_token(id);
    Ok(StatusCode::NO_CONTENT)
}
