//! Owner-only API-key issuance intents.
//!
//! An intent is deliberately not a credential.  It contains the metadata
//! needed to let a tenant administrator request owner provisioning, but never
//! stores a plaintext key, ciphertext, key hash, or recoverable secret.

use super::{
    api_key::{CreateProduceAiKeyRequest, KeyRemoval, ProduceAiKey, ProduceAiKeyResponse},
    tenant::Tenant,
    tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin},
    tenant_control::{self, TenantAuthzSnapshot},
    tenant_membership::TenantMembership,
    user::User,
};
use crate::DbError;
use chrono::{DateTime, Duration, Utc};
use keycompute_types::{AuditResult, AuditScopeType, CredentialKind, TenantRole, TenantScope};
use rand::RngCore;
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const MAX_KEY_ISSUANCE_PAGE_SIZE: i64 = 100;
pub const KEY_ISSUANCE_TTL_SECONDS: i64 = 15 * 60;
pub const MAX_KEY_LIFETIME_SECONDS: i64 = 10 * 365 * 24 * 60 * 60;

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct KeyIssuanceIntent {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub owner_user_id: Uuid,
    pub requested_by_user_id: Uuid,
    pub replaces_key_id: Option<Uuid>,
    pub requested_name: String,
    pub requested_expires_at: Option<DateTime<Utc>>,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub created_key_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub requested_by_token_version: i32,
    #[serde(skip_serializing)]
    pub requested_by_authz_version: i64,
    #[serde(skip_serializing)]
    pub owner_token_version: i32,
    #[serde(skip_serializing)]
    pub owner_authz_version: i64,
    #[serde(skip_serializing)]
    pub tenant_authz_version: i64,
}

#[derive(Clone, Serialize)]
pub struct ClaimedKey {
    pub intent: KeyIssuanceIntent,
    pub key: ProduceAiKeyResponse,
    #[serde(skip_serializing)]
    pub secret: String,
}

