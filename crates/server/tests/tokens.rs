mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};
use server::state::AppState;
use sqlx::PgPool;
use tower::ServiceExt;

const HOST: &str = "toolport.test";

struct App {
    router: Router,
    state: AppState,
}

impl App {
    /// The user `octocat` (id 1) with the empty server `pets`.
    async fn new(pool: PgPool) -> Self {
        common::create_user(&pool, 1, "octocat").await;
        let state = common::state(pool).await;
        let app = Self {
            router: server::app(state.clone()),
            state,
        };
        let (status, _) = app
            .json(1, "POST", "/api/servers", Some(json!({ "name": "pets" })))
            .await;
        assert_eq!(status, StatusCode::CREATED);
        app
    }

    async fn json(
        &self,
        user_id: i64,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let access = self.state.auth.jwt.issue_access(user_id).unwrap();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .header(header::CONTENT_TYPE, "application/json");
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        (status, common::json_body(response).await)
    }

    async fn create(&self, name: &str) -> Value {
        let (status, created) = self
            .json(
                1,
                "POST",
                "/api/servers/pets/tokens",
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        created
    }

    async fn list(&self) -> Value {
        let (status, list) = self.json(1, "GET", "/api/servers/pets/tokens", None).await;
        assert_eq!(status, StatusCode::OK);
        list
    }

    async fn initialize(&self, token: &str) -> Response {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        });
        let request = Request::post("/mcp/pets")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_returns_the_value_once_and_list_never_does(pool: PgPool) {
    let app = App::new(pool).await;
    assert_eq!(app.list().await, json!([]));

    let created = app.create("  claude-code  ").await;
    assert!(created["token"].as_str().unwrap().len() > 20);
    assert_eq!(created["expires_in_days"], 90);
    assert_eq!(created["row"]["name"], "claude-code");
    assert!(created["row"]["created_ms"].as_i64().unwrap() > 0);
    assert_eq!(created["row"]["last_used_ms"], Value::Null);
    assert_eq!(created["row"]["revoked_ms"], Value::Null);

    app.create("cursor").await;
    let list = app.list().await;
    let names: Vec<_> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["cursor", "claude-code"]);
    assert!(list[1].get("token").is_none());
    assert_eq!(list[1], created["row"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn token_names_are_checked(pool: PgPool) {
    let app = App::new(pool).await;
    for name in ["", "   ", &"x".repeat(65)] {
        let (status, error) = app
            .json(
                1,
                "POST",
                "/api/servers/pets/tokens",
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
        assert_eq!(error["error"], "invalid_token_name");
    }
    // The limit counts characters, not bytes.
    app.create(&"ä".repeat(64)).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn revoke_stops_the_token_at_once_and_keeps_the_row(pool: PgPool) {
    let app = App::new(pool).await;
    let created = app.create("laptop").await;
    let token = created["token"].as_str().unwrap();
    let id = created["row"]["id"].as_i64().unwrap();
    assert_eq!(app.initialize(token).await.status(), StatusCode::OK);

    let path = format!("/api/servers/pets/tokens/{id}/revoke");
    let (status, revoked) = app.json(1, "POST", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(revoked["revoked_ms"].as_i64().is_some());
    assert_eq!(
        app.initialize(token).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let list = app.list().await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0], revoked);
    let (_, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(servers[0]["active_tokens"], 0);

    let (status, error) = app.json(1, "POST", &path, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "token_already_revoked");

    let (status, error) = app
        .json(1, "POST", "/api/servers/pets/tokens/999999/revoke", None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"], "not_found");
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_cannot_see_or_revoke_tokens(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    let pets_token = app.create("laptop").await;
    let pets_id = pets_token["row"]["id"].as_i64().unwrap();

    let mallory = common::create_user(&pool, 2, "mallory").await;
    app.json(
        mallory,
        "POST",
        "/api/servers",
        Some(json!({ "name": "evil" })),
    )
    .await;

    for (method, path) in [
        ("GET", "/api/servers/pets/tokens".to_owned()),
        ("POST", "/api/servers/pets/tokens".to_owned()),
        ("POST", format!("/api/servers/pets/tokens/{pets_id}/revoke")),
    ] {
        let (status, _) = app
            .json(mallory, method, &path, Some(json!({ "name": "x" })))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }

    // A token id of another server looks the same as an unknown id.
    let unknown = app
        .json(
            mallory,
            "POST",
            "/api/servers/evil/tokens/999999/revoke",
            None,
        )
        .await;
    let foreign = app
        .json(
            mallory,
            "POST",
            &format!("/api/servers/evil/tokens/{pets_id}/revoke"),
            None,
        )
        .await;
    assert_eq!(foreign, unknown);
    assert_eq!(foreign.0, StatusCode::NOT_FOUND);
    assert_eq!(app.list().await[0]["revoked_ms"], Value::Null);
}

#[sqlx::test(migrations = "../../migrations")]
async fn mcp_request_sets_the_last_use(pool: PgPool) {
    let app = App::new(pool).await;
    let created = app.create("laptop").await;
    assert_eq!(app.list().await[0]["last_used_ms"], Value::Null);

    let response = app.initialize(created["token"].as_str().unwrap()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let last_used = app.list().await[0]["last_used_ms"].as_i64();
    assert!(last_used.unwrap() >= created["row"]["created_ms"].as_i64().unwrap());
}

impl App {
    async fn create_with(&self, days: Value) -> (StatusCode, Value) {
        self.json(
            1,
            "POST",
            "/api/servers/pets/tokens",
            Some(json!({ "name": "t", "expires_in_days": days })),
        )
        .await
    }
}

const DAY_MS: i64 = 86_400_000;

#[sqlx::test(migrations = "../../migrations")]
async fn each_lifetime_gives_its_expiry(pool: PgPool) {
    let app = App::new(pool).await;
    for days in [7, 30, 90, 180, 365] {
        let (status, created) = app.create_with(json!(days)).await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["expires_in_days"], days);
        let row = &created["row"];
        let life = row["expires_ms"].as_i64().unwrap() - row["created_ms"].as_i64().unwrap();
        assert!((life - days * DAY_MS).abs() < 5_000, "{days}: {life}");
        let claims = app
            .state
            .auth
            .jwt
            .verify_mcp(created["token"].as_str().unwrap())
            .unwrap();
        let exp_ms = claims.exp.unwrap() as i64 * 1000;
        assert!((exp_ms - row["expires_ms"].as_i64().unwrap()).abs() < 5_000);
    }

    // A missing field gives the default.
    let created = app.create("default").await;
    assert_eq!(created["expires_in_days"], 90);
    let row = &created["row"];
    let life = row["expires_ms"].as_i64().unwrap() - row["created_ms"].as_i64().unwrap();
    assert!((life - 90 * DAY_MS).abs() < 5_000);
}

#[sqlx::test(migrations = "../../migrations")]
async fn out_of_range_lifetimes_are_refused(pool: PgPool) {
    let app = App::new(pool).await;
    for days in [0, -1, -365, 366, 100_000] {
        let (status, error) = app.create_with(json!(days)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{days}");
        assert_eq!(error["error"], "invalid_token_lifetime");
    }
    let (status, error) = app.create_with(json!("never")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid_body");
    assert_eq!(app.list().await, json!([]));
}

#[sqlx::test(migrations = "../../migrations")]
async fn never_expiring_token_works_until_revoke(pool: PgPool) {
    let app = App::new(pool).await;
    let (status, created) = app.create_with(Value::Null).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["expires_in_days"], Value::Null);
    assert_eq!(created["row"]["expires_ms"], Value::Null);
    let token = created["token"].as_str().unwrap();
    assert_eq!(app.state.auth.jwt.verify_mcp(token).unwrap().exp, None);

    let list = app.list().await;
    assert_eq!(list[0]["expires_ms"], Value::Null);
    let (_, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(servers[0]["active_tokens"], 1);

    assert_eq!(app.initialize(token).await.status(), StatusCode::OK);
    let id = created["row"]["id"].as_i64().unwrap();
    let path = format!("/api/servers/pets/tokens/{id}/revoke");
    let (status, _) = app.json(1, "POST", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        app.initialize(token).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_token_is_refused(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    let (_, created) = app.create_with(json!(7)).await;
    let token = created["token"].as_str().unwrap();
    assert_eq!(app.initialize(token).await.status(), StatusCode::OK);

    sqlx::query("UPDATE mcp_tokens SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        app.initialize(token).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let (_, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(servers[0]["active_tokens"], 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn access_jwt_without_expiry_is_refused(pool: PgPool) {
    let app = App::new(pool).await;
    let key = jsonwebtoken::EncodingKey::from_secret(common::JWT_SECRET.as_bytes());
    let status = |claims: Value| {
        let access = jsonwebtoken::encode(&jsonwebtoken::Header::default(), &claims, &key).unwrap();
        let request = Request::get("/api/servers/pets/tokens")
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .body(Body::empty())
            .unwrap();
        let router = app.router.clone();
        async move { router.oneshot(request).await.unwrap().status() }
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({ "sub": "1", "aud": "toolport-dashboard", "iat": now });
    assert_eq!(status(claims.clone()).await, StatusCode::UNAUTHORIZED);

    // The same claims with `exp` pass, so the 401 above comes from the missing `exp`.
    let mut with_exp = claims;
    with_exp["exp"] = json!(now + 60);
    assert_eq!(status(with_exp).await, StatusCode::OK);
}
