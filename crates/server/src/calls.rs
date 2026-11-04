//! The call log and the metrics of the Live page.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use ts_rs::TS;

use crate::error::error_chain;

/// The metrics use the calls of this many last seconds.
pub const METRICS_WINDOW_SECS: i64 = 60;
/// The chart shows this many seconds, in buckets of `BUCKET_SECS`.
pub const CHART_SECS: i64 = 300;
pub const BUCKET_SECS: i64 = 5;
/// Rows that one cleanup statement deletes. Small batches keep each lock short.
pub const CLEANUP_BATCH: u32 = 5000;

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

/// A time window of the Live chart that the SSE stream does not cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, TS)]
#[ts(export)]
pub enum SeriesWindow {
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "30d")]
    Month,
}

impl SeriesWindow {
    /// The bucket size in seconds. Each window has 60 to 168 buckets.
    pub fn bucket_secs(self) -> i64 {
        match self {
            Self::Hour => 60,
            Self::Day => 15 * 60,
            Self::Week => 3600,
            Self::Month => 6 * 3600,
        }
    }

    pub fn window_secs(self) -> i64 {
        match self {
            Self::Hour => 3600,
            Self::Day => 24 * 3600,
            Self::Week => 7 * 24 * 3600,
            Self::Month => 30 * 24 * 3600,
        }
    }

    pub fn bucket_count(self) -> i64 {
        self.window_secs() / self.bucket_secs()
    }
}

/// The calls of one server over a long window. It has every bucket, also the empty ones.
#[derive(Debug, Clone, Serialize, PartialEq, TS)]
#[ts(export)]
pub struct CallSeries {
    #[ts(type = "number")]
    pub bucket_secs: i64,
    /// The oldest bucket first. The last bucket holds the current time.
    pub buckets: Vec<Bucket>,
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

/// The buckets start on multiples of the bucket size since the Unix epoch, in UTC.
pub async fn series(
    pool: &PgPool,
    server_id: i64,
    window: SeriesWindow,
) -> Result<CallSeries, sqlx::Error> {
    let bucket_secs = window.bucket_secs();
    let buckets = sqlx::query_as(
        "WITH b AS (
             SELECT make_interval(secs => $2) AS size,
                    date_bin(make_interval(secs => $2), now(), TIMESTAMPTZ 'epoch') AS last
         ),
         slots AS (
             SELECT generate_series(b.last - b.size * ($3 - 1), b.last, b.size) AS start FROM b
         ),
         counts AS (
             SELECT date_bin(b.size, c.created_at, TIMESTAMPTZ 'epoch') AS start,
                    count(*) AS calls,
                    count(*) FILTER (WHERE c.result <> 'ok') AS errors
             FROM calls c, b
             WHERE c.server_id = $1 AND c.created_at >= b.last - b.size * ($3 - 1)
             GROUP BY 1
         )
         SELECT (extract(epoch FROM s.start) * 1000)::bigint AS start_ms,
                coalesce(n.calls, 0) AS calls,
                coalesce(n.errors, 0) AS errors
         FROM slots s LEFT JOIN counts n ON n.start = s.start
         ORDER BY s.start",
    )
    .bind(server_id)
    .bind(bucket_secs as f64)
    .bind(window.bucket_count() as i32)
    .fetch_all(pool)
    .await?;
    Ok(CallSeries {
        bucket_secs,
        buckets,
    })
}

/// Deletes the calls that are older than `retention_days`, in batches of `batch` rows.
/// `retention_days = 0` keeps all calls. Returns the number of deleted rows.
pub async fn delete_old_calls(
    pool: &PgPool,
    retention_days: u32,
    batch: u32,
) -> Result<u64, sqlx::Error> {
    if retention_days == 0 {
        return Ok(0);
    }
    let days = i32::try_from(retention_days).unwrap_or(i32::MAX);
    let mut deleted = 0;
    loop {
        let rows = sqlx::query(
            "DELETE FROM calls WHERE id IN (
                 SELECT id FROM calls WHERE created_at < now() - make_interval(days => $1)
                 LIMIT $2
             )",
        )
        .bind(days)
        .bind(i64::from(batch))
        .execute(pool)
        .await?
        .rows_affected();
        deleted += rows;
        if rows == 0 || rows < u64::from(batch) {
            return Ok(deleted);
        }
    }
}

/// Runs `delete_old_calls` at each interval until the process stops.
pub fn spawn_cleanup(pool: PgPool, retention_days: u32, interval: Duration) {
    if retention_days == 0 {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match delete_old_calls(&pool, retention_days, CLEANUP_BATCH).await {
                Ok(0) => {}
                Ok(deleted) => tracing::info!(deleted, retention_days, "old calls deleted"),
                Err(error) => {
                    tracing::error!(error = %error_chain(&error), "call cleanup failed");
                }
            }
        }
    });
}