impl std::fmt::Debug for ClaimedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimedKey")
            .field("intent", &self.intent)
            .field("key", &self.key)
            .field("secret", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KeyIssuancePage {
    pub page: i64,
    pub page_size: i64,
    pub offset: i64,
}

impl KeyIssuancePage {
    pub fn bounded(page: i64, page_size: i64) -> Self {
        let page_size = page_size.clamp(1, MAX_KEY_ISSUANCE_PAGE_SIZE);
        let page = page.clamp(1, 1_000_000);
        Self {
            page,
            page_size,
            offset: (page - 1).checked_mul(page_size).unwrap_or(i64::MAX),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyMetadataPatch {
    pub expected_updated_at: DateTime<Utc>,
    pub name: Option<String>,
    pub expires_at: Option<Option<DateTime<Utc>>>,
}

fn invalid(message: impl Into<String>) -> DbError {
    DbError::Other(message.into())
}

fn conflict(message: impl Into<String>) -> DbError {
    DbError::Other(format!("conflict: {}", message.into()))
}

fn validate_name(name: &str) -> Result<String, DbError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 255 || name.chars().any(char::is_control) {
        return Err(invalid("invalid key name"));
    }
    Ok(name.to_owned())
}

fn validate_expiration(expires_at: Option<DateTime<Utc>>) -> Result<(), DbError> {
    let Some(expires_at) = expires_at else {
        return Ok(());
    };
    let now = Utc::now();
    if expires_at <= now {
        return Err(invalid("key expiration must be in the future"));
    }
    if expires_at > now + Duration::seconds(MAX_KEY_LIFETIME_SECONDS) {
        return Err(invalid("key expiration exceeds the maximum lifetime"));
    }
    Ok(())
}

async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='8s'")
        .await?;
    Ok(tx)
}

struct LockedAuthority {
    tenant_id: Uuid,
    tenant_authz_version: i64,
    actor: AuditContext,
    users: Vec<User>,
    memberships: Vec<TenantMembership>,
}

fn member(memberships: &[TenantMembership], user_id: Uuid) -> Option<&TenantMembership> {
    memberships.iter().find(|member| member.user_id == user_id)
}

fn user(users: &[User], user_id: Uuid) -> Option<&User> {
    users.iter().find(|user| user.id == user_id)
}

async fn lock_issuance_authority(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    audit: &AuditContext,
    related_users: &[Uuid],
    require_admin: bool,
) -> Result<LockedAuthority, DbError> {
    if audit.actor_user_id != scope.user_id()
        || audit.credential_kind != CredentialKind::Jwt
        || scope.tenant_id().is_nil()
        || snapshot.tenant_authz_version <= 0
        || snapshot.membership_authz_version <= 0
        || snapshot.token_version < 0
    {
        return Err(invalid("current JWT tenant authority required"));
    }
    lock_identity_admin(tx).await?;
    let tenant = Tenant::find_by_id_for_update(tx, scope.tenant_id())
        .await?
        .filter(|tenant| tenant.is_active())
        .ok_or_else(|| invalid("active tenant required"))?;
    if tenant.authz_version != snapshot.tenant_authz_version {
        return Err(conflict("tenant authorization changed"));
    }
    let mut ids = Vec::with_capacity(1 + related_users.len());
    ids.push(scope.user_id());
    ids.extend_from_slice(related_users);
    ids.sort_unstable();
    ids.dedup();
    if ids.iter().any(Uuid::is_nil) {
        return Err(invalid("real user IDs are required"));
    }

    // Reject foreign selectors before acquiring locks on global identity rows.
    // The identity administration fence keeps these membership identities stable.
    let members = tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS count FROM tenant_memberships WHERE tenant_id=$1 AND user_id=ANY($2)",
        [tenant.id.into(), ids.clone().into()],
    )).await?.ok_or_else(|| invalid("tenant memberships are required"))?;
    if members.try_get::<i64>("", "count")? != ids.len() as i64 {
        return Err(invalid("tenant memberships are required"));
    }

    let users = User::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM users WHERE id=ANY($1) ORDER BY id FOR UPDATE",
        [ids.clone().into()],
    ))
    .all(tx)
    .await?;
    if users.len() != ids.len() {
        return Err(invalid("tenant users are required"));
    }

    let memberships = TenantMembership::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM tenant_memberships WHERE tenant_id=$1 AND user_id=ANY($2) ORDER BY user_id FOR UPDATE",
        [tenant.id.into(), ids.clone().into()],
    ))
    .all(tx)
    .await?;
    if memberships.len() != ids.len() {
        return Err(invalid("tenant memberships are required"));
    }
    let actor_user = users
        .iter()
        .find(|user| user.id == scope.user_id())
        .filter(|user| user.status == "active" && user.token_version == snapshot.token_version)
        .ok_or_else(|| conflict("user authorization changed"))?;
    let actor_membership = memberships
        .iter()
        .find(|membership| membership.user_id == scope.user_id())
        .filter(|membership| {
            membership.status == "active"
                && membership.authz_version == snapshot.membership_authz_version
        })
        .ok_or_else(|| conflict("membership authorization changed"))?;
    let role = actor_membership.tenant_role()?;
    if role != scope.tenant_role() || (require_admin && role != TenantRole::Admin) {
        return Err(invalid("tenant administrator membership required"));
    }
    Ok(LockedAuthority {
        tenant_id: tenant.id,
        tenant_authz_version: tenant.authz_version,
        actor: AuditContext {
            actor_platform_role: actor_user.platform_role()?,
            actor_tenant_role: Some(role),
            ..*audit
        },
        users,
        memberships,
    })
}

fn intent_columns() -> &'static str {
    "id,tenant_id,owner_user_id,requested_by_user_id,replaces_key_id,requested_name,requested_expires_at,status,expires_at,claimed_at,created_key_id,created_at,requested_by_token_version,requested_by_authz_version,owner_token_version,owner_authz_version,tenant_authz_version"
}

