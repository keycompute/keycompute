//! Explicit tenant-scoped console access.
//!
//! `ConsoleAuth` contains the already-selected, database-validated tenant in
//! `AuthExtractor::tenant_id`. It is not a global context with an optional
//! selector. These extractors add the route binding and operation-specific
//! permission check so a platform role can never widen a tenant URL.

use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, RequestId},
    state::AppState,
};
use axum::{
    extract::{FromRequestParts, Path},
    http::request::Parts,
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::AuditContext;
use keycompute_types::{TenantRole, TenantScope};
use std::collections::HashMap;
use uuid::Uuid;

async fn route_tenant_id(parts: &mut Parts, state: &AppState) -> Result<Uuid> {
    let Path(parameters) = Path::<HashMap<String, String>>::from_request_parts(parts, state)
        .await
        .map_err(|_| ApiError::BadRequest("tenant_id must be a UUID".into()))?;
    let value = parameters
        .get("tenant_id")
        .ok_or_else(|| ApiError::BadRequest("tenant_id is required".into()))?;
    let tenant_id = Uuid::parse_str(value)
        .map_err(|_| ApiError::BadRequest("tenant_id must be a UUID".into()))?;
    if tenant_id.is_nil() {
        return Err(ApiError::BadRequest("tenant_id must be a real UUID".into()));
    }
    Ok(tenant_id)
}

fn require_selected_tenant(auth: &ConsoleAuth, route_tenant_id: Uuid) -> Result<()> {
    if auth.tenant_id.is_nil() {
        return Err(ApiError::Auth("a selected tenant is required".into()));
    }
    if auth.tenant_id != route_tenant_id {
        return Err(ApiError::Forbidden(
            "the selected tenant does not match the route".into(),
        ));
    }
    Ok(())
}

fn scope_for(auth: &ConsoleAuth) -> TenantScope {
    TenantScope::checked(
        auth.tenant_id,
        auth.user_id,
        auth.tenant_role
            .expect("tenant access extraction validates an active membership"),
    )
    .expect("tenant access extraction validates non-nil identity")
}

fn audit_for(auth: &ConsoleAuth, request_id: RequestId) -> AuditContext {
    AuditContext {
        actor_user_id: auth.user_id,
        credential_kind: auth.credential_kind,
        actor_platform_role: auth.platform_role,
        actor_tenant_role: auth.tenant_role,
        request_id: Some(request_id.0),
    }
}

/// A verified JWT console identity with an active admin membership in the
/// named tenant. The wrapped concrete `ConsoleAuth` is the shared auth type;
/// its fields remain private to the extractor proof.
#[derive(Debug, Clone)]
pub struct TenantAdmin {
    auth: ConsoleAuth,
    tenant_id: Uuid,
}

impl TenantAdmin {
    pub fn auth(&self) -> &ConsoleAuth {
        &self.auth
    }

    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn require_path_tenant(&self, tenant_id: Uuid) -> Result<()> {
        if tenant_id.is_nil() {
            return Err(ApiError::BadRequest("tenant_id must be a real UUID".into()));
        }
        require_selected_tenant(&self.auth, tenant_id)
    }

    pub fn require(&self, action: AuthorizationAction) -> Result<TenantScope> {
        self.auth.require_tenant(action)?;
        Ok(self.scope())
    }

    pub fn scope(&self) -> TenantScope {
        scope_for(&self.auth)
    }

    pub fn audit(&self, request_id: RequestId) -> AuditContext {
        audit_for(&self.auth, request_id)
    }
}

impl FromRequestParts<AppState> for TenantAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let tenant_id = route_tenant_id(parts, state).await?;
        let auth = ConsoleAuth::from_request_parts(parts, state).await?;
        require_selected_tenant(&auth, tenant_id)?;
        if auth.tenant_role != Some(TenantRole::Admin) {
            return Err(ApiError::Forbidden(
                "active tenant administrator membership required".into(),
            ));
        }
        auth.require_tenant(AuthorizationAction::View)?;
        Ok(Self { auth, tenant_id })
    }
}

/// A verified JWT console identity with an active membership in the named
/// tenant. This is used for tenant context reads.
#[derive(Debug, Clone)]
pub struct TenantMember {
    auth: ConsoleAuth,
    tenant_id: Uuid,
}

impl TenantMember {
    pub fn auth(&self) -> &ConsoleAuth {
        &self.auth
    }

    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn require_path_tenant(&self, tenant_id: Uuid) -> Result<()> {
        if tenant_id.is_nil() {
            return Err(ApiError::BadRequest("tenant_id must be a real UUID".into()));
        }
        require_selected_tenant(&self.auth, tenant_id)
    }

    pub fn require(&self, action: AuthorizationAction) -> Result<TenantScope> {
        self.auth.require_tenant(action)?;
        Ok(self.scope())
    }

    pub fn scope(&self) -> TenantScope {
        scope_for(&self.auth)
    }

    pub fn audit(&self, request_id: RequestId) -> AuditContext {
        audit_for(&self.auth, request_id)
    }
}

impl FromRequestParts<AppState> for TenantMember {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        let tenant_id = route_tenant_id(parts, state).await?;
        let auth = ConsoleAuth::from_request_parts(parts, state).await?;
        require_selected_tenant(&auth, tenant_id)?;
        if auth.tenant_role.is_none() {
            return Err(ApiError::Forbidden(
                "active tenant membership required".into(),
            ));
        }
        auth.require_tenant(AuthorizationAction::View)?;
        Ok(Self { auth, tenant_id })
    }
}
