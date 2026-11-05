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
    calls::{self, CallResult},
    config::Config,
    import::{ImportRequest, Owner, import_spec},
    mcp_tokens,
    secrets::{SecretsKey, seal},
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
            owner: Owner::Login("octocat"),
            server,
            api_name: None,
            spec_text: spec,
            base_url: Some(base_url),
            spec_url: None,
        },
    )
    .await
    .unwrap();
}

/// A user, the server `pets` with the fake upstream API, and the app.
async fn setup(pool: PgPool, timeout_ms: u64) -> (Router, AppState) {
    setup_with(pool, timeout_ms, true).await
}

async fn setup_with(pool: PgPool, timeout_ms: u64, allow_local: bool) -> (Router, AppState) {
    setup_config(pool, |config| {
        config.upstream_timeout_ms = timeout_ms;
        config.allow_local_upstream = allow_local;
    })
    .await
}

/// The user is on Pro, because the tests use several servers and more tools than Free allows.
async fn setup_config(pool: PgPool, change: impl FnOnce(&mut Config)) -> (Router, AppState) {
    let user = common::create_user(&pool, 1, "octocat").await;
    common::set_plan(&pool, user, "pro").await;
    import(&pool, "pets", UPSTREAM_SPEC, fake_upstream().await).await;
    let mut config = common::config(&common::fake_github().await);
    change(&mut config);
    let state = AppState::new(pool, &config).unwrap();
    (server::app(state.clone()), state)
}

async fn token(state: &AppState, server: &str) -> String {
    let ttl = Duration::from_secs(3600);
    mcp_tokens::create(&state.pool, &state.auth.jwt, server, "test", Some(ttl))
        .await
        .unwrap()
        .value
}

/// The router stores the MCP sessions, so all requests of one test must use the same router.
async fn post_mcp(
    app: &Router,
    path: &str,
    host: &str,
    token: Option<&str>,
    session: Option<&str>,
    body: Value,
) -> Response {
    let mut request = Request::post(path)
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
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
    token: String,
    session: String,
}

impl Client {
    /// Creates a token for the server and opens a session with it.
    async fn connect(app: &Router, state: &AppState, server: &str) -> Self {
        let token = token(state, server).await;
        Self::connect_with(app, server, token).await
    }

