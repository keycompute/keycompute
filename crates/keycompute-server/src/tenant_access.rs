//! Explicit tenant-scoped console access.
//!
//! `ConsoleAuth` contains the already-selected, database-validated tenant in
//! `AuthExtractor::tenant_id`. It is not a global context with an optional
//! selector. These extractors add the route binding and operation-specific
//! permission check. Platform identities use separate `/platform/.../control`
//! routes and never acquire a synthetic membership row.

use crate::{
    error::{ApiError, Result},
    extractors::{ConsoleAuth, GlobalConsoleAuth, RequestId},
    state::AppState,
};
use axum::{
    extract::{FromRequestParts, Path},
    http::request::Parts,
};
use keycompute_auth::AuthorizationAction;
use keycompute_db::{AuditContext, models::tenant_control::TenantAuthzSnapshot};
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

    pub fn snapshot(&self) -> TenantAuthzSnapshot {
        TenantAuthzSnapshot {
            token_version: self.auth.token_version,
            tenant_authz_version: self.auth.authz_version,
            membership_authz_version: self.auth.membership_authz_version,
        }
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

/// Explicit root/operator authority over one target tenant. This scope exists
/// only on platform-target routes and does not create or imply membership.
#[derive(Debug, Clone)]
pub struct PlatformTenantAdmin {
    auth: GlobalConsoleAuth,
    tenant_id: Uuid,
}

impl PlatformTenantAdmin {
    pub fn auth(&self) -> &keycompute_auth::AuthContext {
        &self.auth.0
    }

    pub const fn tenant_id(&self) -> Uuid {
        self.tenant_id
    }

    pub fn require_path_tenant(&self, tenant_id: Uuid) -> Result<()> {
        if tenant_id.is_nil() {
            return Err(ApiError::BadRequest("tenant_id must be a real UUID".into()));
        }
        if tenant_id != self.tenant_id {
            return Err(ApiError::Forbidden(
                "the target tenant does not match the route".into(),
            ));
        }
        Ok(())
    }

    pub fn require(&self, _action: AuthorizationAction) -> Result<TenantScope> {
        self.auth
            .require_platform(AuthorizationAction::ManagePlatform)?;
        Ok(self.scope())
    }

    pub fn platform_scope(&self) -> Result<keycompute_types::PlatformScope> {
        self.auth
            .require_platform(AuthorizationAction::ManagePlatform)
            .map_err(ApiError::from)
    }

    pub fn scope(&self) -> TenantScope {
        TenantScope::checked(self.tenant_id, self.auth.user_id, TenantRole::Admin)
            .expect("platform tenant extraction validates non-nil identities")
    }

    pub fn audit(&self, request_id: RequestId) -> AuditContext {
        AuditContext {
            actor_user_id: self.auth.user_id,
            credential_kind: self.auth.credential_kind,
            actor_platform_role: self.auth.platform_role,
            actor_tenant_role: None,
            request_id: Some(request_id.0),
        }
    }

    pub fn snapshot(&self) -> TenantAuthzSnapshot {
        TenantAuthzSnapshot {
            token_version: self.auth.token_version,
            tenant_authz_version: 0,
            membership_authz_version: 0,
        }
    }
}

impl FromRequestParts<AppState> for PlatformTenantAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        let tenant_id = route_tenant_id(parts, state).await?;
        let auth = GlobalConsoleAuth::from_request_parts(parts, state).await?;
        if auth.selected_tenant_id.is_some() {
            return Err(ApiError::Forbidden(
                "global platform session required".into(),
            ));
        }
        auth.require_platform(AuthorizationAction::ManagePlatform)?;
        Ok(Self { auth, tenant_id })
    }
}

/// Route-aware tenant administration. Ordinary tenant URLs remain strictly
/// membership-bound; only explicit platform control URLs accept root/operator.
#[derive(Debug, Clone)]
pub enum TenantAdministration {
    Tenant(TenantAdmin),
    Platform(PlatformTenantAdmin),
}

impl TenantAdministration {
    pub fn authorization_context(&self) -> keycompute_auth::AuthContext {
        match self {
            Self::Tenant(access) => access.auth().authorization_context(),
            Self::Platform(access) => access.auth().clone(),
        }
    }

    pub fn platform_scope(&self) -> Result<Option<keycompute_types::PlatformScope>> {
        match self {
            Self::Tenant(_) => Ok(None),
            Self::Platform(access) => access.platform_scope().map(Some),
        }
    }

    pub fn require_path_tenant(&self, tenant_id: Uuid) -> Result<()> {
        match self {
            Self::Tenant(access) => access.require_path_tenant(tenant_id),
            Self::Platform(access) => access.require_path_tenant(tenant_id),
        }
    }

    pub fn require(&self, action: AuthorizationAction) -> Result<TenantScope> {
        match self {
            Self::Tenant(access) => access.require(action),
            Self::Platform(access) => access.require(action),
        }
    }

    pub fn scope(&self) -> TenantScope {
        match self {
            Self::Tenant(access) => access.scope(),
            Self::Platform(access) => access.scope(),
        }
    }

    pub fn audit(&self, request_id: RequestId) -> AuditContext {
        match self {
            Self::Tenant(access) => access.audit(request_id),
            Self::Platform(access) => access.audit(request_id),
        }
    }

    pub fn snapshot(&self) -> TenantAuthzSnapshot {
        match self {
            Self::Tenant(access) => access.snapshot(),
            Self::Platform(access) => access.snapshot(),
        }
    }
}

impl FromRequestParts<AppState> for TenantAdministration {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        if parts.uri.path().starts_with("/api/v1/platform/") {
            PlatformTenantAdmin::from_request_parts(parts, state)
                .await
                .map(Self::Platform)
        } else {
            TenantAdmin::from_request_parts(parts, state)
                .await
                .map(Self::Tenant)
        }
    }
}

/// Route-aware tenant context read. Members may read their selected workspace;
/// platform administrators may read an explicit target through control URLs.
#[derive(Debug, Clone)]
pub enum TenantContextAccess {
    Tenant(TenantMember),
    Platform(PlatformTenantAdmin),
}

impl TenantContextAccess {
    pub fn require_path_tenant(&self, tenant_id: Uuid) -> Result<()> {
        match self {
            Self::Tenant(access) => access.require_path_tenant(tenant_id),
            Self::Platform(access) => access.require_path_tenant(tenant_id),
        }
    }

    pub fn scope(&self) -> TenantScope {
        match self {
            Self::Tenant(access) => access.scope(),
            Self::Platform(access) => access.scope(),
        }
    }
}

impl FromRequestParts<AppState> for TenantContextAccess {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        if parts.uri.path().starts_with("/api/v1/platform/") {
            PlatformTenantAdmin::from_request_parts(parts, state)
                .await
                .map(Self::Platform)
        } else {
            TenantMember::from_request_parts(parts, state)
                .await
                .map(Self::Tenant)
        }
    }
}
