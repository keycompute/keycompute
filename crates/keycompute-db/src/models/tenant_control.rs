//! Safe tenant-control projections and transaction-bound mutations.
//!
//! This module deliberately does not expose `platform_role`, `token_version`,
//! password material, invitation hashes, or memberships outside the fixed
//! tenant predicate. Every write takes the identity fence, locks the tenant
//! parent first, and records its audit event in the same transaction.

use super::{
    tenant::Tenant,
    tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin},
    tenant_invitation::{CreateTenantInvitationRequest, CreatedTenantInvitation, TenantInvitation},
    tenant_membership::TenantMembership,
    user::{User, normalized_email},
};
use crate::DbError;
use chrono::{DateTime, Duration, Utc};
use keycompute_types::{
    AuditResult, AuditScopeType, MembershipStatus, TenantInvitationStatus, TenantRole, TenantScope,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_TENANT_CONTROL_PAGE_SIZE: i64 = 100;
pub const MIN_INVITATION_TTL_SECONDS: i64 = 300;
pub const MAX_INVITATION_TTL_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const MAX_TENANT_DESCRIPTION_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TenantContext {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    pub default_rpm_limit: i32,
    pub default_tpm_limit: i32,
    pub authz_version: i64,
    pub tenant_role: String,
    pub membership_authz_version: i64,
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TenantMember {
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub user_status: String,
    pub tenant_role: String,
    pub membership_status: String,
    pub authz_version: i64,
    pub invited_by: Option<Uuid>,
    pub joined_at: DateTime<Utc>,
    pub removed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TenantInvitationView {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub invited_by: Uuid,
    pub email: String,
    pub tenant_role: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_by: Option<Uuid>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct TenantAuditView {
    pub id: Uuid,
    pub scope_type: String,
    pub tenant_id: Option<Uuid>,
    pub actor_user_id: Uuid,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_id: Option<Uuid>,
    pub credential_kind: String,
    pub platform_role: String,
    pub tenant_role: Option<String>,
    pub metadata: serde_json::Value,
    pub result: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub page: i64,
    pub page_size: i64,
    pub offset: i64,
}

impl Page {
    pub fn bounded(page: i64, page_size: i64) -> Self {
        let page_size = page_size.clamp(1, MAX_TENANT_CONTROL_PAGE_SIZE);
        let page = page.clamp(1, 1_000_000);
        Self {
            page,
            page_size,
            offset: (page - 1).checked_mul(page_size).unwrap_or(i64::MAX),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MemberPatch {
    pub expected_authz_version: i64,
    pub tenant_role: Option<TenantRole>,
    pub status: Option<MembershipStatus>,
}

#[derive(Debug, Clone)]
pub struct TenantPatch {
    pub expected_authz_version: i64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub default_rpm_limit: Option<i32>,
    pub default_tpm_limit: Option<i32>,
}

/// Authorization values carried by a verified selected-tenant JWT.
#[derive(Debug, Clone, Copy)]
pub struct TenantAuthzSnapshot {
    pub token_version: i32,
    pub tenant_authz_version: i64,
    pub membership_authz_version: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct TenantWriteAuthority {
    pub tenant_id: Uuid,
    pub actor: AuditContext,
}

fn conflict(entity: &str, id: impl std::fmt::Display) -> DbError {
    DbError::OptimisticConflict {
        entity: entity.into(),
        id: id.to_string(),
    }
}

fn invariant(message: impl Into<String>) -> DbError {
    DbError::Other(message.into())
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => escaped.push_str(r"\%"),
            '_' => escaped.push_str(r"\_"),
            '\\' => escaped.push_str(r"\\"),
            character => escaped.push(character),
        }
    }
    escaped
}

const ADMIN_ACTOR_PREDICATE: &str = "EXISTS (
    SELECT 1
    FROM tenant_memberships actor_membership
    JOIN users actor_user ON actor_user.id = actor_membership.user_id
    JOIN tenants actor_tenant ON actor_tenant.id = actor_membership.tenant_id
    WHERE actor_membership.tenant_id = $1
      AND actor_membership.user_id = $2
      AND actor_membership.tenant_role = 'admin'
      AND actor_membership.status = 'active'
      AND actor_user.status = 'active'
      AND actor_tenant.status = 'active'
)";

const MEMBER_ACTOR_PREDICATE: &str = "EXISTS (
    SELECT 1
    FROM tenant_memberships actor_membership
    JOIN users actor_user ON actor_user.id = actor_membership.user_id
    JOIN tenants actor_tenant ON actor_tenant.id = actor_membership.tenant_id
    WHERE actor_membership.tenant_id = $1
      AND actor_membership.user_id = $2
      AND actor_membership.status = 'active'
      AND actor_user.status = 'active'
      AND actor_tenant.status = 'active'
)";

async fn lock_tenant_parent_after_fence(
    tx: &DatabaseTransaction,
    tenant_id: Uuid,
) -> Result<Tenant, DbError> {
    if tenant_id.is_nil() {
        return Err(invariant("tenant ID is required"));
    }
    Tenant::find_by_id_for_update(tx, tenant_id)
        .await?
        .filter(|tenant| tenant.is_active())
        .ok_or_else(|| DbError::not_found("Tenant", tenant_id))
}

/// Lock the tenant parent after acquiring the identity-admin fence.
pub async fn lock_tenant_parent(
    tx: &DatabaseTransaction,
    scope: TenantScope,
) -> Result<Tenant, DbError> {
    lock_identity_admin(tx).await?;
    lock_tenant_parent_after_fence(tx, scope.tenant_id()).await
}

/// Compare the verified JWT fence with live rows under deterministic locks.
/// The returned audit roles are derived from those locked rows.
pub async fn revalidate_in_transaction(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    actor: &AuditContext,
) -> Result<TenantWriteAuthority, DbError> {
    if scope.tenant_role() != TenantRole::Admin
        || actor.actor_user_id != scope.user_id()
        || actor.credential_kind != keycompute_types::CredentialKind::Jwt
        || actor.actor_user_id.is_nil()
        || snapshot.token_version < 0
        || snapshot.tenant_authz_version <= 0
        || snapshot.membership_authz_version <= 0
    {
        return Err(invariant("current JWT tenant authority required"));
    }

    lock_identity_admin(tx).await?;
    let tenant = lock_tenant_parent_after_fence(tx, scope.tenant_id()).await?;
    let user = User::find_by_id_for_update(tx, scope.user_id())
        .await?
        .filter(|user| user.status == "active")
        .ok_or_else(|| invariant("active actor required"))?;
    let membership = TenantMembership::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 AND status='active' FOR UPDATE",
        [tenant.id.into(), user.id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| invariant("active tenant membership required"))?;

    if user.token_version != snapshot.token_version
        || tenant.authz_version != snapshot.tenant_authz_version
        || membership.authz_version != snapshot.membership_authz_version
        || membership.tenant_role()? != scope.tenant_role()
    {
        return Err(conflict("tenant authority", tenant.id));
    }

    Ok(TenantWriteAuthority {
        tenant_id: tenant.id,
        actor: AuditContext {
            actor_platform_role: user.platform_role()?,
            actor_tenant_role: Some(membership.tenant_role()?),
            ..*actor
        },
    })
}

async fn lock_target_membership(
    tx: &DatabaseTransaction,
    tenant_id: Uuid,
    user_id: Uuid,
) -> Result<(User, TenantMembership), DbError> {
    if user_id.is_nil() {
        return Err(DbError::not_found("Tenant member", user_id));
    }
    // A foreign identity must never be locked through a tenant member URL.
    let user = User::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT u.* FROM users u JOIN tenant_memberships m ON m.user_id=u.id WHERE m.tenant_id=$1 AND u.id=$2 FOR UPDATE OF u",
        [tenant_id.into(), user_id.into()],
    )).one(tx).await?.ok_or_else(|| DbError::not_found("Tenant member", user_id))?;
    let membership = TenantMembership::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",
        [tenant_id.into(), user_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("Tenant member", user_id))?;
    Ok((user, membership))
}

pub async fn get_tenant_context(
    db: &impl ConnectionTrait,
    scope: TenantScope,
) -> Result<Option<TenantContext>, DbError> {
    let sql = format!(
        "SELECT t.id,t.name,t.slug,t.description,t.status,t.default_rpm_limit,t.default_tpm_limit,t.authz_version,m.tenant_role,m.authz_version AS membership_authz_version
         FROM tenants t
         JOIN tenant_memberships m ON m.tenant_id=t.id AND m.user_id=$2 AND m.status='active'
         JOIN users u ON u.id=m.user_id AND u.status='active'
         WHERE t.id=$1 AND t.status='active' AND {MEMBER_ACTOR_PREDICATE}"
    );
    Ok(
        TenantContext::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [scope.tenant_id().into(), scope.user_id().into()],
        ))
        .one(db)
        .await?,
    )
}