    async fn connect_with(app: &Router, server: &str, token: String) -> Self {
        let path = format!("/mcp/{server}");
        let response = post_mcp(
            app,
            &path,
            HOST,
            Some(&token),
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
            Some(&token),
            Some(&session),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        Self {
            app: app.clone(),
            path,
            token,
            session,
        }
    }

    async fn send(&self, method: &str, params: Value) -> Response {
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
        post_mcp(
            &self.app,
            &self.path,
            HOST,
            Some(&self.token),
            Some(&self.session),
            body,
        )
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Value {
        first_message(self.send(method, params).await).await
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
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    let list = client.request("tools/list", json!({})).await;
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["getPet", "createPet", "slow", "big"]);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["id"]));
    assert_eq!(tools[0]["description"], "GET /pets/{id}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn call_fills_the_path_the_query_and_the_headers(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
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
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
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
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    let result = client.call("getPet", json!({ "id": "404" })).await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        text(&result),
        "the upstream API answered HTTP 404 Not Found\n\nno such pet"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn slow_upstream_times_out(pool: PgPool) {
    let (app, state) = setup(pool, 200).await;
    let client = Client::connect(&app, &state, "pets").await;
    let result = client.call("slow", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        text(&result),
        "the upstream API did not answer within 200 ms"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn large_response_is_cut(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
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
    let state = common::state(pool).await;
    let app = server::app(state.clone());

    let client = Client::connect(&app, &state, "pets").await;
    let result = client.call("slow", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(text(&result), "the upstream API cannot be reached");
}

#[sqlx::test(migrations = "../../migrations")]
async fn missing_argument_is_an_invalid_params_error(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    let error = client.call("getPet", json!({})).await;
    assert_eq!(error["code"], -32602);
    assert_eq!(error["message"], "the argument id is required");
}

#[sqlx::test(migrations = "../../migrations")]
async fn dot_dot_path_argument_is_rejected(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    let error = client.call("getPet", json!({ "id": ".." })).await;
    assert_eq!(error["code"], -32602);
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_tool_is_an_invalid_params_error(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    let error = client.call("nope", json!({})).await;
    assert_eq!(error["code"], -32602);
    assert_eq!(error["message"], "the tool nope does not exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn disabled_tool_is_hidden_and_cannot_be_called(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    sqlx::query("UPDATE tools SET enabled = false WHERE name = 'slow'")
        .execute(&pool)
        .await
        .unwrap();
    let client = Client::connect(&app, &state, "pets").await;
    assert_eq!(client.tool_names().await, ["getPet", "createPet", "big"]);
    let error = client.call("slow", json!({})).await;
    assert_eq!(error["message"], "the tool slow does not exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_server_shows_only_its_own_tools(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let other_spec = json!({
        "openapi": "3.0.3",
        "info": { "title": "Other", "version": "1" },
        "paths": { "/ping": { "get": { "operationId": "ping", "responses": {} } } },
    })
    .to_string();
    import(&pool, "other", &other_spec, fake_upstream().await).await;

    let pets = Client::connect(&app, &state, "pets").await;
    let other = Client::connect(&app, &state, "other").await;
    assert_eq!(
        pets.tool_names().await,
        ["getPet", "createPet", "slow", "big"]
    );
    assert_eq!(other.tool_names().await, ["ping"]);
    let error = other.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(error["message"], "the tool getPet does not exist");
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_host_is_rejected(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let token = token(&state, "pets").await;
    let response = post_mcp(
        &app,
        "/mcp/pets",
        "evil.example",
        Some(&token),
        None,
        json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
    )
    .await;
    assert!(response.status().is_client_error(), "{}", response.status());
}

async fn initialize_with(app: &Router, server: &str, token: Option<&str>) -> Response {
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
    post_mcp(app, &format!("/mcp/{server}"), HOST, token, None, body).await
}

async fn assert_unauthorized(response: Response) {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    assert_eq!(common::json_body(response).await["error"], "unauthorized");
}

#[sqlx::test(migrations = "../../migrations")]
async fn request_without_a_token_gets_401(pool: PgPool) {
    let (app, _) = setup(pool, 10_000).await;
    assert_unauthorized(initialize_with(&app, "pets", None).await).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_tokens_get_401(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    assert_unauthorized(initialize_with(&app, "pets", Some("not-a-jwt")).await).await;
    // A dashboard token has another audience.
    let access = state.auth.jwt.issue_access(1).unwrap();
    assert_unauthorized(initialize_with(&app, "pets", Some(&access)).await).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn token_opens_only_its_own_server(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    import(&pool, "other", UPSTREAM_SPEC, fake_upstream().await).await;
    let other_token = token(&state, "other").await;
    assert_unauthorized(initialize_with(&app, "pets", Some(&other_token)).await).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_server_looks_the_same_as_a_bad_token(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let token = token(&state, "pets").await;
    assert_unauthorized(initialize_with(&app, "nope", Some(&token)).await).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn revoke_stops_an_open_session_at_once(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    assert_eq!(client.tool_names().await.len(), 4);
    client.call("getPet", json!({ "id": "1" })).await;

    let tokens = mcp_tokens::list(&pool, "pets").await.unwrap();
    assert!(mcp_tokens::revoke(&pool, tokens[0].id).await.unwrap());
    assert_unauthorized(client.send("tools/list", json!({})).await).await;
    assert!(mcp_tokens::list(&pool, "pets").await.unwrap().is_empty());
    // A second revoke finds no token.
    assert!(!mcp_tokens::revoke(&pool, tokens[0].id).await.unwrap());

    // The Live snapshot and the call log read `calls::recent`. The call keeps the token name.
    let calls = calls::recent(&pool, 1, 10).await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].token_name.as_deref(), Some("test"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn expired_token_row_gets_401(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let token = token(&state, "pets").await;
    sqlx::query("UPDATE mcp_tokens SET expires_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_unauthorized(initialize_with(&app, "pets", Some(&token)).await).await;
}

#[sqlx::test(migrations = "../../migrations")]
async fn use_of_a_token_sets_last_used(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let token = token(&state, "pets").await;
    let before = mcp_tokens::list(&pool, "pets").await.unwrap();
    assert_eq!(before[0].last_used_at, None);
    assert!(!before[0].expired);

    Client::connect_with(&app, "pets", token).await;
    let after = mcp_tokens::list(&pool, "pets").await.unwrap();
    assert!(after[0].last_used_at.is_some());
}

#[sqlx::test(migrations = "../../migrations")]
async fn token_needs_a_known_server_and_a_name(pool: PgPool) {
    let (_, state) = setup(pool.clone(), 10_000).await;
    let ttl = Duration::from_secs(60);
    let keys = &state.auth.jwt;
    let unknown = mcp_tokens::create(&pool, keys, "nope", "x", Some(ttl)).await;
    assert!(matches!(
        unknown,
        Err(mcp_tokens::TokenError::UnknownServer(_))
    ));
    let empty = mcp_tokens::create(&pool, keys, "pets", "  ", Some(ttl)).await;
    assert!(matches!(empty, Err(mcp_tokens::TokenError::EmptyName)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_call_is_saved(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 200).await;
    let client = Client::connect(&app, &state, "pets").await;
    client.call("getPet", json!({ "id": "1" })).await;
    client.call("getPet", json!({ "id": "404" })).await;
    client.call("slow", json!({})).await;
    // A call with bad arguments never reaches the upstream API, so it is not saved.
    client.call("getPet", json!({})).await;

    let calls = calls::recent(&pool, 1, 10).await.unwrap();
    let rows: Vec<_> = calls
        .iter()
        .map(|c| (c.tool.as_str(), c.result, c.http_status))
        .collect();
    assert_eq!(
        rows,
        [
            ("slow", CallResult::TimedOut, None),
            ("getPet", CallResult::NotFound, Some(404)),
            ("getPet", CallResult::Ok, Some(200)),
        ]
    );
    assert_eq!(calls[0].client_name.as_deref(), Some("test"));
    assert_eq!(calls[0].token_name.as_deref(), Some("test"));
    assert!(calls[0].latency_ms >= 200);
}

/// 12 calls per minute give a burst of 2 calls.
async fn setup_rate_limit(pool: PgPool, per_minute: u32) -> (Router, AppState) {
    setup_config(pool, |config| config.rate_limit_per_minute = per_minute).await
}

#[sqlx::test(migrations = "../../migrations")]
async fn calls_over_the_rate_limit_are_refused(pool: PgPool) {
    let (app, state) = setup_rate_limit(pool.clone(), 12).await;
    let client = Client::connect(&app, &state, "pets").await;
    for _ in 0..2 {
        let result = client.call("getPet", json!({ "id": "1" })).await;
        assert_eq!(result["isError"], false);
    }
    let refused = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(refused["isError"], true);
    assert_eq!(
        text(&refused),
        "rate limit exceeded for this token, retry in 5 s"
    );
    // The refused call never reaches the upstream API, so it is not saved.
    assert_eq!(calls::recent(&pool, 1, 10).await.unwrap().len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_token_has_its_own_rate_limit(pool: PgPool) {
    let (app, state) = setup_rate_limit(pool, 6).await;
    let first = Client::connect(&app, &state, "pets").await;
    let second = Client::connect(&app, &state, "pets").await;
    assert_eq!(
        first.call("getPet", json!({ "id": "1" })).await["isError"],
        false
    );
    assert_eq!(
        first.call("getPet", json!({ "id": "1" })).await["isError"],
        true
    );
    assert_eq!(
        second.call("getPet", json!({ "id": "1" })).await["isError"],
        false
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn tokens_of_one_user_share_the_user_rate_limit(pool: PgPool) {
    // The user limit of 12 calls per minute gives a burst of 2 calls.
    let (app, state) = setup_config(pool.clone(), |config| {
        config.user_rate_limit_per_minute = 12;
    })
    .await;
    import(&pool, "pets-two", UPSTREAM_SPEC, fake_upstream().await).await;
    common::create_user(&pool, 2, "hubot").await;
    import_spec(
        &pool,
        ImportRequest {
            owner: Owner::Login("hubot"),
            server: "cats",
            api_name: None,
            spec_text: UPSTREAM_SPEC,
            base_url: Some(fake_upstream().await),
            spec_url: None,
        },
    )
    .await
    .unwrap();

    let first = Client::connect(&app, &state, "pets").await;
    let second = Client::connect(&app, &state, "pets-two").await;
    let other_user = Client::connect(&app, &state, "cats").await;
    for client in [&first, &second] {
        let result = client.call("getPet", json!({ "id": "1" })).await;
        assert_eq!(result["isError"], false);
    }
    for client in [&first, &second] {
        let refused = client.call("getPet", json!({ "id": "1" })).await;
        assert_eq!(refused["isError"], true);
        assert_eq!(
            text(&refused),
            "rate limit exceeded for this account, retry in 5 s"
        );
    }
    for _ in 0..2 {
        let result = other_user.call("getPet", json!({ "id": "1" })).await;
        assert_eq!(result["isError"], false);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn zero_turns_the_rate_limit_off(pool: PgPool) {
    let (app, state) = setup_config(pool, |config| {
        config.rate_limit_per_minute = 0;
        config.user_rate_limit_per_minute = 0;
    })
    .await;
    let client = Client::connect(&app, &state, "pets").await;
    for _ in 0..30 {
        let result = client.call("getPet", json!({ "id": "1" })).await;
        assert_eq!(result["isError"], false);
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn metrics_use_the_last_minute(pool: PgPool) {
    let (_, state) = setup(pool.clone(), 10_000).await;
    token(&state, "pets").await;
    let token_id: i64 = sqlx::query_scalar("SELECT id FROM mcp_tokens")
        .fetch_one(&pool)
        .await
        .unwrap();
    for (latency_ms, result) in [
        (10, CallResult::Ok),
        (20, CallResult::Ok),
        (30, CallResult::Ok),
        (400, CallResult::UpstreamError),
    ] {
        let call = calls::NewCall {
            server_id: 1,
            token_id,
            token_name: "test",
            client_name: None,
            tool: "getPet",
            result,
            http_status: Some(200),
            latency_ms,
        };
        calls::record(&pool, call).await.unwrap();
    }
    // An old call is outside the window and must not count.
    sqlx::query(
        "INSERT INTO calls (server_id, tool, result, latency_ms, created_at)
         VALUES (1, 'old', 'ok', 9999, now() - interval '2 minutes')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let metrics = calls::metrics(&pool, 1).await.unwrap();
    assert_eq!(
        metrics,
        calls::Metrics {
            calls_per_minute: 4,
            error_rate: 0.25,
            p50_ms: Some(20),
            p95_ms: Some(400),
        }
    );
    let buckets = calls::buckets(&pool, 1).await.unwrap();
    assert_eq!(buckets.iter().map(|b| b.calls).sum::<i64>(), 5);
    assert_eq!(buckets.iter().map(|b| b.errors).sum::<i64>(), 1);
    assert!(buckets.iter().all(|b| b.start_ms % 5000 == 0));
}

#[sqlx::test(migrations = "../../migrations")]
async fn empty_window_has_no_latency(pool: PgPool) {
    let (_, _) = setup(pool.clone(), 10_000).await;
    let metrics = calls::metrics(&pool, 1).await.unwrap();
    assert_eq!(metrics.calls_per_minute, 0);
    assert_eq!(metrics.error_rate, 0.0);
    assert_eq!(metrics.p50_ms, None);
}

/// Reads server-sent events from a streaming response body.
struct SseReader {
    body: Body,
    buffer: String,
}

impl SseReader {
    fn new(response: Response) -> Self {
        Self {
            body: response.into_body(),
            buffer: String::new(),
        }
    }

    /// Returns the next event name and its JSON data. Keep-alive comments are skipped.
    async fn next(&mut self) -> (String, Value) {
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let mut name = None;
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(value) = line.strip_prefix("event:") {
                        name = Some(value.trim().to_owned());
                    } else if let Some(value) = line.strip_prefix("data:") {
                        data.push_str(value.trim());
                    }
                }
                if let Some(name) = name {
                    return (name, serde_json::from_str(&data).unwrap());
                }
            }
            let frame = tokio::time::timeout(Duration::from_secs(5), self.body.frame())
                .await
                .expect("no SSE event within 5 s")
                .expect("the stream ended")
                .unwrap();
            if let Ok(bytes) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&bytes).unwrap());
            }
        }
    }
}

async fn open_live(app: &Router, server: &str, access: Option<&str>) -> Response {
    let mut request = Request::get(format!("/api/servers/{server}/live"));
    if let Some(access) = access {
        request = request.header(header::AUTHORIZATION, format!("Bearer {access}"));
    }
    app.clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_stream_sends_a_snapshot_and_then_new_calls(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    client.call("getPet", json!({ "id": "1" })).await;

    let access = state.auth.jwt.issue_access(1).unwrap();
    let response = open_live(&app, "pets", Some(&access)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut events = SseReader::new(response);

    let (name, snapshot) = events.next().await;
    assert_eq!(name, "snapshot");
    assert_eq!(snapshot["calls"][0]["tool"], "getPet");
    assert_eq!(snapshot["metrics"]["calls_per_minute"], 1);
    assert_eq!(snapshot["clients"][0]["client_name"], "test");
    assert_eq!(snapshot["clients"][0]["token_name"], "test");
    let tokens = mcp_tokens::list(&state.pool, "pets").await.unwrap();
    assert_eq!(snapshot["clients"][0]["token_id"], tokens[0].id);

    client.call("getPet", json!({ "id": "404" })).await;
    let (name, call) = events.next().await;
    assert_eq!(name, "call");
    assert_eq!(call["result"], "not_found");
    assert_eq!(call["http_status"], 404);
}

#[sqlx::test(migrations = "../../migrations")]
async fn live_stream_needs_the_owner(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let response = open_live(&app, "pets", None).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let other_user = common::create_user(&pool, 2, "mallory").await;
    let access = state.auth.jwt.issue_access(other_user).unwrap();
    let response = open_live(&app, "pets", Some(&access)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

const ECHO_SPEC: &str = r#"{
  "openapi": "3.0.3",
  "info": { "title": "Echo", "version": "1" },
  "paths": { "/whoami": { "get": { "operationId": "whoami", "responses": {} } } }
}"#;

/// A fake upstream that tells whether the two auth headers have the expected values.
/// It must not echo them, because ToolPort masks the secret in responses.
async fn echo_upstream() -> Url {
    let app = Router::new().route(
        "/v1/whoami",
        get(|headers: HeaderMap| async move {
            let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
            Json(json!({
                "bearer_ok": get("authorization") == Some("Bearer t0ken"),
                "key_ok": get("x-api-key") == Some("k3y"),
                "has_authorization": get("authorization").is_some(),
            }))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Url::parse(&format!("http://{addr}/v1")).unwrap()
}

/// Adds the server `echo` and seals `secret` into its API with the given auth settings.
async fn echo_with_auth(state: &AppState, auth_type: &str, header: Option<&str>, secret: &str) {
    import(&state.pool, "echo", ECHO_SPEC, echo_upstream().await).await;
    let (api_id,): (i64,) = sqlx::query_as(
        "SELECT a.id FROM apis a JOIN servers s ON s.id = a.server_id WHERE s.name = 'echo'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let key = SecretsKey::from_hex(&"0".repeat(64)).unwrap();
    let sealed = seal(&key, api_id, secret).unwrap();
    sqlx::query("UPDATE apis SET auth_type = $1, auth_header = $2, auth_secret = $3 WHERE id = $4")
        .bind(auth_type)
        .bind(header)
        .bind(sealed)
        .bind(api_id)
        .execute(&state.pool)
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn bearer_auth_reaches_upstream(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    echo_with_auth(&state, "bearer", None, "t0ken").await;
    let client = Client::connect(&app, &state, "echo").await;
    let result = client.call("whoami", json!({})).await;
    let answer: Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(answer["bearer_ok"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn api_key_auth_reaches_upstream(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    echo_with_auth(&state, "api_key", Some("X-Api-Key"), "k3y").await;
    let client = Client::connect(&app, &state, "echo").await;
    let result = client.call("whoami", json!({})).await;
    let answer: Value = serde_json::from_str(text(&result)).unwrap();
    assert_eq!(answer["key_ok"], true);
    assert_eq!(answer["has_authorization"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn blocked_base_url_gives_a_tool_error(pool: PgPool) {
    let (app, state) = setup_with(pool, 10_000, false).await;
    let client = Client::connect(&app, &state, "pets").await;
    let result = client.call("slow", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(text(&result), "the upstream address is not allowed");
    let (saved,): (String,) = sqlx::query_as("SELECT result FROM calls")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(saved, "upstream_error");
}

#[sqlx::test(migrations = "../../migrations")]
async fn list_changed_reaches_an_open_session(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    // The session is known to the server after its first tools request.
    client.tool_names().await;

    let request = Request::get(&client.path)
        .header(header::HOST, HOST)
        .header(header::ACCEPT, "text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", client.token))
        .header(SESSION_HEADER, &client.session)
        .body(Body::empty())
        .unwrap();
    let mut stream = app.clone().oneshot(request).await.unwrap().into_body();
    let server_id: i64 = sqlx::query_scalar("SELECT id FROM servers WHERE name = 'pets'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    state.peers.notify_tools_changed(server_id).await;

    let mut seen = String::new();
    while !seen.contains("notifications/tools/list_changed") {
        let frame = tokio::time::timeout(Duration::from_secs(5), stream.frame())
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
async fn first_request_sends_a_client_event(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let access = state.auth.jwt.issue_access(1).unwrap();
    let mut events = SseReader::new(open_live(&app, "pets", Some(&access)).await);
    assert_eq!(events.next().await.0, "snapshot");

    let client = Client::connect(&app, &state, "pets").await;
    client.tool_names().await;
    let (name, first) = events.next().await;
    assert_eq!(name, "client");
    assert_eq!(first["method"], "tools/list");
    assert_eq!(first["tools_sent"], 4);
    assert_eq!(first["client_name"], "test");
    assert_eq!(first["token_name"], "test");
    let tokens = mcp_tokens::list(&state.pool, "pets").await.unwrap();
    assert_eq!(first["token_id"], tokens[0].id);

    // The same client again sends no event, so the next event is the call.
    client.tool_names().await;
    client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(events.next().await.0, "call");
}

#[sqlx::test(migrations = "../../migrations")]
async fn first_request_as_a_tool_call_has_no_tool_count(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let access = state.auth.jwt.issue_access(1).unwrap();
    let mut events = SseReader::new(open_live(&app, "pets", Some(&access)).await);
    events.next().await;

    let client = Client::connect(&app, &state, "pets").await;
    client.call("getPet", json!({ "id": "1" })).await;
    let (name, first) = events.next().await;
    assert_eq!(name, "client");
    assert_eq!(first["method"], "tools/call");
    assert!(first.get("tools_sent").is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn client_event_goes_only_to_its_own_server(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    import(&pool, "other", UPSTREAM_SPEC, fake_upstream().await).await;
    let access = state.auth.jwt.issue_access(1).unwrap();
    let mut events = SseReader::new(open_live(&app, "other", Some(&access)).await);
    events.next().await;

    let ttl = Duration::from_secs(3600);
    let keys = &state.auth.jwt;
    let pets_token = mcp_tokens::create(&pool, keys, "pets", "pets-token", Some(ttl))
        .await
        .unwrap()
        .value;
    let other_token = mcp_tokens::create(&pool, keys, "other", "other-token", Some(ttl))
        .await
        .unwrap()
        .value;

    let pets = Client::connect_with(&app, "pets", pets_token).await;
    pets.tool_names().await;
    let other = Client::connect_with(&app, "other", other_token).await;
    other.tool_names().await;
    // The pets event must not arrive here, so the first event is the one of `other`.
    let (name, first) = events.next().await;
    assert_eq!(name, "client");
    assert_eq!(first["token_name"], "other-token");
}

/// Sends a dashboard request as user 1, the owner of `pets`.
async fn dashboard(
    app: &Router,
    state: &AppState,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Response {
    let access = state.auth.jwt.issue_access(1).unwrap();
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .header(header::CONTENT_TYPE, "application/json");
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    app.clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap()
}

async fn has_session(state: &AppState, session: &str) -> bool {
    state
        .mcp_sessions
        .sessions
        .read()
        .await
        .contains_key(session)
}

#[sqlx::test(migrations = "../../migrations")]
async fn server_list_counts_connected_clients_and_the_last_call(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    client.call("getPet", json!({ "id": "1" })).await;

    let response = dashboard(&app, &state, "GET", "/api/servers", None).await;
    let servers = common::json_body(response).await;
    assert_eq!(servers[0]["connected_clients"], 1);
    assert!(servers[0]["last_call_ms"].as_i64().unwrap() > 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rename_moves_the_mcp_url(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    assert!(has_session(&state, &client.session).await);

    let response = dashboard(
        &app,
        &state,
        "PUT",
        "/api/servers/pets",
        Some(json!({ "name": "animals" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    // The old URL looks like an unknown server, and the old session is closed.
    assert_unauthorized(client.send("tools/list", json!({})).await).await;
    assert_unauthorized(initialize_with(&app, "pets", Some(&client.token)).await).await;
    assert!(!has_session(&state, &client.session).await);

    // The same token works on the new URL.
    let moved = Client::connect_with(&app, "animals", client.token.clone()).await;
    assert_eq!(moved.tool_names().await.len(), 4);
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_stops_the_tokens_and_closes_the_sessions(pool: PgPool) {
    let (app, state) = setup(pool, 10_000).await;
    let client = Client::connect(&app, &state, "pets").await;
    client.tool_names().await;

    let response = dashboard(&app, &state, "DELETE", "/api/servers/pets", None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    assert!(!has_session(&state, &client.session).await);
    assert_unauthorized(client.send("tools/list", json!({})).await).await;
    assert_unauthorized(initialize_with(&app, "pets", Some(&client.token)).await).await;

    // A new server with the old name does not accept the old token.
    let response = dashboard(
        &app,
        &state,
        "POST",
        "/api/servers",
        Some(json!({ "name": "pets" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_unauthorized(initialize_with(&app, "pets", Some(&client.token)).await).await;

    let response = dashboard(&app, &state, "GET", "/api/servers", None).await;
    assert_eq!(common::json_body(response).await[0]["connected_clients"], 0);
}

async fn octocat(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT id FROM users WHERE github_login = 'octocat'")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The calls of the user in the current quota period.
async fn usage(state: &AppState, user: i64) -> i64 {
    let account = server::plan::account_plan(&state.pool, user)
        .await
        .unwrap()
        .unwrap();
    server::plan::calls_used(&state.pool, user, &account)
        .await
        .unwrap()
}

async fn set_usage(pool: &PgPool, user: i64, period_start: &str, calls: i32) {
    sqlx::query(
        "INSERT INTO usage (user_id, period_start, calls) VALUES ($1, $2::timestamptz, $3)
         ON CONFLICT (user_id, period_start) DO UPDATE SET calls = EXCLUDED.calls",
    )
    .bind(user)
    .bind(period_start)
    .bind(calls)
    .execute(pool)
    .await
    .unwrap();
}

macro_rules! this_month {
    () => {
        "date_trunc('month', now(), 'UTC')"
    };
}

async fn this_month(pool: &PgPool) -> String {
    sqlx::query_scalar(concat!("SELECT ", this_month!(), "::text"))
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn saved_calls(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM calls")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A move to Free pauses the enabled tools after the first 3 by id. Pro turns them on again.
#[sqlx::test(migrations = "../../migrations")]
async fn free_plan_serves_the_first_three_tools(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let user = octocat(&pool).await;
    common::set_plan(&pool, user, "free").await;
    let client = Client::connect(&app, &state, "pets").await;
    assert_eq!(client.tool_names().await, ["getPet", "createPet", "slow"]);

    let result = client.call("big", json!({})).await;
    assert_eq!(result["isError"], true);
    assert_eq!(
        text(&result),
        "the tool big is paused: the Free plan allows 3 enabled tools per server. Upgrade or turn off another tool."
    );
    // A paused tool does not reach the upstream, so it is not logged and not counted.
    assert_eq!(saved_calls(&pool).await, 0);
    assert_eq!(usage(&state, user).await, 0);

    // Pro serves all tools again.
    common::set_plan(&pool, user, "pro").await;
    assert_eq!(
        client.tool_names().await,
        ["getPet", "createPet", "slow", "big"]
    );
}

/// A manual edit of the plans table sends no webhook. The hourly reconcile applies it.
#[sqlx::test(migrations = "../../migrations")]
async fn a_change_in_the_plans_table_applies_after_the_reconcile(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    common::set_plan(&pool, octocat(&pool).await, "free").await;
    let client = Client::connect(&app, &state, "pets").await;
    assert_eq!(client.tool_names().await.len(), 3);
    sqlx::query("UPDATE plans SET max_tools_per_server = 2 WHERE code = 'free'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(client.tool_names().await.len(), 3);
    assert_eq!(server::plan::reconcile_all(&state).await.unwrap(), 1);
    assert_eq!(client.tool_names().await, ["getPet", "createPet"]);
    // The next run finds nothing to do.
    assert_eq!(server::plan::reconcile_all(&state).await.unwrap(), 0);
}

/// Only the oldest servers of the account serve MCP on a smaller plan.
#[sqlx::test(migrations = "../../migrations")]
async fn server_over_the_plan_limit_is_paused(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    import(&pool, "pets-two", UPSTREAM_SPEC, fake_upstream().await).await;
    let first = Client::connect(&app, &state, "pets").await;
    let second = Client::connect(&app, &state, "pets-two").await;
    common::set_plan(&pool, octocat(&pool).await, "free").await;

    assert_eq!(first.tool_names().await.len(), 3);
    let message =
        "this server is paused: the Free plan allows 1 server. Upgrade or delete a server.";
    let list = second.request("tools/list", json!({})).await;
    assert_eq!(list["error"]["message"], message);
    assert_eq!(list["error"]["data"]["reason"], "server_paused");
    let call = second.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(call["message"], message);
    assert_eq!(saved_calls(&pool).await, 0);

    // A delete of the older server lets the newer one serve again.
    sqlx::query("DELETE FROM servers WHERE name = 'pets'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(second.tool_names().await.len(), 3);
}

/// The 100th call of the month passes. The 101st is refused and is not counted.
#[sqlx::test(migrations = "../../migrations")]
async fn monthly_quota_stops_at_the_limit(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let user = octocat(&pool).await;
    common::set_plan(&pool, user, "free").await;
    set_usage(&pool, user, &this_month(&pool).await, 99).await;
    let client = Client::connect(&app, &state, "pets").await;

    let result = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(result["isError"], false);
    assert_eq!(usage(&state, user).await, 100);

    let refused = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(refused["isError"], true);
    let reset_day: String = sqlx::query_scalar(concat!(
        "SELECT to_char((",
        this_month!(),
        " AT TIME ZONE 'UTC' + interval '1 month'), 'YYYY-MM-DD')"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        text(&refused),
        format!("monthly tool call limit reached (100 on the Free plan), resets on {reset_day}")
    );
    assert_eq!(usage(&state, user).await, 100);
    assert_eq!(saved_calls(&pool).await, 1);
}

/// The count of last month does not stop calls in the new month.
#[sqlx::test(migrations = "../../migrations")]
async fn monthly_quota_resets_at_the_period_start(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let user = octocat(&pool).await;
    common::set_plan(&pool, user, "free").await;
    let last_month: String = sqlx::query_scalar(concat!(
        "SELECT ((",
        this_month!(),
        " AT TIME ZONE 'UTC' - interval '1 month') AT TIME ZONE 'UTC')::text"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    set_usage(&pool, user, &last_month, 100).await;
    let client = Client::connect(&app, &state, "pets").await;
    let result = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(result["isError"], false);
    assert_eq!(usage(&state, user).await, 1);
}

/// A paid plan counts per billing period. An upgrade in the month starts a new count.
#[sqlx::test(migrations = "../../migrations")]
async fn paid_plan_counts_per_billing_period(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let user = octocat(&pool).await;
    set_usage(&pool, user, &this_month(&pool).await, 100).await;
    sqlx::query(
        "UPDATE users SET current_period_start = date_trunc('second', now()) - interval '1 day',
                          current_period_end = date_trunc('second', now()) + interval '29 days'
         WHERE id = $1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE plans SET max_calls_per_month = 1 WHERE code = 'pro'")
        .execute(&pool)
        .await
        .unwrap();
    let client = Client::connect(&app, &state, "pets").await;
    assert_eq!(
        client.call("getPet", json!({ "id": "1" })).await["isError"],
        false
    );
    let refused = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(refused["isError"], true);
    assert!(text(&refused).contains("(1 on the Pro plan)"), "{refused}");

    // A period that ended without a webhook moves forward by one month, so the count resets.
    sqlx::query(
        "UPDATE users SET current_period_start = date_trunc('second', now()) - interval '40 days',
                          current_period_end = date_trunc('second', now()) - interval '10 days'
         WHERE id = $1",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        client.call("getPet", json!({ "id": "1" })).await["isError"],
        false
    );
}

/// Calls that do not reach the upstream API do not use the quota.
#[sqlx::test(migrations = "../../migrations")]
async fn refused_calls_do_not_count(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let user = octocat(&pool).await;
    common::set_plan(&pool, user, "free").await;
    set_usage(&pool, user, &this_month(&pool).await, 99).await;
    let client = Client::connect(&app, &state, "pets").await;

    let unknown = client.call("nope", json!({})).await;
    assert!(unknown["message"].is_string(), "{unknown}");
    let missing_arg = client.call("getPet", json!({})).await;
    assert!(missing_arg["message"].is_string(), "{missing_arg}");
    assert_eq!(usage(&state, user).await, 99);

    // The last place of the quota is still free.
    let result = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(result["isError"], false);
    assert_eq!(usage(&state, user).await, 100);
}

#[sqlx::test(migrations = "../../migrations")]
async fn rate_limited_calls_do_not_count(pool: PgPool) {
    // 12 calls per minute give a burst of 2 calls.
    let (app, state) = setup_rate_limit(pool.clone(), 12).await;
    let user = octocat(&pool).await;
    let client = Client::connect(&app, &state, "pets").await;
    for _ in 0..2 {
        let result = client.call("getPet", json!({ "id": "1" })).await;
        assert_eq!(result["isError"], false);
    }
    let refused = client.call("getPet", json!({ "id": "1" })).await;
    assert_eq!(refused["isError"], true);
    assert_eq!(usage(&state, user).await, 2);
}

/// `plan_changed` is the function that the Stripe webhook calls after a plan change.
#[sqlx::test(migrations = "../../migrations")]
async fn plan_change_notifies_the_open_sessions(pool: PgPool) {
    let (app, state) = setup(pool.clone(), 10_000).await;
    let user = octocat(&pool).await;
    let client = Client::connect(&app, &state, "pets").await;
    client.tool_names().await;
    let request = Request::get("/mcp/pets")
        .header(header::HOST, HOST)
        .header(header::ACCEPT, "text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", client.token))
        .header(SESSION_HEADER, &client.session)
        .body(Body::empty())
        .unwrap();
    let mut stream = app.clone().oneshot(request).await.unwrap().into_body();

    common::set_plan(&pool, user, "free").await;
    server::plan::plan_changed(&state, user).await.unwrap();
    let mut seen = String::new();
    while !seen.contains("notifications/tools/list_changed") {
        let frame = tokio::time::timeout(Duration::from_secs(5), stream.frame())
            .await
            .expect("no event in 5 s")
            .expect("stream ended")
            .unwrap();
        if let Some(data) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(data));
        }
    }
}

/// A Stripe webhook that ends the subscription moves the user to Free and tells the clients.
#[sqlx::test(migrations = "../../migrations")]
async fn webhook_plan_change_sends_list_changed(pool: PgPool) {
    let (stripe_url, stripe) = common::fake_stripe().await;
    let (app, state) = setup_config(pool.clone(), |config| {
        common::with_stripe(config, &stripe_url);
    })
    .await;
    let user = octocat(&pool).await;
    sqlx::query("UPDATE users SET stripe_customer_id = 'cus_test_1' WHERE id = $1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    let ended = common::subscription_json(
        "sub_1",
        "cus_test_1",
        "canceled",
        common::PRO_LOOKUP_KEY,
        (1_780_000_000, 1_782_592_000),
        false,
    );
    stripe
        .lock()
        .unwrap()
        .subscriptions
        .insert("sub_1".into(), ended.clone());

    let client = Client::connect(&app, &state, "pets").await;
    client.tool_names().await;
    let request = Request::get("/mcp/pets")
        .header(header::HOST, HOST)
        .header(header::ACCEPT, "text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {}", client.token))
        .header(SESSION_HEADER, &client.session)
        .body(Body::empty())
        .unwrap();
    let mut stream = app.clone().oneshot(request).await.unwrap().into_body();

    let payload = common::stripe_event("evt_1", "customer.subscription.deleted", ended);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let webhook = common::signed_webhook(&payload, common::WEBHOOK_SECRET, now);
    let response = app.clone().oneshot(webhook).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut seen = String::new();
    while !seen.contains("notifications/tools/list_changed") {
        let frame = tokio::time::timeout(Duration::from_secs(5), stream.frame())
            .await
            .expect("no event in 5 s")
            .expect("stream ended")
            .unwrap();
        if let Some(data) = frame.data_ref() {
            seen.push_str(&String::from_utf8_lossy(data));
        }
    }
    // The downgrade paused the 4th tool.
    assert_eq!(client.tool_names().await, ["getPet", "createPet", "slow"]);
}
