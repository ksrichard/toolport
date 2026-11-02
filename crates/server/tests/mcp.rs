mod common;

use std::time::Duration;

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, RawQuery},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use reqwest::Url;
use serde_json::{Value, json};
use server::{
    import::{ImportRequest, import_spec},
    state::AppState,
};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

const SESSION_HEADER: &str = "mcp-session-id";
const HOST: &str = "toolport.test";

/// The spec of the fake upstream API below. It is a raw string, because `json!` sorts the paths.
const UPSTREAM_SPEC: &str = r#"{
  "openapi": "3.0.3",
  "info": { "title": "Fake pets", "version": "1" },
  "paths": {
    "/pets/{id}": { "get": {
      "operationId": "getPet",
      "parameters": [
        { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } },
        { "name": "fields", "in": "query", "schema": { "type": "array", "items": { "type": "string" } } },
        { "name": "X-Trace", "in": "header", "schema": { "type": "string" } }
      ],
      "responses": {}
    } },
    "/pets": { "post": {
      "operationId": "createPet",
      "requestBody": {
        "required": true,
        "content": { "application/json": { "schema": { "type": "object" } } }
      },
      "responses": {}
    } },
    "/slow": { "get": { "operationId": "slow", "responses": {} } },
    "/big": { "get": { "operationId": "big", "responses": {} } }
  }
}"#;

