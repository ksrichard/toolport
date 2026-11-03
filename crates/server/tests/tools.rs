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
use server::{
    import::{ImportRequest, Owner, import_spec},
    mcp_tokens,
    state::AppState,
};
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
            "/api/servers/pets/tool-test",
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
            "/api/servers/pets/tool-test",
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
            "/api/servers/pets/tool-test",
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
    let (status, error) = app.json("POST", "/api/servers/pets/tool-test", body).await;
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
            "/api/servers/pets/tool-test",
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
            "/api/servers/pets/tool-test",
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
            "/api/servers/pets/tool-test",
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

const PETSTORE: &str = include_str!("../../core/tests/fixtures/petstore.json");
const HOST: &str = "toolport.test";

fn forecast_draft() -> Value {
    json!({
        "name": "getForecast", "description": "Gets the forecast", "method": "GET",
        "path": "/forecast", "query": "latitude={lat}&longitude={lon}", "headers": [],
        "inputs": [
            { "name": "lat", "input_type": "number", "required": true, "description": "Latitude" },
            { "name": "lon", "input_type": "number", "required": true, "description": "" },
        ],
    })
}

/// A manual API on the fake upstream and a manual tool on it. Returns the API id and the view.
async fn add_forecast(app: &App) -> (i64, Value) {
    let api_id = app.create_api(&fake_upstream().await, SECRET).await;
    let (status, view) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": forecast_draft() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    (api_id, view)
}

async fn import_petstore(app: &App) {
    import_spec(
        &app.state.pool,
        ImportRequest {
            owner: Owner::Login("octocat"),
            server: "pets",
            api_name: None,
            spec_text: PETSTORE,
            base_url: Some("https://petstore.example/api/v3".parse().unwrap()),
        },
    )
    .await
    .unwrap();
}

/// One MCP session on the server `pets`.
struct Mcp {
    router: Router,
    token: String,
    session: String,
}

impl Mcp {
    async fn post(&self, session: Option<&str>, body: Value) -> Response {
        let mut request = Request::post("/mcp/pets")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
        self.router.clone().oneshot(request).await.unwrap()
    }