async fn append_intent_audit(
    tx: &DatabaseTransaction,
    tenant_id: Uuid,
    actor: &AuditContext,
    action: &str,
    intent: Uuid,
    metadata: serde_json::Value,
) -> Result<(), DbError> {
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(tenant_id),
        actor,
        action,
        "key_issuance",
        Some(&intent.to_string()),
        AuditResult::Success,
        metadata,
    )
    .await?;
    Ok(())
}

struct IssuanceRequest<'a> {
    owner_user_id: Uuid,
    replaces_key_id: Option<Uuid>,
    requested_name: &'a str,
    requested_expires_at: Option<DateTime<Utc>>,
}

async fn request_intent_in_tx(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    request: IssuanceRequest<'_>,
    actor: &AuditContext,
) -> Result<(KeyIssuanceIntent, bool), DbError> {
    let IssuanceRequest {
        owner_user_id,
        replaces_key_id,
        requested_name,
        requested_expires_at,
    } = request;
    let requested_name = validate_name(requested_name)?;
    validate_expiration(requested_expires_at)?;
    if owner_user_id.is_nil() || actor.actor_user_id != scope.user_id() {
        return Err(invalid("tenant key owner and actor are required"));
    }
    let authority =
        lock_issuance_authority(tx, scope, snapshot, actor, &[owner_user_id], true).await?;
    let tenant_id = authority.tenant_id;
    let users = &authority.users;
    let memberships = &authority.memberships;
    let owner = user(users, owner_user_id)
        .filter(|user| user.status == "active")
        .ok_or_else(|| invalid("active key owner required"))?;
    let owner_membership = member(memberships, owner_user_id)
        .filter(|membership| membership.status == "active")
        .ok_or_else(|| invalid("active key owner membership required"))?;
    let requester = user(users, authority.actor.actor_user_id)
        .filter(|user| user.status == "active")
        .ok_or_else(|| invalid("active requester required"))?;
    let requester_membership = member(memberships, authority.actor.actor_user_id)
        .filter(|membership| {
            membership.status == "active" && membership.tenant_role == TenantRole::Admin.as_str()
        })
        .ok_or_else(|| invalid("tenant administrator membership required"))?;

    if let Some(replaces_key_id) = replaces_key_id {
        let pending = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {} FROM tenant_key_issuance_intents WHERE tenant_id=$1 AND replaces_key_id=$2 AND status='pending' FOR UPDATE",
                intent_columns()
            ),
            [tenant_id.into(), replaces_key_id.into()],
        ))
        .one(tx)
        .await?;
        if let Some(pending) = pending {
            if pending.expires_at <= Utc::now() {
                tx.execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE tenant_key_issuance_intents SET status='expired' WHERE tenant_id=$1 AND id=$2 AND status='pending'",
                    [tenant_id.into(), pending.id.into()],
                ))
                .await?;
                append_intent_audit(
                    tx,
                    tenant_id,
                    &authority.actor,
                    "key_issuance.expire",
                    pending.id,
                    serde_json::json!({"owner_user_id": pending.owner_user_id}),
                )
                .await?;
            } else if pending.requested_by_user_id == requester.id
                && pending.requested_by_token_version == requester.token_version
                && pending.requested_by_authz_version == requester_membership.authz_version
                && pending.owner_token_version == owner.token_version
                && pending.owner_authz_version == owner_membership.authz_version
                && pending.tenant_authz_version == authority.tenant_authz_version
                && pending.owner_user_id == owner_user_id
                && pending.requested_name == requested_name
                && pending.requested_expires_at == requested_expires_at
            {
                return Ok((pending, true));
            } else {
                return Err(conflict("a different rotation is already pending"));
            }
        }
    }

    if let Some(replaces_key_id) = replaces_key_id {
        let key = ProduceAiKey::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE",
            [
                tenant_id.into(),
                owner_user_id.into(),
                replaces_key_id.into(),
            ],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("ProduceAiKey", replaces_key_id))?;
        if key.revoked || key.expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(conflict(
                "the key selected for rotation is no longer active",
            ));
        }
    }

    validate_expiration(requested_expires_at)?;
    let now = Utc::now();
    let intent = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "INSERT INTO tenant_key_issuance_intents
             (tenant_id,owner_user_id,requested_by_user_id,replaces_key_id,requested_name,requested_expires_at,status,expires_at,requested_by_token_version,requested_by_authz_version,owner_token_version,owner_authz_version,tenant_authz_version)
             VALUES ($1,$2,$3,$4,$5,$6,'pending',$7,$8,$9,$10,$11,$12)
             RETURNING {}",
            intent_columns()
        ),
        [
            tenant_id.into(),
            owner.id.into(),
            requester.id.into(),
            replaces_key_id.into(),
            requested_name.into(),
            requested_expires_at.into(),
            (now + Duration::seconds(KEY_ISSUANCE_TTL_SECONDS)).into(),
            requester.token_version.into(),
            requester_membership.authz_version.into(),
            owner.token_version.into(),
            owner_membership.authz_version.into(),
            authority.tenant_authz_version.into(),
        ],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| invalid("key issuance intent insert returned no row"))?;
    append_intent_audit(
        tx,
        tenant_id,
        &authority.actor,
        "key_issuance.request",
        intent.id,
        serde_json::json!({
            "owner_user_id": intent.owner_user_id,
            "requested_by_user_id": intent.requested_by_user_id,
            "replaces_key_id": intent.replaces_key_id,
            "requested_expires_at": intent.requested_expires_at,
        }),
    )
    .await?;
    Ok((intent, false))
}

