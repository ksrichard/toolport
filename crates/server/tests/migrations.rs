mod common;

use sqlx::PgPool;

async fn insert_api(pool: &PgPool, kind: &str, spec: Option<&str>) -> Result<i64, sqlx::Error> {
    let user = common::create_user(pool, 1, "owner").await;
    let server: i64 =
        sqlx::query_scalar("INSERT INTO servers (user_id, name) VALUES ($1, 'srv') RETURNING id")
            .bind(user)
            .fetch_one(pool)
            .await?;
    sqlx::query_scalar(
        "INSERT INTO apis (server_id, name, base_url, kind, spec) \
         VALUES ($1, 'petstore', 'https://example.test', $2, $3) RETURNING id",
    )
    .bind(server)
    .bind(kind)
    .bind(spec)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_api_with_a_spec_is_rejected(pool: PgPool) {
    assert!(insert_api(&pool, "manual", Some("{}")).await.is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn manual_api_without_a_spec_is_accepted(pool: PgPool) {
    insert_api(&pool, "manual", None).await.unwrap();
}

/// The migration must fill the new columns of rows that exist before it runs.
#[sqlx::test(migrations = false)]
async fn old_tool_rows_get_spec_tool_and_operation(pool: PgPool) {
    let all = sqlx::migrate!("../../migrations");
    let before = sqlx::migrate::Migrator {
        migrations: all
            .migrations
            .iter()
            .filter(|m| m.version < 20261103000000)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await.unwrap();

    let user = common::create_user(&pool, 1, "owner").await;
    let server: i64 =
        sqlx::query_scalar("INSERT INTO servers (user_id, name) VALUES ($1, 'srv') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    let api: i64 = sqlx::query_scalar(
        "INSERT INTO apis (server_id, name, base_url, spec) \
         VALUES ($1, 'petstore', 'https://example.test', '{}') RETURNING id",
    )
    .bind(server)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tools (api_id, server_id, name, description, input_schema, request) \
         VALUES ($1, $2, 'getPetById', 'Find a pet', '{}', \
         '{\"method\": \"GET\", \"path\": \"/pet/{petId}\"}')",
    )
    .bind(api)
    .bind(server)
    .execute(&pool)
    .await
    .unwrap();

    all.run(&pool).await.unwrap();

    let (name, operation, source): (String, String, String) = sqlx::query_as(
        "SELECT spec_tool->>'name', operation, source FROM tools WHERE name = 'getPetById'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(name, "getPetById");
    assert_eq!(operation, "GET /pet/{petId}");
    assert_eq!(source, "spec");
}

/// The migration deletes revoked tokens, keeps active ones, and keeps the token name on calls.
#[sqlx::test(migrations = false)]
async fn revoked_tokens_are_deleted_and_calls_keep_the_name(pool: PgPool) {
    let all = sqlx::migrate!("../../migrations");
    let before = sqlx::migrate::Migrator {
        migrations: all
            .migrations
            .iter()
            .filter(|m| m.version < 20261104000003)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&pool).await.unwrap();

    let user = common::create_user(&pool, 1, "owner").await;
    let server: i64 =
        sqlx::query_scalar("INSERT INTO servers (user_id, name) VALUES ($1, 'srv') RETURNING id")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
    for (jti, name, revoked) in [("a", "active", false), ("r", "old laptop", true)] {
        let token: i64 = sqlx::query_scalar(
            "INSERT INTO mcp_tokens (server_id, jti, name, revoked_at) \
             VALUES ($1, $2, $3, CASE WHEN $4 THEN now() END) RETURNING id",
        )
        .bind(server)
        .bind(jti)
        .bind(name)
        .bind(revoked)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO calls (server_id, token_id, tool, result, latency_ms) \
             VALUES ($1, $2, 'getPet', 'ok', 5)",
        )
        .bind(server)
        .bind(token)
        .execute(&pool)
        .await
        .unwrap();
    }

    all.run(&pool).await.unwrap();

    let tokens: Vec<(String, String)> =
        sqlx::query_as("SELECT jti, name FROM mcp_tokens ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(tokens, [("a".to_owned(), "active".to_owned())]);
    let calls: Vec<(Option<String>, bool)> =
        sqlx::query_as("SELECT token_name, token_id IS NULL FROM calls ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        calls,
        [
            (Some("active".to_owned()), false),
            (Some("old laptop".to_owned()), true)
        ]
    );
}
