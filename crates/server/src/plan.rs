//! Plans and their limits. The limits live in the `plans` table, and each user row points to
//! one plan with `plan_id`. The server reads the plan with each request and has no cache,
//! so a change in the table applies to the next request.

use serde::Serialize;
use sqlx::{PgConnection, PgExecutor, PgPool};
use ts_rs::TS;

use crate::state::AppState;

/// One row of the `plans` table. `None` in a `max_*` field means no limit.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, TS)]
#[ts(export)]
pub struct Plan {
    #[serde(skip)]
    #[ts(skip)]
    pub id: i64,
    /// For example `free` or `pro`.
    pub code: String,
    pub name: String,
    pub price_cents: i32,
    /// Lowercase ISO code, for example `usd`.
    pub currency: String,
    /// `month`, `year`, or `None` for a free plan.
    pub interval: Option<String>,
    /// The lookup key of the Stripe price. Only the Stripe code needs it.
    #[serde(skip)]
    #[ts(skip)]
    pub stripe_lookup_key: Option<String>,
    pub max_servers: Option<i32>,
    pub max_tools_per_server: Option<i32>,
    pub max_calls_per_month: Option<i32>,
    pub history_hours: i32,
    pub is_default: bool,
}

/// The limits of a plan, in the types that the checks use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanLimits {
    pub servers: Option<i64>,
    pub tools_per_server: Option<i64>,
    pub calls_per_month: Option<i64>,
    pub history_hours: i64,
}

impl Plan {
    pub fn limits(&self) -> PlanLimits {
        PlanLimits {
            servers: self.max_servers.map(i64::from),
            tools_per_server: self.max_tools_per_server.map(i64::from),
            calls_per_month: self.max_calls_per_month.map(i64::from),
            history_hours: i64::from(self.history_hours),
        }
    }
}

/// The plan of a user and the current quota period.
#[derive(Debug, Clone)]
pub struct AccountPlan {
    pub plan: Plan,
    /// Seconds since the Unix epoch. The quota counter uses it as its key.
    pub period_start_secs: i64,
    pub resets_at_secs: i64,
    /// The reset day in UTC, for example `2026-12-01`.
    pub resets_on: String,
}

#[derive(Debug, sqlx::FromRow)]
struct AccountRow {
    #[sqlx(flatten)]
    plan: Plan,
    period_start_secs: i64,
    resets_at_secs: i64,
    resets_on: String,
}

/// The columns of a `Plan`. A macro, because sqlx needs static SQL strings.
macro_rules! plan_columns {
    () => {
        "p.id, p.code, p.name, p.price_cents, p.currency, p.interval, p.stripe_lookup_key,
         p.max_servers, p.max_tools_per_server, p.max_calls_per_month, p.history_hours,
         p.is_default"
    };
}

/// The plans that users can choose, in display order.
pub async fn active_plans(pool: &PgPool) -> Result<Vec<Plan>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        plan_columns!(),
        " FROM plans p WHERE p.active ORDER BY p.sort_order, p.id"
    ))
    .fetch_all(pool)
    .await
}

