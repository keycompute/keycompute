//! Explicit key-management scopes. Credential validation is a separate path.
use super::*;
use crate::{AuditContext, TenantAuditEvent};
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, PlatformRole, PlatformScope, TenantRole,
    TenantScope,
};
use sea_orm::DatabaseTransaction;

const COLUMNS: &str = "k.id,k.tenant_id,k.user_id,k.name,k.produce_ai_key_preview,k.revoked,k.revoked_at,k.expires_at,k.last_used_at,k.created_at,k.updated_at";

#[derive(Clone, Copy)]
enum Scope {
    Owned(TenantScope),
    Tenant(TenantScope),
    Platform(PlatformScope, Uuid),
}
impl Scope {
    fn tenant(self) -> Uuid {
        match self {
            Self::Owned(s) | Self::Tenant(s) => s.tenant_id(),
            Self::Platform(_, t) => t,
        }
    }
    fn actor(self) -> Uuid {
        match self {
            Self::Owned(s) | Self::Tenant(s) => s.user_id(),
            Self::Platform(s, _) => s.user_id(),
        }
    }
    fn validate(self) -> Result<(), DbError> {
        match self {
            Self::Tenant(s) if s.tenant_role() != TenantRole::Admin => Err(denied()),
            Self::Platform(s, t) if s.platform_role() != PlatformRole::Root || t.is_nil() => {
                Err(denied())
            }
            _ => Ok(()),
        }
    }
}
fn denied() -> DbError {
    DbError::Other("key management authorization denied".into())
}
fn missing(id: Uuid) -> DbError {
    DbError::not_found("ProduceAiKey", id)
}