pub async fn request_new(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    owner_user_id: Uuid,
    requested_name: &str,
    requested_expires_at: Option<DateTime<Utc>>,
    actor: &AuditContext,
) -> Result<(KeyIssuanceIntent, bool), DbError> {
    let tx = begin(db).await?;
    let result = async {
        let result = request_intent_in_tx(
            &tx,
            scope,
            snapshot,
            IssuanceRequest {
                owner_user_id,
                replaces_key_id: None,
                requested_name,
                requested_expires_at,
            },
            actor,
        )
        .await?;
        Ok(result)
    }
    .await;
    finish(tx, result).await
}

pub async fn request_rotation(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    replaces_key_id: Uuid,
    requested_name: &str,
    requested_expires_at: Option<DateTime<Utc>>,
    actor: &AuditContext,
) -> Result<(KeyIssuanceIntent, bool), DbError> {
    let tx = begin(db).await?;
    let result = async {
        let current = ProduceAiKey::find_in_tenant(&tx, scope, replaces_key_id)
            .await?
            .ok_or_else(|| DbError::not_found("ProduceAiKey", replaces_key_id))?;
        let result = request_intent_in_tx(
            &tx,
            scope,
            snapshot,
            IssuanceRequest {
                owner_user_id: current.user_id,
                replaces_key_id: Some(replaces_key_id),
                requested_name,
                requested_expires_at,
            },
            actor,
        )
        .await?;
        Ok(result)
    }
    .await;
    finish(tx, result).await
}

pub async fn list_for_owner(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    page: KeyIssuancePage,
) -> Result<Vec<KeyIssuanceIntent>, DbError> {
    Ok(
        KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {} FROM tenant_key_issuance_intents i
             WHERE i.tenant_id=$1 AND i.owner_user_id=$2 AND i.status='pending'
               AND i.expires_at>NOW()
               AND EXISTS (
                   SELECT 1 FROM tenant_memberships m
                   JOIN users u ON u.id=m.user_id
                   JOIN tenants t ON t.id=m.tenant_id
                   WHERE m.tenant_id=$1 AND m.user_id=$2
                     AND m.status='active' AND u.status='active' AND t.status='active'
               )
             ORDER BY i.created_at DESC,i.id DESC LIMIT $3 OFFSET $4",
                intent_columns()
            ),
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

pub async fn count_for_owner(
    db: &impl ConnectionTrait,
    scope: TenantScope,
) -> Result<i64, DbError> {
    #[derive(Debug, FromQueryResult)]
    struct Count {
        total: i64,
    }
    Ok(Count::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS total FROM tenant_key_issuance_intents i
         WHERE i.tenant_id=$1 AND i.owner_user_id=$2 AND i.status='pending' AND i.expires_at>NOW()
           AND EXISTS (
               SELECT 1 FROM tenant_memberships m
               JOIN users u ON u.id=m.user_id
               JOIN tenants t ON t.id=m.tenant_id
               WHERE m.tenant_id=$1 AND m.user_id=$2
                 AND m.status='active' AND u.status='active' AND t.status='active'
           )",
        [scope.tenant_id().into(), scope.user_id().into()],
    ))
    .one(db)
    .await?
    .map(|count| count.total)
    .unwrap_or(0))
}