/// Returns the plan of a user and the quota period. `None` when the user does not exist.
///
/// The default plan counts per calendar month in UTC. A paid plan counts per Stripe billing
/// period. When the stored period is over (for example a webhook is late), the period moves
/// forward in steps of one month, so the quota still resets.
pub async fn account_plan<'e>(
    db: impl PgExecutor<'e>,
    user_id: i64,
) -> Result<Option<AccountPlan>, sqlx::Error> {
    let row: Option<AccountRow> = sqlx::query_as(concat!(
        "WITH a AS (
             SELECT ",
        plan_columns!(),
        ",
                    u.current_period_start AS ps, u.current_period_end AS pe,
                    (p.is_default OR u.current_period_start IS NULL
                        OR u.current_period_end IS NULL
                        OR u.current_period_end <= u.current_period_start) AS calendar
             FROM users u JOIN plans p ON p.id = u.plan_id WHERE u.id = $1
         ),
         b AS (
             SELECT a.*,
                    CASE
                        WHEN a.calendar THEN date_trunc('month', now(), 'UTC')
                        WHEN now() < a.pe THEN a.ps
                        ELSE (SELECT max(s) FROM generate_series(a.pe AT TIME ZONE 'UTC',
                                  now() AT TIME ZONE 'UTC', INTERVAL '1 month') s) AT TIME ZONE 'UTC'
                    END AS start
             FROM a
         ),
         c AS (
             SELECT b.*,
                    CASE
                        WHEN NOT b.calendar AND now() < b.pe THEN b.pe
                        ELSE (b.start AT TIME ZONE 'UTC' + INTERVAL '1 month') AT TIME ZONE 'UTC'
                    END AS reset
             FROM b
         )
         SELECT c.id, c.code, c.name, c.price_cents, c.currency, c.interval, c.stripe_lookup_key,
                c.max_servers, c.max_tools_per_server, c.max_calls_per_month, c.history_hours,
                c.is_default,
                extract(epoch FROM c.start)::bigint AS period_start_secs,
                extract(epoch FROM c.reset)::bigint AS resets_at_secs,
                to_char(c.reset AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS resets_on
         FROM c"
    ))
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|row| AccountPlan {
        plan: row.plan,
        period_start_secs: row.period_start_secs,
        resets_at_secs: row.resets_at_secs,
        resets_on: row.resets_on,
    }))
}

/// Errors when an action would go over a plan limit.
#[derive(Debug, thiserror::Error)]
pub enum LimitError {
    #[error("the {plan} plan allows {} {}, upgrade your plan or delete a server", .max, servers(*.max))]
    Servers { plan: String, max: i64 },
    #[error(
        "the {plan} plan allows {max} enabled tools per server, turn off another tool or upgrade your plan"
    )]
    Tools { plan: String, max: i64 },
    #[error("the user does not exist")]
    UnknownUser,
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
}

fn servers(count: i64) -> &'static str {
    if count == 1 { "server" } else { "servers" }
}

/// The message for an MCP request to a paused server.
pub fn paused_server_message(plan: &Plan) -> String {
    let max = plan.max_servers.unwrap_or(0);
    format!(
        "this server is paused: the {} plan allows {max} {}. Upgrade or delete a server.",
        plan.name,
        servers(i64::from(max))
    )
}

/// The message for an MCP call to an enabled tool over the plan limit.
pub fn paused_tool_message(plan: &Plan, tool: &str) -> String {
    format!(
        "the tool {tool} is paused: the {} plan allows {} enabled tools per server. Upgrade or turn off another tool.",
        plan.name,
        plan.max_tools_per_server.unwrap_or(0)
    )
}

/// The tool result text for a call over the monthly quota.
pub fn quota_message(account: &AccountPlan) -> String {
    format!(
        "monthly tool call limit reached ({} on the {} plan), resets on {}",
        account.plan.max_calls_per_month.unwrap_or(0),
        account.plan.name,
        account.resets_on
    )
}

/// Locks the user row until the transaction ends. Two requests that add servers for the same
/// user then run one after the other, so both cannot pass the limit check.
async fn lock_user(tx: &mut PgConnection, user_id: i64) -> Result<(), LimitError> {
    sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(LimitError::UnknownUser)?;
    Ok(())
}

/// Checks that the user can have `extra` more servers. `extra = 0` checks the servers that
/// exist now, after the caller inserted a new one in the same transaction.
pub async fn check_server_limit(
    tx: &mut PgConnection,
    user_id: i64,
    extra: i64,
) -> Result<(), LimitError> {
    lock_user(tx, user_id).await?;
    let account = account_plan(&mut *tx, user_id)
        .await?
        .ok_or(LimitError::UnknownUser)?;
    let Some(max) = account.plan.limits().servers else {
        return Ok(());
    };
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM servers WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    if count + extra > max {
        return Err(LimitError::Servers {
            plan: account.plan.name,
            max,
        });
    }
    Ok(())
}