pub async fn list_members(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    search: Option<&str>,
    status: Option<MembershipStatus>,
    page: Page,
) -> Result<Vec<TenantMember>, DbError> {
    let page = Page::bounded(page.page, page.page_size);

    let search = search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(escape_like_pattern);
    let sql = format!(
        "SELECT m.user_id,u.email,u.name,u.status AS user_status,m.tenant_role,m.status AS membership_status,m.authz_version,m.invited_by,m.joined_at,m.removed_at
         FROM tenant_memberships m
         JOIN users u ON u.id=m.user_id
         WHERE m.tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE}
           AND ($3::text IS NULL OR LOWER(u.email) LIKE '%' || LOWER($3) || '%' ESCAPE '\\'
                OR LOWER(COALESCE(u.name,'')) LIKE '%' || LOWER($3) || '%' ESCAPE '\\'
                OR u.id::text LIKE '%' || LOWER($3) || '%' ESCAPE '\\')
           AND ($4::text IS NULL OR m.status=$4)
         ORDER BY LOWER(u.email),m.user_id
         LIMIT $5 OFFSET $6"
    );
    Ok(
        TenantMember::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                scope.tenant_id().into(),
                scope.user_id().into(),
                search.into(),
                status.map(|value| value.as_str()).into(),
                page.page_size.into(),
                page.offset.into(),
            ],
        ))
        .all(db)
        .await?,
    )
}

