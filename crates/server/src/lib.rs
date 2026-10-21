pub mod auth;
pub mod config;
pub mod error;
pub mod state;
pub mod users;

use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};

use crate::{error::ApiError, state::AppState};

pub fn app(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .merge(users::routes())
        .fallback(|| async { ApiError::NotFound });
    Router::new()
        .nest("/api", api)
        .merge(auth::routes())
        .with_state(state)
}

/// Returns 503 when the database does not answer, so Docker and Caddy see the real state.
async fn health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1")
        .execute(&state.pool)
        .await
        .map_err(ApiError::DatabaseUnavailable)?;
    Ok(Json(json!({ "status": "ok" })))
}
