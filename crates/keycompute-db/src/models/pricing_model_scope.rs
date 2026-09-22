use crate::DbError;
use crate::models::tenant_audit_event::{AuditContext, lock_identity_admin};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

/// A pricing scope is constructed by trusted console code and is never
/// deserialized from an HTTP request. The DAO still rechecks the current
/// authority in SQL while holding the transaction locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantPricingScope {
    tenant_id: Uuid,
    actor_user_id: Uuid,
    credential_kind: CredentialKind,
    token_version: i32,
    tenant_authz_version: i64,
    membership_authz_version: i64,
}

impl TenantPricingScope {
    pub fn checked(
        tenant_id: Uuid,
        actor_user_id: Uuid,
        credential_kind: CredentialKind,
        token_version: i32,
        tenant_authz_version: i64,
        membership_authz_version: i64,
    ) -> Result<Self, DbError> {
        if tenant_id.is_nil()
            || actor_user_id.is_nil()
            || token_version < 0
            || tenant_authz_version <= 0
            || membership_authz_version <= 0
        {
            return Err(DbError::Other(
                "tenant pricing scope requires current tenant authority".into(),
            ));
        }
        Ok(Self {
            tenant_id,
            actor_user_id,
            credential_kind,
            token_version,
            tenant_authz_version,
            membership_authz_version,
        })
    }

    pub const fn tenant_id(self) -> Uuid {
        self.tenant_id
    }

    pub const fn actor_user_id(self) -> Uuid {
        self.actor_user_id
    }

    pub const fn credential_kind(self) -> CredentialKind {
        self.credential_kind
    }

    pub const fn token_version(self) -> i32 {
        self.token_version
    }

    pub const fn tenant_authz_version(self) -> i64 {
        self.tenant_authz_version
    }

    pub const fn membership_authz_version(self) -> i64 {
        self.membership_authz_version
    }
}

/// A platform pricing scope is deliberately narrower than a generic
/// PlatformScope: pricing management is a root-only action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformPricingScope {
    actor_user_id: Uuid,
    credential_kind: CredentialKind,
    token_version: i32,
}

impl PlatformPricingScope {
    pub fn checked(
        actor_user_id: Uuid,
        credential_kind: CredentialKind,
        token_version: i32,
    ) -> Result<Self, DbError> {
        if actor_user_id.is_nil() || token_version < 0 {
            return Err(DbError::Other(
                "platform pricing scope requires current actor authority".into(),
            ));
        }
        Ok(Self {
            actor_user_id,
            credential_kind,
            token_version,
        })
    }

    pub const fn actor_user_id(self) -> Uuid {
        self.actor_user_id
    }

    pub const fn credential_kind(self) -> CredentialKind {
        self.credential_kind
    }

