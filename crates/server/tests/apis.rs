mod common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use server::{spec_check::check_due_apis, state::AppState};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// A spec with one GET operation per name, at `/things/<name>`.
fn spec(ops: &[&str]) -> String {
    titled_spec("Things", ops)
}

/// The title is the API name, so two APIs on one server need two titles.
fn titled_spec(title: &str, ops: &[&str]) -> String {
    let paths: serde_json::Map<String, Value> = ops
        .iter()
        .map(|op| {
            let operation = json!({ "get": {
                "operationId": op, "summary": format!("Runs {op}"),
                "responses": { "200": { "description": "ok" } },
            }});
            (format!("/things/{op}"), operation)
        })
        .collect();
    json!({
        "openapi": "3.0.3",
        "info": { "title": title, "version": "1" },
        "servers": [{ "url": "https://things.example/v1" }],
        "paths": paths,
    })
    .to_string()
}

type Specs = Arc<Mutex<HashMap<String, (StatusCode, String)>>>;

/// Serves specs at `/<name>`. A test changes them while the host runs.
struct SpecHost {
    base: String,
    specs: Specs,
}

impl SpecHost {
    async fn start() -> Self {
        let specs: Specs = Arc::default();
        let app = Router::new()
            .route(
                "/{name}",
                get(
                    |State(specs): State<Specs>, Path(name): Path<String>| async move {
                        let found = specs.lock().unwrap().get(&name).cloned();
                        match found {
                            Some((status, body)) => (status, body).into_response(),
                            None => StatusCode::NOT_FOUND.into_response(),
                        }
                    },
                ),
            )
            .with_state(specs.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base: format!("http://{addr}"),
            specs,
        }
    }

    fn set(&self, name: &str, status: StatusCode, body: &str) {
        self.specs
            .lock()
            .unwrap()
            .insert(name.to_owned(), (status, body.to_owned()));
    }

    fn url(&self, name: &str) -> String {
        format!("{}/{name}", self.base)
    }
}

struct App {
    router: Router,
    state: AppState,
    user: i64,
}

