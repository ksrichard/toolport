//! The user row: GitHub sign-in upsert, the current-user route, and the saved theme.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use ts_rs::TS;

use crate::{
    auth::{AuthUser, github::GithubUser},
    error::{ApiError, ApiJson},
    state::AppState,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, sqlx::Type, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum Theme {
    Neon,
    Calm,
    Light,
}

#[derive(Debug, Serialize, sqlx::FromRow, TS)]
#[ts(export)]
pub struct User {
    #[ts(type = "number")]
    pub id: i64,
    pub github_login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub theme: Theme,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/me/theme", put(set_theme))
}

/// Creates the user on the first sign-in. Later sign-ins update the GitHub profile fields.
pub async fn upsert_github_user(pool: &PgPool, github: &GithubUser) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO users (github_id, github_login, name, avatar_url)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (github_id) DO UPDATE SET
             github_login = EXCLUDED.github_login,
             name = EXCLUDED.name,
             avatar_url = EXCLUDED.avatar_url,
             updated_at = now()
         RETURNING id",
    )
    .bind(github.id)
    .bind(&github.login)
    .bind(&github.name)
    .bind(&github.avatar_url)
    .fetch_one(pool)
    .await
}

async fn me(State(state): State<AppState>, auth: AuthUser) -> Result<Json<User>, ApiError> {
    let user = sqlx::query_as::<_, User>(
        "SELECT id, github_login, name, avatar_url, theme FROM users WHERE id = $1",
    )
    .bind(auth.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::Database)?
    // The token is valid, but the user row is gone. The client must sign in again.
    .ok_or(ApiError::Unauthorized)?;
    Ok(Json(user))
}

#[derive(Deserialize)]
struct ThemeUpdate {
    theme: Theme,
}

async fn set_theme(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(body): ApiJson<ThemeUpdate>,
) -> Result<StatusCode, ApiError> {
    sqlx::query("UPDATE users SET theme = $1, updated_at = now() WHERE id = $2")
        .bind(body.theme)
        .bind(auth.user_id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::Database)?;
    Ok(StatusCode::NO_CONTENT)
}
