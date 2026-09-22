//! Current replay authority. Stores IDs/versions only, never bearer secrets.
use super::store::Scope;
use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::http::HeaderMap;
use keycompute_auth::Permission;
use keycompute_types::CredentialKind;
use uuid::Uuid;

/// Parameters $1..$4 are the immutable resource scope and ID; $5..$9 are
/// this connection's already-verified credential identity and versions.
/// Folded into existing state/event SELECTs, not a separate per-event query.
pub(super) const CURRENT_REPLAY_ACTOR: &str = "EXISTS (
    SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id
    JOIN tenants t ON t.id=m.tenant_id
    WHERE m.tenant_id=r.tenant_id AND m.user_id=r.user_id
      AND u.status='active' AND m.status='active' AND t.status='active'
      AND u.token_version=$5 AND t.authz_version=$6 AND m.authz_version=$7
      AND ($8::uuid IS NULL OR EXISTS (
          SELECT 1 FROM produce_ai_keys k WHERE k.tenant_id=m.tenant_id
            AND k.user_id=m.user_id AND k.id=$8 AND NOT k.revoked
            AND (k.expires_at IS NULL OR k.expires_at>statement_timestamp())))
      AND ($9::bigint IS NULL OR $9>EXTRACT(EPOCH FROM statement_timestamp())))";
#[derive(Debug, Clone, Copy)]
pub(super) struct ReplayAuthority {
    tenant: Uuid,
    user: Uuid,
    key: Option<Uuid>,
    token_version: i32,
    tenant_version: i64,
    membership_version: i64,
    jwt_expiration: Option<i64>,
}
impl ReplayAuthority {
    pub fn new(
        state: &AppState,
        auth: &AuthExtractor,
        scope: Scope,
        headers: &HeaderMap,
    ) -> Result<Self> {
        if auth.tenant_id != scope.tenant
            || auth.user_id != scope.user
            || scope.tenant.is_nil()
            || scope.user.is_nil()
            || auth.token_version < 0
            || auth.authz_version <= 0
            || auth.membership_authz_version <= 0
            || !auth.has_permission(&Permission::UseApi)
        {
            return Err(denied());
        }
        let (key, jwt_expiration) = match auth.credential_kind {
            CredentialKind::ApiKey if !auth.produce_ai_key_id.is_nil() => {
                (Some(auth.produce_ai_key_id), None)
            }
            CredentialKind::Jwt => {
                let token = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .ok_or_else(denied)?;
                let claims = state
                    .auth
                    .get_jwt_validator()
                    .ok_or_else(denied)?
                    .validate_claims(token)
                    .map_err(|_| denied())?;
                if claims.user_id().map_err(|_| denied())? != auth.user_id
                    || claims.tenant_id().map_err(|_| denied())? != Some(auth.tenant_id)
                    || claims.token_version != auth.token_version
                    || claims.authz_version != Some(auth.authz_version)
                    || claims.membership_authz_version != Some(auth.membership_authz_version)
                {
                    return Err(denied());
                }
                (None, Some(claims.exp))
            }
            _ => return Err(denied()),
        };
        Ok(Self {
            tenant: scope.tenant,
            user: scope.user,
            key,
            token_version: auth.token_version,
            tenant_version: auth.authz_version,
            membership_version: auth.membership_authz_version,
            jwt_expiration,
        })
    }
    pub fn values(self, scope: Scope) -> Result<[sea_orm::Value; 5]> {
        if self.tenant != scope.tenant
            || self.user != scope.user
            || self
                .jwt_expiration
                .is_some_and(|t| t <= chrono::Utc::now().timestamp())
        {
            return Err(denied());
        }
        Ok([
            self.token_version.into(),
            self.tenant_version.into(),
            self.membership_version.into(),
            self.key.into(),
            self.jwt_expiration.into(),
        ])
    }
}
fn denied() -> ApiError {
    ApiError::Forbidden("Replay authorization is no longer valid".into())
}
