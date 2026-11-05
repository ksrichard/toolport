mod common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use server::{
    import::{ImportError, ImportRequest, Owner, import_spec},
    state::AppState,
};
use sqlx::PgPool;
use tower::ServiceExt;

const PETSTORE: &str = include_str!("../../core/tests/fixtures/petstore.json");

struct App {
    router: Router,
    state: AppState,
    user: i64,
}

impl App {
    /// A Free user without servers.
    async fn new(pool: PgPool) -> Self {
        let user = common::create_user(&pool, 1, "octocat").await;
        let state = common::state(pool).await;
        Self {
            router: server::app(state.clone()),
            state,
            user,
        }
    }

    async fn json(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let access = self.state.auth.jwt.issue_access(self.user).unwrap();
        let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, format!("Bearer {access}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        if status == StatusCode::NO_CONTENT {
            return (status, Value::Null);
        }
        (status, common::json_body(response).await)
    }

    async fn import_petstore(&self, server: &str) -> server::import::ImportReport {
        import_spec(
            &self.state.pool,
            ImportRequest {
                owner: Owner::UserId(self.user),
                server,
                api_name: None,
                spec_text: PETSTORE,
                base_url: Some("https://petstore.example/api/v3".parse().unwrap()),
                spec_url: None,
            },
        )
        .await
        .unwrap()
    }

    async fn enabled_tools(&self, server: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT t.name FROM tools t JOIN servers s ON s.id = t.server_id
             WHERE s.name = $1 AND t.enabled ORDER BY t.id",
        )
        .bind(server)
        .fetch_all(&self.state.pool)
        .await
        .unwrap()
    }
}

fn manual_api() -> Value {
    json!({ "name": "weather", "base_url": "https://a.example/v1", "auth": { "type": "none" } })
}

fn draft(name: &str) -> Value {
    json!({
        "name": name, "description": "Forecast", "method": "GET", "path": "/forecast",
        "query": "", "headers": [], "inputs": [],
    })
}

const SERVER_LIMIT: &str = "the Free plan allows 1 server, upgrade your plan or delete a server";

#[sqlx::test(migrations = "../../migrations")]
async fn free_plan_allows_one_server(pool: PgPool) {
    let app = App::new(pool).await;
    let (status, _) = app
        .json("POST", "/api/servers", Some(json!({ "name": "one" })))
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, error) = app
        .json("POST", "/api/servers", Some(json!({ "name": "two" })))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "plan_limit");
    assert_eq!(error["message"], SERVER_LIMIT);

    let body = json!({ "server": "three", "api": manual_api(), "draft": draft("forecast") });
    let (status, error) = app.json("POST", "/api/servers/with-tool", Some(body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "plan_limit");

    // The import-spec binary creates a server too.
    let error = import_spec(
        &app.state.pool,
        ImportRequest {
            owner: Owner::Login("octocat"),
            server: "four",
            api_name: None,
            spec_text: PETSTORE,
            base_url: Some("https://petstore.example/api/v3".parse().unwrap()),
            spec_url: None,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, ImportError::Plan(_)), "{error:?}");

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM servers")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    common::set_plan(&app.state.pool, app.user, "pro").await;
    let (status, _) = app
        .json("POST", "/api/servers", Some(json!({ "name": "two" })))
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// On Free a new import turns on only 3 tools. The other new tools start off.
#[sqlx::test(migrations = "../../migrations")]
async fn import_on_free_turns_on_only_the_allowed_tools(pool: PgPool) {
    let app = App::new(pool).await;
    let report = app.import_petstore("pets").await;
    assert!(report.added.len() > 3);
    assert_eq!(report.disabled.len(), report.added.len() - 3);
    assert_eq!(
        app.enabled_tools("pets").await,
        ["updatePet", "addPet", "findPetsByStatus"]
    );
}

/// Tools that are on stay on. Only the new tools of the import start off.
#[sqlx::test(migrations = "../../migrations")]
async fn import_on_free_keeps_the_enabled_tools(pool: PgPool) {
    let app = App::new(pool).await;
    common::set_plan(&app.state.pool, app.user, "pro").await;
    let report = app.import_petstore("pets").await;
    let all = report.added.len();
    common::set_plan(&app.state.pool, app.user, "free").await;

    let manual = json!({ "name": "weather", "base_url": "https://a.example/v1", "auth": { "type": "none" } });
    let (status, api) = app
        .json("POST", "/api/servers/pets/apis/manual", Some(manual))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{api}");
    let body = json!({ "api_id": api["id"], "draft": draft("forecast") });
    let (status, tool) = app
        .json("POST", "/api/servers/pets/tools", Some(body))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{tool}");

    let report = app.import_petstore("pets").await;
    assert!(report.added.is_empty());
    let enabled = app.enabled_tools("pets").await;
    assert_eq!(enabled.len(), all);
    assert!(!enabled.contains(&"forecast".to_owned()));
}

#[sqlx::test(migrations = "../../migrations")]
async fn turning_on_a_tool_over_the_limit_is_refused(pool: PgPool) {
    let app = App::new(pool).await;
    app.import_petstore("pets").await;
    let path = |tool: &str| format!("/api/servers/pets/tools/{tool}/enabled");

    let (status, error) = app
        .json("PUT", &path("deletePet"), Some(json!({ "enabled": true })))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"], "plan_limit");
    assert_eq!(
        error["message"],
        "the Free plan allows 3 enabled tools per server, turn off another tool or upgrade your plan"
    );

    // A tool that is on already can be turned on again.
    let (status, _) = app
        .json("PUT", &path("addPet"), Some(json!({ "enabled": true })))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = app
        .json("PUT", &path("addPet"), Some(json!({ "enabled": false })))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = app
        .json("PUT", &path("deletePet"), Some(json!({ "enabled": true })))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.enabled_tools("pets").await,
        ["updatePet", "findPetsByStatus", "deletePet"]
    );
}