impl App {
    async fn new(pool: PgPool) -> Self {
        let user = common::create_user(&pool, 1, "octocat").await;
        let state = common::state(pool).await;
        sqlx::query("INSERT INTO servers (user_id, name) VALUES ($1, 'things')")
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

    async fn json(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let response = self.as_user(self.user, method, path, body).await;
        let status = response.status();
        let bytes = BodyExt::collect(response.into_body())
            .await
            .unwrap()
            .to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn import_url(&self, url: &str) -> Value {
        let body = json!({ "source": "url", "spec_url": url });
        let (status, imported) = self.json("POST", "/api/servers/things/apis", body).await;
        assert_eq!(status, StatusCode::OK, "{imported}");
        imported[0].clone()
    }

    async fn import_text(&self, text: &str) -> Value {
        let body = json!({ "source": "text", "spec": text });
        let (status, imported) = self.json("POST", "/api/servers/things/apis", body).await;
        assert_eq!(status, StatusCode::OK, "{imported}");
        imported[0].clone()
    }

    async fn apis(&self) -> Vec<Value> {
        let (status, apis) = self
            .json("GET", "/api/servers/things/apis", json!(null))
            .await;
        assert_eq!(status, StatusCode::OK, "{apis}");
        apis.as_array().unwrap().clone()
    }

    async fn api_id(&self) -> i64 {
        self.apis().await[0]["id"].as_i64().unwrap()
    }

    async fn tool_names(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM tools ORDER BY name")
            .fetch_all(&self.state.pool)
            .await
            .unwrap()
    }

    /// Changes the description of a spec tool, so it counts as edited.
    async fn edit_tool(&self, tool: &str) {
        let path = format!("/api/servers/things/tools/{tool}");
        let (_, view) = self.json("GET", &path, json!(null)).await;
        let mut draft = view["draft"].clone();
        draft["description"] = json!("My own text");
        let body = json!({ "api_id": view["api_id"], "draft": draft });
        let (status, saved) = self.json("PUT", &path, body).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
    }
}

fn reimport_path(id: i64) -> String {
    format!("/api/servers/things/apis/{id}/reimport")
}

fn auto_check_path(id: i64) -> String {
    format!("/api/servers/things/apis/{id}/auto-check")
}

#[sqlx::test(migrations = "../../migrations")]
async fn url_import_stores_the_spec_url(pool: PgPool) {
    let host = SpecHost::start().await;
    host.set("a.json", StatusCode::OK, &spec(&["listThings", "getThing"]));
    let app = App::new(pool).await;
    app.import_url(&host.url("a.json")).await;

    let apis = app.apis().await;
    assert_eq!(apis[0]["spec_url"], host.url("a.json"));
    assert_eq!(apis[0]["auto_check"], false);
    assert_eq!(apis[0]["last_check_ms"], Value::Null);
    assert_eq!(apis[0]["last_check_result"], Value::Null);
    assert_eq!(apis[0]["tool_count"], 2);
}

#[sqlx::test(migrations = "../../migrations")]
async fn text_import_has_no_spec_url(pool: PgPool) {
    let app = App::new(pool).await;
    app.import_text(&spec(&["listThings"])).await;
    assert_eq!(app.apis().await[0]["spec_url"], Value::Null);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_from_url_adds_removes_and_keeps_edited_tools(pool: PgPool) {
    let host = SpecHost::start().await;
    host.set("a.json", StatusCode::OK, &spec(&["listThings", "getThing"]));
    let app = App::new(pool).await;
    app.import_url(&host.url("a.json")).await;
    let id = app.api_id().await;
    app.edit_tool("listThings").await;
    // The user changed the base URL. The re-import must keep it.
    let body = json!({ "name": "Things", "base_url": "https://mine.example/v2", "auth": { "type": "none" } });
    let (status, saved) = app
        .json("PUT", &format!("/api/servers/things/apis/{id}"), body)
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");

    host.set("a.json", StatusCode::OK, &spec(&["listThings", "newThing"]));
    let (status, report) = app.json("POST", &reimport_path(id), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["added"], json!(["newThing"]));
    assert_eq!(report["kept"], json!(["listThings"]));
    assert_eq!(report["removed"], json!(["getThing"]));
    assert_eq!(report["base_url"], "https://mine.example/v2");
    assert_eq!(app.tool_names().await, ["listThings", "newThing"]);
    let (description,): (String,) =
        sqlx::query_as("SELECT description FROM tools WHERE name = 'listThings'")
            .fetch_one(&app.state.pool)
            .await
            .unwrap();
    assert_eq!(description, "My own text");
    assert_eq!(app.apis().await[0]["spec_url"], host.url("a.json"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_of_a_file_api_needs_new_text(pool: PgPool) {
    let app = App::new(pool).await;
    app.import_text(&spec(&["listThings"])).await;
    let id = app.api_id().await;

    let (status, error) = app.json("POST", &reimport_path(id), json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "spec_text_needed");

    let body = json!({ "spec": spec(&["listThings", "newThing"]) });
    let (status, report) = app.json("POST", &reimport_path(id), body).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["added"], json!(["newThing"]));
    assert_eq!(app.apis().await[0]["spec_url"], Value::Null);
}

#[sqlx::test(migrations = "../../migrations")]
async fn reimport_of_a_manual_api_is_refused(pool: PgPool) {
    let app = App::new(pool).await;
    let body =
        json!({ "name": "hand", "base_url": "https://hand.example", "auth": { "type": "none" } });
    let (status, api) = app
        .json("POST", "/api/servers/things/apis/manual", body)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    let id = api["id"].as_i64().unwrap();

    let body = json!({ "spec": spec(&["listThings"]) });
    let (status, error) = app.json("POST", &reimport_path(id), body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "not_a_spec_api");
    let (status, error) = app
        .json("PUT", &auto_check_path(id), json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "no_spec_url");
}

#[sqlx::test(migrations = "../../migrations")]
async fn auto_check_can_be_turned_on_and_off_for_a_url_api(pool: PgPool) {
    let host = SpecHost::start().await;
    host.set("a.json", StatusCode::OK, &spec(&["listThings"]));
    let app = App::new(pool).await;
    app.import_url(&host.url("a.json")).await;
    let id = app.api_id().await;

    let (status, view) = app
        .json("PUT", &auto_check_path(id), json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["auto_check"], true);
    assert_eq!(app.apis().await[0]["auto_check"], true);
    let (status, view) = app
        .json("PUT", &auto_check_path(id), json!({ "enabled": false }))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(app.apis().await[0]["auto_check"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auto_check_needs_a_spec_url(pool: PgPool) {
    let app = App::new(pool).await;
    app.import_text(&spec(&["listThings"])).await;
    let id = app.api_id().await;
    let (status, error) = app
        .json("PUT", &auto_check_path(id), json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "no_spec_url");
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_get_not_found(pool: PgPool) {
    let host = SpecHost::start().await;
    host.set("a.json", StatusCode::OK, &spec(&["listThings"]));
    let app = App::new(pool).await;
    app.import_url(&host.url("a.json")).await;
    let id = app.api_id().await;
    let other = common::create_user(&app.state.pool, 2, "mallory").await;

    for (method, path, body) in [
        ("POST", reimport_path(id), json!({})),
        ("PUT", auto_check_path(id), json!({ "enabled": true })),
    ] {
        let response = app.as_user(other, method, &path, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    // An unknown API id on an own server is also 404.
    let (status, _) = app.json("POST", &reimport_path(id + 100), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(app.apis().await[0]["auto_check"], false);
}

/// Adds a URL API with the given ops and turns its daily check on.
async fn checked_api(app: &App, host: &SpecHost, file: &str, ops: &[&str]) -> i64 {
    host.set(file, StatusCode::OK, &titled_spec(file, ops));
    let body = json!({ "source": "url", "spec_url": host.url(file) });
    let (status, imported) = app.json("POST", "/api/servers/things/apis", body).await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    let id: i64 = sqlx::query_scalar("SELECT id FROM apis WHERE spec_url = $1")
        .bind(host.url(file))
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    let (status, view) = app
        .json("PUT", &auto_check_path(id), json!({ "enabled": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    id
}

async fn check_state(app: &App, id: i64) -> (bool, Option<String>) {
    sqlx::query_as("SELECT last_check_at IS NOT NULL, last_check_result FROM apis WHERE id = $1")
        .bind(id)
        .fetch_one(&app.state.pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn check_updates_a_changed_spec(pool: PgPool) {
    let host = SpecHost::start().await;
    let app = App::new(pool).await;
    let id = checked_api(&app, &host, "a.json", &["listThings"]).await;
    host.set(
        "a.json",
        StatusCode::OK,
        &titled_spec("a.json", &["listThings", "newThing"]),
    );

    assert_eq!(check_due_apis(&app.state).await.unwrap(), 1);
    assert_eq!(check_state(&app, id).await, (true, Some("updated".into())));
    assert_eq!(app.tool_names().await, ["listThings", "newThing"]);
    let api = &app.apis().await[0];
    assert!(api["last_check_ms"].as_i64().unwrap() > 0, "{api}");
    assert_eq!(api["last_check_result"], "updated");

    // The API was just checked, so it is not due again.
    assert_eq!(check_due_apis(&app.state).await.unwrap(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn check_records_unchanged_and_does_not_import(pool: PgPool) {
    let host = SpecHost::start().await;
    let app = App::new(pool).await;
    let id = checked_api(&app, &host, "a.json", &["listThings"]).await;
    app.edit_tool("listThings").await;
    let before: Value = sqlx::query_scalar("SELECT to_jsonb(t) FROM tools t")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();

    assert_eq!(check_due_apis(&app.state).await.unwrap(), 1);
    assert_eq!(
        check_state(&app, id).await,
        (true, Some("unchanged".into()))
    );
    let after: Value = sqlx::query_scalar("SELECT to_jsonb(t) FROM tools t")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[sqlx::test(migrations = "../../migrations")]
async fn a_failing_api_does_not_stop_the_others(pool: PgPool) {
    let host = SpecHost::start().await;
    let app = App::new(pool).await;
    let broken = checked_api(&app, &host, "a.json", &["listThings"]).await;
    let good = checked_api(&app, &host, "b.json", &["otherThing"]).await;
    host.set("a.json", StatusCode::INTERNAL_SERVER_ERROR, "boom");
    host.set(
        "b.json",
        StatusCode::OK,
        &titled_spec("b.json", &["otherThing", "newThing"]),
    );

    assert_eq!(check_due_apis(&app.state).await.unwrap(), 2);
    assert_eq!(
        check_state(&app, broken).await,
        (true, Some("failed".into()))
    );
    assert_eq!(
        check_state(&app, good).await,
        (true, Some("updated".into()))
    );
    assert!(app.tool_names().await.contains(&"newThing".to_owned()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn check_skips_apis_that_are_not_due(pool: PgPool) {
    let host = SpecHost::start().await;
    let app = App::new(pool).await;
    let recent = checked_api(&app, &host, "a.json", &["listThings"]).await;
    sqlx::query("UPDATE apis SET last_check_at = now() - interval '1 hour' WHERE id = $1")
        .bind(recent)
        .execute(&app.state.pool)
        .await
        .unwrap();
    host.set(
        "a.json",
        StatusCode::OK,
        &titled_spec("a.json", &["listThings", "newThing"]),
    );
    assert_eq!(check_due_apis(&app.state).await.unwrap(), 0);
    assert_eq!(app.tool_names().await, ["listThings"]);

    // A day later it is due.
    sqlx::query("UPDATE apis SET last_check_at = now() - interval '25 hours' WHERE id = $1")
        .bind(recent)
        .execute(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(check_due_apis(&app.state).await.unwrap(), 1);

    // An API with the check off is never due.
    sqlx::query("UPDATE apis SET auto_check = false, last_check_at = NULL")
        .execute(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(check_due_apis(&app.state).await.unwrap(), 0);
}

fn api_path(id: i64) -> String {
    format!("/api/servers/things/apis/{id}")
}

/// Adds two APIs from text, `A` with `listA` and `getA`, `B` with `listB`.
async fn two_apis(app: &App) -> (i64, i64) {
    app.import_text(&titled_spec("A", &["listA", "getA"])).await;
    app.import_text(&titled_spec("B", &["listB"])).await;
    let apis = app.apis().await;
    let id = |name: &str| {
        apis.iter().find(|api| api["name"] == name).unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    (id("A"), id("B"))
}

#[sqlx::test(migrations = "../../migrations")]
async fn remove_api_deletes_its_tools_and_keeps_the_calls(pool: PgPool) {
    let app = App::new(pool).await;
    let (a, b) = two_apis(&app).await;
    sqlx::query(
        "INSERT INTO calls (server_id, tool, result, http_status, latency_ms)
         SELECT id, 'listA', 'ok', 200, 5 FROM servers WHERE name = 'things'",
    )
    .execute(&app.state.pool)
    .await
    .unwrap();

    let (status, removed) = app.json("DELETE", &api_path(a), json!(null)).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["removed_tools"], 2);
    assert_eq!(app.tool_names().await, ["listB"]);
    let apis = app.apis().await;
    assert_eq!(apis.len(), 1);
    assert_eq!(apis[0]["id"], b);
    let calls: Vec<String> = sqlx::query_scalar("SELECT tool FROM calls")
        .fetch_all(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(calls, ["listA"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn remove_api_of_another_user_or_server_is_not_found(pool: PgPool) {
    let app = App::new(pool).await;
    let (a, _) = two_apis(&app).await;
    let other = common::create_user(&app.state.pool, 2, "mallory").await;
    let response = app
        .as_user(other, "DELETE", &api_path(a), json!(null))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // An API of another own server is also not found under this server.
    sqlx::query("INSERT INTO servers (user_id, name) VALUES ($1, 'second')")
        .bind(app.user)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let path = format!("/api/servers/second/apis/{a}");
    let (status, error) = app.json("DELETE", &path, json!(null)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"], "not_found");

    let (status, error) = app.json("DELETE", &api_path(a + 100), json!(null)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_eq!(error["error"], "not_found");
    assert_eq!(app.tool_names().await, ["getA", "listA", "listB"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn remove_the_only_api_is_refused(pool: PgPool) {
    let app = App::new(pool).await;
    app.import_text(&spec(&["listThings"])).await;
    let id = app.api_id().await;
    let (status, error) = app.json("DELETE", &api_path(id), json!(null)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "last_enabled_tool");
    assert_eq!(app.tool_names().await, ["listThings"]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn remove_api_is_refused_when_the_other_tools_are_off(pool: PgPool) {
    let app = App::new(pool).await;
    let (a, b) = two_apis(&app).await;
    sqlx::query("UPDATE tools SET enabled = false WHERE api_id = $1")
        .bind(b)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (status, error) = app.json("DELETE", &api_path(a), json!(null)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{error}");
    assert_eq!(error["error"], "last_enabled_tool");
    assert_eq!(app.tool_names().await, ["getA", "listA", "listB"]);

    // An API with only disabled tools can go.
    let (status, removed) = app.json("DELETE", &api_path(b), json!(null)).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
    assert_eq!(removed["removed_tools"], 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn parallel_removals_keep_one_api(pool: PgPool) {
    let app = App::new(pool).await;
    let (a, b) = two_apis(&app).await;
    let (path_a, path_b) = (api_path(a), api_path(b));
    let (first, second) = tokio::join!(
        app.json("DELETE", &path_a, json!(null)),
        app.json("DELETE", &path_b, json!(null)),
    );
    let mut statuses = [first.0, second.0];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY]);
    assert_eq!(app.apis().await.len(), 1);
}

const HOST: &str = "toolport.test";

/// Opens an MCP session on `things` and returns its notification stream.
async fn notification_stream(app: &App) -> Body {
    let ttl = std::time::Duration::from_secs(3600);
    let token =
        server::mcp_tokens::create(&app.state.pool, &app.state.auth.jwt, "things", "t", ttl)
            .await
            .unwrap()
            .value;
    let post = |session: Option<String>, body: Value| {
        let mut request = Request::post("/mcp/things")
            .header(header::HOST, HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, format!("Bearer {token}"));
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        let request = request.body(Body::from(body.to_string())).unwrap();
        app.router.clone().oneshot(request)
    };
    let init = json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" },
        },
    });
    let response = post(None, init).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let session = response.headers()["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let ready = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let response = post(Some(session.clone()), ready).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    // The session is known to the server after its first tools request.
    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
    let response = post(Some(session.clone()), list).await.unwrap();
    BodyExt::collect(response.into_body()).await.unwrap();
    let request = Request::get("/mcp/things")
        .header(header::HOST, HOST)
        .header(header::ACCEPT, "text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("mcp-session-id", session)
        .body(Body::empty())
        .unwrap();
    app.router
        .clone()
        .oneshot(request)
        .await
        .unwrap()
        .into_body()
}

#[sqlx::test(migrations = "../../migrations")]
async fn remove_api_notifies_the_open_sessions(pool: PgPool) {
    let app = App::new(pool).await;
    let (a, _) = two_apis(&app).await;
    let mut stream = notification_stream(&app).await;

    let (status, removed) = app.json("DELETE", &api_path(a), json!(null)).await;
    assert_eq!(status, StatusCode::OK, "{removed}");
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