/// Private SQL expressions; all selectors are bound values. Optional owner/id
/// filters only narrow the mandatory tenant scope, never select all tenants.
fn read_query(
    scope: Scope,
    count: bool,
    owner: Option<Uuid>,
    id: Option<Uuid>,
    include_revoked: bool,
    page: Option<(i64, i64)>,
) -> Result<Statement, DbError> {
    scope.validate()?;
    let access = match scope {
        Scope::Owned(_) => {
            "k.user_id=$2 AND EXISTS (SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id JOIN tenants t ON t.id=m.tenant_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.status='active' AND u.status='active' AND t.status='active')"
        }
        Scope::Tenant(_) => {
            "EXISTS (SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id JOIN tenants t ON t.id=m.tenant_id WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.tenant_role='admin' AND m.status='active' AND u.status='active' AND t.status='active')"
        }
        Scope::Platform(_, _) => {
            "EXISTS (SELECT 1 FROM users u WHERE u.id=$2 AND u.platform_role='root' AND u.status='active')"
        }
    };
    let columns = if count { "COUNT(*)" } else { COLUMNS };
    let mut sql = format!(
        "SELECT {columns} FROM produce_ai_keys k WHERE k.tenant_id=$1 AND {access} AND ($3 OR NOT k.revoked) AND ($4::uuid IS NULL OR k.user_id=$4) AND ($5::uuid IS NULL OR k.id=$5)"
    );
    let mut values = vec![
        scope.tenant().into(),
        scope.actor().into(),
        include_revoked.into(),
        owner.into(),
        id.into(),
    ];
    if let Some((limit, offset)) = page {
        sql.push_str(" ORDER BY k.created_at DESC, k.id DESC LIMIT $6 OFFSET $7");
        values.extend([limit.clamp(1, 1000).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}
async fn find(
    db: &impl ConnectionTrait,
    scope: Scope,
    id: Uuid,
) -> Result<Option<ProduceAiKeyResponse>, DbError> {
    Ok(ProduceAiKeyResponse::find_by_statement(read_query(
        scope,
        false,
        None,
        Some(id),
        true,
        None,
    )?)
    .one(db)
    .await?)
}
async fn list(
    db: &impl ConnectionTrait,
    scope: Scope,
    owner: Option<Uuid>,
    include_revoked: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<ProduceAiKeyResponse>, DbError> {
    Ok(ProduceAiKeyResponse::find_by_statement(read_query(
        scope,
        false,
        owner,
        None,
        include_revoked,
        Some((limit, offset)),
    )?)
    .all(db)
    .await?)
}
async fn count(
    db: &impl ConnectionTrait,
    scope: Scope,
    owner: Option<Uuid>,
    include_revoked: bool,
) -> Result<i64, DbError> {
    let row = db
        .query_one(read_query(scope, true, owner, None, include_revoked, None)?)
        .await?
        .ok_or_else(|| DbError::Other("key count returned no row".into()))?;
    Ok(row.try_get_by_index(0)?)
}

/// Revalidate authority under shared parent locks, before locking a key. This
/// follows tenant -> sorted users -> sorted memberships -> key and does not
/// serialize unrelated tenants on the identity-administration write fence.
async fn lock_authority(
    tx: &DatabaseTransaction,
    scope: Scope,
    owner: Uuid,
    audit: &AuditContext,
    creating: bool,
) -> Result<AuditContext, DbError> {
    scope.validate()?;
    if owner.is_nil()
        || audit.credential_kind != CredentialKind::Jwt
        || audit.actor_user_id != scope.actor()
    {
        return Err(denied());
    }
    if matches!(scope, Scope::Owned(_)) && owner != scope.actor() {
        return Err(denied());
    }
    let tenant = Tenant::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM tenants WHERE id = $1 FOR SHARE",
        [scope.tenant().into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(denied)?;
    if !tenant.is_active() && (creating || !matches!(scope, Scope::Platform(_, _))) {
        return Err(denied());
    }
    let mut users = vec![scope.actor(), owner];
    users.sort_unstable();
    users.dedup();
    let rows = User::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM users WHERE id=ANY($1) ORDER BY id FOR SHARE",
        [users.clone().into()],
    ))
    .all(tx)
    .await?;
    let actor = rows
        .iter()
        .find(|u| u.id == scope.actor() && u.status == "active")
        .ok_or_else(denied)?;
    let owner_row = rows.iter().find(|u| u.id == owner).ok_or_else(denied)?;
    let members=TenantMembership::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT * FROM tenant_memberships WHERE tenant_id=$1 AND user_id=ANY($2) ORDER BY user_id FOR SHARE",[scope.tenant().into(),users.into()])).all(tx).await?;
    let member = members.iter().find(|m| m.user_id == scope.actor());
    match scope {
        Scope::Owned(_) => {
            member.filter(|m| m.status == "active").ok_or_else(denied)?;
        }
        Scope::Tenant(_) => {
            member
                .filter(|m| m.status == "active" && m.tenant_role == "admin")
                .ok_or_else(denied)?;
        }
        Scope::Platform(_, _) => {
            if actor.platform_role()? != PlatformRole::Root {
                return Err(denied());
            }
        }
    }
    if creating
        && (owner_row.status != "active"
            || !members
                .iter()
                .any(|m| m.user_id == owner && m.status == "active"))
    {
        return Err(denied());
    }
    Ok(AuditContext {
        actor_platform_role: actor.platform_role()?,
        actor_tenant_role: member
            .filter(|m| m.status == "active")
            .map(|m| m.tenant_role())
            .transpose()?,
        ..*audit
    })
}
async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='5s'")
        .await?;
    Ok(tx)
}
async fn record(
    tx: &DatabaseTransaction,
    scope: Scope,
    actor: &AuditContext,
    key: &ProduceAiKeyResponse,
    action: &str,
    reason: &str,
) -> Result<(), DbError> {
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Tenant,
        Some(scope.tenant()),
        actor,
        action,
        "produce_ai_key",
        Some(&key.id.to_string()),
        AuditResult::Success,
        serde_json::json!({"user_id":key.user_id,"key_id":key.id,"reason":reason}),
    )
    .await?;
    Ok(())
}
fn valid_reason(reason: &str) -> Result<&str, DbError> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 1000 || reason.chars().any(char::is_control) {
        return Err(DbError::Other("invalid key management reason".into()));
    }
    Ok(reason)
}
async fn create(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: Scope,
    req: &CreateProduceAiKeyRequest,
    audit: &AuditContext,
    reason: &str,
) -> Result<ProduceAiKey, DbError> {
    let reason = valid_reason(reason)?;
    if req.tenant_id != scope.tenant() {
        return Err(denied());
    }
    if req.name.trim().is_empty()
        || req.name.chars().count() > 255
        || req.name.chars().any(char::is_control)
    {
        return Err(DbError::Other("invalid key name".into()));
    }
    let tx = begin(db).await?;
    let actor = lock_authority(&tx, scope, req.user_id, audit, true).await?;
    let key=ProduceAiKey::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO produce_ai_keys(tenant_id,user_id,name,produce_ai_key_hash,produce_ai_key_preview,expires_at) VALUES($1,$2,$3,$4,$5,$6) RETURNING *",
        [scope.tenant().into(),req.user_id.into(),req.name.as_str().into(),req.produce_ai_key_hash.as_str().into(),req.produce_ai_key_preview.as_str().into(),req.expires_at.into()])).one(&tx).await?.ok_or_else(||DbError::Other("key insert returned no row".into()))?;
    record(
        &tx,
        scope,
        &actor,
        &key.clone().into(),
        "key.create",
        reason,
    )
    .await?;
    tx.commit().await?;
    Ok(key)
}

