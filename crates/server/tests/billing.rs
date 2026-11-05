//! Stripe Checkout, Customer Portal, and webhook. Only Stripe is fake: `common::fake_stripe`.

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use common::{
    CHECKOUT_URL, PORTAL_URL, PRO_LOOKUP_KEY, SharedStripe, WEBHOOK_SECRET, stripe_event,
    subscription_json,
};
use serde_json::{Value, json};
use server::state::AppState;
use sqlx::PgPool;
use tower::ServiceExt;

const CUSTOMER: &str = "cus_test_1";
const PERIOD: (i64, i64) = (1_780_000_000, 1_782_592_000);
const NEXT_PERIOD: (i64, i64) = (1_782_592_000, 1_785_270_400);

struct App {
    router: Router,
    state: AppState,
    user: i64,
    stripe: SharedStripe,
}

#[derive(Debug, PartialEq)]
struct UserState {
    plan: String,
    status: Option<String>,
    period: Option<(i64, i64)>,
    cancel_at_period_end: bool,
    customer: Option<String>,
    subscription: Option<String>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

impl App {
    /// A Free user and a server with payments on.
    async fn new(pool: PgPool) -> Self {
        let (stripe_url, stripe) = common::fake_stripe().await;
        let mut config = common::config(&common::fake_github().await);
        common::with_stripe(&mut config, &stripe_url);
        Self::with_config(pool, &config, stripe).await
    }

    /// A Free user and a server without the Stripe settings.
    async fn without_stripe(pool: PgPool) -> Self {
        let config = common::config(&common::fake_github().await);
        Self::with_config(pool, &config, SharedStripe::default()).await
    }

