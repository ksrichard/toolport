mod common;

use axum::{
    Json, Router,
    body::Body,
    extract::RawQuery,
    http::{HeaderMap, Request, StatusCode, header},
    response::Response,
    routing::get,
};
use http_body_util::BodyExt;
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
            spec_url: None,
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
        let token =
            mcp_tokens::create(&app.state.pool, &app.state.auth.jwt, "pets", "t", Some(ttl))
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
            spec_url: None,
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

#[sqlx::test(migrations = "../../migrations")]
async fn user_test_route_runs_a_draft_without_a_server(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api = json!({
        "kind": "draft", "base_url": fake_upstream().await,
        "auth": { "type": "bearer", "secret": SECRET },
    });
    let (status, text) = app
        .text("POST", "/api/tool-test", test_body("latitude={lat}", api))
        .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET));
    let result: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result["mcp"]["isError"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn user_test_route_rejects_a_saved_api(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api = json!({ "kind": "saved", "id": 1 });
    let (status, error) = app
        .json("POST", "/api/tool-test", test_body("latitude={lat}", api))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "invalid_test_call");
}

// ---- final review fixes ----

/// A fake Petstore. It answers the paths that the tests below call.
async fn fake_petstore() -> String {
    let app = Router::new()
        .route(
            "/api/v3/pet/{id}",
            get(
                |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    Json(json!({ "id": id, "name": "rex" }))
                },
            ),
        )
        .route(
            "/api/v3/pet/findByStatus",
            get(|RawQuery(query): RawQuery| async move { Json(json!({ "query": query })) }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/api/v3")
}

async fn import_petstore_at(app: &App, base_url: &str) {
    import_spec(
        &app.state.pool,
        ImportRequest {
            owner: Owner::Login("octocat"),
            server: "pets",
            api_name: None,
            spec_text: PETSTORE,
            base_url: Some(base_url.parse().unwrap()),
            spec_url: None,
        },
    )
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_runs_a_spec_tool(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore_at(&app, &fake_petstore().await).await;
    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/getPetById", json!(null))
        .await;
    let api = json!({ "kind": "saved", "id": view["api_id"] });
    let body = json!({
        "draft": view["draft"], "api": api, "args": { "petId": 7 }, "spec_tool": "getPetById",
    });
    let (status, result) = app
        .json("POST", "/api/servers/pets/tool-test", body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["mcp"]["isError"], false, "{result}");
    let text = result["mcp"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("rex"), "{text}");

    // The draft must still follow the stored tool.
    let mut changed = body.clone();
    changed["draft"]["path"] = json!("/other");
    let (status, error) = app
        .json("POST", "/api/servers/pets/tool-test", changed)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "spec_tool_fixed");
    // The no-server route cannot use a stored tool.
    let (status, _) = app.json("POST", "/api/tool-test", body).await;
    assert_ne!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../../migrations")]
async fn clearing_required_on_a_query_input_reaches_the_call(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore_at(&app, &fake_petstore().await).await;
    let view = petstore_status_view(&app).await;
    let mut body = save_body(&view);
    let inputs = body["draft"]["inputs"].as_array_mut().unwrap();
    inputs
        .iter_mut()
        .find(|row| row["name"] == "status")
        .unwrap()["required"] = json!(false);
    let (status, saved) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let (stored,): (Value,) =
        sqlx::query_as("SELECT request FROM tools WHERE name = 'findPetsByStatus'")
            .fetch_one(&app.state.pool)
            .await
            .unwrap();
    assert_eq!(stored["params"][0]["required"], false, "{stored}");
    let mcp = Mcp::connect(&app).await;
    let result = mcp
        .request(
            "tools/call",
            json!({ "name": "findPetsByStatus", "arguments": {} }),
        )
        .await;
    assert_eq!(result["isError"], false, "{result}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn clearing_required_on_a_path_input_is_refused(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let (_, view) = app
        .json("GET", "/api/servers/pets/tools/getPetById", json!(null))
        .await;
    let mut body = save_body(&view);
    body["draft"]["inputs"][0]["required"] = json!(false);
    let (status, error) = app
        .json("PUT", "/api/servers/pets/tools/getPetById", body)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "path_input_optional");
}

async fn blocked_text(app: &App, base_url: &str, path: &str) -> Value {
    let api = json!({ "kind": "draft", "base_url": base_url, "auth": { "type": "none" } });
    let mut body = test_body("latitude={lat}", api);
    body["draft"]["path"] = json!(path);
    let (status, result) = app.json("POST", "/api/servers/pets/tool-test", body).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    result
}

#[sqlx::test(migrations = "../../migrations")]
async fn name_that_resolves_to_loopback_gives_the_blocked_text(pool: PgPool) {
    let app = App::new(pool, false).await;
    let base = fake_upstream().await.replace("127.0.0.1", "localhost");
    let result = blocked_text(&app, &base, "/forecast").await;
    assert_eq!(result["mcp"]["isError"], true);
    assert_eq!(
        result["mcp"]["content"][0]["text"],
        "the upstream address is not allowed"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn redirect_to_a_blocked_address_gives_the_blocked_text(pool: PgPool) {
    let app = App::new(pool, true).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new().route(
        "/v1/redirect",
        get(|| async {
            (
                StatusCode::FOUND,
                [(header::LOCATION, "http://169.254.169.254/")],
            )
        }),
    );
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let result = blocked_text(&app, &format!("http://{addr}/v1"), "/redirect").await;
    assert_eq!(result["mcp"]["isError"], true);
    assert_eq!(
        result["mcp"]["content"][0]["text"],
        "the upstream address is not allowed"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn fixed_headers_with_credentials_are_refused(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api_id = app.create_api(&fake_upstream().await, SECRET).await;
    for name in [
        "Authorization",
        "proxy-authorization",
        "Cookie",
        "X-Api-Key",
        "X-Access-Token",
        "x-client-secret",
        "X-Auth-User",
    ] {
        let mut draft = forecast_draft();
        draft["headers"] = json!([{ "name": name, "value": "abc" }]);
        let (status, error) = app
            .json(
                "POST",
                "/api/servers/pets/tools",
                json!({ "api_id": api_id, "draft": draft }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
        assert_eq!(error["error"], "secret_in_fixed_header", "{name}");
        assert_eq!(
            error["message"],
            "put credentials in the API auth, not in a fixed header"
        );
    }
    // A header that takes its value from an input is not a fixed header.
    let mut draft = forecast_draft();
    draft["headers"] = json!([{ "name": "X-Api-Key", "value": "{lat}" }]);
    let (status, _) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": draft }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    // A harmless fixed header is fine.
    let mut draft = forecast_draft();
    draft["name"] = json!("second");
    draft["headers"] = json!([{ "name": "Accept-Language", "value": "en" }]);
    let (status, _) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": draft }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

impl Mcp {
    /// Opens the GET stream of the session, which carries server notifications.
    async fn notifications(&self) -> Body {
        // The session is known to the server after its first tools request.
        self.request("tools/list", json!({})).await;
        let request = Request::get("/mcp/pets")
            .header(header::HOST, HOST)
            .header(header::ACCEPT, "text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header("mcp-session-id", &self.session)
            .body(Body::empty())
            .unwrap();
        self.router
            .clone()
            .oneshot(request)
            .await
            .unwrap()
            .into_body()
    }
}

async fn wait_for_list_changed(mut stream: Body) {
    let mut seen = String::new();
    while !seen.contains("notifications/tools/list_changed") {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), stream.frame())
            .await
            .expect("no event in 5 s")
            .expect("stream ended")
            .unwrap();
        if let Some(data) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(data));
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn first_import_of_an_api_notifies_the_open_sessions(pool: PgPool) {
    let app = App::new(pool, true).await;
    let stream = Mcp::connect(&app).await.notifications().await;

    // A new API, not a re-import.
    let body = json!({ "source": "text", "spec": PETSTORE, "base_url": "https://a.example/v3" });
    let (status, imported) = app.json("POST", "/api/servers/pets/apis", body).await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported[0]["replaced"], false);
    wait_for_list_changed(stream).await;
}

fn enabled_path(tool: &str) -> String {
    format!("/api/servers/pets/tools/{tool}/enabled")
}

async fn set_enabled(app: &App, tool: &str, enabled: bool) -> (StatusCode, Value) {
    app.json("PUT", &enabled_path(tool), json!({ "enabled": enabled }))
        .await
}

async fn listed_names(mcp: &Mcp) -> Vec<String> {
    let list = mcp.request("tools/list", json!({})).await;
    list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[sqlx::test(migrations = "../../migrations")]
async fn turning_a_tool_off_and_on_changes_tools_list(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let mcp = Mcp::connect(&app).await;
    assert!(listed_names(&mcp).await.contains(&"getPetById".to_owned()));

    let (status, _) = set_enabled(&app, "getPetById", false).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!listed_names(&mcp).await.contains(&"getPetById".to_owned()));
    let (_, tools) = app
        .json("GET", "/api/servers/pets/tools", json!(null))
        .await;
    let row = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "getPetById")
        .unwrap()
        .clone();
    assert_eq!(row["enabled"], false);

    let (status, _) = set_enabled(&app, "getPetById", true).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(listed_names(&mcp).await.contains(&"getPetById".to_owned()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn turning_a_tool_off_notifies_the_open_sessions(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let stream = Mcp::connect(&app).await.notifications().await;
    let (status, _) = set_enabled(&app, "getPetById", false).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    wait_for_list_changed(stream).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn turning_off_the_last_enabled_tool_is_refused(pool: PgPool) {
    let app = App::new(pool, true).await;
    add_forecast(&app).await;
    let (status, error) = set_enabled(&app, "getForecast", false).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "last_enabled_tool");
    // Turning on a tool that is on already is fine.
    let (status, _) = set_enabled(&app, "getForecast", true).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "../../migrations")]
async fn two_concurrent_turn_offs_leave_one_tool_on(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (api_id, _) = add_forecast(&app).await;
    let mut other = forecast_draft();
    other["name"] = json!("other");
    let (status, _) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": other }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (first, second) = tokio::join!(
        set_enabled(&app, "getForecast", false),
        set_enabled(&app, "other", false),
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::NO_CONTENT, StatusCode::UNPROCESSABLE_ENTITY]
    );
    let on: i64 = sqlx::query_scalar("SELECT count(*) FROM tools WHERE enabled")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(on, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn set_enabled_on_unknown_tool_or_other_users_server_is_not_found(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let (status, error) = set_enabled(&app, "noSuchTool", false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["error"], "not_found");

    let other = common::create_user(&app.state.pool, 2, "other").await;
    let response = app
        .as_user(
            other,
            "PUT",
            &enabled_path("getPetById"),
            json!({ "enabled": false }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let off: i64 = sqlx::query_scalar("SELECT count(*) FROM tools WHERE NOT enabled")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(off, 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn invalid_input_names_are_refused(pool: PgPool) {
    let app = App::new(pool, true).await;
    let api_id = app.create_api(&fake_upstream().await, SECRET).await;
    for name in ["__proto__", "1abc", "a b", "a.b", "", &"a".repeat(65)] {
        let mut draft = forecast_draft();
        draft["query"] = json!("");
        draft["inputs"] = json!([
            { "name": name, "input_type": "string", "required": false, "description": "" }
        ]);
        let (status, error) = app
            .json(
                "POST",
                "/api/servers/pets/tools",
                json!({ "api_id": api_id, "draft": draft }),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{name}");
        assert_eq!(error["error"], "invalid_input_name", "{name}");
    }
    let mut draft = forecast_draft();
    draft["query"] = json!("");
    draft["inputs"] = json!([
        { "name": "_a-1", "input_type": "string", "required": false, "description": "" }
    ]);
    let (status, _) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": draft }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_cannot_touch_tools_or_test_routes(pool: PgPool) {
    let app = App::new(pool, true).await;
    let (api_id, _) = add_forecast(&app).await;
    import_petstore(&app).await;
    let other = common::create_user(&app.state.pool, 2, "other").await;
    let (_, spec_view) = app
        .json("GET", "/api/servers/pets/tools/getPetById", json!(null))
        .await;
    let calls = [
        ("GET", "/api/servers/pets/tools/getForecast", json!(null)),
        (
            "PUT",
            "/api/servers/pets/tools/getForecast",
            json!({ "api_id": api_id, "draft": forecast_draft() }),
        ),
        ("DELETE", "/api/servers/pets/tools/getForecast", json!(null)),
        (
            "POST",
            "/api/servers/pets/tools/getPetById/reset",
            json!(null),
        ),
        (
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api_id, "draft": forecast_draft() }),
        ),
        (
            "POST",
            "/api/servers/pets/tool-test",
            json!({
                "draft": forecast_draft(), "args": { "lat": 1, "lon": 2 },
                "api": { "kind": "saved", "id": api_id },
            }),
        ),
        (
            "POST",
            "/api/servers/pets/tool-test",
            json!({
                "draft": spec_view["draft"], "args": { "petId": 1 },
                "api": { "kind": "saved", "id": spec_view["api_id"] },
                "spec_tool": "getPetById",
            }),
        ),
        ("GET", "/api/servers/pets/apis", json!(null)),
        (
            "POST",
            "/api/servers/pets/apis/manual",
            json!({ "name": "x", "base_url": "https://a.example", "auth": { "type": "none" } }),
        ),
    ];
    for (method, path, body) in calls {
        let response = app.as_user(other, method, path, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    // The route without a server never reads a saved API.
    let body = json!({
        "draft": forecast_draft(), "args": { "lat": 1, "lon": 2 },
        "api": { "kind": "saved", "id": api_id },
    });
    let response = app.as_user(other, "POST", "/api/tool-test", body).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    // The owner's data is unchanged.
    let (status, _) = app
        .json("GET", "/api/servers/pets/tools/getForecast", json!(null))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../../migrations")]
async fn api_id_from_another_server_is_not_found(pool: PgPool) {
    let app = App::new(pool, true).await;
    sqlx::query("INSERT INTO servers (user_id, name) VALUES ($1, 'second')")
        .bind(app.user)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let body = json!({
        "name": "weather", "base_url": "https://a.example/v1", "auth": { "type": "none" },
    });
    let (_, api) = app
        .json("POST", "/api/servers/second/apis/manual", body)
        .await;
    let (status, error) = app
        .json(
            "POST",
            "/api/servers/pets/tools",
            json!({ "api_id": api["id"], "draft": forecast_draft() }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
}

fn detach_path(tool: &str) -> String {
    format!("/api/servers/pets/tools/{tool}/detach")
}

async fn detach(app: &App, tool: &str) -> Value {
    let (status, view) = app.json("POST", &detach_path(tool), json!(null)).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    view
}

#[sqlx::test(migrations = "../../migrations")]
async fn detach_makes_a_spec_tool_manual(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = petstore_status_view(&app).await;
    let mut body = save_body(&view);
    body["draft"]["description"] = json!("My own text");
    let (status, edited) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["edited"], true);

    let detached = detach(&app, "findPetsByStatus").await;
    assert_eq!(detached["source"], "manual");
    assert_eq!(detached["edited"], false);
    assert_eq!(detached["spec_changed"], false);
    assert_eq!(detached["api_id"], view["api_id"]);
    assert_eq!(detached["draft"], edited["draft"]);
    assert_eq!(detached["preview"], edited["preview"]);

    let row: (String, bool, bool, bool) = sqlx::query_as(
        "SELECT source, edited, enabled, (spec_tool IS NULL AND spec_hash IS NULL
                AND edited_hash IS NULL AND operation IS NULL)
         FROM tools WHERE name = 'findPetsByStatus'",
    )
    .fetch_one(&app.state.pool)
    .await
    .unwrap();
    // The spec columns are all NULL.
    assert_eq!(row, ("manual".into(), false, true, true));
}

#[sqlx::test(migrations = "../../migrations")]
async fn detach_refuses_a_manual_tool(pool: PgPool) {
    let app = App::new(pool, true).await;
    add_forecast(&app).await;
    let (status, error) = app
        .json("POST", &detach_path("getForecast"), json!(null))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "not_a_spec_tool");
}

#[sqlx::test(migrations = "../../migrations")]
async fn detach_of_unknown_or_other_users_tool_is_not_found(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let (status, _) = app
        .json("POST", &detach_path("noSuchTool"), json!(null))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let other = common::create_user(&app.state.pool, 2, "other").await;
    let response = app
        .as_user(other, "POST", &detach_path("findPetsByStatus"), json!(null))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(petstore_status_view(&app).await["source"], "spec");
}

#[sqlx::test(migrations = "../../migrations")]
async fn detached_tool_can_change_its_path(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = detach(&app, "getPetById").await;
    let mut body = save_body(&view);
    body["draft"]["path"] = json!("/pets/{petId}/details");
    body["draft"]["method"] = json!("POST");
    let (status, saved) = app
        .json("PUT", "/api/servers/pets/tools/getPetById", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["source"], "manual");
    assert_eq!(saved["draft"]["path"], "/pets/{petId}/details");
    assert_eq!(saved["draft"]["method"], "POST");
    // `petId` has a format, so it is a complex row. Its schema must stay as it was.
    assert_eq!(saved["preview"], view["preview"]);
    let listed = Mcp::connect(&app).await.listed("getPetById").await;
    assert_eq!(listed["inputSchema"], saved["preview"]["inputSchema"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn detached_tool_keeps_enum_input_on_save(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = detach(&app, "findPetsByStatus").await;
    let before = view["preview"]["inputSchema"]["properties"]["status"].clone();
    assert!(before["enum"].is_array(), "{before}");

    let mut body = save_body(&view);
    let rows = body["draft"]["inputs"].as_array_mut().unwrap();
    let row = rows.iter_mut().find(|r| r["name"] == "status").unwrap();
    assert_eq!(row["input_type"], "complex");
    row["description"] = json!("The pet status");
    let (status, saved) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    let after = &saved["preview"]["inputSchema"]["properties"]["status"];
    let mut expected = before.clone();
    expected["description"] = json!("The pet status");
    assert_eq!(after, &expected);
    let (schema,): (Value,) =
        sqlx::query_as("SELECT input_schema FROM tools WHERE name = 'findPetsByStatus'")
            .fetch_one(&app.state.pool)
            .await
            .unwrap();
    assert_eq!(schema, saved["preview"]["inputSchema"]);
    let listed = Mcp::connect(&app).await.listed("findPetsByStatus").await;
    assert_eq!(listed["inputSchema"], schema);
}

#[sqlx::test(migrations = "../../migrations")]
async fn detached_tool_refuses_a_new_complex_row(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = detach(&app, "findPetsByStatus").await;
    let mut body = save_body(&view);
    body["draft"]["inputs"].as_array_mut().unwrap().push(
        json!({ "name": "other", "input_type": "complex", "required": false, "description": "" }),
    );
    let (status, error) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "complex_input_on_manual_tool");
}

#[sqlx::test(migrations = "../../migrations")]
async fn test_route_runs_a_detached_tool(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore_at(&app, &fake_petstore().await).await;
    let view = detach(&app, "getPetById").await;
    let api = json!({ "kind": "saved", "id": view["api_id"] });
    let body = json!({
        "draft": view["draft"], "api": api, "args": { "petId": 7 }, "manual_tool": "getPetById",
    });
    let (status, result) = app
        .json("POST", "/api/servers/pets/tool-test", body.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["mcp"]["isError"], false, "{result}");
    let text = result["mcp"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("rex"), "{text}");

    // Without the stored tool, the complex row has no schema.
    let mut plain = body.clone();
    plain.as_object_mut().unwrap().remove("manual_tool");
    let (status, error) = app.json("POST", "/api/servers/pets/tool-test", plain).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "complex_input_on_manual_tool");
    // A spec tool is not a manual tool.
    let (_, spec) = app
        .json(
            "GET",
            "/api/servers/pets/tools/findPetsByStatus",
            json!(null),
        )
        .await;
    let body = json!({
        "draft": spec["draft"], "api": api, "args": {}, "manual_tool": "findPetsByStatus",
    });
    let (status, _) = app.json("POST", "/api/servers/pets/tool-test", body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_leaves_a_detached_tool_alone(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = detach(&app, "findPetsByStatus").await;

    // The spec still has the operation, so the import adds it again under the same name.
    let error = import_spec(
        &app.state.pool,
        ImportRequest {
            owner: Owner::Login("octocat"),
            server: "pets",
            api_name: None,
            spec_text: PETSTORE,
            base_url: Some("https://petstore.example/api/v3".parse().unwrap()),
            spec_url: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, server::import::ImportError::ToolNameTaken(names) if names == &["findPetsByStatus".to_owned()]),
        "{error}"
    );
    assert_eq!(petstore_status_view(&app).await, view);

    // With a new name, the import adds a new spec tool next to the detached one.
    let mut body = save_body(&view);
    body["draft"]["name"] = json!("myStatus");
    let (status, mine) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{mine}");
    let report = reimport(&app, PETSTORE).await;
    assert!(report.tools.contains(&"findPetsByStatus".to_owned()));
    assert!(!report.tools.contains(&"myStatus".to_owned()));
    let (_, after) = app
        .json("GET", "/api/servers/pets/tools/myStatus", json!(null))
        .await;
    assert_eq!(after, mine);
    assert_eq!(petstore_status_view(&app).await["source"], "spec");
}

#[sqlx::test(migrations = "../../migrations")]
async fn spec_api_can_change_base_url_and_auth_but_not_name(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let api_id = petstore_status_view(&app).await["api_id"].clone();
    let path = format!("/api/servers/pets/apis/{api_id}");
    let (_, apis) = app.json("GET", "/api/servers/pets/apis", json!(null)).await;
    let name = apis[0]["name"].clone();

    let body = json!({
        "name": name, "base_url": "https://other.example/v3",
        "auth": { "type": "bearer", "secret": SECRET },
    });
    let (status, saved) = app.json("PUT", &path, body).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["base_url"], "https://other.example/v3");
    assert_eq!(
        saved["auth"],
        json!({ "type": "bearer", "secret_set": true })
    );
    assert!(!saved.to_string().contains(SECRET));

    let body = json!({ "name": "renamed", "base_url": "https://other.example/v3", "auth": { "type": "none" } });
    let (status, error) = app.json("PUT", &path, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "spec_api_fixed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn detached_tool_can_change_required_of_a_complex_input(pool: PgPool) {
    let app = App::new(pool, true).await;
    import_petstore(&app).await;
    let view = detach(&app, "findPetsByStatus").await;
    let mut body = save_body(&view);
    let rows = body["draft"]["inputs"].as_array_mut().unwrap();
    let row = rows.iter_mut().find(|r| r["name"] == "status").unwrap();
    assert_eq!(row["input_type"], "complex");
    let flipped = !row["required"].as_bool().unwrap();
    row["required"] = json!(flipped);
    let (status, saved) = app
        .json("PUT", "/api/servers/pets/tools/findPetsByStatus", body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let required = saved["preview"]["inputSchema"]["required"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(required.contains(&json!("status")), flipped, "{saved}");
    let listed = Mcp::connect(&app).await.listed("findPetsByStatus").await;
    assert_eq!(listed["inputSchema"], saved["preview"]["inputSchema"]);
}
