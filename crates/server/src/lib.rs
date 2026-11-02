pub mod auth;
pub mod config;
pub mod error;
pub mod import;
pub mod mcp;
pub mod state;
pub mod users;

use axum::{Json, Router, extract::State, middleware, routing::get};
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
        .merge(mcp_routes(&state))
        .with_state(state)
}

/// The route layer runs only for matched routes, so `/mcp/{server}` always has the path parameter.
fn mcp_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route_service("/mcp/{server}", state.mcp.clone())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            mcp::resolve_server,
        ))
}

/// Returns 503 when the database does not answer, so Docker and Caddy see the real state.
async fn health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1")
        .execute(&state.pool)
        .await
        .map_err(ApiError::DatabaseUnavailable)?;
    Ok(Json(json!({ "status": "ok" })))
}