    async fn with_config(
        pool: PgPool,
        config: &server::config::Config,
        stripe: SharedStripe,
    ) -> Self {
        let user = common::create_user(&pool, 1, "octocat").await;
        let state = AppState::new(pool, config).unwrap();
        Self {
            router: server::app(state.clone()),
            state,
            user,
            stripe,
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
        (status, common::json_body(response).await)
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (status, common::json_body(response).await)
    }

    /// Sends a webhook event with a good signature.
    async fn webhook(&self, payload: &str) -> StatusCode {
        let request = common::signed_webhook(payload, WEBHOOK_SECRET, now());
        self.send(request).await.0
    }

    async fn checkout(&self, plan: &str) -> (StatusCode, Value) {
        self.json(
            "POST",
            "/api/billing/checkout",
            Some(json!({ "plan": plan })),
        )
        .await
    }

    async fn portal(&self) -> (StatusCode, Value) {
        self.json("POST", "/api/billing/portal", None).await
    }

    /// Sets the subscription that the fake Stripe returns.
    fn stripe_has(&self, subscription: Value) {
        let id = subscription["id"].as_str().unwrap().to_owned();
        self.stripe
            .lock()
            .unwrap()
            .subscriptions
            .insert(id, subscription);
    }

    fn requests(&self, request: &str) -> usize {
        let data = self.stripe.lock().unwrap();
        data.requests.iter().filter(|r| *r == request).count()
    }

    fn last_form(&self) -> String {
        self.stripe.lock().unwrap().forms.last().unwrap().clone()
    }

    /// Stores the Stripe customer, as a Checkout does.
    async fn link_customer(&self) {
        sqlx::query("UPDATE users SET stripe_customer_id = $2 WHERE id = $1")
            .bind(self.user)
            .bind(CUSTOMER)
            .execute(&self.state.pool)
            .await
            .unwrap();
    }

    async fn user_state(&self) -> UserState {
        type Row = (
            String,
            Option<String>,
            Option<i64>,
            Option<i64>,
            bool,
            Option<String>,
            Option<String>,
        );
        let row: Row = sqlx::query_as(
            "SELECT p.code, u.subscription_status,
                    extract(epoch FROM u.current_period_start)::bigint,
                    extract(epoch FROM u.current_period_end)::bigint,
                    u.cancel_at_period_end, u.stripe_customer_id, u.stripe_subscription_id
             FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = $1",
        )
        .bind(self.user)
        .fetch_one(&self.state.pool)
        .await
        .unwrap();
        UserState {
            plan: row.0,
            status: row.1,
            period: row.2.zip(row.3),
            cancel_at_period_end: row.4,
            customer: row.5,
            subscription: row.6,
        }
    }
}

fn subscription(id: &str, status: &str, period: (i64, i64), cancel: bool) -> Value {
    subscription_json(id, CUSTOMER, status, PRO_LOOKUP_KEY, period, cancel)
}

fn subscription_event(id: &str, kind: &str, object: &Value) -> String {
    stripe_event(id, &format!("customer.subscription.{kind}"), object.clone())
}

fn checkout_completed(id: &str, user: i64, subscription: &str) -> String {
    stripe_event(
        id,
        "checkout.session.completed",
        json!({
            "id": "cs_test_1", "object": "checkout.session", "mode": "subscription",
            "customer": CUSTOMER, "subscription": subscription,
            "client_reference_id": user.to_string()
        }),
    )
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_routes_answer_503_without_the_stripe_settings(pool: PgPool) {
    let app = App::without_stripe(pool).await;

    for (status, body) in [app.checkout("pro").await, app.portal().await] {
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "billing_not_configured");
    }
    let payload = stripe_event("evt_1", "customer.created", json!({ "id": CUSTOMER }));
    assert_eq!(app.webhook(&payload).await, StatusCode::SERVICE_UNAVAILABLE);

    let (_, billing) = app.json("GET", "/api/billing", None).await;
    assert_eq!(billing["billing_configured"], false);
}

#[sqlx::test(migrations = "../../migrations")]
async fn billing_is_configured_with_both_stripe_settings(pool: PgPool) {
    let app = App::new(pool).await;
    let (_, billing) = app.json("GET", "/api/billing", None).await;
    assert_eq!(billing["billing_configured"], true);
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_creates_a_customer_and_returns_the_stripe_page(pool: PgPool) {
    let app = App::new(pool).await;

    let (status, body) = app.checkout("pro").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["url"], CHECKOUT_URL);
    assert_eq!(app.user_state().await.customer.as_deref(), Some(CUSTOMER));
    let form = app.last_form();
    for part in [
        "mode=subscription",
        "customer=cus_test_1",
        &format!("client_reference_id={}", app.user),
        "price_pro",
        "checkout%3Dsuccess",
    ] {
        assert!(form.contains(part), "{part} is not in {form}");
    }

    // A second start reuses the customer and the cached price.
    let (status, _) = app.checkout("pro").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.requests("POST /v1/customers"), 1);
    assert_eq!(app.requests("GET /v1/prices"), 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_refuses_a_user_with_a_subscription(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    common::set_plan(&pool, app.user, "pro").await;
    let (status, body) = app.checkout("pro").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "already_subscribed");

    // A subscription that still exists in Stripe blocks a second one, also on the Free plan.
    common::set_plan(&pool, app.user, "free").await;
    sqlx::query("UPDATE users SET subscription_status = 'unpaid' WHERE id = $1")
        .bind(app.user)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(app.checkout("pro").await.0, StatusCode::CONFLICT);

    // A canceled subscription is over, so a new Checkout can start.
    sqlx::query("UPDATE users SET subscription_status = 'canceled' WHERE id = $1")
        .bind(app.user)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(app.checkout("pro").await.0, StatusCode::OK);
    assert!(app.requests("POST /v1/checkout/sessions") == 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_refuses_the_free_plan_and_unknown_plans(pool: PgPool) {
    let app = App::new(pool).await;
    for plan in ["free", "gold"] {
        let (status, body) = app.checkout(plan).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"], "invalid_plan");
    }
    assert!(app.stripe.lock().unwrap().requests.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_without_a_stripe_price_gives_503(pool: PgPool) {
    let app = App::new(pool.clone()).await;
    sqlx::query("UPDATE plans SET stripe_lookup_key = 'toolport_missing' WHERE code = 'pro'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, body) = app.checkout("pro").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "price_not_found");
}

#[sqlx::test(migrations = "../../migrations")]
async fn portal_needs_a_customer(pool: PgPool) {
    let app = App::new(pool).await;
    let (status, body) = app.portal().await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no_customer");

    app.link_customer().await;
    let (status, body) = app.portal().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["url"], PORTAL_URL);
    let form = app.last_form();
    assert!(form.contains("configuration=bpc_toolport"), "{form}");
    assert!(form.contains("customer=cus_test_1"), "{form}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn webhook_checks_the_signature(pool: PgPool) {
    let app = App::new(pool).await;
    let payload = stripe_event("evt_1", "customer.created", json!({ "id": CUSTOMER }));

    // A good signature on an event that ToolPort does not use.
    assert_eq!(app.webhook(&payload).await, StatusCode::OK);

    let wrong_secret = common::signed_webhook(&payload, "other-secret", now());
    let old = common::signed_webhook(&payload, WEBHOOK_SECRET, now() - 600);
    let mut changed_body = common::signed_webhook(&payload, WEBHOOK_SECRET, now());
    *changed_body.body_mut() = Body::from(payload.replace("evt_1", "evt_2"));
    let no_header = Request::post("/api/billing/webhook")
        .body(Body::from(payload.clone()))
        .unwrap();
    for request in [wrong_secret, old, changed_body, no_header] {
        let (status, body) = app.send(request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "invalid_signature");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn checkout_completed_and_then_updated_sets_pro_with_the_period(pool: PgPool) {
    let app = App::new(pool).await;
    // The customer is not stored yet, so the handler finds the user by the client reference.
    app.stripe_has(subscription("sub_1", "active", PERIOD, false));
    let status = app
        .webhook(&checkout_completed("evt_1", app.user, "sub_1"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        app.user_state().await,
        UserState {
            plan: "pro".into(),
            status: Some("active".into()),
            period: Some(PERIOD),
            cancel_at_period_end: false,
            customer: Some(CUSTOMER.into()),
            subscription: Some("sub_1".into()),
        }
    );

    // The user cancels in the Portal. The plan stays until the period ends.
    let canceling = subscription("sub_1", "active", PERIOD, true);
    app.stripe_has(canceling.clone());
    let status = app
        .webhook(&subscription_event("evt_2", "updated", &canceling))
        .await;
    assert_eq!(status, StatusCode::OK);
    let user = app.user_state().await;
    assert_eq!(user.plan, "pro");
    assert!(user.cancel_at_period_end);

    let (_, billing) = app.json("GET", "/api/billing", None).await;
    assert_eq!(billing["plan"]["code"], "pro");
    assert_eq!(billing["cancel_at_period_end"], true);
    assert_eq!(billing["current_period_end_ms"], PERIOD.1 * 1000);
}

#[sqlx::test(migrations = "../../migrations")]
async fn duplicate_event_is_applied_once(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    let sub = subscription("sub_1", "active", PERIOD, false);
    app.stripe_has(sub.clone());
    let payload = subscription_event("evt_1", "created", &sub);

    assert_eq!(app.webhook(&payload).await, StatusCode::OK);
    assert_eq!(app.webhook(&payload).await, StatusCode::OK);
    assert_eq!(app.requests("GET /v1/subscriptions/sub_1"), 1);
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM stripe_events")
        .fetch_one(&app.state.pool)
        .await
        .unwrap();
    assert_eq!(events, 1);
    assert_eq!(app.user_state().await.plan, "pro");
}

#[sqlx::test(migrations = "../../migrations")]
async fn deleted_subscription_sets_the_default_plan(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    let active = subscription("sub_1", "active", PERIOD, true);
    app.stripe_has(active.clone());
    app.webhook(&subscription_event("evt_1", "created", &active))
        .await;
    assert_eq!(app.user_state().await.plan, "pro");

    let ended = subscription("sub_1", "canceled", PERIOD, true);
    app.stripe_has(ended.clone());
    let status = app
        .webhook(&subscription_event("evt_2", "deleted", &ended))
        .await;
    assert_eq!(status, StatusCode::OK);
    let user = app.user_state().await;
    assert_eq!(user.plan, "free");
    assert_eq!(user.status.as_deref(), Some("canceled"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn older_event_does_not_undo_a_newer_state(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    // Stripe already has the newest state: the subscription is over.
    app.stripe_has(subscription("sub_1", "canceled", NEXT_PERIOD, false));
    let ended = subscription("sub_1", "canceled", NEXT_PERIOD, false);
    app.webhook(&subscription_event("evt_3", "deleted", &ended))
        .await;

    // The older events arrive late. Their bodies still say "active".
    let renewed = subscription("sub_1", "active", NEXT_PERIOD, false);
    let created = subscription("sub_1", "active", PERIOD, false);
    app.webhook(&subscription_event("evt_2", "updated", &renewed))
        .await;
    app.webhook(&subscription_event("evt_1", "created", &created))
        .await;

    let user = app.user_state().await;
    assert_eq!(user.plan, "free");
    assert_eq!(user.status.as_deref(), Some("canceled"));
    assert_eq!(user.period, Some(NEXT_PERIOD));
}

#[sqlx::test(migrations = "../../migrations")]
async fn late_event_of_an_old_subscription_keeps_the_new_one(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    app.stripe_has(subscription("sub_1", "canceled", PERIOD, false));
    let new = subscription("sub_2", "active", NEXT_PERIOD, false);
    app.stripe_has(new.clone());
    app.webhook(&subscription_event("evt_2", "created", &new))
        .await;

    let old = subscription("sub_1", "canceled", PERIOD, false);
    app.webhook(&subscription_event("evt_1", "deleted", &old))
        .await;

    let user = app.user_state().await;
    assert_eq!(user.plan, "pro");
    assert_eq!(user.subscription.as_deref(), Some("sub_2"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_lookup_key_keeps_the_default_plan(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    let sub = subscription_json("sub_1", CUSTOMER, "active", "toolport_gold", PERIOD, false);
    app.stripe_has(sub.clone());

    let status = app
        .webhook(&subscription_event("evt_1", "created", &sub))
        .await;
    assert_eq!(status, StatusCode::OK);
    let user = app.user_state().await;
    assert_eq!(user.plan, "free");
    assert_eq!(user.status.as_deref(), Some("active"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn stripe_error_leaves_the_event_for_a_retry(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    let sub = subscription("sub_1", "active", PERIOD, false);
    let payload = subscription_event("evt_1", "created", &sub);

    // The fake Stripe does not know the subscription yet, so the read fails.
    assert_eq!(app.webhook(&payload).await, StatusCode::BAD_GATEWAY);
    assert_eq!(app.user_state().await.plan, "free");

    app.stripe_has(sub);
    assert_eq!(app.webhook(&payload).await, StatusCode::OK);
    assert_eq!(app.user_state().await.plan, "pro");
}

#[sqlx::test(migrations = "../../migrations")]
async fn invoice_event_reads_the_subscription(pool: PgPool) {
    let app = App::new(pool).await;
    app.link_customer().await;
    app.stripe_has(subscription("sub_1", "past_due", PERIOD, false));
    let invoice = json!({
        "id": "in_1", "object": "invoice", "customer": CUSTOMER,
        "parent": { "subscription_details": { "subscription": "sub_1" } }
    });
    let status = app
        .webhook(&stripe_event("evt_1", "invoice.payment_failed", invoice))
        .await;
    assert_eq!(status, StatusCode::OK);
    let user = app.user_state().await;
    assert_eq!(user.plan, "pro");
    assert_eq!(user.status.as_deref(), Some("past_due"));
}
