//! Loads an OpenAPI spec into a server. The `import-spec` binary uses it until the APIs page exists.

use reqwest::Url;
use sqlx::{PgPool, types::Json};
use toolport_core::{SkippedOperation, SpecError, convert, parse_spec, server_name};

pub struct ImportRequest<'a> {
    /// The GitHub login of the server owner. The user must have signed in once.
    pub owner_login: &'a str,
    pub server: &'a str,
    /// The default is the spec title.
    pub api_name: Option<&'a str>,
    pub spec_text: &'a str,
    /// The default is the first `servers` URL in the spec.
    pub base_url: Option<Url>,
}

#[derive(Debug)]
pub struct ImportReport {
    pub server_created: bool,
    pub api_name: String,
    pub base_url: Url,
    /// True when an API with the same name existed. Its old tools are gone.
    pub replaced: bool,
    pub tools: Vec<String>,
    pub skipped: Vec<SkippedOperation>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(
        "the server name {0} is not valid, use 1 to 63 lowercase letters, digits, and `-`, and start with a letter or digit"
    )]
    InvalidServerName(String),
    #[error("the spec is not valid")]
    Spec(#[from] SpecError),
    #[error("the spec has no operation that can become a tool")]
    NoTools,
    #[error("the spec has no absolute base URL, set one with --base-url")]
    MissingBaseUrl,
    #[error("the base URL {0} must start with http:// or https://")]
    BadBaseUrl(Url),
    #[error("no user with the GitHub login {0} exists, sign in to the dashboard once first")]
    UnknownOwner(String),
    #[error("the server {0} belongs to another user")]
    ServerOwnedByOther(String),
    #[error("other APIs of this server already have tools with these names: {}", .0.join(", "))]
    ToolNameTaken(Vec<String>),
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
}

/// Creates the server if needed and adds the API with one tool per operation.
/// An API with the same name on the same server is replaced, so the same command can run again.
pub async fn import_spec(
    pool: &PgPool,
    request: ImportRequest<'_>,
) -> Result<ImportReport, ImportError> {
    if !server_name::is_valid_server_name(request.server) {
        return Err(ImportError::InvalidServerName(request.server.to_owned()));
    }
    let spec = parse_spec(request.spec_text)?;
    let conversion = convert(&spec);
    if conversion.tools.is_empty() {
        return Err(ImportError::NoTools);
    }
    let base_url = match request.base_url {
        Some(url) => url,
        None => spec
            .servers
            .first()
            .and_then(|server| Url::parse(&server.url).ok())
            .ok_or(ImportError::MissingBaseUrl)?,
    };
    if !matches!(base_url.scheme(), "http" | "https") {
        return Err(ImportError::BadBaseUrl(base_url));
    }
    let api_name = request.api_name.unwrap_or(&spec.info.title).to_owned();

    let mut tx = pool.begin().await?;

    let owner_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE github_login = $1")
        .bind(request.owner_login)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| ImportError::UnknownOwner(request.owner_login.to_owned()))?;

    // The no-op update makes ON CONFLICT return the existing row and lock it until commit.
    // `xmax = 0` is true only for a row that this statement inserted.
    let (server_id, server_owner, server_created): (i64, i64, bool) = sqlx::query_as(
        "INSERT INTO servers (user_id, name) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
         RETURNING id, user_id, (xmax = 0)",
    )
    .bind(owner_id)
    .bind(request.server)
    .fetch_one(&mut *tx)
    .await?;
    if server_owner != owner_id {
        return Err(ImportError::ServerOwnedByOther(request.server.to_owned()));
    }

    let replaced = sqlx::query("DELETE FROM apis WHERE server_id = $1 AND name = $2")
        .bind(server_id)
        .bind(&api_name)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;

    let names: Vec<&str> = conversion.tools.iter().map(|t| t.name.as_str()).collect();
    let taken: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM tools WHERE server_id = $1 AND name = ANY($2) ORDER BY name",
    )
    .bind(server_id)
    .bind(&names)
    .fetch_all(&mut *tx)
    .await?;
    if !taken.is_empty() {
        return Err(ImportError::ToolNameTaken(taken));
    }

    let api_id: i64 = sqlx::query_scalar(
        "INSERT INTO apis (server_id, name, base_url, spec) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(server_id)
    .bind(&api_name)
    .bind(base_url.as_str())
    .bind(request.spec_text)
    .fetch_one(&mut *tx)
    .await?;

    for tool in &conversion.tools {
        sqlx::query(
            "INSERT INTO tools (api_id, server_id, name, description, input_schema, request)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(api_id)
        .bind(server_id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(Json(&tool.input_schema))
        .bind(Json(&tool.request))
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(ImportReport {
        server_created,
        api_name,
        base_url,
        replaced,
        tools: names.into_iter().map(str::to_owned).collect(),
        skipped: conversion.skipped,
    })
}
