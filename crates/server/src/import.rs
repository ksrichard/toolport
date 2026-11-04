//! Loads an OpenAPI spec into a server. The dashboard and the `import-spec` binary use it.

use reqwest::Url;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, types::Json};
use toolport_core::{SkippedOperation, SpecError, ToolDef, convert, parse_spec, server_name};

/// The CLI knows the owner by GitHub login. The dashboard knows the signed-in user id.
#[derive(Debug, Clone, Copy)]
pub enum Owner<'a> {
    Login(&'a str),
    UserId(i64),
}

pub struct ImportRequest<'a> {
    /// The user must have signed in once.
    pub owner: Owner<'a>,
    pub server: &'a str,
    /// The default is the spec title.
    pub api_name: Option<&'a str>,
    pub spec_text: &'a str,
    /// The default is the first `servers` URL in the spec.
    pub base_url: Option<Url>,
    /// Set when the spec came from a URL. The daily check downloads it again.
    pub spec_url: Option<&'a str>,
}

#[derive(Debug)]
pub struct ImportReport {
    pub server_created: bool,
    pub api_name: String,
    pub base_url: Url,
    /// True when an API with the same name existed. It is updated in place.
    pub replaced: bool,
    /// The spec tools of the API after the import, under their current names.
    pub tools: Vec<String>,
    /// Spec tools that this import created.
    pub added: Vec<String>,
    /// Unedited spec tools that the new spec changed.
    pub changed: Vec<String>,
    /// Edited tools that stayed. The import changed only their stored spec version.
    pub kept: Vec<String>,
    /// Spec tools deleted because their operation is not in the spec any more.
    pub removed: Vec<String>,
    pub skipped: Vec<SkippedOperation>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(
        "the server name {0} is not valid, use 1 to 63 lowercase letters, digits, and `-`, and start with a letter or digit"
    )]
    InvalidServerName(String),
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error("the spec has no operation that can become a tool")]
    NoTools,
    #[error("the spec has no absolute base URL, set a base URL")]
    MissingBaseUrl,
    #[error("the base URL {0} must start with http:// or https://")]
    BadBaseUrl(Url),
    #[error("the owner {0} does not exist, sign in to the dashboard once first")]
    UnknownOwner(String),
    #[error("the server {0} belongs to another user")]
    ServerOwnedByOther(String),
    #[error("other APIs of this server already have tools with these names: {}", .0.join(", "))]
    ToolNameTaken(Vec<String>),
    #[error("the tool could not be hashed")]
    Hash(#[from] serde_json::Error),
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
}

