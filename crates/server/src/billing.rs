//! The plan list, the "Plan & billing" data of the signed-in user, and the Stripe routes.
//! Stripe manages the subscriptions. ToolPort starts a Checkout for the first subscription,
//! opens the Customer Portal for every later change, and takes the plan state from webhooks.

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stripe_webhook::{Webhook, WebhookError};
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    error::{ApiError, ApiJson},
    payments::{Payments, USER_ID_KEY},
    plan::{self, Plan},
    state::AppState,
};

/// With these Stripe statuses the user keeps the paid plan. `past_due` is in the list, so a
/// failed card does not stop the service at once. Stripe tries again and then cancels.
const PAID_STATUSES: [&str; 3] = ["active", "trialing", "past_due"];
/// A subscription with one of these statuses is over. Any other status still exists in Stripe.
const ENDED_STATUSES: [&str; 2] = ["canceled", "incomplete_expired"];

/// The plan, usage, and subscription of the signed-in user. All values come from the DB.
/// The Stripe webhook is the only writer of the subscription fields.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct Billing {
    pub plan: Plan,
    /// The Stripe status, for example `active` or `past_due`. `None` without a subscription.
    pub subscription_status: Option<String>,
    /// Milliseconds since the Unix epoch.
    #[ts(type = "number | null")]
    pub current_period_end_ms: Option<i64>,
    pub cancel_at_period_end: bool,
    /// True when the user has a Stripe customer, so the Customer Portal can open.
    pub has_customer: bool,
    /// False when the server has no Stripe settings. The payment buttons are then off.
    pub billing_configured: bool,
    #[ts(type = "number")]
    pub calls_used: i64,
    #[ts(type = "number")]
    pub period_start_ms: i64,
    #[ts(type = "number")]
    pub resets_at_ms: i64,
    #[ts(type = "number")]
    pub servers_used: i64,
    #[ts(type = "number")]
    pub paused_servers: i64,
    /// Enabled tools over the plan limit, on all servers of the user.
    #[ts(type = "number")]
    pub paused_tools: i64,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/plans", get(list_plans))
        .route("/billing", get(billing))
        .route("/billing/checkout", post(checkout))
        .route("/billing/portal", post(portal))
        .route("/billing/webhook", post(webhook))
}

/// A Stripe page that the browser opens next.
#[derive(Debug, Serialize, TS)]
#[ts(export)]
pub struct BillingRedirect {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckoutRequest {
    /// The plan code, for example `pro`.
    pub plan: String,
}

/// Public, so the landing page can show the plans before sign-in.
async fn list_plans(State(state): State<AppState>) -> Result<Json<Vec<Plan>>, ApiError> {
    let plans = plan::active_plans(&state.pool)
        .await
        .map_err(ApiError::Database)?;
    Ok(Json(plans))
}

fn billing_configured(state: &AppState) -> bool {
    state.payments.is_some()
}

fn payments(state: &AppState) -> Result<&Payments, ApiError> {
    state
        .payments
        .as_deref()
        .ok_or(ApiError::BillingNotConfigured)
}

fn page_url(state: &AppState, path_and_query: &str) -> String {
    let base = state.auth.public_url.as_str().trim_end_matches('/');
    format!("{base}{path_and_query}")
}

/// Starts a Stripe Checkout. Only a user without a subscription can use it, because the
/// Customer Portal handles every change to an existing subscription.
async fn checkout(
    State(state): State<AppState>,
    auth: AuthUser,
    ApiJson(request): ApiJson<CheckoutRequest>,
) -> Result<Json<BillingRedirect>, ApiError> {
    let payments = payments(&state)?;
    let (login, customer, status, on_default): (String, Option<String>, Option<String>, bool) =
        sqlx::query_as(
            "SELECT u.github_login, u.stripe_customer_id, u.subscription_status, p.is_default
             FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = $1",
        )
        .bind(auth.user_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::Unauthorized)?;
    let live_subscription = status.is_some_and(|s| !ENDED_STATUSES.contains(&s.as_str()));
    if !on_default || live_subscription {
        return Err(ApiError::AlreadySubscribed);
    }
    let lookup_key: String = sqlx::query_scalar(
        "SELECT stripe_lookup_key FROM plans
         WHERE code = $1 AND active AND NOT is_default AND stripe_lookup_key IS NOT NULL",
    )
    .bind(&request.plan)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::Database)?
    .ok_or_else(|| ApiError::InvalidPlan(request.plan.clone()))?;
    let price_id = payments.price_id(&lookup_key).await?;
    let customer = match customer {
        Some(customer) => customer,
        None => {
            let created = payments.create_customer(auth.user_id, &login).await?;
            // Two parallel requests can both create a customer. The first one in the DB wins.
            sqlx::query_scalar(
                "UPDATE users SET stripe_customer_id = coalesce(stripe_customer_id, $2)
                 WHERE id = $1 RETURNING stripe_customer_id",
            )
            .bind(auth.user_id)
            .bind(&created)
            .fetch_one(&state.pool)
            .await
            .map_err(ApiError::Database)?
        }
    };
    let url = payments
        .checkout_url(
            auth.user_id,
            &customer,
            &price_id,
            &page_url(&state, "/billing?checkout=success"),
            &page_url(&state, "/billing"),
        )
        .await?;
    Ok(Json(BillingRedirect { url }))
}