pub async fn count_members(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    search: Option<&str>,
    status: Option<MembershipStatus>,
) -> Result<i64, DbError> {
    #[derive(Debug, FromQueryResult)]
    struct Count {
        total: i64,
    }
    let search = search
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(escape_like_pattern);
    let sql = format!(
        "SELECT COUNT(*)::BIGINT AS total
         FROM tenant_memberships m
         JOIN users u ON u.id=m.user_id
         WHERE m.tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE}
           AND ($3::text IS NULL OR LOWER(u.email) LIKE '%' || LOWER($3) || '%' ESCAPE '\\'
                OR LOWER(COALESCE(u.name,'')) LIKE '%' || LOWER($3) || '%' ESCAPE '\\'
                OR u.id::text LIKE '%' || LOWER($3) || '%' ESCAPE '\\')
           AND ($4::text IS NULL OR m.status=$4)"
    );
    Ok(Count::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        &sql,
        [
            scope.tenant_id().into(),
            scope.user_id().into(),
            search.into(),
            status.map(|value| value.as_str()).into(),
        ],
    ))
    .one(db)
    .await?
    .map(|row| row.total)
    .unwrap_or(0))
}

pub async fn get_member(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    user_id: Uuid,
) -> Result<Option<TenantMember>, DbError> {
    let sql = format!(
        "SELECT m.user_id,u.email,u.name,u.status AS user_status,m.tenant_role,m.status AS membership_status,m.authz_version,m.invited_by,m.joined_at,m.removed_at
         FROM tenant_memberships m
         JOIN users u ON u.id=m.user_id
         WHERE m.tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE} AND m.user_id=$3"
    );
    Ok(
        TenantMember::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                scope.tenant_id().into(),
                scope.user_id().into(),
                user_id.into(),
            ],
        ))
        .one(db)
        .await?,
    )
}

