//! Current console authority for financial reads and transactions.
//! Scope is an immutable selector, never a substitute for current DB authority.
use crate::{AuditContext, DbError};
use chrono::Utc;
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, TenantRole, TenantScope};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement, Value};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
pub struct FinancialMembership {
    pub tenant_id: Uuid,
    pub tenant_role: TenantRole,
    pub tenant_authz_version: i64,
    pub membership_authz_version: i64,
}
#[derive(Debug, Clone, Copy)]
pub struct FinancialSession {
    pub user_id: Uuid,
    pub credential_kind: CredentialKind,
    pub token_version: i32,
    pub expires_at: i64,
    pub selected: Option<FinancialMembership>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinancialAccess {
    Personal,
    TenantAdmin,
    PlatformTenant,
    PlatformGlobal,
}
#[derive(Debug, Clone, Copy)]
pub struct FinancialScope {
    session: FinancialSession,
    access: FinancialAccess,
    tenant_id: Option<Uuid>,
}
fn denied() -> DbError {
    DbError::Other("financial_authority_invalid".into())
}
impl FinancialScope {
    fn validate_session(session: FinancialSession) -> Result<(), DbError> {
        if session.user_id.is_nil()
            || session.credential_kind != CredentialKind::Jwt
            || session.token_version < 0
            || session.expires_at <= Utc::now().timestamp()
            || session.selected.is_some_and(|m| {
                m.tenant_id.is_nil()
                    || m.tenant_authz_version <= 0
                    || m.membership_authz_version <= 0
            })
        {
            return Err(denied());
        }
        Ok(())
    }
    pub fn personal(scope: TenantScope, session: FinancialSession) -> Result<Self, DbError> {
        Self::selected(scope, session, false)
    }
    pub fn tenant_admin(scope: TenantScope, session: FinancialSession) -> Result<Self, DbError> {
        Self::selected(scope, session, true)
    }
    fn selected(
        scope: TenantScope,
        session: FinancialSession,
        admin: bool,
    ) -> Result<Self, DbError> {
        Self::validate_session(session)?;
        let m = session.selected.ok_or_else(denied)?;
        if session.user_id != scope.user_id()
            || m.tenant_id != scope.tenant_id()
            || m.tenant_role != scope.tenant_role()
            || (admin && m.tenant_role != TenantRole::Admin)
        {
            return Err(denied());
        }
        Ok(Self {
            session,
            access: if admin {
                FinancialAccess::TenantAdmin
            } else {
                FinancialAccess::Personal
            },
            tenant_id: Some(m.tenant_id),
        })
    }
    pub fn platform_tenant(
        scope: PlatformScope,
        session: FinancialSession,
        tenant_id: Uuid,
    ) -> Result<Self, DbError> {
        let mut result = Self::platform_global(scope, session)?;
        if tenant_id.is_nil() {
            return Err(denied());
        }
        result.access = FinancialAccess::PlatformTenant;
        result.tenant_id = Some(tenant_id);
        Ok(result)
    }
    pub fn platform_global(
        scope: PlatformScope,
        session: FinancialSession,
    ) -> Result<Self, DbError> {
        Self::validate_session(session)?;
        if scope.user_id() != session.user_id || scope.platform_role() != PlatformRole::Root {
            return Err(denied());
        }
        Ok(Self {
            session,
            access: FinancialAccess::PlatformGlobal,
            tenant_id: None,
        })
    }
    pub fn tenant_id(self) -> Result<Uuid, DbError> {
        self.tenant_id.ok_or_else(denied)
    }
    pub fn user_id(self) -> Uuid {
        self.session.user_id
    }
    pub fn access(self) -> FinancialAccess {
        self.access
    }
    pub fn require_personal(self) -> Result<(), DbError> {
        if self.access == FinancialAccess::Personal {
            Ok(())
        } else {
            Err(denied())
        }
    }
    pub fn require_admin(self) -> Result<(), DbError> {
        if matches!(
            self.access,
            FinancialAccess::TenantAdmin | FinancialAccess::PlatformTenant
        ) {
            Ok(())
        } else {
            Err(denied())
        }
    }
    pub fn require_root_tenant(self) -> Result<(), DbError> {
        if self.access == FinancialAccess::PlatformTenant {
            Ok(())
        } else {
            Err(denied())
        }
    }
    pub fn require_root_global(self) -> Result<(), DbError> {
        if self.access == FinancialAccess::PlatformGlobal {
            Ok(())
        } else {
            Err(denied())
        }
    }
    // Parameters 1..9 are reserved for the complete request authority. No
    // tenant API has a client-controlled missing-tenant/all-rows switch.
    pub(super) fn values(self) -> Vec<Value> {
        let m = self.session.selected;
        vec![
            self.session.user_id.into(),
            self.session.token_version.into(),
            self.session.expires_at.into(),
            m.map(|v| v.tenant_id).into(),
            m.map(|v| v.tenant_role.as_str()).into(),
            m.map(|v| v.tenant_authz_version).into(),
            m.map(|v| v.membership_authz_version).into(),
            self.tenant_id.into(),
            matches!(
                self.access,
                FinancialAccess::PlatformTenant | FinancialAccess::PlatformGlobal
            )
            .into(),
        ]
    }
    pub(super) fn predicate(self) -> &'static str {
        "EXISTS(SELECT 1 FROM users financial_actor WHERE financial_actor.id=$1 AND financial_actor.status='active' AND financial_actor.token_version=$2 AND clock_timestamp()<to_timestamp($3::double precision) AND (NOT $9::boolean OR financial_actor.platform_role='root') AND (($4::uuid IS NULL AND $9::boolean) OR EXISTS(SELECT 1 FROM tenant_memberships financial_m JOIN tenants financial_t ON financial_t.id=financial_m.tenant_id WHERE financial_m.user_id=$1 AND financial_m.tenant_id=$4 AND financial_m.status='active' AND financial_m.tenant_role=$5 AND financial_m.authz_version=$7 AND financial_t.status='active' AND financial_t.authz_version=$6)) AND ($8::uuid IS NULL OR EXISTS(SELECT 1 FROM tenants financial_target WHERE financial_target.id=$8 AND ($9::boolean OR financial_target.status='active'))))"
    }
    pub(super) fn owner_predicate(self, alias: &str) -> String {
        if self.access == FinancialAccess::Personal {
            format!(" AND {alias}.owner_user_id=$1")
        } else {
            String::new()
        }
    }
    pub(super) async fn current_actor(
        self,
        db: &impl ConnectionTrait,
    ) -> Result<AuditContext, DbError> {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT u.platform_role FROM users u WHERE u.id=$1 AND {}",
                    self.predicate()
                ),
                self.values(),
            ))
            .await?
            .ok_or_else(denied)?;
        let role: String = row.try_get("", "platform_role")?;
        Ok(AuditContext {
            actor_user_id: self.session.user_id,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: role.parse().map_err(DbError::Other)?,
            actor_tenant_role: self.session.selected.map(|m| m.tenant_role),
            request_id: None,
        })
    }
    /// Final wall-clock check after an authorized command intentionally changes
    /// its own selected tenant or security state. Call only while authority rows
    /// remain locked by lock_related; this is not a substitute for DB validation.
    pub(crate) fn check_expiry(self) -> Result<(), DbError> {
        Self::validate_session(self.session)
    }
    pub(super) async fn lock(
        self,
        tx: &DatabaseTransaction,
        audit: &AuditContext,
    ) -> Result<AuditContext, DbError> {
        self.lock_related(tx, audit, &[], &[]).await
    }

    /// Deterministic parent/user locks for platform lifecycle operations whose
    /// target may own other tenants. Normal finance callers pass no extra rows.
    pub(crate) async fn lock_related(
        self,
        tx: &DatabaseTransaction,
        audit: &AuditContext,
        tenants: &[Uuid],
        users: &[Uuid],
    ) -> Result<AuditContext, DbError> {
        Self::validate_session(self.session)?;
        if audit.actor_user_id != self.session.user_id
            || audit.credential_kind != CredentialKind::Jwt
            || audit.request_id.is_none_or(|id| id.is_nil())
            || tenants.iter().chain(users).any(Uuid::is_nil)
        {
            return Err(denied());
        }
        super::tenant_audit_event::lock_identity_admin(tx).await?;
        self.current_actor(tx).await?;
        let mut parents = tenants.to_vec();
        if let Some(id) = self.tenant_id {
            parents.push(id);
        }
        if let Some(m) = self.session.selected {
            parents.push(m.tenant_id);
        }
        parents.sort_unstable();
        parents.dedup();
        for id in parents {
            tx.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM tenants WHERE id=$1 FOR UPDATE",
                [id.into()],
            ))
            .await?
            .ok_or_else(|| DbError::not_found("Tenant", id))?;
        }
        let mut actors = users.to_vec();
        actors.push(self.session.user_id);
        actors.sort_unstable();
        actors.dedup();
        for id in actors {
            tx.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM users WHERE id=$1 FOR UPDATE",
                [id.into()],
            ))
            .await?
            .ok_or_else(|| DbError::not_found("User", id))?;
        }
        if let Some(m) = self.session.selected {
            tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT user_id FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",[m.tenant_id.into(),self.session.user_id.into()]))
                .await?.ok_or_else(denied)?;
        }
        let mut actor = self.current_actor(tx).await?;
        actor.request_id = audit.request_id;
        Ok(actor)
    }
}
