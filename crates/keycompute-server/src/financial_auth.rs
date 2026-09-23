//! Signed console provenance for transaction-bound financial authorization.
use crate::{
    error::{ApiError, Result},
    extractors::RequestId,
};
use keycompute_auth::{AuthContext, AuthorizationAction};
use keycompute_db::{
    AuditContext,
    models::financial_scope::{FinancialMembership, FinancialScope, FinancialSession},
};
use uuid::Uuid;

fn session(ctx: &AuthContext) -> Result<FinancialSession> {
    let selected = ctx
        .selected_tenant_id
        .map(|id| {
            Ok::<_, ApiError>(FinancialMembership {
                tenant_id: id,
                tenant_role: ctx
                    .tenant_role
                    .ok_or_else(|| ApiError::Auth("Current membership required".into()))?,
                tenant_authz_version: ctx
                    .authz_version
                    .ok_or_else(|| ApiError::Auth("Current tenant version required".into()))?,
                membership_authz_version: ctx
                    .membership_authz_version
                    .ok_or_else(|| ApiError::Auth("Current membership version required".into()))?,
            })
        })
        .transpose()?;
    Ok(FinancialSession {
        user_id: ctx.user_id,
        credential_kind: ctx.credential_kind,
        token_version: ctx.token_version,
        expires_at: ctx
            .credential_expires_at
            .ok_or_else(|| ApiError::Auth("Expiring console session required".into()))?,
        selected,
    })
}
pub(crate) fn root_scope(ctx: &AuthContext, tenant_id: Uuid) -> Result<FinancialScope> {
    FinancialScope::platform_tenant(
        ctx.require_platform(AuthorizationAction::ManagePlatform)?,
        session(ctx)?,
        tenant_id,
    )
    .map_err(|_| ApiError::Forbidden("Current financial authority required".into()))
}
pub(crate) fn tenant_scope(
    access: &crate::tenant_access::TenantAdmin,
    tenant_id: Uuid,
) -> Result<FinancialScope> {
    access.require_path_tenant(tenant_id)?;
    FinancialScope::tenant_admin(
        access.require(AuthorizationAction::ManageTenantResource)?,
        session(&access.auth().authorization_context())?,
    )
    .map_err(|_| ApiError::Forbidden("Current financial authority required".into()))
}

pub(crate) fn audit(ctx: &AuthContext, id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: ctx.user_id,
        credential_kind: ctx.credential_kind,
        actor_platform_role: ctx.platform_role,
        actor_tenant_role: ctx.tenant_role,
        request_id: Some(id.0),
    }
}