pub async fn update_tenant(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    patch: &TenantPatch,
    actor: &AuditContext,
) -> Result<TenantContext, DbError> {
    if patch.name.is_none()
        && patch.description.is_none()
        && patch.default_rpm_limit.is_none()
        && patch.default_tpm_limit.is_none()
    {
        return Err(invariant("at least one tenant field is required"));
    }
    if patch.expected_authz_version <= 0 {
        return Err(conflict("tenant", scope.tenant_id()));
    }
    if patch
        .name
        .as_deref()
        .is_some_and(|name| name.trim().is_empty() || name.len() > 255)
        || patch
            .description
            .as_deref()
            .is_some_and(|description| description.len() > MAX_TENANT_DESCRIPTION_BYTES)
        || patch.default_rpm_limit.is_some_and(|value| value < 0)
        || patch.default_tpm_limit.is_some_and(|value| value < 0)
    {
        return Err(invariant("invalid tenant configuration"));
    }
    let authority = revalidate_in_transaction(tx, scope, snapshot, actor).await?;
    let tenant = Tenant::find_by_id_for_update(tx, authority.tenant_id)
        .await?
        .ok_or_else(|| DbError::not_found("Tenant", authority.tenant_id))?;
    let updated = Tenant::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET name=COALESCE($2,name),description=COALESCE($3,description),default_rpm_limit=COALESCE($4,default_rpm_limit),default_tpm_limit=COALESCE($5,default_tpm_limit) WHERE id=$1 AND authz_version=$6 RETURNING *",
        [tenant.id.into(), patch.name.as_deref().map(str::trim).into(), patch.description.clone().into(), patch.default_rpm_limit.into(), patch.default_tpm_limit.into(), patch.expected_authz_version.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| conflict("tenant", tenant.id))?;
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(tenant.id),
        &authority.actor,
        "tenant.update",
        "tenant",
        Some(&tenant.id.to_string()),
        AuditResult::Success,
        serde_json::json!({
            "authz_version": updated.authz_version,
            "changed": {
                "name": patch.name.is_some(),
                "description": patch.description.is_some(),
                "default_rpm_limit": patch.default_rpm_limit.is_some(),
                "default_tpm_limit": patch.default_tpm_limit.is_some()
            }
        }),
    )
    .await?;
    get_tenant_context(tx, scope)
        .await?
        .ok_or_else(|| invariant("updated tenant context disappeared"))
}

pub async fn update_member(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    user_id: Uuid,
    patch: &MemberPatch,
    actor: &AuditContext,
) -> Result<TenantMember, DbError> {
    if patch.expected_authz_version <= 0 || (patch.tenant_role.is_none() && patch.status.is_none())
    {
        return Err(invariant(
            "expected_authz_version and at least one membership field are required",
        ));
    }
    let authority = revalidate_in_transaction(tx, scope, snapshot, actor).await?;
    let tenant = Tenant::find_by_id_for_update(tx, authority.tenant_id)
        .await?
        .ok_or_else(|| DbError::not_found("Tenant", authority.tenant_id))?;
    let (target_user, before) = lock_target_membership(tx, tenant.id, user_id).await?;
    if before.authz_version != patch.expected_authz_version {
        return Err(conflict(
            "tenant_membership",
            format!("{}/{}", tenant.id, user_id),
        ));
    }
    let next_role = patch.tenant_role.unwrap_or(before.tenant_role()?);
    let next_status = patch.status.unwrap_or(before.membership_status()?);
    if before.status == MembershipStatus::Removed.as_str()
        && (next_status != MembershipStatus::Removed || patch.tenant_role.is_some())
    {
        return Err(invariant(
            "removed membership requires a newly accepted invitation",
        ));
    }
    if user_id == tenant.owner_user_id
        && (next_role != TenantRole::Admin || next_status != MembershipStatus::Active)
    {
        return Err(invariant(
            "the tenant owner must remain an active tenant administrator",
        ));
    }
    if next_status == MembershipStatus::Active
        && next_role == TenantRole::Admin
        && target_user.status != "active"
    {
        return Err(invariant("active global user required"));
    }
    let active_admins: i64 = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT FROM tenant_memberships m JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.status='active' AND m.tenant_role='admin' AND u.status='active' AND NOT (m.user_id=$2)",
            [tenant.id.into(), user_id.into()],
        ))
        .await?
        .ok_or_else(|| invariant("membership invariant count unavailable"))?
        .try_get_by_index(0)?;
    let final_admins = active_admins
        + i64::from(next_status == MembershipStatus::Active && next_role == TenantRole::Admin);
    if final_admins < 1 {
        return Err(invariant(
            "a tenant must retain at least one active administrator",
        ));
    }
    if next_role == before.tenant_role()? && next_status == before.membership_status()? {
        return get_member(tx, scope, user_id)
            .await?
            .ok_or_else(|| invariant("member disappeared during no-op update"));
    }
    let updated = TenantMembership::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role=$3,status=$4 WHERE tenant_id=$1 AND user_id=$2 AND authz_version=$5 RETURNING *",
        [tenant.id.into(), user_id.into(), next_role.as_str().into(), next_status.as_str().into(), patch.expected_authz_version.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| conflict("tenant_membership", format!("{}/{}", tenant.id, user_id)))?;
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(tenant.id),
        &authority.actor,
        "membership.update",
        "tenant_membership",
        Some(&user_id.to_string()),
        AuditResult::Success,
        serde_json::json!({
            "previous_role": before.tenant_role,
            "tenant_role": updated.tenant_role,
            "previous_status": before.status,
            "status": updated.status,
            "authz_version": updated.authz_version
        }),
    )
    .await?;
    Ok(TenantMember {
        user_id: updated.user_id,
        email: target_user.email,
        name: target_user.name,
        user_status: target_user.status,
        tenant_role: updated.tenant_role,
        membership_status: updated.status,
        authz_version: updated.authz_version,
        invited_by: updated.invited_by,
        joined_at: updated.joined_at,
        removed_at: updated.removed_at,
    })
}