/// Opens the Stripe Customer Portal: cancel, resume, payment method, invoices, and address.
async fn portal(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<BillingRedirect>, ApiError> {
    let payments = payments(&state)?;
    let customer: Option<String> =
        sqlx::query_scalar("SELECT stripe_customer_id FROM users WHERE id = $1")
            .bind(auth.user_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(ApiError::Database)?
            .ok_or(ApiError::Unauthorized)?;
    let customer = customer.ok_or(ApiError::NoCustomer)?;
    let url = payments
        .portal_url(&customer, &page_url(&state, "/billing"))
        .await?;
    Ok(Json(BillingRedirect { url }))
}

#[derive(Debug, Deserialize)]
struct Event {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    data: EventData,
}

#[derive(Debug, Deserialize)]
struct EventData {
    object: Value,
}

/// The subscription that an event is about, and the hints to find its user.
#[derive(Debug)]
struct Target {
    subscription_id: String,
    customer_id: Option<String>,
    user_id: Option<i64>,
}

/// An id field can hold the id or the expanded object.
fn id_of(value: &Value) -> Option<String> {
    match value {
        Value::String(id) => Some(id.clone()),
        Value::Object(object) => object.get("id")?.as_str().map(str::to_owned),
        _ => None,
    }
}

fn user_id_of(value: &Value) -> Option<i64> {
    value.as_str()?.parse().ok()
}

/// Returns `None` for an event that does not change a subscription.
fn target(event: &Event) -> Option<Target> {
    let object = &event.data.object;
    let subscription_id = match event.kind.as_str() {
        "checkout.session.completed" => {
            if object["mode"] != "subscription" {
                return None;
            }
            id_of(&object["subscription"])?
        }
        kind if kind.starts_with("customer.subscription.") => id_of(&object["id"])?,
        // Since API version 2025-03-31 the invoice names its subscription under `parent`.
        "invoice.paid" | "invoice.payment_failed" => id_of(&object["subscription"])
            .or_else(|| id_of(&object["parent"]["subscription_details"]["subscription"]))?,
        _ => return None,
    };
    let user_id = user_id_of(&object["client_reference_id"])
        .or_else(|| user_id_of(&object["metadata"][USER_ID_KEY]));
    Some(Target {
        subscription_id,
        customer_id: id_of(&object["customer"]),
        user_id,
    })
}

/// Stripe calls this route. It has no login, so the signature is the only proof that the
/// body comes from Stripe. The handler does not trust the event order: it reads the current
/// subscription from Stripe and stores that state.
async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let payments = payments(&state)?;
    let signature = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::InvalidSignature)?;
    let payload = std::str::from_utf8(&body).map_err(|_| ApiError::InvalidSignature)?;
    // The crate checks the signature and the age before it parses the body. A parse error
    // therefore means a good signature. ToolPort reads only a few fields with serde, so a
    // new Stripe API version cannot break the handler.
    match Webhook::construct_event(payload, signature, payments.webhook_secret()) {
        Ok(_) | Err(WebhookError::BadParse(_)) => {}
        Err(error) => {
            tracing::warn!(error = %error, "Stripe webhook with a bad signature");
            return Err(ApiError::InvalidSignature);
        }
    }
    let event: Event = serde_json::from_str(payload).map_err(|_| ApiError::InvalidEvent)?;
    let received = Json(serde_json::json!({ "received": true }));
    let Some(target) = target(&event) else {
        return Ok(received);
    };
    apply_event(&state, payments, &event, target).await?;
    Ok(received)
}

