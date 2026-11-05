#![allow(dead_code)]

use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use clap::Parser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use server::{config::Config, state::AppState};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

pub const GOOD_CODE: &str = "good-code";
const GITHUB_TOKEN: &str = "gho_fake_token";

/// Starts a fake GitHub on a free local port and returns its base URL.
/// It accepts only `GOOD_CODE` and returns the "octocat" user.
pub async fn fake_github() -> String {
    let app = Router::new()
        .route(
            "/login/oauth/access_token",
            post(|Json(body): Json<Value>| async move {
                if body["code"] == GOOD_CODE && body["client_secret"] == "test-client-secret" {
                    Json(json!({ "access_token": GITHUB_TOKEN, "token_type": "bearer" }))
                } else {
                    Json(json!({ "error": "bad_verification_code" }))
                }
            }),
        )
        .route(
            "/user",
            get(|headers: HeaderMap| async move {
                let expected = format!("Bearer {GITHUB_TOKEN}");
                if headers.get(header::AUTHORIZATION).map(|v| v.as_bytes())
                    != Some(expected.as_bytes())
                {
                    return Err(StatusCode::UNAUTHORIZED);
                }
                Ok(Json(json!({
                    "id": 583231,
                    "login": "octocat",
                    "name": "The Octocat",
                    "avatar_url": "https://avatars.example/octocat.png",
                })))
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

pub const JWT_SECRET: &str = "test-secret-that-is-at-least-32-characters";

pub fn config(github_url: &str) -> Config {
    Config::try_parse_from([
        "toolport-server",
        "--database-url=postgres://unused",
        "--public-url=https://toolport.test",
        &format!("--jwt-secret={JWT_SECRET}"),
        &format!("--secrets-key={}", "0".repeat(64)),
        "--allow-local-upstream",
        "--github-client-id=test-client-id",
        "--github-client-secret=test-client-secret",
        &format!("--github-oauth-url={github_url}"),
        &format!("--github-api-url={github_url}"),
    ])
    .unwrap()
}

pub async fn state(pool: PgPool) -> AppState {
    let github_url = fake_github().await;
    AppState::new(pool, &config(&github_url)).unwrap()
}

pub async fn send(state: &AppState, request: Request<Body>) -> Response {
    server::app(state.clone()).oneshot(request).await.unwrap()
}

pub async fn json_body(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Returns the full `Set-Cookie` header for the named cookie.
pub fn set_cookie(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(str::to_owned)
}

/// Returns only the value of the named cookie.
pub fn cookie_value(response: &Response, name: &str) -> Option<String> {
    let header = set_cookie(response, name)?;
    let pair = header.split(';').next()?;
    Some(pair[name.len() + 1..].to_owned())
}

pub fn location(response: &Response) -> String {
    response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned()
}

/// Creates a user row, as the first GitHub sign-in does. Returns the user id.
pub async fn create_user(pool: &PgPool, github_id: i64, login: &str) -> i64 {
    sqlx::query_scalar("INSERT INTO users (github_id, github_login) VALUES ($1, $2) RETURNING id")
        .bind(github_id)
        .bind(login)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Moves a user to the plan with this code and applies its tool limit, as the Stripe webhook does.
/// Tests that need several servers or many tools use `pro`.
pub async fn set_plan(pool: &PgPool, user_id: i64, code: &str) {
    let updated = sqlx::query(
        "UPDATE users SET plan_id = (SELECT id FROM plans WHERE code = $2) WHERE id = $1",
    )
    .bind(user_id)
    .bind(code)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(updated.rows_affected(), 1);
    let mut tx = pool.begin().await.unwrap();
    server::plan::reconcile_tools(&mut tx, user_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

pub const STRIPE_KEY: &str = "fake-stripe-key";
pub const WEBHOOK_SECRET: &str = "fake-webhook-secret";
pub const PRO_LOOKUP_KEY: &str = "toolport_pro_monthly";
pub const CHECKOUT_URL: &str = "https://checkout.stripe.test/c/pay/cs_test_1";
pub const PORTAL_URL: &str = "https://billing.stripe.test/p/session/bps_test_1";

/// What the fake Stripe has seen and what it answers.
#[derive(Default)]
pub struct StripeData {
    /// One entry per request, for example `GET /v1/subscriptions/sub_1`.
    pub requests: Vec<String>,
    /// The form bodies of the POST requests, in order.
    pub forms: Vec<String>,
    /// The subscriptions that `GET /v1/subscriptions/{id}` returns.
    pub subscriptions: std::collections::HashMap<String, Value>,
}

pub type SharedStripe = std::sync::Arc<std::sync::Mutex<StripeData>>;

fn stripe_list(url: &str, data: Vec<Value>) -> Value {
    json!({ "object": "list", "data": data, "has_more": false, "url": url })
}

fn stripe_not_found() -> Response {
    let body = json!({ "error": { "type": "invalid_request_error", "message": "No such object" } });
    (StatusCode::NOT_FOUND, Json(body)).into_response()
}

pub fn price_json(lookup_key: &str) -> Value {
    json!({
        "id": "price_pro", "object": "price", "active": true, "billing_scheme": "per_unit",
        "created": 1, "currency": "usd", "livemode": false, "lookup_key": lookup_key,
        "metadata": {}, "product": "prod_toolport_pro", "type": "recurring", "unit_amount": 900,
        "recurring": { "interval": "month", "interval_count": 1, "usage_type": "licensed" }
    })
}

/// A subscription with one item, in the shape of the Stripe API version of async-stripe.
pub fn subscription_json(
    id: &str,
    customer: &str,
    status: &str,
    lookup_key: &str,
    period: (i64, i64),
    cancel_at_period_end: bool,
) -> Value {
    let plan = json!({
        "id": "price_pro", "object": "plan", "active": true, "billing_scheme": "per_unit",
        "created": 1, "currency": "usd", "interval": "month", "interval_count": 1,
        "livemode": false, "usage_type": "licensed", "amount": 900
    });
    json!({
        "id": id, "object": "subscription", "customer": customer, "status": status,
        "automatic_tax": { "enabled": false }, "billing_cycle_anchor": period.0,
        "billing_mode": { "type": "classic" }, "billing_schedules": [],
        "cancel_at_period_end": cancel_at_period_end, "collection_method": "charge_automatically",
        "created": period.0, "currency": "usd", "discounts": [],
        "invoice_settings": { "issuer": { "type": "self" } }, "livemode": false,
        "metadata": {}, "start_date": period.0,
        "items": stripe_list("/v1/subscription_items", vec![json!({
            "id": "si_1", "object": "subscription_item", "created": period.0,
            "current_period_start": period.0, "current_period_end": period.1,
            "discounts": [], "metadata": {}, "plan": plan, "price": price_json(lookup_key),
            "quantity": 1, "subscription": id
        })]),
    })
}

fn checkout_session_json() -> Value {
    json!({
        "id": "cs_test_1", "object": "checkout.session", "automatic_tax": { "enabled": false },
        "created": 1, "custom_fields": [], "custom_text": {}, "expires_at": 2,
        "livemode": false, "mode": "subscription", "payment_method_types": ["card"],
        "payment_status": "unpaid", "shipping_options": [], "url": CHECKOUT_URL
    })
}

fn portal_configuration_json(id: &str, metadata: Value) -> Value {
    json!({
        "id": id, "object": "billing_portal.configuration", "active": true,
        "business_profile": {}, "created": 1, "is_default": false, "livemode": false,
        "login_page": { "enabled": false }, "updated": 1, "metadata": metadata,
        "features": {
            "customer_update": { "allowed_updates": ["address"], "enabled": true },
            "invoice_history": { "enabled": true },
            "payment_method_update": { "enabled": true },
            "subscription_cancel": {
                "cancellation_reason": { "enabled": false, "options": [] },
                "enabled": true, "mode": "at_period_end", "proration_behavior": "none"
            },
            "subscription_update": {
                "default_allowed_updates": [], "enabled": false,
                "proration_behavior": "none", "schedule_at_period_end": { "conditions": [] },
                "trial_update_behavior": "end_trial"
            }
        }
    })
}

async fn fake_stripe_handler(
    axum::extract::State(data): axum::extract::State<SharedStripe>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: String,
) -> Response {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or_default().to_owned();
    let mut data = data.lock().unwrap();
    data.requests.push(format!("{method} {path}"));
    if method == axum::http::Method::POST {
        data.forms.push(body);
    }
    let answer = match (method.as_str(), path.as_str()) {
        ("GET", "/v1/prices") => {
            let found = query.contains(PRO_LOOKUP_KEY);
            let prices = if found {
                vec![price_json(PRO_LOOKUP_KEY)]
            } else {
                vec![]
            };
            stripe_list("/v1/prices", prices)
        }
        ("POST", "/v1/customers") => {
            json!({ "id": "cus_test_1", "object": "customer", "created": 1, "livemode": false })
        }
        ("POST", "/v1/checkout/sessions") => checkout_session_json(),
        ("GET", "/v1/billing_portal/configurations") => stripe_list(
            "/v1/billing_portal/configurations",
            vec![
                portal_configuration_json("bpc_other", json!({})),
                portal_configuration_json("bpc_toolport", json!({ "toolport": "portal" })),
            ],
        ),
        ("POST", "/v1/billing_portal/sessions") => json!({
            "id": "bps_test_1", "object": "billing_portal.session",
            "configuration": "bpc_toolport", "created": 1, "customer": "cus_test_1",
            "livemode": false, "url": PORTAL_URL
        }),
        ("GET", p) if p.starts_with("/v1/subscriptions/") => {
            match data.subscriptions.get(&p["/v1/subscriptions/".len()..]) {
                Some(sub) => sub.clone(),
                None => return stripe_not_found(),
            }
        }
        _ => return stripe_not_found(),
    };
    Json(answer).into_response()
}

/// Starts a fake Stripe API on a free local port. Returns its base URL and its data.
pub async fn fake_stripe() -> (String, SharedStripe) {
    let data = SharedStripe::default();
    let app = Router::new()
        .fallback(fake_stripe_handler)
        .with_state(data.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/"), data)
}

/// Turns on payments in a test config, with the fake Stripe at `stripe_url`.
pub fn with_stripe(config: &mut Config, stripe_url: &str) {
    config.stripe_secret_key = Some(STRIPE_KEY.to_owned());
    config.stripe_webhook_secret = Some(WEBHOOK_SECRET.to_owned());
    config.stripe_api_url = Some(stripe_url.parse().unwrap());
}

/// A Stripe event body. The fields outside `data.object` are the same in each test.
pub fn stripe_event(id: &str, kind: &str, object: Value) -> String {
    json!({
        "id": id, "object": "event", "api_version": "2025-09-30.clover", "created": 1,
        "livemode": false, "pending_webhooks": 1,
        "request": { "id": null, "idempotency_key": null },
        "type": kind, "data": { "object": object }
    })
    .to_string()
}

/// A webhook request, signed like Stripe signs it, at `timestamp` (Unix seconds).
pub fn signed_webhook(payload: &str, secret: &str, timestamp: i64) -> Request<Body> {
    let signature = stripe_webhook::Webhook::generate_test_header(payload, secret, Some(timestamp));
    Request::post("/api/billing/webhook")
        .header("stripe-signature", signature)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_owned()))
        .unwrap()
}