    async fn connect(app: &App) -> Self {
        let ttl = std::time::Duration::from_secs(3600);
        let token = mcp_tokens::create(&app.state.pool, &app.state.auth.jwt, "pets", "t", ttl)
            .await
            .unwrap()
            .value;
        let mut mcp = Self {
            router: app.router.clone(),
            token,
            session: String::new(),
        };
        let init = json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        });
        let response = mcp.post(None, init).await;
        assert_eq!(response.status(), StatusCode::OK);
        mcp.session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        let ready = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let response = mcp.post(Some(&mcp.session), ready).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        mcp
    }

    /// Returns the `result` of the request.
    async fn request(&self, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
        let response = self.post(Some(&self.session), body).await;
        let bytes = http_body_util::BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .find(|data| !data.is_empty())
            .unwrap_or_else(|| panic!("no SSE message in: {text}"));
        let message: Value = serde_json::from_str(data).unwrap();
        message["result"].clone()
    }

    async fn listed(&self, name: &str) -> Value {
        let list = self.request("tools/list", json!({})).await;
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not listed"))
            .clone()
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_manual_tool_appears_in_tools_list(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (_, view) = add_forecast(&app).await;
    assert_eq!(view["source"], "manual");
    assert_eq!(view["edited"], false);

    let listed = Mcp::connect(&app).await.listed("getForecast").await;
    // rmcp may add keys such as `title` or `annotations`. They must stay null or absent.
    let extra: Vec<&String> = listed
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, value)| {
            !["name", "description", "inputSchema"].contains(&key.as_str()) && !value.is_null()
        })
        .map(|(key, _)| key)
        .collect();
    assert!(extra.is_empty(), "extra keys: {extra:?}");
    for key in ["name", "description", "inputSchema"] {
        assert_eq!(listed[key], view["preview"][key], "key {key}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_tool_call_works_end_to_end(pool: PgPool) {
    let app = App::new(pool, true).await;
    add_forecast(&app).await;
    let mcp = Mcp::connect(&app).await;
    let result = mcp
        .request(
            "tools/call",
            json!({ "name": "getForecast", "arguments": { "lat": 47.5, "lon": 8.5 } }),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("latitude=47.5&longitude=8.5"), "{text}");
    assert!(text.contains("\"token_ok\":true"), "{text}");
}

async fn petstore_status_view(app: &App) -> Value {
    let (status, view) = app
        .json(
            "GET",
            "/api/servers/pets/tools/findPetsByStatus",
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

fn save_body(view: &Value) -> Value {
    json!({ "api_id": view["api_id"], "draft": view["draft"] })
}

#[sqlx::test(migrations = "../../migrations")]
async fn edit_spec_tool_keeps_enum(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = petstore_status_view(&app).await;
    assert_eq!(view["source"], "spec");
    let mut body = save_body(&view);
    body["draft"]["description"] = json!("Finds pets by status");
    let (status, saved) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["edited"], true);
    assert_eq!(saved["draft"]["description"], "Finds pets by status");

    let (schema,): (Value,) =
        sqlx::query_as("SELECT input_schema FROM tools WHERE name = 'findPetsByStatus'")
            .fetch_one(&app.state.pool)
            .await
            .unwrap();
    assert!(
        schema["properties"]["status"]["enum"].is_array(),
        "{schema}"
    );
    assert_eq!(schema, view["preview"]["inputSchema"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn edit_spec_tool_cannot_change_path(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = petstore_status_view(&app).await;
    for (field, value) in [
        ("path", json!("/other")),
        ("method", json!("POST")),
        ("query", json!("x={status}")),
        ("headers", json!([{ "name": "X-A", "value": "b" }])),
        ("body_schema", json!({ "type": "object" })),
    ] {
        let mut body = save_body(&view);
        body["draft"][field] = value;
        let (status, error) = app
            .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{field}");
        assert_eq!(error["error"], "spec_tool_fixed", "{field}");
    }
    let mut body = save_body(&view);
    body["api_id"] = json!(view["api_id"].as_i64().unwrap() + 1000);
    let (status, error) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "spec_tool_fixed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn reset_restores_the_spec_version(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = petstore_status_view(&app).await;
    let mut body = save_body(&view);
    body["draft"]["description"] = json!("Changed");
    body["draft"]["inputs"][0]["description"] = json!("Changed input");
    body["draft"]["inputs"][0]["required"] = json!(true);
    app.json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;

    let (status, reset) = app
        .json(
            "POST",
            "/api/servers/pets/tools/findPetsByStatus/reset",
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    assert_eq!(reset["edited"], false);
    assert_eq!(reset["preview"], view["preview"]);
    assert_eq!(reset["draft"]["description"], view["draft"]["description"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reset_refuses_a_manual_tool(pool: PgPool) {
    let app = App::new(pool, true).await;
    add_forecast(&app).await;
    let (status, error) = app
        .json(
            "POST",
            "/api/servers/pets/tools/getForecast/reset",
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "not_a_spec_tool");
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_spec_tool_is_refused(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let (status, error) = app
        .json(
            "DELETE",
            "/api/servers/pets/tools/findPetsByStatus",
            json!(null),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "spec_tool_cannot_be_deleted");
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_last_enabled_tool_is_refused(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (api_id, _) = add_forecast(&app).await;
    let mut other = forecast_draft();
    other["name"] = json!("other");
    app.json(
        "POST",
        "/api/servers/pets/tools",
        json!({ "api_id": api_id, "draft": other }),
    )
    .await;
    sqlx::query("UPDATE tools SET enabled = false WHERE name = 'other'")
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (status, error) = app
        .json("DELETE", "/api/servers/pets/tools/getForecast", json!(null))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "last_enabled_tool");
    // A disabled tool can go.
    let (status, _) = app
        .text("DELETE", "/api/servers/pets/tools/other", json!(null))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn tool_named_test_can_be_read_and_deleted(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api_id = app.create_api(&fake_upstream().await, SECRET).await;
    let mut named = forecast_draft();
    named["name"] = json!("test");
    let body = json!({ "api_id": api_id, "draft": named });
    let (status, _) = app
        .json("POST", "/api/servers/pets/tools", body.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let mut second = forecast_draft();
    second["name"] = json!("keep");
    app.json(
        "POST",
        "/api/servers/pets/tools",
        json!({ "api_id": api_id, "draft": second }),
    )
    .await;
    let (status, view) = app
        .json("GET", "/api/servers/pets/tools/test", json!(null))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["draft"]["name"], "test");
    let (status, _) = app
        .text("DELETE", "/api/servers/pets/tools/test", json!(null))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn input_named_body_is_reserved(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api_id = app.create_api(&fake_upstream().await, SECRET).await;
    let mut draft = forecast_draft();
    draft["body_schema"] = json!({ "type": "object" });
    draft["inputs"].as_array_mut().unwrap().push(
        json!({ "name": "body", "input_type": "string", "required": false, "description": "" }),
    );
    let (status, error) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": draft }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "reserved_input_name");

    // A spec tool with a body rejects a `body` row too.
    import_petstore(&app).await;
    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/addPet", json!(null))
        .await;
    let mut body = save_body(&view);
    body["draft"]["inputs"].as_array_mut().unwrap().push(
        json!({ "name": "body", "input_type": "string", "required": false, "description": "" }),
    );
    let (status, error) = app
        .json("PUT", "/api/servers/pets/tools/addPet", body)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "reserved_input_name");
}

#[sqlx::test(migrations = "../../migrations")]
async fn spec_tool_input_names_cannot_change(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = petstore_status_view(&app).await;
    let mut renamed = save_body(&view);
    renamed["draft"]["inputs"][0]["name"] = json!("state");
    let mut added = save_body(&view);
    added["draft"]["inputs"].as_array_mut().unwrap().push(
        json!({ "name": "extra", "input_type": "string", "required": false, "description": "" }),
    );
    let mut removed = save_body(&view);
    removed["draft"]["inputs"] = json!([]);
    for body in [renamed, added, removed] {
        let (status, error) = app
            .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error["error"], "spec_tool_fixed");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_manual_tool_works(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (api_id, _) = add_forecast(&app).await;
    let mut other = forecast_draft();
    other["name"] = json!("other");
    app.json(
        "POST",
        "/api/servers/pets/tools",
        json!({ "api_id": api_id, "draft": other }),
    )
    .await;
    let (status, _) = app
        .text("DELETE", "/api/servers/pets/tools/getForecast", json!(null))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app
        .json("GET", "/api/servers/pets/tools/getForecast", json!(null))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_name_is_a_conflict(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (api_id, _) = add_forecast(&app).await;
    let (status, error) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": forecast_draft() }),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "tool_name_taken");
}

fn with_tool_body(server: &str, draft: Value) -> Value {
    json!({
        "server": server,
        "api": {
            "name": "weather", "base_url": "https://a.example/v1",
            "auth": { "type": "bearer", "secret": SECRET },
        },
        "draft": draft,
    })
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_server_with_tool_is_atomic(pool: PgPool) {
    let app = App::new(pool, true).await;
    let mut bad = forecast_draft();
    bad["query"] = json!("latitude={missing}");
    let (status, error) = app
        .json(
            "POST",
            "/api/servers/with-tool",
            with_tool_body("fresh", bad),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "unknown_placeholder");
    let (servers, apis): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM servers WHERE name = 'fresh'), (SELECT count(*) FROM apis)",
    )
    .fetch_one(&app.state.pool)
    .await
    .unwrap();
    assert_eq!((servers, apis), (0, 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn create_server_with_tool_works(pool: PgPool) {
    let app = App::new(pool, true).await;
    let body = with_tool_body("fresh", forecast_draft());
    let (status, summary) = app.json("POST", "/api/servers/with-tool", body).await;
    assert_eq!(status, StatusCode::CREATED, "{summary}");
    assert_eq!(summary["name"], "fresh");
    assert_eq!(summary["tools"], 1);
    let (status, view) = app
        .json("GET", "/api/servers/fresh/tools/getForecast", json!(null))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["source"], "manual");
}

#[sqlx::test(migrations = "../../migrations")]
async fn save_twice_gives_a_valid_tool(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (api_id, _) = add_forecast(&app).await;
    let mcp = Mcp::connect(&app).await;
    for description in ["first", "second"] {
        let mut draft = forecast_draft();
        draft["description"] = json!(description);
        let (status, view) = app
            .json(
                "PUT",
                "/api/servers/pets/tools/getForecast",
                json!({ "api_id": api_id, "draft": draft }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{view}");
    }
    let listed = mcp.listed("getForecast").await;
    assert_eq!(listed["description"], "second");
    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/getForecast", json!(null))
        .await;
    assert_eq!(view["draft"]["description"], "second");
    assert_eq!(view["draft"]["query"], "latitude={lat}&longitude={lon}");
    assert_eq!(view["preview"]["inputSchema"], listed["inputSchema"]);
}

const PET_SUMMARY: &str = "Find pet by ID.";

fn petstore_with(from: &str, to: &str) -> String {
    assert!(PETSTORE.contains(from), "{from}");
    PETSTORE.replace(from, to)
}

async fn reimport(app: &App, spec: &str) -> server::import::ImportReport {
    import_spec(
        &app.state.pool,
        ImportRequest {
            owner: Owner::Login("octocat"),
            server: "pets",
            api_name: None,
            spec_text: spec,
            base_url: Some("https://petstore.example/api/v3".parse().unwrap()),
        },
    )
    .await
    .unwrap()
}

/// Edits the tool and gives it a new name. Returns the saved view.
async fn edit_and_rename(app: &App, tool: &str, new_name: &str) -> Value {
    let (_, view) = app
        .json(
            "GET",
            &format!("/api/servers/pets/tools/{tool}"),
            json!(null),
        )
        .await;
    let mut body = save_body(&view);
    body["draft"]["name"] = json!(new_name);
    body["draft"]["description"] = json!("My own text");
    let (status, saved) = app
        .json("PUT", &format!("/api/servers/pets/tools/{tool}"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_keeps_edited_tool_and_its_new_name(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    edit_and_rename(&app, "getPetById", "pet").await;

    let report = reimport(&app, PETSTORE).await;
    assert!(report.replaced);
    assert_eq!(report.kept, vec!["pet".to_owned()]);
    assert!(report.removed.is_empty());

    let (status, view) = app
        .json("GET", "/api/servers/pets/tools/pet", json!(null))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["draft"]["description"], "My own text");
    assert_eq!(view["edited"], true);
    assert_eq!(view["spec_changed"], false);
    let (status, _) = app
        .json("GET", "/api/servers/pets/tools/getPetById", json!(null))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_marks_spec_change(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    edit_and_rename(&app, "getPetById", "pet").await;
    reimport(&app, &petstore_with(PET_SUMMARY, "Find a pet by its ID.")).await;

    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/pet", json!(null))
        .await;
    assert_eq!(view["spec_changed"], true);
    let (_, list) = app
        .json("GET", "/api/servers/pets/tools", json!(null))
        .await;
    let row = list
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "pet")
        .unwrap();
    assert_eq!(row["spec_changed"], true);

    // Reset takes the new spec version and clears the flag.
    let (_, reset) = app
        .json("POST", "/api/servers/pets/tools/pet/reset", json!(null))
        .await;
    assert_eq!(reset["spec_changed"], false);
    assert_eq!(reset["edited"], false);
    assert!(
        reset["draft"]["description"]
            .as_str()
            .unwrap()
            .contains("Find a pet by its ID.")
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_of_an_unedited_tool_takes_the_new_spec(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    reimport(&app, &petstore_with(PET_SUMMARY, "Find a pet by its ID.")).await;
    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/getPetById", json!(null))
        .await;
    assert!(
        view["draft"]["description"]
            .as_str()
            .unwrap()
            .contains("Find a pet by its ID.")
    );
    assert_eq!(view["edited"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_removes_gone_operation_even_if_edited(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    edit_and_rename(&app, "getInventory", "stock").await;
    let spec = petstore_with("\"/store/inventory\"", "\"/store/stock\"");
    let report = reimport(&app, &spec).await;
    assert_eq!(report.removed, vec!["stock".to_owned()]);
    assert!(report.kept.is_empty());
    assert!(report.tools.contains(&"getInventory".to_owned()));
    let (status, _) = app
        .json("GET", "/api/servers/pets/tools/stock", json!(null))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_keeps_enabled_flag(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    sqlx::query("UPDATE tools SET enabled = false WHERE name = 'getPetById'")
        .execute(&app.state.pool)
        .await
        .unwrap();
    reimport(&app, &petstore_with(PET_SUMMARY, "Find a pet by its ID.")).await;
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM tools WHERE name = 'getPetById'")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert!(!enabled);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_keeps_manual_tools_of_the_api(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let api_id: i64 = sqlx::query_scalar("SELECT id FROM apis")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    let (status, view) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": forecast_draft() }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    let name = view["draft"]["name"].as_str().unwrap().to_owned();

    reimport(&app, PETSTORE).await;
    let api_after: i64 = sqlx::query_scalar("SELECT id FROM apis")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(api_after, api_id);
    let manual_api: i64 =
        sqlx::query_scalar("SELECT api_id FROM tools WHERE source = 'manual' AND name = $1")
            .bind(&name)
            .fetch_one(&app.state.pool)
            .await
            .unwrap();
    assert_eq!(manual_api, api_id);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_of_old_rows_without_a_hash_sets_it_and_raises_no_alarm(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    edit_and_rename(&app, "getPetById", "pet").await;
    sqlx::query("UPDATE tools SET spec_hash = NULL, edited_hash = NULL")
        .execute(&app.state.pool)
        .await
        .unwrap();
    reimport(&app, &petstore_with(PET_SUMMARY, "Find a pet by its ID.")).await;
    let missing: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tools WHERE source = 'spec' AND spec_hash IS NULL",
    )
    .fetch_one(&app.state.pool)
    .await
    .unwrap();
    assert_eq!(missing, 0);
    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/pet", json!(null))
        .await;
    assert_eq!(view["spec_changed"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_ignores_the_spec_name_of_an_edited_tool(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let api_id: i64 = sqlx::query_scalar("SELECT id FROM apis")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    // The user renames the spec tool away, then makes a manual tool with the old name.
    edit_and_rename(&app, "getPetById", "pet").await;
    let mut draft = forecast_draft();
    draft["name"] = json!("getPetById");
    let (status, view) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": draft }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{view}");
    // The edited tool keeps its own name, so the spec name causes no clash.
    let report = reimport(&app, PETSTORE).await;
    assert_eq!(report.kept, vec!["pet".to_owned()]);
}