/// The established self-service API revokes a live key first; a later removal
/// may delete its revoked metadata. Neither outcome changes resource ownership.
#[derive(Debug)]
pub enum KeyRemoval {
    Revoked(ProduceAiKeyResponse),
    Deleted(Uuid),
}
async fn change(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: Scope,
    id: Uuid,
    audit: &AuditContext,
    reason: &str,
    remove: bool,
) -> Result<KeyRemoval, DbError> {
    let reason = valid_reason(reason)?;
    let tx = begin(db).await?;
    let snapshot = find(&tx, scope, id).await?.ok_or_else(|| missing(id))?;
    let actor = lock_authority(&tx, scope, snapshot.user_id, audit, false).await?;
    let key = ProduceAiKey::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE",
        [scope.tenant().into(), snapshot.user_id.into(), id.into()],
    ))
    .one(&tx)
    .await?
    .ok_or_else(|| missing(id))?;
    if remove && key.revoked {
        let changed=tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "DELETE FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND revoked",
            [scope.tenant().into(),key.user_id.into(),id.into()])).await?.rows_affected();
        if changed != 1 {
            return Err(missing(id));
        }
        record(&tx, scope, &actor, &key.into(), "key.delete", reason).await?;
        tx.commit().await?;
        return Ok(KeyRemoval::Deleted(id));
    }
    let revoked=ProduceAiKey::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE produce_ai_keys SET revoked=TRUE,revoked_at=COALESCE(revoked_at,NOW()),updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND id=$3 RETURNING *",
        [scope.tenant().into(),key.user_id.into(),id.into()])).one(&tx).await?.ok_or_else(||missing(id))?;
    let result: ProduceAiKeyResponse = revoked.into();
    record(&tx, scope, &actor, &result, "key.revoke", reason).await?;
    tx.commit().await?;
    Ok(KeyRemoval::Revoked(result))
}

impl ProduceAiKey {
    /// Existing platform member-key endpoint returns the full selected member
    /// collection. Do not silently truncate that legacy response contract.
    pub async fn list_platform_member(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant: Uuid,
        owner: Uuid,
    ) -> Result<Vec<ProduceAiKeyResponse>, DbError> {
        if owner.is_nil() {
            return Err(denied());
        }
        let mut query = read_query(
            Scope::Platform(scope, tenant),
            false,
            Some(owner),
            None,
            true,
            None,
        )?;
        query.sql.push_str(" ORDER BY k.created_at DESC, k.id DESC");
        Ok(ProduceAiKeyResponse::find_by_statement(query)
            .all(db)
            .await?)
    }

