pub mod error;

use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::error::ApiError;

pub fn app(pool: PgPool) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .with_state(pool)
}

/// Returns 503 when the database does not answer, so Docker and Caddy see the real state.
async fn health(State(pool): State<PgPool>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(ApiError::DatabaseUnavailable)?;
    Ok(Json(json!({ "status": "ok" })))
}