pub async fn remove_member(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    user_id: Uuid,
    expected_authz_version: i64,
    actor: &AuditContext,
) -> Result<TenantMember, DbError> {
    update_member(
        tx,
        scope,
        snapshot,
        user_id,
        &MemberPatch {
            expected_authz_version,
            tenant_role: None,
            status: Some(MembershipStatus::Removed),
        },
        actor,
    )
    .await
}

pub async fn transfer_ownership(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    new_owner_user_id: Uuid,
    actor: &AuditContext,
) -> Result<TenantContext, DbError> {
    let authority = revalidate_in_transaction(tx, scope, snapshot, actor).await?;
    let tenant = Tenant::find_by_id_for_update(tx, authority.tenant_id)
        .await?
        .ok_or_else(|| DbError::not_found("Tenant", authority.tenant_id))?;
    if tenant.owner_user_id != scope.user_id() {
        return Err(invariant(
            "only the current tenant owner may transfer ownership",
        ));
    }
    let (new_owner, membership) = lock_target_membership(tx, tenant.id, new_owner_user_id).await?;
    if new_owner.status != "active"
        || membership.status != MembershipStatus::Active.as_str()
        || membership.tenant_role()? != TenantRole::Admin
    {
        return Err(invariant(
            "ownership target must be an active tenant administrator",
        ));
    }
    if new_owner_user_id == tenant.owner_user_id {
        return get_tenant_context(tx, scope)
            .await?
            .ok_or_else(|| invariant("tenant context disappeared"));
    }
    let updated = Tenant::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET owner_user_id=$2 WHERE id=$1 RETURNING *",
        [tenant.id.into(), new_owner_user_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("Tenant", tenant.id))?;
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(tenant.id),
        &authority.actor,
        "tenant.transfer_owner",
        "tenant",
        Some(&tenant.id.to_string()),
        AuditResult::Success,
        serde_json::json!({
            "previous_owner_user_id": tenant.owner_user_id,
            "owner_user_id": updated.owner_user_id
        }),
    )
    .await?;
    get_tenant_context(tx, scope)
        .await?
        .ok_or_else(|| invariant("transferred tenant context disappeared"))
}

