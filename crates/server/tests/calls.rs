mod common;

use server::calls;
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
