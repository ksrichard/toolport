//! Tokens that MCP clients send to `/mcp/<server>`. The Tokens page and the `mcp-token`
//! dev binary manage them.

use std::time::Duration;

use sqlx::PgPool;

use crate::auth::{jwt::JwtKeys, refresh::random_hex};

/// The default lifetime of a new token.
pub const DEFAULT_TTL: Duration = Duration::from_secs(90 * 24 * 60 * 60);

pub const MAX_NAME_CHARS: usize = 64;

/// The server updates `last_used_at` at most once in this time, so a busy client does not write on each call.
const LAST_USED_STEP_SECS: f64 = 60.0;

/// The caller of one MCP HTTP request, after a successful token check.
#[derive(Debug, Clone)]
pub struct McpCaller {
    pub server_id: i64,
    pub server_name: String,
    pub token_id: i64,
    pub token_name: String,
}

/// The value is shown once. After that, nobody can get it again.
pub struct CreatedToken {
    pub id: i64,
    pub value: String,
}

#[derive(Debug, sqlx::FromRow)]
pub struct TokenInfo {
    pub id: i64,
    pub name: String,
    /// The times are text in UTC, for example `2026-11-02 21:17`.
    pub created_at: String,
    pub expires_at: String,
    pub last_used_at: Option<String>,
    pub revoked: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("the server {0} does not exist")]
    UnknownServer(String),
    #[error("the token name must not be empty")]
    EmptyName,
    #[error("the token name must have at most {0} characters")]
    NameTooLong(usize),
    #[error("cannot sign the token")]
    Signing(#[from] jsonwebtoken::errors::Error),
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
}

/// Errors of the token check. The client gets the same 401 for each reason except `Database`.
#[derive(Debug, thiserror::Error)]
pub enum AuthorizeError {
    #[error("the request has no Bearer token")]
    Missing,
    #[error("the token is not valid")]
    Invalid(#[from] jsonwebtoken::errors::Error),
    #[error("the token is revoked, expired, or for another server")]
    NotActive,
    #[error("the database query failed")]
    Database(#[from] sqlx::Error),
}

pub async fn create(
    pool: &PgPool,
    keys: &JwtKeys,
    server: &str,
    name: &str,
    ttl: Duration,
) -> Result<CreatedToken, TokenError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(TokenError::EmptyName);
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(TokenError::NameTooLong(MAX_NAME_CHARS));
    }
    let server_id: i64 = sqlx::query_scalar("SELECT id FROM servers WHERE name = $1")
        .bind(server)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| TokenError::UnknownServer(server.to_owned()))?;

    let jti = random_hex(16);
    let issued = keys.issue_mcp(server_id, &jti, ttl)?;
    let id = sqlx::query_scalar(
        "INSERT INTO mcp_tokens (server_id, jti, name, expires_at)
         VALUES ($1, $2, $3, to_timestamp($4)) RETURNING id",
    )
    .bind(server_id)
    .bind(&jti)
    .bind(name)
    .bind(issued.claims.exp as f64)
    .fetch_one(pool)
    .await?;
    Ok(CreatedToken {
        id,
        value: issued.token,
    })
}

pub async fn list(pool: &PgPool, server: &str) -> Result<Vec<TokenInfo>, TokenError> {
    let rows = sqlx::query_as(
        "SELECT t.id, t.name,
                to_char(t.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') AS created_at,
                to_char(t.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') AS expires_at,
                to_char(t.last_used_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') AS last_used_at,
                t.revoked_at IS NOT NULL AS revoked
         FROM mcp_tokens t JOIN servers s ON s.id = t.server_id
         WHERE s.name = $1 ORDER BY t.id",
    )
    .bind(server)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Returns false when no active token has this id.
pub async fn revoke(pool: &PgPool, id: i64) -> Result<bool, TokenError> {
    let result = sqlx::query(
        "UPDATE mcp_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Checks the token of one MCP HTTP request for the server in the URL path.
pub async fn authorize(
    pool: &PgPool,
    keys: &JwtKeys,
    server: &str,
    bearer: Option<&str>,
) -> Result<McpCaller, AuthorizeError> {
    let claims = keys.verify_mcp(bearer.ok_or(AuthorizeError::Missing)?)?;
    let row: Option<(i64, String, bool)> = sqlx::query_as(
        "SELECT t.id, t.name, t.last_used_at IS NULL OR t.last_used_at < now() - make_interval(secs => $4)
         FROM mcp_tokens t JOIN servers s ON s.id = t.server_id
         WHERE t.jti = $1 AND t.server_id = $2 AND s.name = $3
           AND t.revoked_at IS NULL AND t.expires_at > now()",
    )
    .bind(&claims.jti)
    .bind(claims.srv)
    .bind(server)
    .bind(LAST_USED_STEP_SECS)
    .fetch_optional(pool)
    .await?;
    let (token_id, token_name, touch) = row.ok_or(AuthorizeError::NotActive)?;
    if touch {
        sqlx::query("UPDATE mcp_tokens SET last_used_at = now() WHERE id = $1")
            .bind(token_id)
            .execute(pool)
            .await?;
    }
    Ok(McpCaller {
        server_id: claims.srv,
        server_name: server.to_owned(),
        token_id,
        token_name,
    })
}