    pub async fn find_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<ProduceAiKeyResponse>, DbError> {
        find(db, Scope::Owned(scope), id).await
    }
    pub async fn list_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        include_revoked: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ProduceAiKeyResponse>, DbError> {
        list(
            db,
            Scope::Owned(scope),
            None,
            include_revoked,
            limit,
            offset,
        )
        .await
    }
    pub async fn count_owned(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        include_revoked: bool,
    ) -> Result<i64, DbError> {
        count(db, Scope::Owned(scope), None, include_revoked).await
    }
    pub async fn find_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<ProduceAiKeyResponse>, DbError> {
        find(db, Scope::Tenant(scope), id).await
    }
    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        owner: Option<Uuid>,
        include_revoked: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ProduceAiKeyResponse>, DbError> {
        list(
            db,
            Scope::Tenant(scope),
            owner,
            include_revoked,
            limit,
            offset,
        )
        .await
    }
    pub async fn count_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        owner: Option<Uuid>,
        include_revoked: bool,
    ) -> Result<i64, DbError> {
        count(db, Scope::Tenant(scope), owner, include_revoked).await
    }
    pub async fn find_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Option<ProduceAiKeyResponse>, DbError> {
        find(db, Scope::Platform(scope, tenant), id).await
    }
    pub async fn list_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant: Uuid,
        owner: Option<Uuid>,
        include_revoked: bool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ProduceAiKeyResponse>, DbError> {
        list(
            db,
            Scope::Platform(scope, tenant),
            owner,
            include_revoked,
            limit,
            offset,
        )
        .await
    }
    pub async fn count_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        tenant: Uuid,
        owner: Option<Uuid>,
        include_revoked: bool,
    ) -> Result<i64, DbError> {
        count(db, Scope::Platform(scope, tenant), owner, include_revoked).await
    }
    pub async fn create_owned(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        req: &CreateProduceAiKeyRequest,
        audit: &AuditContext,
    ) -> Result<Self, DbError> {
        create(
            db,
            Scope::Owned(scope),
            req,
            audit,
            "self-service key creation",
        )
        .await
    }
    pub async fn create_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        req: &CreateProduceAiKeyRequest,
        audit: &AuditContext,
    ) -> Result<Self, DbError> {
        create(db, Scope::Tenant(scope), req, audit, "tenant key creation").await
    }
    pub async fn create_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        req: &CreateProduceAiKeyRequest,
        audit: &AuditContext,
        reason: &str,
    ) -> Result<Self, DbError> {
        create(
            db,
            Scope::Platform(scope, req.tenant_id),
            req,
            audit,
            reason,
        )
        .await
    }
    pub async fn remove_owned(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        audit: &AuditContext,
    ) -> Result<KeyRemoval, DbError> {
        change(
            db,
            Scope::Owned(scope),
            id,
            audit,
            "self-service key removal",
            true,
        )
        .await
    }
    pub async fn revoke_owned(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        audit: &AuditContext,
    ) -> Result<KeyRemoval, DbError> {
        change(
            db,
            Scope::Owned(scope),
            id,
            audit,
            "self-service key revocation",
            false,
        )
        .await
    }
    pub async fn remove_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        audit: &AuditContext,
    ) -> Result<KeyRemoval, DbError> {
        change(
            db,
            Scope::Tenant(scope),
            id,
            audit,
            "tenant key removal",
            true,
        )
        .await
    }
    pub async fn revoke_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        audit: &AuditContext,
    ) -> Result<KeyRemoval, DbError> {
        change(
            db,
            Scope::Tenant(scope),
            id,
            audit,
            "tenant key revocation",
            false,
        )
        .await
    }
    pub async fn remove_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        tenant: Uuid,
        id: Uuid,
        audit: &AuditContext,
        reason: &str,
    ) -> Result<KeyRemoval, DbError> {
        change(db, Scope::Platform(scope, tenant), id, audit, reason, true).await
    }
    pub async fn revoke_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        tenant: Uuid,
        id: Uuid,
        audit: &AuditContext,
        reason: &str,
    ) -> Result<KeyRemoval, DbError> {
        change(db, Scope::Platform(scope, tenant), id, audit, reason, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_read_predicates_share_scope_without_projecting_hashes() {
        let tenant = Uuid::new_v4();
        let user = Uuid::new_v4();
        for role in [TenantRole::Admin, TenantRole::Member] {
            let scope = Scope::Owned(TenantScope::checked(tenant, user, role).unwrap());
            let list = read_query(scope, false, None, None, true, Some((20, 0))).unwrap();
            let count = read_query(scope, true, None, None, true, None).unwrap();
            assert_eq!(&list.values.unwrap().0[..5], &count.values.unwrap().0[..]);
            assert!(list.sql.contains("k.tenant_id=$1 AND k.user_id=$2"));
            assert!(!COLUMNS.contains("hash"));
        }
    }
    #[test]
    fn key_scope_rejects_insufficient_roles_and_missing_platform_tenant() {
        let id = Uuid::new_v4();
        assert!(
            Scope::Tenant(TenantScope::checked(Uuid::new_v4(), id, TenantRole::Member).unwrap())
                .validate()
                .is_err()
        );
        assert!(
            Scope::Platform(
                PlatformScope::checked(id, PlatformRole::Operator).unwrap(),
                Uuid::new_v4()
            )
            .validate()
            .is_err()
        );
        assert!(
            Scope::Platform(
                PlatformScope::checked(id, PlatformRole::Root).unwrap(),
                Uuid::nil()
            )
            .validate()
            .is_err()
        );
        assert!(valid_reason("").is_err());
        assert!(valid_reason("new\nline").is_err());
    }
}