pub async fn list_invitations(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    page: Page,
) -> Result<Vec<TenantInvitationView>, DbError> {
    let page = Page::bounded(page.page, page.page_size);

    let sql = format!(
        "SELECT id,tenant_id,invited_by,email,tenant_role,status,expires_at,accepted_by,accepted_at,revoked_at,created_at,updated_at
         FROM tenant_invitations
         WHERE tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE}
         ORDER BY created_at DESC,id DESC
         LIMIT $3 OFFSET $4"
    );
    Ok(
        TenantInvitationView::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                scope.tenant_id().into(),
                scope.user_id().into(),
                page.page_size.into(),
                page.offset.into(),
            ],
        ))
        .all(db)
        .await?,
    )
}

pub async fn count_invitations(
    db: &impl ConnectionTrait,
    scope: TenantScope,
) -> Result<i64, DbError> {
    #[derive(Debug, FromQueryResult)]
    struct Count {
        total: i64,
    }
    let sql = format!(
        "SELECT COUNT(*)::BIGINT AS total
         FROM tenant_invitations
         WHERE tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE}"
    );
    Ok(Count::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        &sql,
        [scope.tenant_id().into(), scope.user_id().into()],
    ))
    .one(db)
    .await?
    .map(|row| row.total)
    .unwrap_or(0))
}

pub async fn create_invitation(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    email: &str,
    tenant_role: TenantRole,
    expires_in_seconds: i64,
    actor: &AuditContext,
) -> Result<CreatedTenantInvitation, DbError> {
    if !(MIN_INVITATION_TTL_SECONDS..=MAX_INVITATION_TTL_SECONDS).contains(&expires_in_seconds) {
        return Err(invariant("expires_in_seconds is outside the allowed range"));
    }
    let email = normalized_email(email)?;
    let authority = revalidate_in_transaction(tx, scope, snapshot, actor).await?;
    let tenant = Tenant::find_by_id_for_update(tx, authority.tenant_id)
        .await?
        .ok_or_else(|| DbError::not_found("Tenant", authority.tenant_id))?;
    let active_member = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1
             FROM tenant_memberships m
             JOIN users u ON u.id=m.user_id
             WHERE m.tenant_id=$1 AND m.status='active' AND LOWER(BTRIM(u.email))=$2",
            [tenant.id.into(), email.clone().into()],
        ))
        .await?
        .is_some();
    if active_member {
        return Err(invariant(
            "an active tenant member cannot receive an invitation",
        ));
    }
    TenantInvitation::create(
        tx,
        &CreateTenantInvitationRequest {
            tenant_id: tenant.id,
            invited_by: scope.user_id(),
            email,
            tenant_role,
            expires_at: Utc::now() + Duration::seconds(expires_in_seconds),
        },
        &authority.actor,
    )
    .await
}

pub async fn revoke_invitation(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    invitation_id: Uuid,
    actor: &AuditContext,
) -> Result<TenantInvitationView, DbError> {
    let authority = revalidate_in_transaction(tx, scope, snapshot, actor).await?;
    let exists = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM tenant_invitations WHERE tenant_id=$1 AND id=$2",
            [authority.tenant_id.into(), invitation_id.into()],
        ))
        .await?
        .is_some();
    if !exists {
        return Err(DbError::not_found("Tenant invitation", invitation_id));
    }
    let invitation =
        TenantInvitation::revoke(tx, authority.tenant_id, invitation_id, &authority.actor).await?;
    Ok(TenantInvitationView {
        id: invitation.id,
        tenant_id: invitation.tenant_id,
        invited_by: invitation.invited_by,
        email: invitation.email,
        tenant_role: invitation.tenant_role,
        status: invitation.status,
        expires_at: invitation.expires_at,
        accepted_by: invitation.accepted_by,
        accepted_at: invitation.accepted_at,
        revoked_at: invitation.revoked_at,
        created_at: invitation.created_at,
        updated_at: invitation.updated_at,
    })
}

fn token_hash(token: &str) -> Result<String, DbError> {
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invariant(
            "invitation is unavailable, expired or already used",
        ));
    }
    Ok(hex::encode(Sha256::digest(token.as_bytes())))
}

