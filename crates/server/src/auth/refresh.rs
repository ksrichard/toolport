use std::{fmt::Write, time::Duration};

use sha2::{Digest, Sha256};
use sqlx::PgPool;

pub const REFRESH_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Makes a random token. The database keeps only its SHA-256 hash, so a database leak
/// does not give working tokens.
pub async fn create(pool: &PgPool, user_id: i64) -> Result<String, sqlx::Error> {
    let value = random_hex(32);
    sqlx::query(
        "INSERT INTO refresh_tokens (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + make_interval(secs => $3))",
    )
    .bind(user_id)
    .bind(hash(&value))
    .bind(REFRESH_TOKEN_TTL.as_secs_f64())
    .execute(pool)
    .await?;
    Ok(value)
}

/// Revokes the token and makes a new one for the same user. Returns `None` when the token
/// is unknown, expired, or already used. The single UPDATE makes sure that two parallel
/// requests cannot both use the same token.
pub async fn rotate(pool: &PgPool, value: &str) -> Result<Option<(i64, String)>, sqlx::Error> {
    let user_id: Option<i64> = sqlx::query_scalar(
        "UPDATE refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()
         RETURNING user_id",
    )
    .bind(hash(value))
    .fetch_optional(pool)
    .await?;
    match user_id {
        Some(user_id) => Ok(Some((user_id, create(pool, user_id).await?))),
        None => Ok(None),
    }
}

pub async fn revoke(pool: &PgPool, value: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND revoked_at IS NULL",
    )
    .bind(hash(value))
    .execute(pool)
    .await?;
    Ok(())
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(buf.as_mut_slice());
    to_hex(&buf)
}

fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
