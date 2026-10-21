use std::time::Duration;

use axum::{body::Body, http::Request, http::StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

async fn get_health(pool: PgPool) -> (StatusCode, Value) {
    let response = server::app(pool)
        .oneshot(Request::get("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[sqlx::test(migrations = "../../migrations")]
async fn health_is_ok_when_the_database_answers(pool: PgPool) {
    let (status, body) = get_health(pool).await;
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
    let (status, body) = get_health(pool).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({
            "error": "database_unavailable",
            "message": "the database is not available",
        })
    );
}