pub async fn accept_invitation(
    tx: &DatabaseTransaction,
    actor_user_id: Uuid,
    expected_token_version: i32,
    token: &str,
    actor: &AuditContext,
) -> Result<(TenantInvitation, TenantMember), DbError> {
    if actor_user_id.is_nil()
        || actor.actor_user_id != actor_user_id
        || actor.credential_kind != keycompute_types::CredentialKind::Jwt
    {
        return Err(invariant("global JWT console authority required"));
    }
    let hash = token_hash(token)?;
    let candidate = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT tenant_id,invited_by FROM tenant_invitations WHERE token_hash=$1 AND status='pending' AND expires_at>clock_timestamp()",
            [hash.into()],
        ))
        .await?
        .ok_or_else(|| invariant("invitation is unavailable, expired or already used"))?;
    let tenant_id: Uuid = candidate.try_get_by_index(0)?;
    let invited_by: Uuid = candidate.try_get_by_index(1)?;
    lock_identity_admin(tx).await?;
    lock_tenant_parent_after_fence(tx, tenant_id).await?;
    let mut ids = vec![actor_user_id, invited_by];
    ids.sort_unstable();
    ids.dedup();
    tx.query_all(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id FROM users WHERE id=ANY($1::UUID[]) ORDER BY id FOR SHARE",
        [ids.into()],
    ))
    .await?;
    let current = User::find_by_id(tx, actor_user_id)
        .await?
        .filter(|user| user.status == "active" && user.token_version == expected_token_version)
        .ok_or_else(|| conflict("console session", actor_user_id))?;
    let current_actor = AuditContext {
        actor_platform_role: current.platform_role()?,
        ..*actor
    };
    let invitation =
        TenantInvitation::accept(tx, tenant_id, token, actor_user_id, "", &current_actor).await?;
    let member = TenantMember::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT m.user_id,u.email,u.name,u.status AS user_status,m.tenant_role,m.status AS membership_status,m.authz_version,m.invited_by,m.joined_at,m.removed_at FROM tenant_memberships m JOIN users u ON u.id=m.user_id WHERE m.tenant_id=$1 AND m.user_id=$2",
        [tenant_id.into(), actor_user_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| invariant("accepted membership was not persisted"))?;
    Ok((invitation, member))
}

pub async fn list_audit_events(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    page: Page,
) -> Result<Vec<TenantAuditView>, DbError> {
    let page = Page::bounded(page.page, page.page_size);

    let sql = format!(
        "SELECT id,scope_type,tenant_id,actor_user_id,action,resource_type,resource_id,request_id,credential_kind,platform_role,tenant_role,metadata,result,created_at
         FROM tenant_audit_events
         WHERE scope_type='tenant' AND tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE}
         ORDER BY created_at DESC,id DESC
         LIMIT $3 OFFSET $4"
    );
    Ok(
        TenantAuditView::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                scope.tenant_id().into(),
                scope.user_id().into(),
                page.page_size.into(),
                page.offset.into(),
            ],
        ))
        .all(db)
        .await?,
    )
}

pub async fn count_audit_events(
    db: &impl ConnectionTrait,
    scope: TenantScope,
) -> Result<i64, DbError> {
    #[derive(Debug, FromQueryResult)]
    struct Count {
        total: i64,
    }
    let sql = format!(
        "SELECT COUNT(*)::BIGINT AS total
         FROM tenant_audit_events
         WHERE scope_type='tenant' AND tenant_id=$1 AND {ADMIN_ACTOR_PREDICATE}"
    );
    Ok(Count::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        &sql,
        [scope.tenant_id().into(), scope.user_id().into()],
    ))
    .one(db)
    .await?
    .map(|row| row.total)
    .unwrap_or(0))
}

pub fn invitation_view(invitation: TenantInvitation) -> TenantInvitationView {
    TenantInvitationView {
        id: invitation.id,
        tenant_id: invitation.tenant_id,
        invited_by: invitation.invited_by,
        email: invitation.email,
        tenant_role: invitation.tenant_role,
        status: invitation.status,
        expires_at: invitation.expires_at,
        accepted_by: invitation.accepted_by,
        accepted_at: invitation.accepted_at,
        revoked_at: invitation.revoked_at,
        created_at: invitation.created_at,
        updated_at: invitation.updated_at,
    }
}

pub fn invitation_status(value: &TenantInvitationView) -> Result<TenantInvitationStatus, DbError> {
    value.status.parse().map_err(DbError::Other)
}
