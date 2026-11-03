mod common;

use reqwest::Url;
use server::import::{ImportError, ImportRequest, Owner, import_spec};
use sqlx::PgPool;
use toolport_core::SpecError;

const PETSTORE: &str = include_str!("../../core/tests/fixtures/petstore.json");

fn petstore<'a>(server: &'a str, api_name: Option<&'a str>) -> ImportRequest<'a> {
    ImportRequest {
        owner: Owner::Login("octocat"),
        server,
        api_name,
        spec_text: PETSTORE,
        base_url: Some(Url::parse("https://petstore3.swagger.io/api/v3").unwrap()),
    }
}

async fn tool_count(pool: &PgPool, server: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM tools t JOIN servers s ON s.id = t.server_id WHERE s.name = $1",
    )
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn petstore_import_creates_the_server_and_the_tools(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    let report = import_spec(&pool, petstore("petstore", None))
        .await
        .unwrap();
    assert!(report.server_created);
    assert!(!report.replaced);
    assert_eq!(report.api_name, "Swagger Petstore - OpenAPI 3.0");
    assert_eq!(report.tools.len(), 19);
    assert!(report.skipped.is_empty());
    assert_eq!(tool_count(&pool, "petstore").await, 19);

    let (base_url, spec): (String, String) = sqlx::query_as("SELECT base_url, spec FROM apis")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(base_url, "https://petstore3.swagger.io/api/v3");
    assert_eq!(spec, PETSTORE);
}

#[sqlx::test(migrations = "../../migrations")]
async fn import_with_the_same_api_name_updates_the_api_in_place(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    import_spec(&pool, petstore("petstore", None))
        .await
        .unwrap();
    let api_id: i64 = sqlx::query_scalar("SELECT id FROM apis")
        .fetch_one(&pool)
        .await
        .unwrap();
    let report = import_spec(&pool, petstore("petstore", None))
        .await
        .unwrap();
    assert!(!report.server_created);
    assert!(report.replaced);
    assert!(report.kept.is_empty() && report.removed.is_empty());
    let after: i64 = sqlx::query_scalar("SELECT id FROM apis")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, api_id);
    let unhashed: i64 = sqlx::query_scalar("SELECT count(*) FROM tools WHERE spec_hash IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(unhashed, 0);
    assert_eq!(tool_count(&pool, "petstore").await, 19);
}

#[sqlx::test(migrations = "../../migrations")]
async fn tool_names_must_be_unique_in_a_server(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    import_spec(&pool, petstore("petstore", Some("first")))
        .await
        .unwrap();
    let error = import_spec(&pool, petstore("petstore", Some("second")))
        .await
        .unwrap_err();
    assert!(matches!(&error, ImportError::ToolNameTaken(names) if names.len() == 19));
    // The failed import changes nothing.
    assert_eq!(tool_count(&pool, "petstore").await, 19);
}

#[sqlx::test(migrations = "../../migrations")]
async fn relative_spec_server_needs_a_base_url(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    let mut request = petstore("petstore", None);
    request.base_url = None;
    let error = import_spec(&pool, request).await.unwrap_err();
    assert!(matches!(error, ImportError::MissingBaseUrl));
}

#[sqlx::test(migrations = "../../migrations")]
async fn base_url_must_be_http(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    let mut request = petstore("petstore", None);
    request.base_url = Some(Url::parse("file:///etc/passwd").unwrap());
    let error = import_spec(&pool, request).await.unwrap_err();
    assert!(matches!(error, ImportError::BadBaseUrl(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn unknown_owner_is_rejected(pool: PgPool) {
    let error = import_spec(&pool, petstore("petstore", None))
        .await
        .unwrap_err();
    assert!(matches!(error, ImportError::UnknownOwner(login) if login == "octocat"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn server_of_another_user_is_rejected(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    common::create_user(&pool, 2, "mallory").await;
    import_spec(&pool, petstore("petstore", None))
        .await
        .unwrap();

    let mut request = petstore("petstore", Some("other"));
    request.owner = Owner::Login("mallory");
    let error = import_spec(&pool, request).await.unwrap_err();
    assert!(matches!(error, ImportError::ServerOwnedByOther(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn bad_server_name_is_rejected(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    let error = import_spec(&pool, petstore("Pet Store", None))
        .await
        .unwrap_err();
    assert!(matches!(error, ImportError::InvalidServerName(_)));
}

#[sqlx::test(migrations = "../../migrations")]
async fn swagger_2_spec_is_rejected(pool: PgPool) {
    common::create_user(&pool, 1, "octocat").await;
    let mut request = petstore("petstore", None);
    request.spec_text = r#"{ "swagger": "2.0", "info": {}, "paths": {} }"#;
    let error = import_spec(&pool, request).await.unwrap_err();
    assert!(matches!(error, ImportError::Spec(SpecError::Swagger2)));
}
