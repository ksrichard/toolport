//! The call log and the metrics of the Live page.

use serde::Serialize;
use sqlx::PgPool;
use ts_rs::TS;

/// The metrics use the calls of this many last seconds.
pub const METRICS_WINDOW_SECS: i64 = 60;
/// The chart shows this many seconds, in buckets of `BUCKET_SECS`.
pub const CHART_SECS: i64 = 300;
pub const BUCKET_SECS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, sqlx::Type, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum CallResult {
    Ok,
    NotFound,
    UpstreamError,
    TimedOut,
}

pub struct NewCall<'a> {
    pub server_id: i64,
    pub token_id: i64,
    pub client_name: Option<&'a str>,
    pub tool: &'a str,
    pub result: CallResult,
    pub http_status: Option<u16>,
    pub latency_ms: u32,
}

/// One row of the call log, as the dashboard gets it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, TS)]
#[ts(export)]
pub struct CallRecord {
    #[ts(type = "number")]
    pub id: i64,
    /// Milliseconds since the Unix epoch.
    #[ts(type = "number")]
    pub at_ms: i64,
    pub tool: String,
    pub client_name: Option<String>,
    pub token_name: Option<String>,
    pub result: CallResult,
    pub http_status: Option<i16>,
    pub latency_ms: i32,
}

#[derive(Debug, Clone, Serialize, PartialEq, TS)]
#[ts(export)]
pub struct Metrics {
    #[ts(type = "number")]
    pub calls_per_minute: i64,
    /// From 0.0 to 1.0. Every result except `ok` counts as an error.
    pub error_rate: f64,
    /// `None` when the window has no calls.
    pub p50_ms: Option<i32>,
    pub p95_ms: Option<i32>,
}

/// The chart buckets that have calls. The dashboard fills the empty ones.
#[derive(Debug, Clone, Serialize, sqlx::FromRow, PartialEq, TS)]
#[ts(export)]
pub struct Bucket {
    /// The bucket start in milliseconds since the Unix epoch. It is a multiple of `BUCKET_SECS`.
    #[ts(type = "number")]
    pub start_ms: i64,
    #[ts(type = "number")]
    pub calls: i64,
    #[ts(type = "number")]
    pub errors: i64,
}

/// The columns of a `CallRecord`. A macro, because sqlx needs static SQL strings and
/// `concat!` takes only literals.
macro_rules! record_columns {
    () => {
        "c.id, (extract(epoch FROM c.created_at) * 1000)::bigint AS at_ms, c.tool, c.client_name,
         t.name AS token_name, c.result, c.http_status, c.latency_ms"
    };
}

pub async fn record(pool: &PgPool, call: NewCall<'_>) -> Result<CallRecord, sqlx::Error> {
    let latency_ms = i32::try_from(call.latency_ms).unwrap_or(i32::MAX);
    let http_status = call.http_status.and_then(|s| i16::try_from(s).ok());
    sqlx::query_as(concat!(
        "WITH c AS (
             INSERT INTO calls (server_id, token_id, client_name, tool, result, http_status, latency_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *
         ) SELECT ",
        record_columns!(),
        " FROM c LEFT JOIN mcp_tokens t ON t.id = c.token_id"
    ))
    .bind(call.server_id)
    .bind(call.token_id)
    .bind(call.client_name)
    .bind(call.tool)
    .bind(call.result)
    .bind(http_status)
    .bind(latency_ms)
    .fetch_one(pool)
    .await
}

/// The newest calls first.
pub async fn recent(
    pool: &PgPool,
    server_id: i64,
    limit: i64,
) -> Result<Vec<CallRecord>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT ",
        record_columns!(),
        " FROM calls c LEFT JOIN mcp_tokens t ON t.id = c.token_id
         WHERE c.server_id = $1 ORDER BY c.id DESC LIMIT $2"
    ))
    .bind(server_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn metrics(pool: &PgPool, server_id: i64) -> Result<Metrics, sqlx::Error> {
    let (calls, errors, p50_ms, p95_ms): (i64, i64, Option<i32>, Option<i32>) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE result <> 'ok'),
                percentile_disc(0.5) WITHIN GROUP (ORDER BY latency_ms),
                percentile_disc(0.95) WITHIN GROUP (ORDER BY latency_ms)
         FROM calls WHERE server_id = $1 AND created_at > now() - make_interval(secs => $2)",
    )
    .bind(server_id)
    .bind(METRICS_WINDOW_SECS as f64)
    .fetch_one(pool)
    .await?;
    Ok(Metrics {
        calls_per_minute: calls * 60 / METRICS_WINDOW_SECS,
        error_rate: if calls == 0 {
            0.0
        } else {
            errors as f64 / calls as f64
        },
        p50_ms,
        p95_ms,
    })
}

pub async fn buckets(pool: &PgPool, server_id: i64) -> Result<Vec<Bucket>, sqlx::Error> {
    sqlx::query_as(
        "SELECT (floor(extract(epoch FROM created_at) / $2) * $2 * 1000)::bigint AS start_ms,
                count(*) AS calls,
                count(*) FILTER (WHERE result <> 'ok') AS errors
         FROM calls WHERE server_id = $1 AND created_at > now() - make_interval(secs => $3)
         GROUP BY 1 ORDER BY 1",
    )
    .bind(server_id)
    .bind(BUCKET_SECS as f64)
    .bind(CHART_SECS as f64)
    .fetch_all(pool)
    .await
}
