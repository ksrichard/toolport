//! The plan list and the "Plan & billing" data of the signed-in user.

use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use ts_rs::TS;

use crate::{
    auth::AuthUser,
    error::ApiError,
    plan::{self, Plan},
    state::AppState,
};

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
}

/// Public, so the landing page can show the plans before sign-in.
async fn list_plans(State(state): State<AppState>) -> Result<Json<Vec<Plan>>, ApiError> {
    let plans = plan::active_plans(&state.pool)
        .await
        .map_err(ApiError::Database)?;
    Ok(Json(plans))
}

/// The Stripe part sets this to true when the payment settings are present.
fn billing_configured(_state: &AppState) -> bool {
    false
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