pub async fn list_in_tenant(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    owner_user_id: Option<Uuid>,
    page: KeyIssuancePage,
) -> Result<Vec<KeyIssuanceIntent>, DbError> {
    if scope.tenant_role() != TenantRole::Admin {
        return Err(invalid("tenant administrator membership required"));
    }
    Ok(
        KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {} FROM tenant_key_issuance_intents i
             WHERE i.tenant_id=$1 AND i.status='pending' AND i.expires_at>NOW()
               AND ($2::uuid IS NULL OR i.owner_user_id=$2)
               AND EXISTS (
                   SELECT 1 FROM tenant_memberships m
                   JOIN users u ON u.id=m.user_id
                   JOIN tenants t ON t.id=m.tenant_id
                   WHERE m.tenant_id=$1 AND m.user_id=$3
                     AND m.status='active' AND m.tenant_role='admin'
                     AND u.status='active' AND t.status='active'
               )
             ORDER BY i.created_at DESC,i.id DESC LIMIT $4 OFFSET $5",
                intent_columns()
            ),
            [
                scope.tenant_id().into(),
                owner_user_id.into(),
                scope.user_id().into(),
                page.page_size.into(),
                page.offset.into(),
            ],
        ))
        .all(db)
        .await?,
    )
}

pub async fn count_in_tenant(
    db: &impl ConnectionTrait,
    scope: TenantScope,
    owner_user_id: Option<Uuid>,
) -> Result<i64, DbError> {
    if scope.tenant_role() != TenantRole::Admin {
        return Err(invalid("tenant administrator membership required"));
    }
    #[derive(Debug, FromQueryResult)]
    struct Count {
        total: i64,
    }
    Ok(Count::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS total FROM tenant_key_issuance_intents i
         WHERE i.tenant_id=$1 AND i.status='pending' AND i.expires_at>NOW()
           AND ($2::uuid IS NULL OR i.owner_user_id=$2)
           AND EXISTS (
               SELECT 1 FROM tenant_memberships m
               JOIN users u ON u.id=m.user_id
               JOIN tenants t ON t.id=m.tenant_id
               WHERE m.tenant_id=$1 AND m.user_id=$3
                 AND m.status='active' AND m.tenant_role='admin'
                 AND u.status='active' AND t.status='active'
           )",
        [
            scope.tenant_id().into(),
            owner_user_id.into(),
            scope.user_id().into(),
        ],
    ))
    .one(db)
    .await?
    .map(|count| count.total)
    .unwrap_or(0))
}

async fn revalidate_admin(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    actor: &AuditContext,
) -> Result<tenant_control::TenantWriteAuthority, DbError> {
    if scope.tenant_role() != TenantRole::Admin {
        return Err(invalid("tenant administrator membership required"));
    }
    tenant_control::revalidate_in_transaction(tx, scope, snapshot, actor).await
}

