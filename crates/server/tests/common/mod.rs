#![allow(dead_code)]

use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
    response::Response,
    routing::{get, post},
};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use server::{config::Config, state::AppState};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

pub const GOOD_CODE: &str = "good-code";
const GITHUB_TOKEN: &str = "gho_fake_token";

/// Starts a fake GitHub on a free local port and returns its base URL.
/// It accepts only `GOOD_CODE` and returns the "octocat" user.
pub async fn fake_github() -> String {
    let app = Router::new()
        .route(
            "/login/oauth/access_token",
            post(|Json(body): Json<Value>| async move {
                if body["code"] == GOOD_CODE && body["client_secret"] == "test-client-secret" {
                    Json(json!({ "access_token": GITHUB_TOKEN, "token_type": "bearer" }))
                } else {
                    Json(json!({ "error": "bad_verification_code" }))
                }
            }),
        )
        .route(
            "/user",
            get(|headers: HeaderMap| async move {
                let expected = format!("Bearer {GITHUB_TOKEN}");
                if headers.get(header::AUTHORIZATION).map(|v| v.as_bytes())
                    != Some(expected.as_bytes())
                {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                Ok(Json(json!({
                    "id": 583231,
                    "login": "octocat",
                    "name": "The Octocat",
                    "avatar_url": "https://avatars.example/octocat.png",
                })))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

pub const JWT_SECRET: &str = "test-secret-that-is-at-least-32-characters";

pub fn config(github_url: &str) -> Config {
    Config::try_parse_from([
        "toolport-server",
        "--database-url=postgres://unused",
        "--public-url=https://toolport.test",
        &format!("--jwt-secret={JWT_SECRET}"),
        &format!("--secrets-key={}", "0".repeat(64)),
        "--allow-local-upstream",
        "--github-client-id=test-client-id",
        "--github-client-secret=test-client-secret",
        &format!("--github-oauth-url={github_url}"),
        &format!("--github-api-url={github_url}"),
    ])
    .unwrap()
}

pub async fn state(pool: PgPool) -> AppState {
    let github_url = fake_github().await;
    AppState::new(pool, &config(&github_url)).unwrap()
}

pub async fn send(state: &AppState, request: Request<Body>) -> Response {
    server::app(state.clone()).oneshot(request).await.unwrap()
}

pub async fn json_body(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Returns the full `Set-Cookie` header for the named cookie.
pub fn set_cookie(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(str::to_owned)
}

/// Returns only the value of the named cookie.
pub fn cookie_value(response: &Response, name: &str) -> Option<String> {
    let header = set_cookie(response, name)?;
    let pair = header.split(';').next()?;
    Some(pair[name.len() + 1..].to_owned())
}

pub fn location(response: &Response) -> String {
    response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned()
}

/// Creates a user row, as the first GitHub sign-in does. Returns the user id.
pub async fn create_user(pool: &PgPool, github_id: i64, login: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO users (github_id, github_login) VALUES ($1, $2) RETURNING id")
        .bind(github_id)
        .bind(login)
        .fetch_one(pool)
        .await
        .unwrap()
}
