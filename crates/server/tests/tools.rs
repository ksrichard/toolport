mod common;

use axum::{
    Json, Router,
    body::Body,
    extract::RawQuery,
    http::{HeaderMap, Request, StatusCode, header},
    response::Response,
    routing::get,
};
use serde_json::{Value, json};
use server::state::AppState;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

const SECRET: &str = "s3cret-t0ken-value";

/// Answers with the query and tells whether the bearer token has the expected value.
/// It does not echo the token, because ToolPort masks the secret in responses.
async fn fake_upstream() -> String {
    let app = Router::new().route(
        "/v1/forecast",
        get(|RawQuery(query): RawQuery, headers: HeaderMap| async move {
            let auth = headers.get("authorization").and_then(|v| v.to_str().ok());
            Json(json!({ "query": query, "token_ok": auth == Some(&format!("Bearer {SECRET}")) }))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

struct App {
    router: Router,
    state: AppState,
    user: i64,
}

impl App {
    async fn new(pool: PgPool, allow_local: bool) -> Self {
        let user = common::create_user(&pool, 1, "octocat").await;
        let mut config = common::config(&common::fake_github().await);
        config.allow_local_upstream = allow_local;
        let state = AppState::new(pool, &config).unwrap();
        sqlx::query("INSERT INTO servers (user_id, name) VALUES ($1, 'pets')")
            .bind(user)
            .execute(&state.pool)
            .await
            .unwrap();
        Self {
            router: server::app(state.clone()),
            state,
            user,
        }
    }

    async fn as_user(&self, user: i64, method: &str, path: &str, body: Value) -> Response {
        let access = self.state.auth.jwt.issue_access(user).unwrap();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    /// Returns the status and the raw response text.
    async fn text(&self, method: &str, path: &str, body: Value) -> (StatusCode, String) {
        let response = self.as_user(self.user, method, path, body).await;
        let status = response.status();
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn json(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let (status, text) = self.text(method, path, body).await;
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    async fn create_api(&self, base_url: &str, secret: &str) -> i64 {
        let body = json!({
            "name": "weather", "base_url": base_url,
            "auth": { "type": "bearer", "secret": secret },
        });
        let (status, api) = self
            .json("POST", "/api/servers/pets/apis/manual", body)
            .await;
        assert_eq!(status, StatusCode::CREATED);
        api["id"].as_i64().unwrap()
    }
}

fn draft(query: &str) -> Value {
    json!({
        "name": "getForecast", "description": "Gets the forecast", "method": "GET",
        "path": "/forecast", "query": query, "headers": [],
        "inputs": [{ "name": "lat", "input_type": "number", "required": true, "description": "" }],
    })
}

fn test_body(query: &str, api: Value) -> Value {
    json!({ "draft": draft(query), "api": api, "args": { "lat": 47.5 } })
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_manual_api_hides_the_secret(pool: PgPool) {
    let app = App::new(pool, true).await;
    let body = json!({
        "name": "weather", "base_url": "https://api.example.com/v1",
        "auth": { "type": "bearer", "secret": SECRET },
    });
    let (status, text) = app
        .text("POST", "/api/servers/pets/apis/manual", body)
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(!text.contains(SECRET));
    let api: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(api["kind"], "manual");
    assert_eq!(api["auth"], json!({ "type": "bearer", "secret_set": true }));

    let (stored,): (Vec<u8>,) = sqlx::query_as("SELECT auth_secret FROM apis")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert!(!stored.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));

    let (_, list) = app.text("GET", "/api/servers/pets/apis", json!(null)).await;
    assert!(!list.contains(SECRET));
    assert!(list.contains("secret_set"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn edit_api_without_secret_keeps_the_old_one(pool: PgPool) {
    let app = App::new(pool, true).await;
    let base = fake_upstream().await;
    let id = app.create_api(&base, SECRET).await;
    let body = json!({ "name": "weather2", "base_url": base, "auth": { "type": "bearer" } });
    let (status, api) = app
        .json("PUT", &format!("/api/servers/pets/apis/{id}"), body)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(api["name"], "weather2");
    assert_eq!(api["auth"]["secret_set"], true);

    let api = json!({ "kind": "saved", "id": id });
    let (status, result) = app
        .json(
            "POST",
            "/api/servers/pets/tools/test",
            test_body("latitude={lat}", api),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let text = result["mcp"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"token_ok\":true"), "{text}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn edit_without_secret_fails_when_auth_needs_one(pool: PgPool) {
    let app = App::new(pool, true).await;
    let body = json!({ "name": "w", "base_url": "https://a.example", "auth": { "type": "none" } });
    let (_, api) = app
        .json("POST", "/api/servers/pets/apis/manual", body)
        .await;
    let path = format!("/api/servers/pets/apis/{}", api["id"]);
    let body =
        json!({ "name": "w", "base_url": "https://a.example", "auth": { "type": "bearer" } });
    let (status, error) = app.json("PUT", &path, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "missing_secret");
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_api_is_not_found(pool: PgPool) {
    let app = App::new(pool, true).await;
    let id = app.create_api("https://a.example/v1", SECRET).await;
    let other = common::create_user(&app.state.pool, 2, "other").await;
    let body = json!({ "name": "x", "base_url": "https://a.example", "auth": { "type": "none" } });
    let response = app
        .as_user(other, "PUT", &format!("/api/servers/pets/apis/{id}"), body)
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = app
        .as_user(other, "GET", "/api/servers/pets/apis", json!(null))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_returns_mcp_result_and_masked_trace(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api = json!({
        "kind": "draft", "base_url": fake_upstream().await,
        "auth": { "type": "bearer", "secret": SECRET },
    });
    let (status, text) = app
        .text(
            "POST",
            "/api/servers/pets/tools/test",
            test_body("latitude={lat}", api),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET));
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["mcp"]["isError"], false);
    assert!(
        result["http"]["request"]["url"]
            .as_str()
            .unwrap()
            .contains("latitude=47.5")
    );
    let headers = result["http"]["request"]["headers"].as_array().unwrap();
    assert!(headers.contains(&json!(["authorization", "Bearer ••••"])));
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_uses_saved_secret(pool: PgPool) {
    let app = App::new(pool, true).await;
    let id = app.create_api(&fake_upstream().await, SECRET).await;
    let api = json!({ "kind": "saved", "id": id });
    let (status, result) = app
        .json(
            "POST",
            "/api/servers/pets/tools/test",
            test_body("latitude={lat}", api),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let text = result["mcp"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"token_ok\":true"), "{text}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_reports_draft_errors(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api =
        json!({ "kind": "draft", "base_url": "https://a.example", "auth": { "type": "none" } });
    let mut body = test_body("latitude={lat}&longitude={lon}", api);
    body["args"] = json!({});
    let (status, error) = app.json("POST", "/api/servers/pets/tools/test", body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "unknown_placeholder");
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_needs_a_secret_for_a_draft_with_auth(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api =
        json!({ "kind": "draft", "base_url": "https://a.example", "auth": { "type": "bearer" } });
    let (status, error) = app
        .json(
            "POST",
            "/api/servers/pets/tools/test",
            test_body("latitude={lat}", api),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "missing_secret");
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_does_not_save_a_call(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api =
        json!({ "kind": "draft", "base_url": fake_upstream().await, "auth": { "type": "none" } });
    let (status, _) = app
        .json(
            "POST",
            "/api/servers/pets/tools/test",
            test_body("latitude={lat}", api),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM calls")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_to_blocked_address(pool: PgPool) {
    let app = App::new(pool, false).await;
    let api =
        json!({ "kind": "draft", "base_url": fake_upstream().await, "auth": { "type": "none" } });
    let (status, result) = app
        .json(
            "POST",
            "/api/servers/pets/tools/test",
            test_body("latitude={lat}", api),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["mcp"]["isError"], true);
    assert_eq!(
        result["mcp"]["content"][0]["text"],
        "the upstream address is not allowed"
    );
}

#[test]
fn draft_to_tool_maps_inputs() {
    use server::tools::{DraftError, ToolDraft, draft_to_tool};
    let mut value = draft("lat={lat}");
    value["path"] = json!("/forecast/{id}");
    value["headers"] =
        json!([{ "name": "X-Trace", "value": "{trace}" }, { "name": "X-Fixed", "value": "yes" }]);
    value["inputs"] = json!([
        { "name": "lat", "input_type": "number", "required": false, "description": "" },
        { "name": "id", "input_type": "string", "required": false, "description": "" },
        { "name": "trace", "input_type": "string", "required": false, "description": "" },
    ]);
    let parsed: ToolDraft = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(
        draft_to_tool(&parsed),
        Err(DraftError::PathInputOptional(_))
    ));

    value["inputs"][1]["required"] = json!(true);
    let parsed: ToolDraft = serde_json::from_value(value.clone()).unwrap();
    let tool = draft_to_tool(&parsed).unwrap();
    assert_eq!(tool.request.params.len(), 3);
    assert_eq!(
        tool.request.fixed_headers,
        vec![("X-Fixed".to_owned(), "yes".to_owned())]
    );

    value["name"] = json!("bad name!");
    let parsed: ToolDraft = serde_json::from_value(value).unwrap();
    assert!(matches!(
        draft_to_tool(&parsed),
        Err(DraftError::InvalidToolName(_))
    ));
}