pub async fn cancel_in_tenant(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    intent_id: Uuid,
    actor: &AuditContext,
) -> Result<KeyIssuanceIntent, DbError> {
    let tx = begin(db).await?;
    let result = async {
        let authority = revalidate_admin(&tx, scope, snapshot, actor).await?;
        let current = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT {} FROM tenant_key_issuance_intents WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
            intent_columns()
        ),
        [authority.tenant_id.into(), intent_id.into()],
    ))
    .one(&tx)
    .await?
    .ok_or_else(|| DbError::not_found("Key issuance intent", intent_id))?;
        if current.status != "pending" {
            return Err(conflict("key issuance intent is no longer pending"));
        }
        let updated = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE tenant_key_issuance_intents SET status='cancelled'
             WHERE tenant_id=$1 AND id=$2 AND status='pending' RETURNING {}",
                intent_columns()
            ),
            [authority.tenant_id.into(), intent_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| conflict("key issuance intent changed while cancelling"))?;
        append_intent_audit(
            &tx,
            authority.tenant_id,
            &authority.actor,
            "key_issuance.cancel",
            intent_id,
            serde_json::json!({"owner_user_id": updated.owner_user_id}),
        )
        .await?;
        Ok(updated)
    }
    .await;
    finish(tx, result).await
}

pub async fn decline_for_owner(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    intent_id: Uuid,
    actor: &AuditContext,
) -> Result<KeyIssuanceIntent, DbError> {
    if actor.actor_user_id != scope.user_id() {
        return Err(invalid("issuance owner authority required"));
    }
    let tx = begin(db).await?;
    let result = async {
        let authority = lock_issuance_authority(&tx, scope, snapshot, actor, &[], false).await?;
        let current = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT {} FROM tenant_key_issuance_intents
             WHERE tenant_id=$1 AND id=$2 AND owner_user_id=$3 FOR UPDATE",
                intent_columns()
            ),
            [
                authority.tenant_id.into(),
                intent_id.into(),
                authority.actor.actor_user_id.into(),
            ],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("Key issuance intent", intent_id))?;
        if current.status != "pending" {
            return Err(conflict("key issuance intent is no longer pending"));
        }
        let updated = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "UPDATE tenant_key_issuance_intents SET status='cancelled'
             WHERE tenant_id=$1 AND id=$2 AND owner_user_id=$3 AND status='pending'
             RETURNING {}",
                intent_columns()
            ),
            [
                authority.tenant_id.into(),
                intent_id.into(),
                authority.actor.actor_user_id.into(),
            ],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| conflict("key issuance intent changed while declining"))?;
        append_intent_audit(
            &tx,
            authority.tenant_id,
            &authority.actor,
            "key_issuance.decline",
            intent_id,
            serde_json::json!({"owner_user_id": updated.owner_user_id}),
        )
        .await?;
        Ok(updated)
    }
    .await;
    finish(tx, result).await
}

pub async fn update_key(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    key_id: Uuid,
    patch: &KeyMetadataPatch,
    actor: &AuditContext,
) -> Result<ProduceAiKeyResponse, DbError> {
    let name = patch.name.as_deref().map(validate_name).transpose()?;
    if name.is_none() && patch.expires_at.is_none() {
        return Err(invalid("name or expiration is required"));
    }
    let tx = begin(db).await?;
    let result =
        async {
            let candidate = ProduceAiKey::find_in_tenant(&tx, scope, key_id)
                .await?
                .ok_or_else(|| DbError::not_found("ProduceAiKey", key_id))?;
            let authority =
                lock_issuance_authority(&tx, scope, snapshot, actor, &[candidate.user_id], true)
                    .await?;
            let current = ProduceAiKey::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE",
        [scope.tenant_id().into(), candidate.user_id.into(), key_id.into()],
    )).one(&tx).await?.ok_or_else(|| DbError::not_found("ProduceAiKey", key_id))?;
            validate_expiration(patch.expires_at.flatten())?;
            if patch.expires_at.is_some()
                && (current.revoked || current.expires_at.is_some_and(|at| at <= Utc::now()))
            {
                return Err(conflict(
                    "cannot extend a revoked or expired key; request a new issuance",
                ));
            }
            let updated = ProduceAiKey::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE produce_ai_keys SET name=COALESCE($5,name),
         expires_at=CASE WHEN $6 THEN $7 ELSE expires_at END,
         updated_at=GREATEST(updated_at,clock_timestamp())+INTERVAL '1 microsecond'
         WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND updated_at=$4 RETURNING *",
                [
                    scope.tenant_id().into(),
                    current.user_id.into(),
                    key_id.into(),
                    patch.expected_updated_at.into(),
                    name.into(),
                    patch.expires_at.is_some().into(),
                    patch.expires_at.flatten().into(),
                ],
            ))
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::OptimisticConflict {
                entity: "ProduceAiKey".into(),
                id: key_id.to_string(),
            })?;
            TenantAuditEvent::append(
                &tx,
                AuditScopeType::Tenant,
                Some(scope.tenant_id()),
                &authority.actor,
                "key.update",
                "produce_ai_key",
                Some(&key_id.to_string()),
                AuditResult::Success,
                serde_json::json!({"user_id":current.user_id, "key_id":key_id,
            "reason":"tenant key metadata update", "expires_at":updated.expires_at,
            "previous_expires_at":current.expires_at}),
            )
            .await?;
            Ok(updated.into())
        }
        .await;
    finish(tx, result).await
}

