use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

pub const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

/// Each kind of token has its own audience, so one kind cannot open the routes of the other.
const DASHBOARD_AUDIENCE: &str = "toolport-dashboard";
const MCP_AUDIENCE: &str = "toolport-mcp";

#[derive(Serialize, Deserialize)]
struct AccessClaims {
    sub: String,
    aud: String,
    iat: u64,
    exp: u64,
}

/// An MCP client token. It is valid only while its `jti` row exists and is not revoked.
#[derive(Serialize, Deserialize)]
pub struct McpClaims {
    /// The id of the one server that the token opens.
    pub srv: i64,
    pub jti: String,
    aud: String,
    iat: u64,
    pub exp: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AccessTokenError {
    #[error("the access token is not valid")]
    Invalid(#[from] jsonwebtoken::errors::Error),
    #[error("the access token has a subject that is not a user id")]
    BadSubject,
}

pub struct JwtKeys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    mcp_validation: Validation,
}

impl JwtKeys {
    pub fn new(secret: &[u8]) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[DASHBOARD_AUDIENCE]);
        let mut mcp_validation = Validation::new(Algorithm::HS256);
        mcp_validation.set_audience(&[MCP_AUDIENCE]);
        mcp_validation.set_required_spec_claims(&["exp", "aud"]);
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
            mcp_validation,
        }
    }

    pub fn issue_access(&self, user_id: i64) -> Result<String, jsonwebtoken::errors::Error> {
        let now = unix_now();
        let claims = AccessClaims {
            sub: user_id.to_string(),
            aud: DASHBOARD_AUDIENCE.to_owned(),
            iat: now,
            exp: now + ACCESS_TOKEN_TTL.as_secs(),
        };
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
    }

    /// Returns the user id in the token.
    pub fn verify_access(&self, token: &str) -> Result<i64, AccessTokenError> {
        let data = jsonwebtoken::decode::<AccessClaims>(token, &self.decoding, &self.validation)?;
        data.claims
            .sub
            .parse()
            .map_err(|_| AccessTokenError::BadSubject)
    }

    pub fn issue_mcp(
        &self,
        server_id: i64,
        jti: &str,
        ttl: Duration,
    ) -> Result<McpClaimsAndToken, jsonwebtoken::errors::Error> {
        let now = unix_now();
        let claims = McpClaims {
            srv: server_id,
            jti: jti.to_owned(),
            aud: MCP_AUDIENCE.to_owned(),
            iat: now,
            exp: now + ttl.as_secs(),
        };
        let token = jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)?;
        Ok(McpClaimsAndToken { claims, token })
    }

    /// Checks the signature, the audience, and the expiry. The caller must still check the `jti` row.
    pub fn verify_mcp(&self, token: &str) -> Result<McpClaims, jsonwebtoken::errors::Error> {
        jsonwebtoken::decode::<McpClaims>(token, &self.decoding, &self.mcp_validation)
            .map(|data| data.claims)
    }
}

pub struct McpClaimsAndToken {
    pub claims: McpClaims,
    pub token: String,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> JwtKeys {
        JwtKeys::new(b"test-secret-that-is-at-least-32-characters")
    }

    #[test]
    fn mcp_token_round_trip() {
        let keys = keys();
        let issued = keys.issue_mcp(7, "abc", Duration::from_secs(60)).unwrap();
        let claims = keys.verify_mcp(&issued.token).unwrap();
        assert_eq!((claims.srv, claims.jti.as_str()), (7, "abc"));
    }

    #[test]
    fn audiences_do_not_mix() {
        let keys = keys();
        let access = keys.issue_access(1).unwrap();
        assert!(keys.verify_mcp(&access).is_err());
        let mcp = keys.issue_mcp(1, "abc", Duration::from_secs(60)).unwrap();
        assert!(keys.verify_access(&mcp.token).is_err());
    }

    #[test]
    fn expired_mcp_token_is_rejected() {
        let keys = keys();
        let mut claims = keys.issue_mcp(1, "abc", Duration::ZERO).unwrap().claims;
        // Older than the 60 s leeway of the validation.
        claims.exp -= 120;
        let token =
            jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &keys.encoding).unwrap();
        assert!(keys.verify_mcp(&token).is_err());
    }

    #[test]
    fn token_from_another_secret_is_rejected() {
        let other = JwtKeys::new(b"another-secret-that-is-at-least-32-chars");
        let token = other
            .issue_mcp(1, "abc", Duration::from_secs(60))
            .unwrap()
            .token;
        assert!(keys().verify_mcp(&token).is_err());
    }
}