/// Starts the fake upstream API on a free port. Its base URL has a path, as many real APIs do.
async fn fake_upstream() -> Url {
    let app = Router::new()
        .route(
            "/v1/pets/{id}",
            get(
                |Path(id): Path<String>, RawQuery(query): RawQuery, headers: HeaderMap| async move {
                    if id == "404" {
                        return (StatusCode::NOT_FOUND, "no such pet").into_response();
                    }
                    let trace = headers.get("x-trace").and_then(|v| v.to_str().ok());
                    Json(json!({ "id": id, "query": query, "trace": trace })).into_response()
                },
            ),
        )
        .route(
            "/v1/pets",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                let content_type = headers[header::CONTENT_TYPE].to_str().unwrap().to_owned();
                Json(json!({ "created": body, "content_type": content_type }))
            }),
        )
        .route(
            "/v1/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "too late"
            }),
        )
        .route("/v1/big", get(|| async { "a".repeat(2 * 1024 * 1024) }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{addr}/v1")).unwrap()
}

async fn import(pool: &PgPool, server: &str, spec: &str, base_url: Url) {
    import_spec(
        pool,
        ImportRequest {
            owner_login: "octocat",
            server,
            api_name: None,
            spec_text: spec,
            base_url: Some(base_url),
        },
    )
    .await
    .unwrap();
}

/// A user, the server `pets` with the fake upstream API, and the app.
async fn setup(pool: PgPool, timeout_ms: u64) -> Router {
    common::create_user(&pool, 1, "octocat").await;
    import(&pool, "pets", UPSTREAM_SPEC, fake_upstream().await).await;
    let mut config = common::config(&common::fake_github().await);
    config.upstream_timeout_ms = timeout_ms;
    server::app(AppState::new(pool, &config).unwrap())
}

/// The MCP sessions live in the router, so all requests of one test must use the same router.
async fn post_mcp(
    app: &Router,
    path: &str,
    host: &str,
    session: Option<&str>,
    body: Value,
) -> Response {
    let mut request = Request::post(path)
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(session) = session {
        request = request.header(SESSION_HEADER, session);
    }
    app.clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// Returns the first JSON-RPC message in an SSE response body.
async fn first_message(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    text.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .map(|data| serde_json::from_str(data).unwrap())
        .unwrap_or_else(|| panic!("no SSE message in: {text}"))
}

/// One MCP client session on one server path.
struct Client {
    app: Router,
    path: String,
    session: String,
}

impl Client {
    async fn connect(app: &Router, server: &str) -> Self {
        let path = format!("/mcp/{server}");
        let response = post_mcp(
            app,
            &path,
            HOST,
            None,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" },
                },
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session = response.headers()[SESSION_HEADER]
            .to_str()
            .unwrap()
            .to_owned();
        let init = first_message(response).await;
        assert_eq!(init["result"]["serverInfo"]["name"], "toolport");

        let response = post_mcp(
            app,
            &path,
            HOST,
            Some(&session),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        Self {
            app: app.clone(),
            path,
            session,
        }
    }

    async fn request(&self, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
        let response = post_mcp(&self.app, &self.path, HOST, Some(&self.session), body).await;
        first_message(response).await
    }

    async fn tool_names(&self) -> Vec<String> {
        let list = self.request("tools/list", json!({})).await;
        list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Returns the `result` of a tool call, or the JSON-RPC `error`.
    async fn call(&self, tool: &str, arguments: Value) -> Value {
        let message = self
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
            )
            .await;
        message
            .get("result")
            .or(message.get("error"))
            .unwrap()
            .clone()
    }
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn tools_list_shows_the_server_tools_in_spec_order(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let list = client.request("tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["getPet", "createPet", "slow", "big"]);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["id"]));
    assert_eq!(tools[0]["description"], "GET /pets/{id}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn call_fills_the_path_the_query_and_the_headers(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let result = client
        .call(
            "getPet",
            json!({ "id": "a b/c", "fields": ["x", "y"], "X-Trace": "t1" }),
        )
        .await;
    assert_eq!(result["isError"], false);
    let answer: Value = serde_json::from_str(text(&result)).unwrap();
    // The `/` stays inside the one path segment, so the upstream route still matches.
    assert_eq!(
        answer,
        json!({ "id": "a b/c", "query": "fields=x&fields=y", "trace": "t1" })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn call_sends_the_json_body(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let result = client
        .call("createPet", json!({ "body": { "name": "Rex" } }))
        .await;
    let answer: Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(
        answer,
        json!({ "created": { "name": "Rex" }, "content_type": "application/json" })
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn upstream_error_status_is_a_tool_error(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let result = client.call("getPet", json!({ "id": "404" })).await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        text(&result),
        "the upstream API answered HTTP 404 Not Found\n\nno such pet"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn slow_upstream_times_out(pool: PgPool) {
    let app = setup(pool, 200).await;
    let client = Client::connect(&app, "pets").await;
    let result = client.call("slow", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        text(&result),
        "the upstream API did not answer within 200 ms"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn large_response_is_cut(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let result = client.call("big", json!({})).await;
    assert_eq!(result["isError"], false);
    let body = text(&result);
    let expected = format!(
        "{}\n\n[ToolPort cut this response at 1 MiB]",
        "a".repeat(1024 * 1024)
    );
    assert!(body == expected, "the body has {} bytes", body.len());
}

#[sqlx::test(migrations = "../../migrations")]
async fn unreachable_upstream_is_a_tool_error(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    // Bind and drop a listener to get a local port that nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let base_url = Url::parse(&format!("http://127.0.0.1:{port}/v1")).unwrap();
    import(&pool, "pets", UPSTREAM_SPEC, base_url).await;
    let app = server::app(common::state(pool).await);

    let client = Client::connect(&app, "pets").await;
    let result = client.call("slow", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(text(&result), "the upstream API cannot be reached");
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_argument_is_an_invalid_params_error(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let error = client.call("getPet", json!({})).await;
    assert_eq!(error["code"], -32602);
    assert_eq!(error["message"], "the argument id is required");
}

#[sqlx::test(migrations = "../../migrations")]
async fn dot_dot_path_argument_is_rejected(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let error = client.call("getPet", json!({ "id": ".." })).await;
    assert_eq!(error["code"], -32602);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_tool_is_an_invalid_params_error(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let client = Client::connect(&app, "pets").await;
    let error = client.call("nope", json!({})).await;
    assert_eq!(error["code"], -32602);
    assert_eq!(error["message"], "the tool nope does not exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn disabled_tool_is_hidden_and_cannot_be_called(pool: PgPool) {
    let app = setup(pool.clone(), 10_000).await;
    sqlx::query("UPDATE tools SET enabled = false WHERE name = 'slow'")
        .execute(&pool)
        .await
        .unwrap();
    let client = Client::connect(&app, "pets").await;
    assert_eq!(client.tool_names().await, ["getPet", "createPet", "big"]);
    let error = client.call("slow", json!({})).await;
    assert_eq!(error["message"], "the tool slow does not exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_server_shows_only_its_own_tools(pool: PgPool) {
    let app = setup(pool.clone(), 10_000).await;
    let other_spec = json!({
        "openapi": "3.0.3",
        "info": { "title": "Other", "version": "1" },
        "paths": { "/ping": { "get": { "operationId": "ping", "responses": {} } } },
    })
    .to_string();
    import(&pool, "other", &other_spec, fake_upstream().await).await;

    let pets = Client::connect(&app, "pets").await;
    let other = Client::connect(&app, "other").await;
    assert_eq!(
        pets.tool_names().await,
        ["getPet", "createPet", "slow", "big"]
    );
    assert_eq!(other.tool_names().await, ["ping"]);
    let error = other.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(error["message"], "the tool getPet does not exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_server_gives_a_json_404(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let response = post_mcp(
        &app,
        "/mcp/nope",
        HOST,
        None,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        common::json_body(response).await["error"],
        "mcp_server_not_found"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_host_is_rejected(pool: PgPool) {
    let app = setup(pool, 10_000).await;
    let response = post_mcp(
        &app,
        "/mcp/pets",
        "evil.example",
        None,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .await;
    assert!(response.status().is_client_error(), "{}", response.status());
}
