mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use server::{calls, state::AppState};
use sqlx::PgPool;

/// Makes a server with `old` calls of 31 days ago and `new` calls of now.
async fn server_with_calls(pool: &PgPool, old: i32, new: i32) -> i64 {
    let user = common::create_user(pool, 1, "octocat").await;
    let server: i64 =
        sqlx::query_scalar("INSERT INTO servers (user_id, name) VALUES ($1, 'pets') RETURNING id")
            .bind(user)
            .fetch_one(pool)
            .await
            .unwrap();
    for (count, age) in [(old, "31 days"), (new, "0 days")] {
        sqlx::query(
            "INSERT INTO calls (server_id, tool, result, latency_ms, created_at)
             SELECT $1, 'tool_' || n, 'ok', 10, now() - $3::interval
             FROM generate_series(1, $2) AS n",
        )
        .bind(server)
        .bind(count)
        .bind(age)
        .execute(pool)
        .await
        .unwrap();
    }
    server
}

async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM calls")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn old_calls_are_deleted_and_new_calls_stay(pool: PgPool) {
    let server = server_with_calls(&pool, 3, 2).await;
    let deleted = calls::delete_old_calls(&pool, 30, calls::CLEANUP_BATCH)
        .await
        .unwrap();
    assert_eq!(deleted, 3);
    assert_eq!(count(&pool).await, 2);

    // The Live page data still loads from the rows that stay.
    assert_eq!(calls::recent(&pool, server, 10).await.unwrap().len(), 2);
    assert_eq!(
        calls::metrics(&pool, server)
            .await
            .unwrap()
            .calls_per_minute,
        2
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn cleanup_deletes_in_many_batches(pool: PgPool) {
    server_with_calls(&pool, 7, 1).await;
    let deleted = calls::delete_old_calls(&pool, 30, 2).await.unwrap();
    assert_eq!(deleted, 7);
    assert_eq!(count(&pool).await, 1);
}

#[sqlx::test(migrations = "../../migrations")]
async fn calls_inside_the_retention_stay(pool: PgPool) {
    server_with_calls(&pool, 3, 2).await;
    let deleted = calls::delete_old_calls(&pool, 60, calls::CLEANUP_BATCH)
        .await
        .unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(count(&pool).await, 5);
}

#[sqlx::test(migrations = "../../migrations")]
async fn zero_retention_keeps_all_calls(pool: PgPool) {
    server_with_calls(&pool, 3, 2).await;
    let deleted = calls::delete_old_calls(&pool, 0, calls::CLEANUP_BATCH)
        .await
        .unwrap();
    assert_eq!(deleted, 0);
    assert_eq!(count(&pool).await, 5);
}

async fn get_series(state: &AppState, user_id: i64, path: &str) -> (StatusCode, Value) {
    let access = state.auth.jwt.issue_access(user_id).unwrap();
    let request = Request::get(path)
        .header(header::AUTHORIZATION, format!("Bearer {access}"))
        .body(Body::empty())
        .unwrap();
    let response = common::send(state, request).await;
    let status = response.status();
    (status, common::json_body(response).await)
}

/// Adds calls next to the start of the bucket before the current 6 h bucket.
async fn insert_at_bucket_edges(pool: &PgPool, server: i64) {
    sqlx::query(
        "WITH b AS (SELECT date_bin('6 hours', now(), TIMESTAMPTZ 'epoch') - interval '6 hours' AS start)
         INSERT INTO calls (server_id, tool, result, latency_ms, created_at)
         SELECT $1, 'edge', r, 10, t FROM b, LATERAL (VALUES
             ('ok', b.start),
             ('timed_out', b.start),
             ('ok', b.start - interval '1 microsecond'),
             ('ok', now() - interval '31 days')
         ) AS v (r, t)",
    )
    .bind(server)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrations = "../../migrations")]
async fn series_has_every_bucket_and_exact_edges(pool: PgPool) {
    let server = server_with_calls(&pool, 0, 0).await;
    insert_at_bucket_edges(&pool, server).await;
    let state = common::state(pool.clone()).await;

    let (status, body) = get_series(&state, 1, "/api/servers/pets/calls/series?window=30d").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["bucket_secs"], 6 * 3600);
    let buckets = body["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 120);

    let last: i64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM date_bin('6 hours', now(), TIMESTAMPTZ 'epoch')) * 1000)::bigint",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let step = 6 * 3600 * 1000;
    for (i, bucket) in buckets.iter().enumerate() {
        assert_eq!(bucket["start_ms"], last - (119 - i as i64) * step);
    }
    let counts: Vec<(i64, i64)> = buckets
        .iter()
        .map(|b| (b["calls"].as_i64().unwrap(), b["errors"].as_i64().unwrap()))
        .collect();
    assert_eq!(counts[119], (0, 0));
    assert_eq!(counts[118], (2, 1));
    assert_eq!(counts[117], (1, 0));
    // The call of 31 days ago is outside the window, so all other buckets are empty.
    assert!(counts[..117].iter().all(|&c| c == (0, 0)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn each_window_has_its_bucket_size(pool: PgPool) {
    server_with_calls(&pool, 0, 3).await;
    let state = common::state(pool).await;
    for (window, bucket_secs, count) in [
        ("1h", 60, 60),
        ("24h", 900, 96),
        ("7d", 3600, 168),
        ("30d", 21_600, 120),
    ] {
        let path = format!("/api/servers/pets/calls/series?window={window}");
        let (status, body) = get_series(&state, 1, &path).await;
        assert_eq!(status, StatusCode::OK, "{window}");
        assert_eq!(body["bucket_secs"], bucket_secs, "{window}");
        let buckets = body["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), count, "{window}");
        let total: i64 = buckets.iter().map(|b| b["calls"].as_i64().unwrap()).sum();
        assert_eq!(total, 3, "{window}");
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn series_needs_an_own_server_and_a_known_window(pool: PgPool) {
    server_with_calls(&pool, 0, 1).await;
    common::create_user(&pool, 2, "other").await;
    let state = common::state(pool).await;

    let (status, body) = get_series(&state, 2, "/api/servers/pets/calls/series?window=1h").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");

    for query in ["window=5m", "window=", ""] {
        let path = format!("/api/servers/pets/calls/series?{query}");
        let (status, body) = get_series(&state, 1, &path).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["error"], "invalid_query", "{query}");
    }
}
