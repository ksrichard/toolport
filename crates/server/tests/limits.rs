mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use server::state::AppState;
use sqlx::PgPool;

async fn state_with_limit(pool: PgPool, per_minute: u32) -> AppState {
    let github_url = common::fake_github().await;
    let mut config = common::config(&github_url);
    config.rate_limit_per_minute = per_minute;
    AppState::new(pool, &config).unwrap()
}

async fn get_limits(state: &AppState, access: Option<String>) -> (StatusCode, Value) {
    let mut request = Request::get("/api/limits");
    if let Some(access) = access {
        request = request.header(header::AUTHORIZATION, format!("Bearer {access}"));
    }
    let response = common::send(state, request.body(Body::empty()).unwrap()).await;
    (response.status(), common::json_body(response).await)
}

#[sqlx::test(migrations = "../../migrations")]
async fn limits_show_the_rate_limit_and_burst(pool: PgPool) {
    let user_id = common::create_user(&pool, 1, "octocat").await;
    let state = state_with_limit(pool, 60).await;
    let access = state.auth.jwt.issue_access(user_id).unwrap();
    let (status, body) = get_limits(&state, Some(access)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({ "rate_limit_per_minute": 60, "rate_limit_burst": 10 })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn limits_are_zero_when_the_limit_is_off(pool: PgPool) {
    let user_id = common::create_user(&pool, 1, "octocat").await;
    let state = state_with_limit(pool, 0).await;
    let access = state.auth.jwt.issue_access(user_id).unwrap();
    let (_, body) = get_limits(&state, Some(access)).await;
    assert_eq!(
        body,
        json!({ "rate_limit_per_minute": 0, "rate_limit_burst": 0 })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn limits_need_a_login(pool: PgPool) {
    let state = common::state(pool).await;
    let (status, body) = get_limits(&state, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");
}
