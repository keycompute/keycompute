//! Console session data and explicit, verified tenant selection.
use crate::{AuthContext, AuthService, Permission, permissions_for};
use chrono::{DateTime, Utc};
use keycompute_db::User;
use keycompute_types::{
    CredentialKind, KeyComputeError, PlatformRole, Result, TenantRole, UserStatus,
};
use sea_orm::{DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct SessionMembership {
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub tenant_role: TenantRole,
    pub status: String,
    pub authz_version: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct SelectedTenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub tenant_role: TenantRole,
    pub authz_version: i64,
    pub membership_authz_version: i64,
}
#[derive(Debug, Clone, Serialize)]
pub struct SessionCapabilities {
    pub platform: Vec<String>,
    pub tenant: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ConsoleSession {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub platform_role: PlatformRole,
    pub status: UserStatus,
    pub token_version: i32,
    pub selected_tenant: Option<SelectedTenant>,
    pub memberships: Vec<SessionMembership>,
    pub capabilities: SessionCapabilities,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Serialize)]
pub struct SessionTokenResponse {
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub platform_role: PlatformRole,
    pub status: UserStatus,
    pub selected_tenant: Option<SelectedTenant>,
    pub memberships: Vec<SessionMembership>,
    pub capabilities: SessionCapabilities,
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
}
impl SessionTokenResponse {
    pub fn new(session: ConsoleSession, token: String, expires_in: i64) -> Self {
        Self {
            user_id: session.id,
            email: session.email,
            name: session.name,
            platform_role: session.platform_role,
            status: session.status,
            selected_tenant: session.selected_tenant,
            memberships: session.memberships,
            capabilities: session.capabilities,
            access_token: token,
            token_type: "Bearer",
            expires_in,
        }
    }
}
#[derive(FromQueryResult)]
struct MembershipRow {
    tenant_id: Uuid,
    tenant_name: String,
    tenant_slug: String,
    tenant_role: String,
    membership_authz_version: i64,
    authz_version: i64,
}
fn db_error(error: impl std::fmt::Display) -> KeyComputeError {
    KeyComputeError::DatabaseError(error.to_string())
}
impl AuthService {
    pub async fn console_session(&self, ctx: &AuthContext) -> Result<ConsoleSession> {
        if ctx.credential_kind != CredentialKind::Jwt
            || !ctx.has_permission(&Permission::AccessConsole)
        {
            return Err(KeyComputeError::PermissionDenied(
                "console session required".into(),
            ));
        }
        let service = self.user_service.as_ref().ok_or_else(|| {
            KeyComputeError::ServiceUnavailable("identity storage unavailable".into())
        })?;
        let pool = service.primary_pool()?;
        let user = User::find_by_id(pool.write_conn(), ctx.user_id)
            .await
            .map_err(db_error)?
            .filter(|user| user.status == "active" && user.token_version == ctx.token_version)
            .ok_or_else(|| KeyComputeError::AuthError("identity has changed".into()))?;
        let platform_role = user.platform_role().map_err(db_error)?;
        if platform_role != ctx.platform_role {
            return Err(KeyComputeError::AuthError("identity has changed".into()));
        }
        let rows=MembershipRow::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT m.tenant_id,t.name AS tenant_name,t.slug AS tenant_slug,m.tenant_role,m.authz_version AS membership_authz_version,t.authz_version FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id WHERE m.user_id=$1 AND m.status='active' AND t.status='active' ORDER BY t.name,m.tenant_id",
            [ctx.user_id.into()],
        )).all(pool.write_conn()).await.map_err(db_error)?;
        let mut memberships = Vec::with_capacity(rows.len());
        let mut selected = None;
        for row in rows {
            let tenant_role = row.tenant_role.parse::<TenantRole>().map_err(db_error)?;
            if Some(row.tenant_id) == ctx.selected_tenant_id {
                if Some(row.membership_authz_version) != ctx.membership_authz_version
                    || Some(row.authz_version) != ctx.authz_version
                    || Some(tenant_role) != ctx.tenant_role
                {
                    return Err(KeyComputeError::AuthError(
                        "selected tenant authority has changed".into(),
                    ));
                }
                selected = Some(SelectedTenant {
                    id: row.tenant_id,
                    name: row.tenant_name.clone(),
                    slug: row.tenant_slug,
                    tenant_role,
                    authz_version: row.authz_version,
                    membership_authz_version: row.membership_authz_version,
                });
            }
            memberships.push(SessionMembership {
                tenant_id: row.tenant_id,
                tenant_name: row.tenant_name,
                tenant_role,
                status: "active".into(),
                authz_version: row.membership_authz_version,
            });
        }
        if ctx.selected_tenant_id.is_some() && selected.is_none() {
            return Err(KeyComputeError::AuthError(
                "selected membership unavailable".into(),
            ));
        }
        let platform = if platform_role == PlatformRole::None {
            Vec::new()
        } else {
            permissions_for(CredentialKind::Jwt, platform_role, None)
                .into_iter()
                .filter(|permission| *permission != Permission::AccessConsole)
                .map(|permission| permission.as_str().to_string())
                .collect()
        };
        let tenant = if let Some(selected) = &selected {
            permissions_for(
                CredentialKind::Jwt,
                PlatformRole::None,
                Some(selected.tenant_role),
            )
            .into_iter()
            .filter(|permission| *permission != Permission::AccessConsole)
            .map(|permission| permission.as_str().to_string())
            .collect()
        } else {
            Vec::new()
        };
        Ok(ConsoleSession {
            id: user.id,
            email: user.email,
            name: user.name,
            platform_role,
            status: UserStatus::Active,
            token_version: user.token_version,
            selected_tenant: selected,
            memberships,
            capabilities: SessionCapabilities { platform, tenant },
            created_at: user.created_at,
            updated_at: user.updated_at,
        })
    }

    /// Selection grants no authority by itself; both identity and the target
    /// membership are loaded from the writer before signing a scoped token.
    pub async fn select_tenant(
        &self,
        ctx: &AuthContext,
        target: Option<Uuid>,
    ) -> Result<SessionTokenResponse> {
        if ctx.credential_kind != CredentialKind::Jwt
            || !ctx.has_permission(&Permission::AccessConsole)
        {
            return Err(KeyComputeError::PermissionDenied(
                "console session required".into(),
            ));
        }
        let service = self.user_service.as_ref().ok_or_else(|| {
            KeyComputeError::ServiceUnavailable("identity storage unavailable".into())
        })?;
        let (token_version, authz_version, membership_authz_version) = if let Some(tenant) = target
        {
            if tenant.is_nil() {
                return Err(KeyComputeError::ValidationError(
                    "tenant ID must be nonzero".into(),
                ));
            }
            let identity = service
                .load_jwt_identity(ctx.user_id, Some(tenant))
                .await?
                .filter(|row| row.active && row.token_version == ctx.token_version)
                .ok_or_else(|| {
                    KeyComputeError::PermissionDenied("active target membership required".into())
                })?;
            (
                identity.token_version,
                Some(identity.authz_version),
                Some(identity.membership_authz_version),
            )
        } else {
            let user = service.load_user(ctx.user_id).await?;
            if user.token_version != ctx.token_version {
                return Err(KeyComputeError::AuthError("identity has changed".into()));
            }
            (user.token_version, None, None)
        };
        let validator = self
            .get_jwt_validator()
            .ok_or_else(|| KeyComputeError::ServiceUnavailable("JWT signing unavailable".into()))?;
        let expires = validator.default_expiration();
        let token = validator.generate_identity_token(
            ctx.user_id,
            target,
            token_version,
            authz_version,
            membership_authz_version,
            expires,
        )?;
        self.session_response(token, expires).await
    }
    /// Revalidate a newly issued token before returning its presentation data.
    pub async fn session_response(
        &self,
        token: String,
        expires: i64,
    ) -> Result<SessionTokenResponse> {
        let ctx = self.verify_token(&token).await?;
        let session = self.console_session(&ctx).await?;
        Ok(SessionTokenResponse::new(session, token, expires))
    }
}