/// SHA-256 hex of the converted tool. A changed hash means the spec changed this tool.
pub fn spec_hash(tool: &ToolDef) -> Result<String, serde_json::Error> {
    use std::fmt::Write;
    let digest = Sha256::digest(serde_json::to_vec(tool)?);
    Ok(digest.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

/// Creates the server if needed and adds the API with one tool per operation.
/// An API with the same name on the same server is updated in place, so the same command can
/// run again. A spec tool is matched by its operation. Edited tools keep the user's fields.
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

    let owner_id: Option<i64> = match request.owner {
        Owner::Login(login) => {
            sqlx::query_scalar("SELECT id FROM users WHERE github_login = $1")
                .bind(login)
                .fetch_optional(&mut *tx)
                .await?
        }
        Owner::UserId(id) => {
            sqlx::query_scalar("SELECT id FROM users WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
        }
    };
    let owner_id = owner_id.ok_or_else(|| {
        ImportError::UnknownOwner(match request.owner {
            Owner::Login(login) => login.to_owned(),
            Owner::UserId(id) => format!("with id {id}"),
        })
    })?;

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

    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM apis WHERE server_id = $1 AND name = $2 FOR UPDATE")
            .bind(server_id)
            .bind(&api_name)
            .fetch_optional(&mut *tx)
            .await?;
    let replaced = existing.is_some();
    let api_id: i64 = match existing {
        Some(id) => {
            // A spec without a URL cannot be checked, so the check turns off.
            sqlx::query(
                "UPDATE apis SET base_url = $1, spec = $2, spec_url = $3,
                        auto_check = auto_check AND $3 IS NOT NULL, updated_at = now()
                 WHERE id = $4",
            )
            .bind(base_url.as_str())
            .bind(request.spec_text)
            .bind(request.spec_url)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            id
        }
        None => {
            sqlx::query_scalar(
                "INSERT INTO apis (server_id, name, base_url, spec, spec_url)
                 VALUES ($1, $2, $3, $4, $5) RETURNING id",
            )
            .bind(server_id)
            .bind(&api_name)
            .bind(base_url.as_str())
            .bind(request.spec_text)
            .bind(request.spec_url)
            .fetch_one(&mut *tx)
            .await?
        }
    };

    let operations: Vec<String> = conversion.tools.iter().map(operation).collect();

    // Delete first, so a new tool can take the name of a deleted one.
    let removed: Vec<String> = sqlx::query_scalar(
        "DELETE FROM tools WHERE api_id = $1 AND source = 'spec' AND NOT (operation = ANY($2))
         RETURNING name",
    )
    .bind(api_id)
    .bind(&operations)
    .fetch_all(&mut *tx)
    .await?;

    let old_rows: Vec<(i64, String, bool, String, Option<String>)> = sqlx::query_as(
        "SELECT id, operation, edited, name, spec_hash FROM tools
         WHERE api_id = $1 AND source = 'spec'",
    )
    .bind(api_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut tools = Vec::with_capacity(conversion.tools.len());
    let mut kept = Vec::new();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut taken = Vec::new();
    for (tool, operation) in conversion.tools.iter().zip(&operations) {
        let hash = spec_hash(tool)?;
        let old = old_rows.iter().find(|row| &row.1 == operation);
        // An edited tool keeps its own name, so the spec name cannot clash.
        if !old.is_some_and(|row| row.2) {
            let clash: Option<String> = sqlx::query_scalar(
                "SELECT name FROM tools WHERE server_id = $1 AND name = $2 AND id <> $3",
            )
            .bind(server_id)
            .bind(&tool.name)
            .bind(old.map_or(0, |row| row.0))
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(name) = clash {
                taken.push(name);
                continue;
            }
        }
        match old {
            None => {
                sqlx::query(
                    "INSERT INTO tools (api_id, server_id, name, description, input_schema,
                                        request, source, spec_tool, spec_hash, operation)
                     VALUES ($1, $2, $3, $4, $5, $6, 'spec', $7, $8, $9)",
                )
                .bind(api_id)
                .bind(server_id)
                .bind(&tool.name)
                .bind(&tool.description)
                .bind(Json(&tool.input_schema))
                .bind(Json(&tool.request))
                .bind(Json(tool))
                .bind(&hash)
                .bind(operation)
                .execute(&mut *tx)
                .await?;
                added.push(tool.name.clone());
                tools.push(tool.name.clone());
            }
            Some((id, _, true, name, _)) => {
                sqlx::query("UPDATE tools SET spec_tool = $1, spec_hash = $2 WHERE id = $3")
                    .bind(Json(tool))
                    .bind(&hash)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                kept.push(name.clone());
                tools.push(name.clone());
            }
            Some((id, _, false, _, old_hash)) => {
                if old_hash.as_deref() != Some(hash.as_str()) {
                    changed.push(tool.name.clone());
                }
                sqlx::query(
                    "UPDATE tools SET name = $1, description = $2, input_schema = $3,
                            request = $4, spec_tool = $5, spec_hash = $6
                     WHERE id = $7",
                )
                .bind(&tool.name)
                .bind(&tool.description)
                .bind(Json(&tool.input_schema))
                .bind(Json(&tool.request))
                .bind(Json(tool))
                .bind(&hash)
                .bind(id)
                .execute(&mut *tx)
                .await?;
                tools.push(tool.name.clone());
            }
        }
    }
    if !taken.is_empty() {
        taken.sort();
        taken.dedup();
        return Err(ImportError::ToolNameTaken(taken));
    }

    tx.commit().await?;

    Ok(ImportReport {
        server_created,
        api_name,
        base_url,
        replaced,
        tools,
        added,
        changed,
        kept,
        removed,
        skipped: conversion.skipped,
    })
}

fn operation(tool: &ToolDef) -> String {
    format!("{} {}", tool.request.method, tool.request.path)
}
