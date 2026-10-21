mod common;

use std::time::Duration;

use axum::{body::Body, http::Request, http::StatusCode};
use serde_json::json;
use server::state::AppState;
use sqlx::{PgPool, postgres::PgPoolOptions};

async fn get(state: &AppState, path: &str) -> (StatusCode, serde_json::Value) {
    let response = common::send(state, Request::get(path).body(Body::empty()).unwrap()).await;
    (response.status(), common::json_body(response).await)
}

#[sqlx::test(migrations = "../../migrations")]
async fn health_is_ok_when_the_database_answers(pool: PgPool) {
    let state = common::state(pool).await;
    let (status, body) = get(&state, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "ok" }));
}

#[sqlx::test(migrations = "../../migrations")]
async fn migrations_create_the_users_table(pool: PgPool) {
    let theme: String = sqlx::query_scalar(
        "INSERT INTO users (github_id, github_login) VALUES (1, 'octocat') RETURNING theme",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(theme, "neon");

    let duplicate = sqlx::query("INSERT INTO users (github_id, github_login) VALUES (1, 'other')")
        .execute(&pool)
        .await;
    assert!(duplicate.is_err(), "github_id must be unique");
}

#[tokio::test]
async fn health_is_503_when_the_database_is_down() {
    // Port 1 has no Postgres, so every query fails after the acquire timeout.
    let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(500))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    let state = common::state(pool).await;
    let (status, body) = get(&state, "/api/health").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({
            "error": "database_unavailable",
            "message": "the database is not available",
        })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_api_route_gives_json_404(pool: PgPool) {
    let state = common::state(pool).await;
    let (status, body) = get(&state, "/api/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}