/// Stores the current state of the subscription. The event id goes into `stripe_events` in
/// the same transaction, so a repeated event changes nothing.
async fn apply_event(
    state: &AppState,
    payments: &Payments,
    event: &Event,
    target: Target,
) -> Result<(), ApiError> {
    let mut tx = state.pool.begin().await.map_err(ApiError::Database)?;
    let new = sqlx::query(
        "INSERT INTO stripe_events (id, type) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
    )
    .bind(&event.id)
    .bind(&event.kind)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Database)?
    .rows_affected()
        == 1;
    if !new {
        return Ok(());
    }

    // The row lock makes parallel events of one user wait. Each one then reads the newest
    // subscription from Stripe, so an older state cannot overwrite a newer one.
    type UserRow = (i64, i64, Option<String>, Option<String>);
    let user: Option<UserRow> = sqlx::query_as(
        "SELECT id, plan_id, stripe_subscription_id, subscription_status FROM users
         WHERE (stripe_customer_id = $1)
            OR (id = $2 AND (stripe_customer_id IS NULL OR stripe_customer_id = $1))
         ORDER BY stripe_customer_id = $1 DESC NULLS LAST
         LIMIT 1
         FOR UPDATE",
    )
    .bind(&target.customer_id)
    .bind(target.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::Database)?;
    let Some((user_id, old_plan_id, stored_subscription, stored_status)) = user else {
        tracing::warn!(event = %event.id, kind = %event.kind, "Stripe event for an unknown customer");
        tx.commit().await.map_err(ApiError::Database)?;
        return Ok(());
    };

    let sub = payments.subscription(&target.subscription_id).await?;
    let paid = PAID_STATUSES.contains(&sub.status.as_str());

    // A late event of an old subscription must not end a newer one.
    let other_live = stored_subscription
        .as_deref()
        .is_some_and(|id| id != sub.id)
        && stored_status.is_some_and(|s| !ENDED_STATUSES.contains(&s.as_str()));
    if other_live && !paid {
        tracing::info!(user_id, subscription = %sub.id, "ignored an old Stripe subscription");
        tx.commit().await.map_err(ApiError::Database)?;
        return Ok(());
    }

    let paid_plan: Option<i64> = match (&sub.lookup_key, paid) {
        (Some(key), true) => {
            sqlx::query_scalar("SELECT id FROM plans WHERE stripe_lookup_key = $1")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await
                .map_err(ApiError::Database)?
        }
        _ => None,
    };
    if paid && paid_plan.is_none() {
        tracing::warn!(
            user_id,
            lookup_key = ?sub.lookup_key,
            "no plan has the lookup key of the Stripe price, the user keeps the default plan"
        );
    }
    let plan_id: i64 = match paid_plan {
        Some(id) => id,
        None => sqlx::query_scalar("SELECT id FROM plans WHERE is_default")
            .fetch_one(&mut *tx)
            .await
            .map_err(ApiError::Database)?,
    };

    sqlx::query(
        "UPDATE users SET plan_id = $2, stripe_customer_id = $3, stripe_subscription_id = $4,
                subscription_status = $5, current_period_start = to_timestamp($6::bigint),
                current_period_end = to_timestamp($7::bigint), cancel_at_period_end = $8,
                updated_at = now()
         WHERE id = $1",
    )
    .bind(user_id)
    .bind(plan_id)
    .bind(&sub.customer_id)
    .bind(&sub.id)
    .bind(&sub.status)
    .bind(sub.period_start)
    .bind(sub.period_end)
    .bind(sub.cancel_at_period_end)
    .execute(&mut *tx)
    .await
    .map_err(ApiError::Database)?;
    tx.commit().await.map_err(ApiError::Database)?;
    tracing::info!(user_id, status = %sub.status, plan_id, "Stripe subscription stored");

    if plan_id != old_plan_id {
        plan::plan_changed(state, user_id)
            .await
            .map_err(ApiError::Database)?;
    }
    Ok(())
}

async fn billing(State(state): State<AppState>, auth: AuthUser) -> Result<Json<Billing>, ApiError> {
    let account = plan::account_plan(&state.pool, auth.user_id)
        .await
        .map_err(ApiError::Database)?
        .ok_or(ApiError::Unauthorized)?;
    let calls_used = plan::calls_used(&state.pool, auth.user_id, &account)
        .await
        .map_err(ApiError::Database)?;
    type Row = (Option<String>, Option<i64>, bool, bool, i64, i64, i64);
    let (status, period_end_ms, cancel, has_customer, servers, paused_servers, paused_tools): Row =
        sqlx::query_as(
            "SELECT u.subscription_status,
                    (extract(epoch FROM u.current_period_end) * 1000)::bigint,
                    u.cancel_at_period_end,
                    u.stripe_customer_id IS NOT NULL,
                    (SELECT count(*) FROM servers s WHERE s.user_id = u.id),
                    (SELECT greatest(count(*) - coalesce(p.max_servers, count(*)), 0)
                        FROM servers s WHERE s.user_id = u.id),
                    (SELECT count(*) FROM (
                        SELECT row_number() OVER (PARTITION BY t.server_id ORDER BY t.id) AS n
                        FROM tools t JOIN servers s ON s.id = t.server_id
                        WHERE s.user_id = u.id AND t.enabled
                     ) e WHERE e.n > p.max_tools_per_server)
             FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = $1",
        )
        .bind(auth.user_id)
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::Database)?;
    Ok(Json(Billing {
        plan: account.plan,
        subscription_status: status,
        current_period_end_ms: period_end_ms,
        cancel_at_period_end: cancel,
        has_customer,
        billing_configured: billing_configured(&state),
        calls_used,
        period_start_ms: account.period_start_secs * 1000,
        resets_at_ms: account.resets_at_secs * 1000,
        servers_used: servers,
        paused_servers,
        paused_tools,
    }))
}
