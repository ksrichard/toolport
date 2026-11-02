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