    pub const fn token_version(self) -> i32 {
        self.token_version
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PricingTarget {
    Platform,
    Tenant(Uuid),
}

impl PricingTarget {
    pub const fn scope_type(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Tenant(_) => "tenant",
        }
    }

    pub const fn tenant_id(self) -> Option<Uuid> {
        match self {
            Self::Platform => None,
            Self::Tenant(id) => Some(id),
        }
    }

    pub fn validate(self) -> Result<(), DbError> {
        if matches!(self, Self::Tenant(id) if id.is_nil()) {
            return Err(DbError::Other(
                "tenant pricing target must be non-nil".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PricingGroup {
    pub target: PricingTarget,
    pub model_name: String,
    pub billing_dimension: String,
}

impl PricingGroup {
    pub fn new(
        target: PricingTarget,
        model_name: impl Into<String>,
        billing_dimension: impl Into<String>,
    ) -> Result<Self, DbError> {
        target.validate()?;
        let model_name = model_name.into();
        let billing_dimension = billing_dimension.into();
        if model_name.trim().is_empty() || billing_dimension.trim().is_empty() {
            return Err(DbError::Other("pricing group labels are required".into()));
        }
        Ok(Self {
            target,
            model_name,
            billing_dimension,
        })
    }

    pub fn lock_key(&self) -> String {
        fn frame(value: &str) -> String {
            format!("{}:{}", value.len(), value)
        }

        match self.target {
            PricingTarget::Platform => format!(
                "platform|{}|{}",
                frame(&self.model_name),
                frame(&self.billing_dimension)
            ),
            PricingTarget::Tenant(tenant_id) => format!(
                "tenant|{}|{}|{}",
                frame(&tenant_id.to_string()),
                frame(&self.model_name),
                frame(&self.billing_dimension)
            ),
        }
    }
}

#[derive(Debug, FromQueryResult)]
struct CurrentUserRole {
    platform_role: String,
}

#[derive(Debug, FromQueryResult)]
struct CurrentMembershipRole {
    tenant_role: String,
    authz_version: i64,
}

/// Validate the credential and actor identity before reading any pricing row.
pub(crate) fn validate_actor(
    actor: &AuditContext,
    actor_user_id: Uuid,
    credential_kind: CredentialKind,
) -> Result<(), DbError> {
    if credential_kind != CredentialKind::Jwt
        || actor.credential_kind != CredentialKind::Jwt
        || actor.actor_user_id != actor_user_id
    {
        return Err(DbError::Other(
            "JWT pricing console authority required".into(),
        ));
    }
    Ok(())
}

/// Lock tenant, actor and membership in parent-first order. Tenant writes
/// always require an active tenant, including deletion and restriction paths.
pub(crate) async fn lock_tenant_authority(
    tx: &DatabaseTransaction,
    scope: TenantPricingScope,
    actor: &AuditContext,
) -> Result<AuditContext, DbError> {
    validate_actor(actor, scope.actor_user_id, scope.credential_kind)?;
    lock_identity_admin(tx).await?;

    let tenant = CurrentTenant::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT authz_version FROM tenants WHERE id=$1 AND status='active' FOR UPDATE",
        [scope.tenant_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::Other("active pricing tenant required".into()))?;
    if tenant.authz_version != scope.tenant_authz_version() {
        return Err(DbError::OptimisticConflict {
            entity: "tenant pricing authority".into(),
            id: scope.tenant_id().to_string(),
        });
    }

    let current = CurrentUserRole::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT platform_role FROM users WHERE id=$1 AND status='active' AND token_version=$2 FOR UPDATE",
        [scope.actor_user_id.into(), scope.token_version.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::Other("current pricing actor required".into()))?;
    let platform_role = current
        .platform_role
        .parse::<PlatformRole>()
        .map_err(DbError::Other)?;

    let membership = CurrentMembershipRole::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT tenant_role,authz_version FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 AND status='active' FOR UPDATE",
        [scope.tenant_id.into(), scope.actor_user_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::Other("active tenant membership required".into()))?;
    if membership.authz_version != scope.membership_authz_version() {
        return Err(DbError::OptimisticConflict {
            entity: "tenant pricing membership authority".into(),
            id: scope.actor_user_id().to_string(),
        });
    }
    let tenant_role = membership
        .tenant_role
        .parse::<TenantRole>()
        .map_err(DbError::Other)?;
    if tenant_role != TenantRole::Admin {
        return Err(DbError::Other(
            "tenant pricing administrator required".into(),
        ));
    }

    Ok(AuditContext {
        actor_platform_role: platform_role,
        actor_tenant_role: Some(tenant_role),
        ..*actor
    })
}

#[derive(Debug, FromQueryResult)]
struct CurrentTenant {
    authz_version: i64,
}

/// Lock the platform actor after the identity fence and any explicit target
/// tenant. The token version is checked while the actor row remains locked.
pub(crate) async fn lock_platform_authority(
    tx: &DatabaseTransaction,
    scope: PlatformPricingScope,
    target: PricingTarget,
    actor: &AuditContext,
    require_active_tenant: bool,
) -> Result<AuditContext, DbError> {
    validate_actor(actor, scope.actor_user_id, scope.credential_kind)?;
    target.validate()?;
    lock_identity_admin(tx).await?;

    if let PricingTarget::Tenant(tenant_id) = target {
        let tenant = tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status FROM tenants WHERE id=$1 FOR UPDATE",
                [tenant_id.into()],
            ))
            .await?
            .ok_or_else(|| DbError::not_found("pricing tenant", tenant_id))?;
        if require_active_tenant && tenant.try_get_by_index::<String>(0)? != "active" {
            return Err(DbError::Other("target pricing tenant is inactive".into()));
        }
    }

    let current = CurrentUserRole::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT platform_role FROM users WHERE id=$1 AND status='active' AND token_version=$2 FOR UPDATE",
        [scope.actor_user_id.into(), scope.token_version.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::Other("current root pricing actor required".into()))?;
    let role = current
        .platform_role
        .parse::<PlatformRole>()
        .map_err(DbError::Other)?;
    if role != PlatformRole::Root {
        return Err(DbError::Other("root pricing authority required".into()));
    }
    Ok(AuditContext {
        actor_platform_role: role,
        actor_tenant_role: None,
        ..*actor
    })
}

pub(crate) async fn lock_pricing_groups(
    tx: &DatabaseTransaction,
    groups: &[PricingGroup],
) -> Result<(), DbError> {
    let mut keys = groups
        .iter()
        .map(PricingGroup::lock_key)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    for key in keys {
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [key.into()],
        ))
        .await?;
    }
    Ok(())
}