/// The dashboard lists show which servers and tools the plan pauses.
#[sqlx::test(migrations = "../../migrations")]
async fn lists_mark_the_paused_servers_and_tools(pool: PgPool) {
    let app = App::new(pool).await;
    common::set_plan(&app.state.pool, app.user, "pro").await;
    let report = app.import_petstore("pets").await;
    app.import_petstore("pets-two").await;
    common::set_plan(&app.state.pool, app.user, "free").await;

    let (status, servers) = app.json("GET", "/api/servers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(servers[0]["paused"], false);
    assert_eq!(servers[1]["paused"], true);
    let extra = report.added.len() - 3;
    assert_eq!(servers[0]["paused_tools"], extra);

    let (_, tools) = app.json("GET", "/api/servers/pets/tools", None).await;
    let paused: Vec<bool> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["paused"].as_bool().unwrap())
        .collect();
    assert_eq!(paused[..3], [false, false, false]);
    assert!(paused[3..].iter().all(|p| *p));
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_shows_the_plan_and_the_usage(pool: PgPool) {
    let app = App::new(pool).await;
    let (status, billing) = app.json("GET", "/api/billing", None).await;
    assert_eq!(status, StatusCode::OK, "{billing}");
    assert_eq!(billing["plan"]["code"], "free");
    assert_eq!(billing["plan"]["max_servers"], 1);
    assert_eq!(billing["plan"]["max_tools_per_server"], 3);
    assert_eq!(billing["plan"]["max_calls_per_month"], 100);
    assert_eq!(billing["plan"]["history_hours"], 24);
    assert_eq!(billing["calls_used"], 0);
    assert_eq!(billing["servers_used"], 0);
    assert_eq!(billing["paused_servers"], 0);
    assert_eq!(billing["paused_tools"], 0);
    assert_eq!(billing["billing_configured"], false);
    assert_eq!(billing["has_customer"], false);
    assert_eq!(billing["subscription_status"], Value::Null);
    let (start, reset): (i64, i64) = sqlx::query_as(
        "SELECT (extract(epoch FROM date_trunc('month', now(), 'UTC')) * 1000)::bigint,
                (extract(epoch FROM (date_trunc('month', now(), 'UTC') AT TIME ZONE 'UTC'
                    + interval '1 month') AT TIME ZONE 'UTC') * 1000)::bigint",
    )
    .fetch_one(&app.state.pool)
    .await
    .unwrap();
    assert_eq!(billing["period_start_ms"], start);
    assert_eq!(billing["resets_at_ms"], reset);

    // A Pro user with a subscription that ends at the period end.
    app.import_petstore("pets").await;
    sqlx::query(
        "UPDATE users SET plan_id = (SELECT id FROM plans WHERE code = 'pro'),
                stripe_customer_id = 'cus_test', subscription_status = 'active',
                current_period_start = to_timestamp(1800000000),
                current_period_end = to_timestamp(4000000000), cancel_at_period_end = true
         WHERE id = $1",
    )
    .bind(app.user)
    .execute(&app.state.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO usage (user_id, period_start, calls) VALUES ($1, to_timestamp(1800000000), 42)")
        .bind(app.user)
        .execute(&app.state.pool)
        .await
        .unwrap();
    let (_, billing) = app.json("GET", "/api/billing", None).await;
    assert_eq!(billing["plan"]["code"], "pro");
    assert_eq!(billing["plan"]["max_tools_per_server"], Value::Null);
    assert_eq!(billing["subscription_status"], "active");
    assert_eq!(billing["cancel_at_period_end"], true);
    assert_eq!(billing["has_customer"], true);
    assert_eq!(billing["current_period_end_ms"], 4_000_000_000_000_i64);
    assert_eq!(billing["period_start_ms"], 1_800_000_000_000_i64);
    assert_eq!(billing["resets_at_ms"], 4_000_000_000_000_i64);
    assert_eq!(billing["calls_used"], 42);
    assert_eq!(billing["servers_used"], 1);
    assert!(billing["plan"].get("stripe_lookup_key").is_none());
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_counts_the_paused_servers_and_tools(pool: PgPool) {
    let app = App::new(pool).await;
    common::set_plan(&app.state.pool, app.user, "pro").await;
    let report = app.import_petstore("pets").await;
    app.import_petstore("pets-two").await;
    common::set_plan(&app.state.pool, app.user, "free").await;
    let (_, billing) = app.json("GET", "/api/billing", None).await;
    assert_eq!(billing["servers_used"], 2);
    assert_eq!(billing["paused_servers"], 1);
    assert_eq!(billing["paused_tools"], 2 * (report.added.len() - 3));
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_needs_a_signed_in_user(pool: PgPool) {
    let state = common::state(pool).await;
    let request = Request::get("/api/billing").body(Body::empty()).unwrap();
    let response = common::send(&state, request).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// The landing page reads the plans before sign-in.
#[sqlx::test(migrations = "../../migrations")]
async fn plans_are_public_and_come_from_the_table(pool: PgPool) {
    let state = common::state(pool).await;
    let get_plans = || async {
        let request = Request::get("/api/plans").body(Body::empty()).unwrap();
        let response = common::send(&state, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        common::json_body(response).await
    };
    let plans = get_plans().await;
    assert_eq!(
        plans,
        json!([
            {
                "code": "free", "name": "Free", "price_cents": 0, "currency": "usd",
                "interval": null, "max_servers": 1, "max_tools_per_server": 3,
                "max_calls_per_month": 100, "history_hours": 24, "is_default": true,
            },
            {
                "code": "pro", "name": "Pro", "price_cents": 900, "currency": "usd",
                "interval": "month", "max_servers": 10, "max_tools_per_server": null,
                "max_calls_per_month": 10000, "history_hours": 720, "is_default": false,
            },
        ])
    );

    sqlx::query("UPDATE plans SET max_servers = 2 WHERE code = 'free'")
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE plans SET active = false WHERE code = 'pro'")
        .execute(&state.pool)
        .await
        .unwrap();
    let plans = get_plans().await;
    assert_eq!(plans.as_array().unwrap().len(), 1);
    assert_eq!(plans[0]["max_servers"], 2);
}

/// A new user gets the default plan from the database trigger.
#[sqlx::test(migrations = "../../migrations")]
async fn new_users_get_the_default_plan(pool: PgPool) {
    let user = common::create_user(&pool, 1, "octocat").await;
    let code: String = sqlx::query_scalar(
        "SELECT p.code FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = $1",
    )
    .bind(user)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(code, "free");

    // Only one plan can be the default.
    let second_default = sqlx::query("UPDATE plans SET is_default = true WHERE code = 'pro'")
        .execute(&pool)
        .await;
    assert!(second_default.is_err());
}
