//! One authoritative assignment per (tenant, global user).
use super::tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin};
use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType, MembershipStatus, TenantRole, TenantScope};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct TenantMembership {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub tenant_role: String,
    pub status: String,
    pub invited_by: Option<Uuid>,
    pub joined_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
    pub authz_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenantMembershipRequest {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub tenant_role: TenantRole,
}

fn conflict(tenant: Uuid, user: Uuid) -> DbError {
    DbError::OptimisticConflict {
        entity: "tenant_membership".into(),
        id: format!("{tenant}/{user}"),
    }
}

impl TenantMembership {
    pub fn tenant_role(&self) -> Result<TenantRole, DbError> {
        self.tenant_role.parse().map_err(DbError::Other)
    }
    pub fn membership_status(&self) -> Result<MembershipStatus, DbError> {
        self.status.parse().map_err(DbError::Other)
    }

    pub async fn find(
        db: &impl ConnectionTrait,
        tenant: Uuid,
        user: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT m.* FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND t.status='active' AND u.status='active'",
            [tenant.into(),user.into()],
        )).one(db).await?)
    }
    /// Internal explicit identity lookup; includes retained inactive membership.
    pub async fn find_any(
        db: &impl ConnectionTrait,
        tenant: Uuid,
        user: Uuid,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2",
            [tenant.into(), user.into()],
        ))
        .one(db)
        .await?)
    }
    pub async fn list_active_for_user(
        db: &impl ConnectionTrait,
        user: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT m.* FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id JOIN users u ON u.id=m.user_id WHERE m.user_id=$1 AND m.status='active' AND t.status='active' AND u.status='active' ORDER BY m.created_at,m.tenant_id",
            [user.into()],
        )).all(db).await?)
    }
    pub async fn list_active_for_tenant(
        db: &impl ConnectionTrait,
        tenant: Uuid,
    ) -> Result<Vec<Self>, DbError> {
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT m.* FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.status='active' AND t.status='active' AND u.status='active' ORDER BY m.created_at,m.user_id",
            [tenant.into()],
        )).all(db).await?)
    }
    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Self>, DbError> {
        if scope.tenant_role() != TenantRole::Admin {
            return Err(DbError::Other("tenant admin required".into()));
        }
        Ok(Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT member.* FROM tenant_memberships member WHERE member.tenant_id=$1 AND EXISTS (SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id JOIN tenants t ON t.id=m.tenant_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.tenant_role='admin' AND m.status='active' AND u.status='active' AND t.status='active') ORDER BY member.created_at,member.user_id LIMIT $3 OFFSET $4",
            [scope.tenant_id().into(),scope.user_id().into(),limit.clamp(1,100).into(),offset.max(0).into()],
        )).all(db).await?)
    }

    /// Trusted administrative addition. Public email-based onboarding uses the
    /// invitation flow so an unrelated user cannot be silently enrolled.
    pub async fn create(
        tx: &DatabaseTransaction,
        req: &CreateTenantMembershipRequest,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, req.tenant_id).await?;
        if req.user_id.is_nil() {
            return Err(DbError::Other("member ID is required".into()));
        }
        super::user::User::find_by_id_for_no_key_update(tx, req.user_id)
            .await?
            .filter(|user| user.status == "active")
            .ok_or_else(|| DbError::Other("active member identity required".into()))?;
        let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status,invited_by) VALUES($1,$2,$3,'active',$4) ON CONFLICT (tenant_id,user_id) DO NOTHING RETURNING *",
            [req.tenant_id.into(),req.user_id.into(),req.tenant_role.as_str().into(),actor.actor_user_id.into()],
        )).one(tx).await?.ok_or_else(||conflict(req.tenant_id,req.user_id))?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(req.tenant_id),
            &actor,
            "membership.create",
            "tenant_membership",
            Some(&req.user_id.to_string()),
            AuditResult::Success,
            serde_json::json!({"user_id":req.user_id,"tenant_role":req.tenant_role.as_str()}),
        )
        .await?;
        Ok(row)
    }

    pub async fn set_role(
        tx: &DatabaseTransaction,
        tenant: Uuid,
        user: Uuid,
        role: TenantRole,
        expected: i64,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, tenant).await?;
        super::user::User::find_by_id_for_no_key_update(tx, user)
            .await?
            .ok_or_else(|| DbError::not_found("User", user))?;
        let before = Self::find_any(tx, tenant, user)
            .await?
            .ok_or_else(|| conflict(tenant, user))?;
        if expected <= 0 {
            return Err(conflict(tenant, user));
        }
        let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role=$3 WHERE tenant_id=$1 AND user_id=$2 AND status='active' AND authz_version=$4 RETURNING *",
            [tenant.into(),user.into(),role.as_str().into(),expected.into()],
        )).one(tx).await?.ok_or_else(||conflict(tenant,user))?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(tenant),
            &actor,
            "membership.role",
            "tenant_membership",
            Some(&user.to_string()),
            AuditResult::Success,
            serde_json::json!({"previous_tenant_role":before.tenant_role,"tenant_role":row.tenant_role,"authz_version":row.authz_version}),
        )
        .await?;
        Ok(row)
    }

    pub async fn set_status(
        tx: &DatabaseTransaction,
        tenant: Uuid,
        user: Uuid,
        status: MembershipStatus,
        expected: i64,
        actor: &AuditContext,
    ) -> Result<Self, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, tenant).await?;
        super::user::User::find_by_id_for_no_key_update(tx, user)
            .await?
            .ok_or_else(|| DbError::not_found("User", user))?;
        let before = Self::find_any(tx, tenant, user)
            .await?
            .ok_or_else(|| conflict(tenant, user))?;
        if expected <= 0 || (before.status == "removed" && status != MembershipStatus::Removed) {
            return Err(DbError::Other(
                "removed membership requires a new invitation".into(),
            ));
        }
        let row=Self::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE tenant_memberships SET status=$3,removed_at=CASE WHEN $3='removed' THEN clock_timestamp() ELSE NULL END WHERE tenant_id=$1 AND user_id=$2 AND authz_version=$4 RETURNING *",
            [tenant.into(),user.into(),status.as_str().into(),expected.into()],
        )).one(tx).await?.ok_or_else(||conflict(tenant,user))?;
        // The database trigger permanently revokes associated keys and pending
        // invitations. A later resume only changes this membership row.
        TenantAuditEvent::append(tx,AuditScopeType::Tenant,Some(tenant),&actor,"membership.status","tenant_membership",
            Some(&user.to_string()),AuditResult::Success,serde_json::json!({"previous_status":before.status,"status":row.status,"authz_version":row.authz_version})).await?;
        Ok(row)
    }
}