pub async fn remove_key(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    key_id: Uuid,
    actor: &AuditContext,
    revoke_only: bool,
) -> Result<KeyRemoval, DbError> {
    let tx = begin(db).await?;
    let result = async {
        let authority = revalidate_admin(&tx, scope, snapshot, actor).await?;
        let result = if revoke_only {
            ProduceAiKey::revoke_in_tenant(&tx, scope, key_id, &authority.actor).await?
        } else {
            ProduceAiKey::remove_in_tenant(&tx, scope, key_id, &authority.actor).await?
        };
        Ok(result)
    }
    .await;
    finish(tx, result).await
}

async fn claim_in_tx_inner(
    tx: &DatabaseTransaction,
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    intent_id: Uuid,
    actor: &AuditContext,
) -> Result<ClaimedKey, DbError> {
    // Read the immutable requester identity before taking locks so the lock
    // helper can acquire identity-fence -> tenant -> sorted users -> sorted
    // memberships in the same order as every other administrative mutation.
    let candidate = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT {} FROM tenant_key_issuance_intents
             WHERE tenant_id=$1 AND id=$2 AND owner_user_id=$3",
            intent_columns()
        ),
        [
            scope.tenant_id().into(),
            intent_id.into(),
            scope.user_id().into(),
        ],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("Key issuance intent", intent_id))?;
    let authority = lock_issuance_authority(
        tx,
        scope,
        snapshot,
        actor,
        &[candidate.requested_by_user_id],
        false,
    )
    .await?;
    let intent = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT {} FROM tenant_key_issuance_intents
             WHERE tenant_id=$1 AND id=$2 AND owner_user_id=$3 FOR UPDATE",
            intent_columns()
        ),
        [
            authority.tenant_id.into(),
            intent_id.into(),
            authority.actor.actor_user_id.into(),
        ],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("Key issuance intent", intent_id))?;
    if intent.tenant_authz_version != authority.tenant_authz_version {
        return Err(conflict(
            "tenant authorization changed; request a new key issuance",
        ));
    }
    if intent.status != "pending" {
        return Err(conflict(
            "key issuance intent was already claimed or closed",
        ));
    }
    if intent.expires_at <= Utc::now() {
        return Err(conflict("key issuance intent has expired"));
    }
    validate_expiration(intent.requested_expires_at)?;

    let requester_user = user(&authority.users, intent.requested_by_user_id)
        .ok_or_else(|| conflict("the requesting administrator is unavailable"))?;
    let requester_membership = member(&authority.memberships, intent.requested_by_user_id)
        .ok_or_else(|| conflict("the requesting administrator is no longer a member"))?;
    if requester_user.status != "active"
        || requester_membership.status != "active"
        || requester_membership.tenant_role != TenantRole::Admin.as_str()
        || requester_user.token_version != intent.requested_by_token_version
        || requester_membership.authz_version != intent.requested_by_authz_version
    {
        return Err(conflict(
            "the requesting administrator authority changed; request a new key",
        ));
    }

    let owner_user = user(&authority.users, intent.owner_user_id)
        .ok_or_else(|| invalid("key owner is unavailable"))?;
    let owner_membership = member(&authority.memberships, intent.owner_user_id)
        .ok_or_else(|| invalid("key owner membership is unavailable"))?;
    if owner_user.status != "active"
        || owner_membership.status != "active"
        || owner_user.token_version != intent.owner_token_version
        || owner_membership.authz_version != intent.owner_authz_version
        || intent.owner_user_id != authority.actor.actor_user_id
    {
        return Err(conflict(
            "only the current active owner may claim this key request",
        ));
    }

    let owner_scope = TenantScope::checked(
        authority.tenant_id,
        authority.actor.actor_user_id,
        owner_membership.tenant_role()?,
    )
    .map_err(DbError::Other)?;
    let old_key = if let Some(replaces_key_id) = intent.replaces_key_id {
        let old_key = ProduceAiKey::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM produce_ai_keys
             WHERE tenant_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE",
            [
                authority.tenant_id.into(),
                intent.owner_user_id.into(),
                replaces_key_id.into(),
            ],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| conflict("the key selected for rotation is unavailable"))?;
        if old_key.revoked || old_key.expires_at.is_some_and(|at| at <= Utc::now()) {
            return Err(conflict(
                "the key selected for rotation is revoked or expired",
            ));
        }
        Some(old_key)
    } else {
        None
    };

    let secret = generate_secret();
    let saved_old = if let Some(old_key) = old_key {
        Some(ProduceAiKey::revoke_owned(tx, owner_scope, old_key.id, &authority.actor).await?)
    } else {
        None
    };
    let _ = saved_old;
    let saved = ProduceAiKey::create_owned(
        tx,
        owner_scope,
        &CreateProduceAiKeyRequest {
            tenant_id: authority.tenant_id,
            user_id: authority.actor.actor_user_id,
            name: intent.requested_name.clone(),
            produce_ai_key_hash: hash_secret(&secret),
            produce_ai_key_preview: format!("{}****", &secret[..8.min(secret.len())]),
            expires_at: intent.requested_expires_at,
        },
        &authority.actor,
    )
    .await?;
    let claimed = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "UPDATE tenant_key_issuance_intents SET status='claimed',claimed_at=NOW(),created_key_id=$3
             WHERE tenant_id=$1 AND id=$2 AND status='pending' RETURNING {}",
            intent_columns()
        ),
        [authority.tenant_id.into(), intent_id.into(), saved.id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| conflict("key issuance intent changed while claiming"))?;
    append_intent_audit(
        tx,
        authority.tenant_id,
        &authority.actor,
        "key_issuance.claim",
        intent_id,
        serde_json::json!({
            "owner_user_id": claimed.owner_user_id,
            "created_key_id": saved.id,
            "replaced_key_id": claimed.replaces_key_id,
        }),
    )
    .await?;
    Ok(ClaimedKey {
        intent: claimed,
        key: saved.into(),
        secret,
    })
}

fn generate_secret() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(51);
    out.push_str("sk-");
    let mut rng = rand::thread_rng();
    let cutoff = u8::MAX - (u8::MAX % ALPHABET.len() as u8);
    while out.len() < 51 {
        let mut byte = [0u8; 1];
        rng.fill_bytes(&mut byte);
        if byte[0] < cutoff {
            out.push(ALPHABET[(byte[0] as usize) % ALPHABET.len()] as char);
        }
    }
    out
}

fn hash_secret(secret: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(secret.as_bytes());
    hex::encode(digest.finalize())
}

pub async fn claim(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: TenantScope,
    snapshot: TenantAuthzSnapshot,
    intent_id: Uuid,
    actor: &AuditContext,
) -> Result<ClaimedKey, DbError> {
    let tx = begin(db).await?;
    let result = async {
        let claimed = claim_in_tx_inner(&tx, scope, snapshot, intent_id, actor).await?;
        Ok(claimed)
    }
    .await;
    finish(tx, result).await
}

async fn finish<T>(tx: DatabaseTransaction, result: Result<T, DbError>) -> Result<T, DbError> {
    match result {
        Ok(value) => {
            tx.commit().await?;
            Ok(value)
        }
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
