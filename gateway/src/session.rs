use anyhow::{anyhow, Result};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use streamio_types::{Role, SessionClaims};
use uuid::Uuid;

const SESSION_DURATION_SECS: i64 = 8 * 3600; // 8 hours
pub const COOKIE_NAME: &str = "sid";

pub struct SessionManager {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
}

impl SessionManager {
    pub fn new(secret: String) -> Self {
        SessionManager {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
        }
    }

    /// Issue a signed JWT for a user.
    pub fn issue(
        &self,
        sub: String,
        email: String,
        role: Role,
        backend_id: Option<Uuid>,
    ) -> Result<String> {
        let claims = SessionClaims {
            sub,
            email,
            role,
            backend_id,
            exp: Utc::now().timestamp() + SESSION_DURATION_SECS,
        };
        encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|e| anyhow!("JWT encode error: {e}"))
    }

    /// Verify and decode a JWT. Returns the claims or an error.
    pub fn verify(&self, token: &str) -> Result<SessionClaims> {
        let mut validation = Validation::default();
        validation.validate_exp = true;
        decode::<SessionClaims>(token, &self.decoding_key, &validation)
            .map(|d| d.claims)
            .map_err(|e| anyhow!("JWT verify error: {e}"))
    }
}
