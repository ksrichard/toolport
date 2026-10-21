mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use reqwest::Url;
use serde_json::json;
use server::{auth::jwt::JwtKeys, state::AppState};
use sqlx::PgPool;

use common::{GOOD_CODE, cookie_value, json_body, location, send, set_cookie};

/// Runs the full GitHub sign-in and returns the refresh token cookie value.
async fn sign_in(state: &AppState) -> String {
    let response = send(
        state,
        Request::get("/auth/github/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let oauth_state = cookie_value(&response, "tp_oauth_state").unwrap();

    let response = send(
        state,
        Request::get(format!(
            "/auth/github/callback?code={GOOD_CODE}&state={oauth_state}"
        ))
        .header(header::COOKIE, format!("tp_oauth_state={oauth_state}"))
        .body(Body::empty())
        .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/");
    cookie_value(&response, "tp_refresh").expect("callback must set the refresh cookie")
}

async fn refresh(state: &AppState, refresh_token: &str) -> axum::response::Response {
    send(
        state,
        Request::post("/auth/refresh")
            .header(header::COOKIE, format!("tp_refresh={refresh_token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn access_token(state: &AppState) -> String {
    let refresh_token = sign_in(state).await;
    let response = refresh(state, &refresh_token).await;
    json_body(response).await["access_token"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn get_me(state: &AppState, authorization: Option<&str>) -> axum::response::Response {
    let mut request = Request::get("/api/me");
    if let Some(value) = authorization {
        request = request.header(header::AUTHORIZATION, value);
    }
    send(state, request.body(Body::empty()).unwrap()).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn login_redirects_to_github_with_a_state(pool: PgPool) {
    let state = common::state(pool).await;
    let response = send(
        &state,
        Request::get("/auth/github/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = set_cookie(&response, "tp_oauth_state").unwrap();
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("Path=/auth/github"));

    let url = Url::parse(&location(&response)).unwrap();
    assert_eq!(url.path(), "/login/oauth/authorize");
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["client_id"], "test-client-id");
    assert_eq!(
        query["redirect_uri"],
        "https://toolport.test/auth/github/callback"
    );
    assert_eq!(
        query["state"],
        cookie_value(&response, "tp_oauth_state").unwrap()
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn callback_rejects_bad_input(pool: PgPool) {
    let state = common::state(pool).await;
    let cases = [
        // (query, state cookie, expected login_error)
        (
            format!("code={GOOD_CODE}&state=a"),
            Some("b"),
            "invalid_state",
        ),
        (format!("code={GOOD_CODE}&state=a"), None, "invalid_state"),
        (
            "error=access_denied&state=a".to_owned(),
            Some("a"),
            "github_denied",
        ),
        ("state=a".to_owned(), Some("a"), "missing_code"),
        (
            "code=wrong&state=a".to_owned(),
            Some("a"),
            "github_rejected",
        ),
    ];
    for (query, cookie, expected) in cases {
        let mut request = Request::get(format!("/auth/github/callback?{query}"));
        if let Some(value) = cookie {
            request = request.header(header::COOKIE, format!("tp_oauth_state={value}"));
        }
        let response = send(&state, request.body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER, "{query}");
        assert_eq!(
            location(&response),
            format!("/?login_error={expected}"),
            "{query}"
        );
        assert!(set_cookie(&response, "tp_refresh").is_none(), "{query}");
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn sign_in_sets_a_strict_refresh_cookie_and_stores_only_a_hash(pool: PgPool) {
    let state = common::state(pool).await;
    let response = send(
        &state,
        Request::get("/auth/github/login")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let oauth_state = cookie_value(&response, "tp_oauth_state").unwrap();
    let response = send(
        &state,
        Request::get(format!(
            "/auth/github/callback?code={GOOD_CODE}&state={oauth_state}"
        ))
        .header(header::COOKIE, format!("tp_oauth_state={oauth_state}"))
        .body(Body::empty())
        .unwrap(),
    )
    .await;

    let cookie = set_cookie(&response, "tp_refresh").unwrap();
    for part in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/auth"] {
        assert!(cookie.contains(part), "{cookie} has no {part}");
    }
    let value = cookie_value(&response, "tp_refresh").unwrap();
    let stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE token_hash = $1::bytea")
            .bind(value.as_bytes())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 0, "the raw token must not be in the database");
}

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_gives_a_bearer_token_that_opens_the_api(pool: PgPool) {
    let state = common::state(pool).await;
    let refresh_token = sign_in(&state).await;

    let response = refresh(&state, &refresh_token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let new_refresh = cookie_value(&response, "tp_refresh").unwrap();
    assert_ne!(new_refresh, refresh_token, "refresh must rotate the token");
    let body = json_body(response).await;
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 900);

    let token = body["access_token"].as_str().unwrap();
    let response = get_me(&state, Some(&format!("Bearer {token}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let me = json_body(response).await;
    assert_eq!(me["github_login"], "octocat");
    assert_eq!(me["name"], "The Octocat");
    assert_eq!(me["theme"], "neon");
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_used_refresh_token_does_not_work_again(pool: PgPool) {
    let state = common::state(pool).await;
    let refresh_token = sign_in(&state).await;
    assert_eq!(
        refresh(&state, &refresh_token).await.status(),
        StatusCode::OK
    );

    let response = refresh(&state, &refresh_token).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(json_body(response).await["error"], "unauthorized");
}

#[sqlx::test(migrations = "../../migrations")]
async fn refresh_without_a_cookie_is_401(pool: PgPool) {
    let state = common::state(pool).await;
    let response = send(
        &state,
        Request::post("/auth/refresh").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn api_rejects_missing_and_bad_bearer_tokens(pool: PgPool) {
    let state = common::state(pool).await;
    let foreign = JwtKeys::new(b"another-secret-that-is-at-least-32-chars")
        .issue_access(1)
        .unwrap();
    let foreign = format!("Bearer {foreign}");
    let cases = [
        None,
        Some("Bearer not-a-jwt"),
        Some("Basic abc"),
        Some(foreign.as_str()),
    ];
    for authorization in cases {
        let response = get_me(&state, authorization).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{authorization:?}"
        );
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        assert_eq!(
            json_body(response).await,
            json!({ "error": "unauthorized", "message": "authentication is required" })
        );
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn sign_in_twice_keeps_one_user(pool: PgPool) {
    let state = common::state(pool).await;
    sign_in(&state).await;
    sign_in(&state).await;
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(users, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn theme_can_be_changed_and_bad_values_are_rejected(pool: PgPool) {
    let state = common::state(pool).await;
    let bearer = format!("Bearer {}", access_token(&state).await);

    let put = |theme: serde_json::Value| {
        Request::put("/api/me/theme")
            .header(header::AUTHORIZATION, &bearer)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "theme": theme }).to_string()))
            .unwrap()
    };

    let response = send(&state, put(json!("light"))).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let me = json_body(get_me(&state, Some(&bearer)).await).await;
    assert_eq!(me["theme"], "light");

    let response = send(&state, put(json!("pink"))).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await["error"], "invalid_body");
    let me = json_body(get_me(&state, Some(&bearer)).await).await;
    assert_eq!(me["theme"], "light");
}

#[sqlx::test(migrations = "../../migrations")]
async fn logout_revokes_the_refresh_token_and_clears_the_cookie(pool: PgPool) {
    let state = common::state(pool).await;
    let refresh_token = sign_in(&state).await;

    let response = send(
        &state,
        Request::post("/auth/logout")
            .header(header::COOKIE, format!("tp_refresh={refresh_token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cleared = set_cookie(&response, "tp_refresh").unwrap();
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
    assert!(cleared.contains("Path=/auth"), "{cleared}");

    assert_eq!(
        refresh(&state, &refresh_token).await.status(),
        StatusCode::UNAUTHORIZED
    );
}
