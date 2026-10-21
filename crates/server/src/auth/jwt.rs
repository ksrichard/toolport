use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

pub const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);

/// Dashboard tokens carry this audience. MCP tokens (milestone 4) get a different one,
/// so one kind of token cannot open the other kind of route.
const DASHBOARD_AUDIENCE: &str = "toolport-dashboard";

#[derive(Serialize, Deserialize)]
struct AccessClaims {
    sub: String,
    aud: String,
    iat: u64,
    exp: u64,
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
}

impl JwtKeys {
    pub fn new(secret: &[u8]) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_audience(&[DASHBOARD_AUDIENCE]);
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
        }
    }

    pub fn issue_access(&self, user_id: i64) -> Result<String, jsonwebtoken::errors::Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
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
}