/// The room for more enabled tools on a server. `None` means no limit.
/// It locks the server row, so two requests cannot both take the last free place.
pub async fn tool_room(tx: &mut PgConnection, server_id: i64) -> Result<ToolRoom, LimitError> {
    // The count is a second statement, so it sees the tools that a request committed while
    // this one waited for the lock.
    sqlx::query_scalar::<_, i64>("SELECT id FROM servers WHERE id = $1 FOR NO KEY UPDATE")
        .bind(server_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(LimitError::UnknownUser)?;
    let (enabled, max, plan): (i64, Option<i32>, String) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM tools t WHERE t.server_id = s.id AND t.enabled),
                p.max_tools_per_server, p.name
         FROM servers s JOIN users u ON u.id = s.user_id JOIN plans p ON p.id = u.plan_id
         WHERE s.id = $1",
    )
    .bind(server_id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(ToolRoom {
        enabled,
        max: max.map(i64::from),
        plan,
    })
}

#[derive(Debug, Clone)]
pub struct ToolRoom {
    pub enabled: i64,
    pub max: Option<i64>,
    pub plan: String,
}

impl ToolRoom {
    /// True when one more tool can be on. It counts the tool as on.
    pub fn take(&mut self) -> bool {
        if self.max.is_some_and(|max| self.enabled >= max) {
            return false;
        }
        self.enabled += 1;
        true
    }

    pub fn error(&self) -> LimitError {
        LimitError::Tools {
            plan: self.plan.clone(),
            max: self.max.unwrap_or(0),
        }
    }
}

/// True when the server is one of the oldest `max_servers` servers of its owner.
pub async fn server_is_served(
    pool: &PgPool,
    server_id: i64,
    plan: &Plan,
) -> Result<bool, sqlx::Error> {
    let Some(max) = plan.limits().servers else {
        return Ok(true);
    };
    let older: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM servers s JOIN servers me ON me.user_id = s.user_id
         WHERE me.id = $1 AND (s.created_at, s.id) < (me.created_at, me.id)",
    )
    .bind(server_id)
    .fetch_one(pool)
    .await?;
    Ok(older < max)
}

/// Takes one call from the monthly quota with one atomic statement. Returns false when the
/// quota is used up. A plan without a call limit still counts, so the dashboard shows usage.
pub async fn reserve_call(
    pool: &PgPool,
    user_id: i64,
    account: &AccountPlan,
) -> Result<bool, sqlx::Error> {
    let limit = account.plan.max_calls_per_month;
    if limit.is_some_and(|max| max <= 0) {
        return Ok(false);
    }
    let calls: Option<i32> = sqlx::query_scalar(
        "INSERT INTO usage (user_id, period_start, calls) VALUES ($1, to_timestamp($2), 1)
         ON CONFLICT (user_id, period_start) DO UPDATE SET calls = usage.calls + 1
         WHERE $3::int IS NULL OR usage.calls < $3
         RETURNING calls",
    )
    .bind(user_id)
    .bind(account.period_start_secs)
    .bind(limit)
    .fetch_optional(pool)
    .await?;
    Ok(calls.is_some())
}

/// Gives back a reserved call that did not reach the upstream API.
pub async fn release_call(
    pool: &PgPool,
    user_id: i64,
    account: &AccountPlan,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE usage SET calls = calls - 1
         WHERE user_id = $1 AND period_start = to_timestamp($2) AND calls > 0",
    )
    .bind(user_id)
    .bind(account.period_start_secs)
    .execute(pool)
    .await?;
    Ok(())
}

/// The calls of the user in the current quota period.
pub async fn calls_used(
    pool: &PgPool,
    user_id: i64,
    account: &AccountPlan,
) -> Result<i64, sqlx::Error> {
    let calls: Option<i32> = sqlx::query_scalar(
        "SELECT calls FROM usage WHERE user_id = $1 AND period_start = to_timestamp($2)",
    )
    .bind(user_id)
    .bind(account.period_start_secs)
    .fetch_optional(pool)
    .await?;
    Ok(calls.map_or(0, i64::from))
}

/// Tells the open MCP sessions of every server of the user that the tool list changed.
/// Call it after a change of `users.plan_id`, because the served servers and tools change
/// with the plan. A server delete calls it too, because a paused server can become served.
pub async fn plan_changed(state: &AppState, user_id: i64) -> Result<(), sqlx::Error> {
    let servers: Vec<i64> = sqlx::query_scalar("SELECT id FROM servers WHERE user_id = $1")
        .bind(user_id)
        .fetch_all(&state.pool)
        .await?;
    for server_id in servers {
        state.peers.notify_tools_changed(server_id).await;
    }
    Ok(())
}
