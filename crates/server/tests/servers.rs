mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
    routing::get,
};
use serde_json::{Value, json};
use server::{mcp_tokens, state::AppState};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

const SWAGGER_SPEC: &str =
    r#"{"swagger": "2.0", "info": {"title": "old", "version": "1"}, "paths": {}}"#;

const SMALL_SPEC: &str = r#"{
  "openapi": "3.0.3",
  "info": { "title": "Small", "version": "1" },
  "servers": [{ "url": "https://small.example/v1" }],
  "paths": { "/things": { "get": { "operationId": "listThings", "responses": { "200": { "description": "ok" } } } } }
}"#;

struct App {
    router: Router,
    state: AppState,
}

impl App {
    async fn new(pool: PgPool) -> Self {
        common::create_user(&pool, 1, "octocat").await;
        let state = common::state(pool).await;
        Self {
            router: server::app(state.clone()),
            state,
        }
    }

    async fn request(
        &self,
        user_id: i64,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Response {
        let access = self.state.auth.jwt.issue_access(user_id).unwrap();
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .header(header::CONTENT_TYPE, "application/json");
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        self.router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }

    async fn json(
        &self,
        user_id: i64,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let response = self.request(user_id, method, path, body).await;
        let status = response.status();
        (status, common::json_body(response).await)
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn new_user_has_no_servers_and_sees_the_templates(pool: PgPool) {
    let app = App::new(pool).await;
    let (status, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(servers, json!([]));

    let (_, templates) = app.json(1, "GET", "/api/templates", None).await;
    let ids: Vec<&str> = templates
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["weather", "github-issues", "hacker-news"]);
    assert!(templates[0].get("specs").is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn onboarding_from_a_template(pool: PgPool) {
    let app = App::new(pool.clone()).await;

    let (status, created) = app
        .json(
            1,
            "POST",
            "/api/servers",
            Some(json!({ "name": "weather" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["mcp_url"], "https://toolport.test/mcp/weather");

    let body = json!({ "source": "template", "template": "weather" });
    let (status, imported) = app
        .json(1, "POST", "/api/servers/weather/apis", Some(body))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imported[0]["tools"], json!(["searchPlaces"]));
    assert_eq!(imported[1]["tools"], json!(["getWeather"]));
    assert_eq!(imported[1]["base_url"], "https://api.open-meteo.com/v1");

    let (status, token) = app
        .json(
            1,
            "POST",
            "/api/servers/weather/tokens",
            Some(json!({ "name": "claude-code" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(token["expires_in_days"], 90);
    let value = token["token"].as_str().unwrap();
    let caller = mcp_tokens::authorize(&pool, &app.state.auth.jwt, "weather", Some(value))
        .await
        .unwrap();
    assert_eq!(caller.token_name, "claude-code");

    let (_, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(servers[0]["apis"], 2);
    assert_eq!(servers[0]["tools"], 2);
    assert_eq!(servers[0]["enabled_tools"], 2);
    assert_eq!(servers[0]["active_tokens"], 1);

    let (_, tools) = app.json(1, "GET", "/api/servers/weather/tools", None).await;
    assert_eq!(tools[1]["name"], "getWeather");
    assert_eq!(tools[1]["method"], "GET");
    assert_eq!(tools[1]["path"], "/forecast");
    assert_eq!(tools[1]["api"], "Open-Meteo forecast");
    assert_eq!(tools[1]["enabled"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn importing_a_template_again_replaces_its_apis(pool: PgPool) {
    let app = App::new(pool).await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "news" })))
        .await;
    let body = json!({ "source": "template", "template": "hacker-news" });
    app.request(1, "POST", "/api/servers/news/apis", Some(body.clone()))
        .await;
    let (status, imported) = app
        .json(1, "POST", "/api/servers/news/apis", Some(body))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imported[0]["replaced"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn server_names_are_checked(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    let (status, error) = app
        .json(
            1,
            "POST",
            "/api/servers",
            Some(json!({ "name": "Bad Name" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "invalid_server_name");

    app.request(1, "POST", "/api/servers", Some(json!({ "name": "pets" })))
        .await;
    common::create_user(&pool, 2, "mallory").await;
    let (status, error) = app
        .json(2, "POST", "/api/servers", Some(json!({ "name": "pets" })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "server_name_taken");
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_cannot_touch_a_server(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "pets" })))
        .await;
    common::create_user(&pool, 2, "mallory").await;

    let template = json!({ "source": "template", "template": "weather" });
    for (method, path, body) in [
        ("GET", "/api/servers/pets/tools", None),
        ("POST", "/api/servers/pets/apis", Some(template)),
        (
            "POST",
            "/api/servers/pets/tokens",
            Some(json!({ "name": "x" })),
        ),
    ] {
        let response = app.request(2, method, path, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    let (_, servers) = app.json(2, "GET", "/api/servers", None).await;
    assert_eq!(servers, json!([]));
}

#[sqlx::test(migrations = "../../migrations")]
async fn import_errors_tell_the_user_what_is_wrong(pool: PgPool) {
    let app = App::new(pool).await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "pets" })))
        .await;

    let body = json!({ "source": "text", "spec": SWAGGER_SPEC });
    let (status, error) = app
        .json(1, "POST", "/api/servers/pets/apis", Some(body))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "invalid_spec");
    assert!(error["message"].as_str().unwrap().contains("Swagger 2.0"));

    let body = json!({ "source": "template", "template": "nope" });
    let response = app
        .request(1, "POST", "/api/servers/pets/apis", Some(body))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = json!({ "source": "url", "spec_url": "ftp://example.com/spec.json" });
    let (_, error) = app
        .json(1, "POST", "/api/servers/pets/apis", Some(body))
        .await;
    assert_eq!(error["error"], "invalid_spec_url");

    let body = json!({ "source": "text", "spec": SMALL_SPEC, "base_url": "not a url" });
    let (_, error) = app
        .json(1, "POST", "/api/servers/pets/apis", Some(body))
        .await;
    assert_eq!(error["error"], "bad_base_url");

    let response = app
        .request(
            1,
            "POST",
            "/api/servers/pets/tokens",
            Some(json!({ "name": "  " })),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "../../migrations")]
async fn import_from_a_spec_url(pool: PgPool) {
    let spec_host = Router::new().route("/spec.json", get(|| async { SMALL_SPEC }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, spec_host).await.unwrap() });

    let app = App::new(pool).await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "small" })))
        .await;
    let body = json!({
        "source": "url",
        "spec_url": format!("http://{addr}/spec.json"),
        "base_url": "https://other.example/api",
    });
    let (status, imported) = app
        .json(1, "POST", "/api/servers/small/apis", Some(body))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imported[0]["tools"], json!(["listThings"]));
    assert_eq!(imported[0]["base_url"], "https://other.example/api");

    let body = json!({ "source": "url", "spec_url": format!("http://{addr}/missing.json") });
    let (status, error) = app
        .json(1, "POST", "/api/servers/small/apis", Some(body))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "spec_download_failed");
}

#[sqlx::test(migrations = "../../migrations")]
async fn routes_need_a_signed_in_user(pool: PgPool) {
    let app = App::new(pool).await;
    let request = Request::get("/api/servers").body(Body::empty()).unwrap();
    let response = app.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrations = "../../migrations")]
async fn server_list_shows_the_status(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    for name in ["empty", "weather"] {
        app.request(1, "POST", "/api/servers", Some(json!({ "name": name })))
            .await;
    }
    let body = json!({ "source": "template", "template": "weather" });
    app.request(1, "POST", "/api/servers/weather/apis", Some(body))
        .await;
    app.request(
        1,
        "POST",
        "/api/servers/weather/tokens",
        Some(json!({ "name": "laptop" })),
    )
    .await;
    sqlx::query(
        "INSERT INTO calls (server_id, tool, result, latency_ms, created_at)
         SELECT id, 'getWeather', 'ok', 5, '2026-11-04T10:00:00Z' FROM servers WHERE name = 'weather'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let (status, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(servers[0]["name"], "empty");
    assert_eq!(servers[0]["last_call_ms"], Value::Null);
    assert_eq!(servers[0]["connected_clients"], 0);
    assert_eq!(servers[1]["apis"], 2);
    assert_eq!(servers[1]["enabled_tools"], 2);
    assert_eq!(servers[1]["active_tokens"], 1);
    assert_eq!(servers[1]["last_call_ms"], 1_793_786_400_000_i64);
    assert!(servers[1].get("id").is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn rename_checks_the_new_name(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    for name in ["pets", "shop"] {
        app.request(1, "POST", "/api/servers", Some(json!({ "name": name })))
            .await;
    }

    let (status, renamed) = app
        .json(
            1,
            "PUT",
            "/api/servers/pets",
            Some(json!({ "name": " pets-prod " })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "pets-prod");
    assert_eq!(renamed["mcp_url"], "https://toolport.test/mcp/pets-prod");
    let response = app.request(1, "GET", "/api/servers/pets/tools", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let (status, error) = app
        .json(
            1,
            "PUT",
            "/api/servers/pets-prod",
            Some(json!({ "name": "shop" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "server_name_taken");

    for bad in ["Bad Name", "", "with-tool"] {
        let (status, error) = app
            .json(
                1,
                "PUT",
                "/api/servers/pets-prod",
                Some(json!({ "name": bad })),
            )
            .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_eq!(error["error"], "invalid_server_name");
    }

    // The same name again is not a change and not a conflict.
    let response = app
        .request(
            1,
            "PUT",
            "/api/servers/pets-prod",
            Some(json!({ "name": "pets-prod" })),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test(migrations = "../../migrations")]
async fn names_of_other_users_are_taken_too(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "pets" })))
        .await;
    common::create_user(&pool, 2, "mallory").await;
    app.request(2, "POST", "/api/servers", Some(json!({ "name": "mine" })))
        .await;
    let (status, error) = app
        .json(
            2,
            "PUT",
            "/api/servers/mine",
            Some(json!({ "name": "pets" })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "server_name_taken");
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_removes_the_server_and_its_rows(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    app.request(
        1,
        "POST",
        "/api/servers",
        Some(json!({ "name": "weather" })),
    )
    .await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "keep" })))
        .await;
    let body = json!({ "source": "template", "template": "weather" });
    app.request(1, "POST", "/api/servers/weather/apis", Some(body))
        .await;
    app.request(
        1,
        "POST",
        "/api/servers/weather/tokens",
        Some(json!({ "name": "laptop" })),
    )
    .await;
    sqlx::query(
        "INSERT INTO calls (server_id, tool, result, latency_ms)
         SELECT id, 'getWeather', 'ok', 5 FROM servers WHERE name = 'weather'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let response = app.request(1, "DELETE", "/api/servers/weather", None).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    for query in [
        "SELECT count(*) FROM apis",
        "SELECT count(*) FROM tools",
        "SELECT count(*) FROM mcp_tokens",
        "SELECT count(*) FROM calls",
    ] {
        let count: i64 = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0, "{query}");
    }
    let (_, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(servers.as_array().unwrap().len(), 1);
    assert_eq!(servers[0]["name"], "keep");

    let response = app.request(1, "DELETE", "/api/servers/weather", None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "../../migrations")]
async fn other_users_cannot_rename_or_delete_a_server(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    app.request(1, "POST", "/api/servers", Some(json!({ "name": "pets" })))
        .await;
    common::create_user(&pool, 2, "mallory").await;

    for (method, path, body) in [
        (
            "PUT",
            "/api/servers/pets",
            Some(json!({ "name": "stolen" })),
        ),
        ("DELETE", "/api/servers/pets", None),
        (
            "PUT",
            "/api/servers/nope",
            Some(json!({ "name": "stolen" })),
        ),
        ("DELETE", "/api/servers/nope", None),
    ] {
        let (status, error) = app.json(2, method, path, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
        assert_eq!(error["error"], "not_found");
    }
    let (_, servers) = app.json(1, "GET", "/api/servers", None).await;
    assert_eq!(servers[0]["name"], "pets");
}

#[sqlx::test(migrations = "../../migrations")]
async fn reserved_route_names_are_not_server_names(pool: PgPool) {
    let app = App::new(pool).await;
    let (status, error) = app
        .json(
            1,
            "POST",
            "/api/servers",
            Some(json!({ "name": "with-tool" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "invalid_server_name");
}
