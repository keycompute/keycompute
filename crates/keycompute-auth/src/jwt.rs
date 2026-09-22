//! Identity-only JWT parsing and signing.
use crate::AuthContext;
use chrono::{Duration, Utc};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use keycompute_types::{CredentialKind, KeyComputeError, PlatformRole, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JwtClaims {
    pub sub: String,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub authz_version: Option<i64>,
    #[serde(default)]
    pub membership_authz_version: Option<i64>,
    pub exp: i64,
    pub iat: i64,
    pub iss: String,
    pub token_version: i32,
}
impl JwtClaims {
    pub fn new(
        user_id: Uuid,
        tenant_id: Option<Uuid>,
        token_version: i32,
        expires: i64,
        issuer: &str,
    ) -> Self {
        let now = Utc::now().timestamp();
        Self {
            sub: user_id.to_string(),
            tenant_id: tenant_id.map(|v| v.to_string()),
            authz_version: None,
            membership_authz_version: None,
            exp: now.saturating_add(expires),
            iat: now,
            iss: issuer.to_owned(),
            token_version,
        }
    }
    pub fn user_id(&self) -> Result<Uuid> {
        let id = Uuid::parse_str(&self.sub)
            .map_err(|e| KeyComputeError::AuthError(format!("Invalid user ID in token: {e}")))?;
        if id.is_nil() {
            return Err(KeyComputeError::AuthError("nil user ID in token".into()));
        }
        Ok(id)
    }
    pub fn tenant_id(&self) -> Result<Option<Uuid>> {
        let id = self
            .tenant_id
            .as_deref()
            .map(Uuid::parse_str)
            .transpose()
            .map_err(|e| KeyComputeError::AuthError(format!("Invalid tenant ID in token: {e}")))?;
        if id.is_some_and(|v| v.is_nil()) {
            return Err(KeyComputeError::AuthError("nil tenant ID in token".into()));
        }
        Ok(id)
    }
    pub fn is_expired(&self) -> bool {
        self.exp < Utc::now().timestamp()
    }
    pub fn default_expiration() -> i64 {
        Duration::hours(24).num_seconds()
    }
}
#[derive(Clone)]
pub struct JwtValidator {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    issuer: String,
    default_expiration: i64,
}
impl std::fmt::Debug for JwtValidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtValidator")
            .field("issuer", &self.issuer)
            .field("default_expiration", &self.default_expiration)
            .finish()
    }
}
impl JwtValidator {
    pub fn new(secret: impl AsRef<[u8]>, issuer: impl Into<String>) -> Self {
        let s = secret.as_ref();
        Self {
            encoding_key: EncodingKey::from_secret(s),
            decoding_key: DecodingKey::from_secret(s),
            issuer: issuer.into(),
            default_expiration: Duration::hours(24).num_seconds(),
        }
    }
    pub fn with_expiration(mut self, seconds: i64) -> Self {
        self.default_expiration = seconds;
        self
    }
    pub fn default_expiration(&self) -> i64 {
        self.default_expiration
    }
    pub fn validate_claims(&self, token: &str) -> Result<JwtClaims> {
        let mut v = Validation::new(jsonwebtoken::Algorithm::HS256);
        v.set_issuer(&[&self.issuer]);
        v.validate_exp = true;
        v.leeway = 0;
        let data = decode::<JwtClaims>(token, &self.decoding_key, &v)
            .map_err(|e| KeyComputeError::AuthError(format!("Token validation failed: {e}")))?;
        let c = data.claims;
        let now = Utc::now().timestamp();
        if c.exp <= now || c.iat >= c.exp || c.iat > now.saturating_add(30) {
            return Err(KeyComputeError::AuthError("invalid token lifetime".into()));
        }
        c.user_id()?;
        let selected = c.tenant_id()?;
        if c.token_version < 0 {
            return Err(KeyComputeError::AuthError("negative token version".into()));
        }
        match selected {
            Some(_) if c.authz_version.is_none() || c.membership_authz_version.is_none() => {
                return Err(KeyComputeError::AuthError(
                    "selected tenant requires both authorization versions".into(),
                ));
            }
            None if c.authz_version.is_some() || c.membership_authz_version.is_some() => {
                return Err(KeyComputeError::AuthError(
                    "tenant versions require selected tenant".into(),
                ));
            }
            _ => {}
        }
        if c.authz_version.is_some_and(|v| v <= 0)
            || c.membership_authz_version.is_some_and(|v| v <= 0)
        {
            return Err(KeyComputeError::AuthError(
                "tenant versions must be positive".into(),
            ));
        }
        Ok(c)
    }
    /// Structural validation returns a context with no authority. Callers must
    /// load current user/membership state before authorizing any operation.
    pub fn validate(&self, token: &str) -> Result<AuthContext> {
        let c = self.validate_claims(token)?;
        let user_id = c.user_id()?;
        let tenant_id = c.tenant_id()?;
        Ok(AuthContext {
            user_id,
            selected_tenant_id: tenant_id,
            platform_role: PlatformRole::None,
            tenant_role: None,
            credential_kind: CredentialKind::Jwt,
            produce_ai_key_id: Uuid::nil(),
            permissions: Vec::new(),
            token_version: c.token_version,
            membership_authz_version: c.membership_authz_version,
            authz_version: c.authz_version,
            user_info: None,
            tenant_info: None,
        })
    }
    pub fn generate_identity_token(
        &self,
        user_id: Uuid,
        tenant_id: Option<Uuid>,
        token_version: i32,
        authz_version: Option<i64>,
        membership_authz_version: Option<i64>,
        expires: i64,
    ) -> Result<String> {
        if user_id.is_nil() || token_version < 0 {
            return Err(KeyComputeError::ValidationError(
                "invalid identity token subject/version".into(),
            ));
        }
        if tenant_id.is_some_and(|v| v.is_nil()) {
            return Err(KeyComputeError::ValidationError(
                "nil tenant in identity token".into(),
            ));
        }
        if tenant_id.is_some() != authz_version.is_some()
            || tenant_id.is_some() != membership_authz_version.is_some()
        {
            return Err(KeyComputeError::ValidationError(
                "selected tenant and versions must be supplied together".into(),
            ));
        }
        if authz_version.is_some_and(|v| v <= 0) || membership_authz_version.is_some_and(|v| v <= 0)
        {
            return Err(KeyComputeError::ValidationError(
                "tenant versions must be positive".into(),
            ));
        }
        let mut c = JwtClaims::new(user_id, tenant_id, token_version, expires, &self.issuer);
        c.authz_version = authz_version;
        c.membership_authz_version = membership_authz_version;
        encode(&Header::default(), &c, &self.encoding_key)
            .map_err(|e| KeyComputeError::Internal(format!("Failed to generate token: {e}")))
    }
    pub fn refresh_token(&self, token: &str) -> Result<String> {
        let c = self.validate_claims(token)?;
        self.generate_identity_token(
            c.user_id()?,
            c.tenant_id()?,
            c.token_version,
            c.authz_version,
            c.membership_authz_version,
            self.default_expiration,
        )
    }
}

#[cfg(test)]
#[path = "jwt_tests.rs"]
mod tests;
