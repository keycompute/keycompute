//! Explicit account-management scopes.
//!
//! This module is private to `account.rs`: callers receive typed scope
//! methods, never a nullable tenant selector or an unguarded account lookup.
//! Read projections intentionally omit encrypted credentials.

use super::super::{
    passthrough_binding::PassthroughBinding,
    query::escape_like_pattern,
    response_affinity::ResponseAffinity,
    tenant_audit_event::{AuditContext, TenantAuditEvent},
    upstream_access::lock_configuration,
};
use super::{
    Account, AccountCount, CreateAccountRequest, UpdateAccountRequest, validate_priority,
    validate_rate_limits,
};
use crate::DbError;
use keycompute_types::{
    AuditResult, AuditScopeType, CredentialKind, PlatformRole, PlatformScope, TenantRole,
    TenantScope,
};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct AccountManagementView {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_active: bool,
    pub provider: String,
    pub name: String,
    pub endpoint: String,
    pub upstream_api_key_preview: String,
    pub rpm_limit: i32,
    pub tpm_limit: i32,
    pub priority: i32,
    pub enabled: bool,
    pub pool_enabled: bool,
    pub passthrough_binding_count: i64,
    pub models_supported: Vec<String>,
    pub api_capabilities: Vec<String>,
    pub visibility: String,
    pub health_status: String,
    pub health_reason: Option<String>,
    pub health_penalty: i32,
    pub last_probe_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_probe_status: Option<String>,
    pub last_probe_error_code: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct AccountListFilter {
    pub provider: Option<String>,
    pub enabled: Option<bool>,
    pub search: Option<String>,
    pub tenant_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy)]
pub enum AccountManagementScope {
    Tenant(TenantScope),
    Platform(PlatformScope),
}

/// Immutable authorization values carried by the verified console JWT.
///
/// Tenant-scoped writes must match all three live values under retained locks.
/// Platform-root writes intentionally carry only the global token version.
#[derive(Debug, Clone, Copy)]
pub struct ProviderAuthzSnapshot {
    pub token_version: i32,
    pub tenant_authz_version: Option<i64>,
    pub membership_authz_version: Option<i64>,
}

impl ProviderAuthzSnapshot {
    pub const fn platform(token_version: i32) -> Self {
        Self {
            token_version,
            tenant_authz_version: None,
            membership_authz_version: None,
        }
    }

    pub const fn tenant(
        token_version: i32,
        tenant_authz_version: i64,
        membership_authz_version: i64,
    ) -> Self {
        Self {
            token_version,
            tenant_authz_version: Some(tenant_authz_version),
            membership_authz_version: Some(membership_authz_version),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ReadScope {
    Tenant(TenantScope),
    Platform(PlatformScope),
}

#[derive(Debug, Clone, Copy)]
enum WriteScope {
    Tenant(TenantScope),
    Platform(PlatformScope),
}

const VIEW_COLUMNS: &str = r#"
    a.id,a.tenant_id,(owner.status='active') AS tenant_active,
    a.provider,a.name,
    CASE WHEN a.endpoint LIKE '%@%'
              OR a.endpoint LIKE '%?%'
              OR a.endpoint LIKE '%#%'
              OR a.endpoint ~ '[[:cntrl:]]'
         THEN '' ELSE a.endpoint END AS endpoint,
    a.upstream_api_key_preview,
    a.rpm_limit,a.tpm_limit,a.priority,a.enabled,a.pool_enabled,
    (SELECT COUNT(*)::BIGINT FROM passthrough_bindings binding
     WHERE binding.account_id=a.id) AS passthrough_binding_count,
    a.models_supported,a.api_capabilities,a.visibility,
    a.health_status,a.health_reason,a.health_penalty,
    a.last_probe_at,a.last_probe_status,a.last_probe_error_code,
    a.created_at,a.updated_at
"#;

fn denied() -> DbError {
    DbError::Other("account management authorization denied".into())
}

fn validate_endpoint(endpoint: &str) -> Result<(), DbError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Ok(());
    }
    if (!endpoint.starts_with("http://") && !endpoint.starts_with("https://"))
        || endpoint
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || endpoint.contains('?')
        || endpoint.contains('#')
    {
        return Err(DbError::Other(
            "account endpoint must be a URL without credentials, query, or fragment".into(),
        ));
    }
    let authority = endpoint
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
        .filter(|authority| !authority.is_empty())
        .ok_or_else(|| DbError::Other("account endpoint must include a host".into()))?;
    if authority.contains('@') {
        return Err(DbError::Other(
            "account endpoint must not include username or password credentials".into(),
        ));
    }
    Ok(())
}

fn validate_audit(scope: WriteScope, audit: &AuditContext) -> Result<(), DbError> {
    if audit.credential_kind != CredentialKind::Jwt || audit.actor_user_id.is_nil() {
        return Err(denied());
    }
    match scope {
        WriteScope::Tenant(s)
            if s.tenant_role() == TenantRole::Admin && audit.actor_user_id == s.user_id() => {}
        WriteScope::Platform(s)
            if s.platform_role() == PlatformRole::Root && audit.actor_user_id == s.user_id() => {}
        _ => return Err(denied()),
    }
    Ok(())
}

fn validate_read(scope: ReadScope) -> Result<(), DbError> {
    match scope {
        ReadScope::Tenant(scope) if scope.tenant_role() == TenantRole::Admin => Ok(()),
        ReadScope::Platform(scope) if scope.platform_role() == PlatformRole::Root => Ok(()),
        _ => Err(denied()),
    }
}

fn read_scope(scope: AccountManagementScope) -> ReadScope {
    match scope {
        AccountManagementScope::Tenant(scope) => ReadScope::Tenant(scope),
        AccountManagementScope::Platform(scope) => ReadScope::Platform(scope),
    }
}

fn write_scope(scope: AccountManagementScope) -> WriteScope {
    match scope {
        AccountManagementScope::Tenant(scope) => WriteScope::Tenant(scope),
        AccountManagementScope::Platform(scope) => WriteScope::Platform(scope),
    }
}

fn scope_tenant(scope: WriteScope) -> Option<Uuid> {
    match scope {
        WriteScope::Tenant(scope) => Some(scope.tenant_id()),
        WriteScope::Platform(_) => None,
    }
}

fn scope_actor(scope: WriteScope) -> Uuid {
    match scope {
        WriteScope::Tenant(scope) => scope.user_id(),
        WriteScope::Platform(scope) => scope.user_id(),
    }
}

fn account_query(
    scope: ReadScope,
    filter: &AccountListFilter,
    id: Option<Uuid>,
    count: bool,
    page: Option<(i64, i64)>,
) -> Result<Statement, DbError> {
    validate_read(scope)?;
    let (tenant_selector, actor, authority) = match scope {
        ReadScope::Tenant(scope) => (
            Some(scope.tenant_id()),
            scope.user_id(),
            "a.tenant_id=$1 AND EXISTS (
                SELECT 1 FROM tenant_memberships m
                JOIN users u ON u.id=m.user_id
                JOIN tenants t ON t.id=m.tenant_id
                WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.tenant_role='admin'
                  AND m.status='active' AND u.status='active' AND t.status='active'
            )",
        ),
        ReadScope::Platform(scope) => (
            filter.tenant_id,
            scope.user_id(),
            "($1::uuid IS NULL OR a.tenant_id=$1) AND EXISTS (
                SELECT 1 FROM users actor
                WHERE actor.id=$2 AND actor.platform_role='root' AND actor.status='active'
            )",
        ),
    };
    let search = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(escape_like_pattern);
    let projection = if count {
        "COUNT(*)::BIGINT AS total"
    } else {
        VIEW_COLUMNS
    };
    let mut sql = format!(
        "SELECT {projection} FROM accounts a
         JOIN tenants owner ON owner.id=a.tenant_id
         WHERE {authority}
           AND ($3::uuid IS NULL OR a.id=$3)
           AND ($4::text IS NULL OR LOWER(a.provider)=LOWER($4))
           AND ($5::boolean IS NULL OR a.enabled=$5)
           AND ($6::text IS NULL OR
                LOWER(a.name) LIKE '%'||LOWER($6)||'%' ESCAPE '\\'
                OR LOWER(a.provider) LIKE '%'||LOWER($6)||'%' ESCAPE '\\'
                OR LOWER(a.id::text) LIKE '%'||LOWER($6)||'%' ESCAPE '\\'
                OR LOWER(a.tenant_id::text) LIKE '%'||LOWER($6)||'%' ESCAPE '\\')"
    );
    let mut values = vec![
        tenant_selector.into(),
        actor.into(),
        id.into(),
        filter.provider.as_deref().into(),
        filter.enabled.into(),
        search.as_deref().into(),
    ];
    if let Some((limit, offset)) = page {
        sql.push_str(" ORDER BY a.priority DESC,a.created_at ASC,a.id ASC LIMIT $7 OFFSET $8");
        values.extend([limit.clamp(1, 1000).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}

fn paged_account_query(
    scope: ReadScope,
    filter: &AccountListFilter,
    limit: i64,
    offset: i64,
) -> Result<Statement, DbError> {
    account_query(scope, filter, None, false, Some((limit, offset)))
}

async fn current_actor(
    tx: &DatabaseTransaction,
    scope: WriteScope,
    tenant_ids: &[Uuid],
    supplied: &AuditContext,
    snapshot: ProviderAuthzSnapshot,
) -> Result<AuditContext, DbError> {
    if supplied.credential_kind != CredentialKind::Jwt
        || supplied.actor_user_id != scope_actor(scope)
        || snapshot.token_version < 0
    {
        return Err(denied());
    }
    if matches!(scope, WriteScope::Platform(_))
        && (snapshot.tenant_authz_version.is_some() || snapshot.membership_authz_version.is_some())
    {
        return Err(denied());
    }
    if matches!(scope, WriteScope::Tenant(_))
        && (snapshot
            .tenant_authz_version
            .is_none_or(|version| version <= 0)
            || snapshot
                .membership_authz_version
                .is_none_or(|version| version <= 0))
    {
        return Err(denied());
    }
    let mut sorted = tenant_ids
        .iter()
        .copied()
        .filter(|id| !id.is_nil())
        .collect::<Vec<_>>();
    if let WriteScope::Tenant(scope) = scope {
        sorted.push(scope.tenant_id());
    }
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() && matches!(scope, WriteScope::Tenant(_)) {
        return Err(denied());
    }
    if !sorted.is_empty() {
        let rows = tx
            .query_all(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id,status,authz_version FROM tenants WHERE id=ANY($1::uuid[]) ORDER BY id FOR UPDATE",
                [sorted.clone().into()],
            ))
            .await?;
        if rows.len() != sorted.len() {
            return Err(denied());
        }
        if let WriteScope::Tenant(scope) = scope {
            let selected = rows
                .iter()
                .find(|row| {
                    row.try_get::<Uuid>("", "id")
                        .map(|id| id == scope.tenant_id())
                        .unwrap_or(false)
                })
                .ok_or_else(denied)?;
            if selected.try_get::<String>("", "status")? != "active" {
                return Err(denied());
            }
            if selected.try_get::<i64>("", "authz_version")?
                != snapshot
                    .tenant_authz_version
                    .expect("tenant snapshot version was validated")
            {
                return Err(DbError::OptimisticConflict {
                    entity: "tenant authority".into(),
                    id: scope.tenant_id().to_string(),
                });
            }
        }
    }
    let actor = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id,platform_role,status,token_version FROM users WHERE id=$1 FOR UPDATE",
            [scope_actor(scope).into()],
        ))
        .await?
        .ok_or_else(denied)?;
    let actor_status: String = actor.try_get("", "status")?;
    let actor_role: String = actor.try_get("", "platform_role")?;
    let token_version: i32 = actor.try_get("", "token_version")?;
    if actor_status != "active" {
        return Err(denied());
    }
    if token_version != snapshot.token_version {
        return Err(DbError::Other("console token is no longer current".into()));
    }
    let platform_role = actor_role.parse().map_err(DbError::Other)?;
    let tenant_role = match scope {
        WriteScope::Platform(_) if platform_role == PlatformRole::Root => None,
        WriteScope::Tenant(scope)
            if matches!(
                platform_role,
                PlatformRole::None | PlatformRole::Operator | PlatformRole::Root
            ) =>
        {
            let member = tx
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT tenant_role,authz_version FROM tenant_memberships
                     WHERE tenant_id=$1 AND user_id=$2 AND tenant_role='admin'
                       AND status='active'
                     FOR UPDATE",
                    [scope.tenant_id().into(), scope.user_id().into()],
                ))
                .await?;
            let member = member.ok_or_else(denied)?;
            Some({
                if member.try_get::<i64>("", "authz_version")?
                    != snapshot
                        .membership_authz_version
                        .expect("membership snapshot version was validated")
                {
                    return Err(DbError::OptimisticConflict {
                        entity: "tenant membership authority".into(),
                        id: scope.tenant_id().to_string(),
                    });
                }
                member
                    .try_get::<String>("", "tenant_role")?
                    .parse()
                    .map_err(DbError::Other)?
            })
        }
        _ => return Err(denied()),
    };
    Ok(AuditContext {
        actor_user_id: scope_actor(scope),
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: platform_role,
        actor_tenant_role: tenant_role,
        request_id: supplied.request_id,
    })
}

async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='5s'")
        .await?;
    // Match identity/membership administration before acquiring configuration
    // and resource locks; only console mutations use this transaction helper.
    super::super::tenant_audit_event::lock_identity_admin(&tx).await?;
    lock_configuration(&tx).await?;
    Ok(tx)
}

async fn audit(
    tx: &DatabaseTransaction,
    scope: WriteScope,
    actor: &AuditContext,
    action: &str,
    account_id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), DbError> {
    let (kind, tenant) = match scope {
        WriteScope::Tenant(scope) => (AuditScopeType::Tenant, Some(scope.tenant_id())),
        WriteScope::Platform(_) => (AuditScopeType::Platform, None),
    };
    TenantAuditEvent::append(
        tx,
        kind,
        tenant,
        actor,
        action,
        "account",
        Some(&account_id.to_string()),
        AuditResult::Success,
        metadata,
    )
    .await?;
    Ok(())
}

async fn owner_for_account(
    tx: &DatabaseTransaction,
    scope: WriteScope,
    id: Uuid,
) -> Result<Uuid, DbError> {
    let (tenant_id, actor_id, authority) = match scope {
        WriteScope::Tenant(scope) => (
            Some(scope.tenant_id()),
            scope.user_id(),
            "a.tenant_id=$2
             AND EXISTS (
                 SELECT 1 FROM tenant_memberships m
                 JOIN users u ON u.id=m.user_id
                 JOIN tenants t ON t.id=m.tenant_id
                 WHERE m.tenant_id=$2 AND m.user_id=$3
                   AND m.tenant_role='admin' AND m.status='active'
                   AND u.status='active' AND t.status='active'
             )",
        ),
        WriteScope::Platform(scope) => (
            None,
            scope.user_id(),
            "EXISTS (
                 SELECT 1 FROM users actor
                 WHERE actor.id=$3 AND actor.platform_role='root' AND actor.status='active'
             )",
        ),
    };
    let row = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT a.tenant_id FROM accounts a WHERE a.id=$1 AND {authority}"),
            [id.into(), tenant_id.into(), actor_id.into()],
        ))
        .await?
        .ok_or_else(|| DbError::not_found("account", id))?;
    Ok(row.try_get("", "tenant_id")?)
}

async fn account_for_update(
    tx: &DatabaseTransaction,
    id: Uuid,
    tenant_id: Uuid,
) -> Result<Account, DbError> {
    Account::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
        [id.into(), tenant_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("account", id))
}

async fn account_for_key_share(
    tx: &DatabaseTransaction,
    id: Uuid,
    tenant_id: Uuid,
) -> Result<Account, DbError> {
    Account::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR KEY SHARE",
        [id.into(), tenant_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("account", id))
}

async fn ensure_active_tenants(
    tx: &DatabaseTransaction,
    tenant_ids: &[Uuid],
) -> Result<(), DbError> {
    let mut sorted = tenant_ids
        .iter()
        .copied()
        .filter(|id| !id.is_nil())
        .collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() {
        return Err(DbError::Other("active tenant required".into()));
    }
    let rows = tx
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM tenants WHERE id=ANY($1::uuid[]) AND status='active' ORDER BY id FOR UPDATE",
            [sorted.clone().into()],
        ))
        .await?;
    if rows.len() != sorted.len() {
        return Err(DbError::Other("active tenant required".into()));
    }
    Ok(())
}

fn verify_owner(scope: WriteScope, owner_tenant: Uuid) -> Result<(), DbError> {
    if let Some(tenant) = scope_tenant(scope)
        && tenant != owner_tenant
    {
        return Err(denied());
    }
    Ok(())
}

async fn account_view(
    db: &impl ConnectionTrait,
    scope: ReadScope,
    filter: &AccountListFilter,
    id: Uuid,
) -> Result<Option<AccountManagementView>, DbError> {
    Ok(AccountManagementView::find_by_statement(account_query(
        scope,
        filter,
        Some(id),
        false,
        None,
    )?)
    .one(db)
    .await?)
}

impl Account {
    pub async fn find_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<AccountManagementView>, DbError> {
        account_view(
            db,
            ReadScope::Tenant(scope),
            &AccountListFilter::default(),
            id,
        )
        .await
    }

    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        filter: &AccountListFilter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AccountManagementView>, DbError> {
        Ok(
            AccountManagementView::find_by_statement(paged_account_query(
                ReadScope::Tenant(scope),
                filter,
                limit,
                offset,
            )?)
            .all(db)
            .await?,
        )
    }

    pub async fn count_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        filter: &AccountListFilter,
    ) -> Result<i64, DbError> {
        Ok(AccountCount::find_by_statement(account_query(
            ReadScope::Tenant(scope),
            filter,
            None,
            true,
            None,
        )?)
        .one(db)
        .await?
        .map(|row| row.total)
        .unwrap_or(0))
    }

    pub async fn find_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        id: Uuid,
    ) -> Result<Option<AccountManagementView>, DbError> {
        account_view(
            db,
            ReadScope::Platform(scope),
            &AccountListFilter::default(),
            id,
        )
        .await
    }

    pub async fn list_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        filter: &AccountListFilter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<AccountManagementView>, DbError> {
        Ok(
            AccountManagementView::find_by_statement(paged_account_query(
                ReadScope::Platform(scope),
                filter,
                limit,
                offset,
            )?)
            .all(db)
            .await?,
        )
    }

    pub async fn count_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        filter: &AccountListFilter,
    ) -> Result<i64, DbError> {
        Ok(AccountCount::find_by_statement(account_query(
            ReadScope::Platform(scope),
            filter,
            None,
            true,
            None,
        )?)
        .one(db)
        .await?
        .map(|row| row.total)
        .unwrap_or(0))
    }

    pub async fn create_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        req: &CreateAccountRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Account, DbError> {
        Self::create_scoped(
            db,
            WriteScope::Tenant(scope),
            scope.tenant_id(),
            req,
            audit_ctx,
            snapshot,
        )
        .await
    }

    pub async fn create_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        tenant_id: Uuid,
        req: &CreateAccountRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Account, DbError> {
        Self::create_scoped(
            db,
            WriteScope::Platform(scope),
            tenant_id,
            req,
            audit_ctx,
            snapshot,
        )
        .await
    }

    async fn create_scoped(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: WriteScope,
        tenant_id: Uuid,
        req: &CreateAccountRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Account, DbError> {
        validate_audit(scope, audit_ctx)?;
        if tenant_id.is_nil() || req.tenant_id != tenant_id {
            return Err(denied());
        }
        if matches!(scope, WriteScope::Tenant(_))
            && req.visibility.as_deref().unwrap_or("tenant") == "global"
        {
            return Err(DbError::Other(
                "global account publication requires root platform authorization".into(),
            ));
        }
        validate_priority(req.priority)?;
        validate_rate_limits(req.rpm_limit, req.tpm_limit)?;
        validate_endpoint(&req.endpoint)?;
        let tx = begin(db).await?;
        let result = async {
            let audit_ctx =
                current_actor(&tx, scope, &[tenant_id], audit_ctx, snapshot).await?;
            ensure_active_tenants(&tx, &[tenant_id]).await?;
            let tenant = tx
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT status FROM tenants WHERE id=$1",
                    [tenant_id.into()],
                ))
                .await?
                .ok_or_else(|| DbError::not_found("tenant", tenant_id))?;
            if tenant.try_get::<String>("", "status")? != "active" {
                return Err(DbError::Other("active tenant required".into()));
            }
            let account = Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO accounts(
                    tenant_id,provider,name,endpoint,upstream_api_key_encrypted,
                    upstream_api_key_preview,rpm_limit,tpm_limit,priority,
                    models_supported,api_capabilities,visibility,pool_enabled
                 ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)
                 RETURNING *",
                [
                    tenant_id.into(),
                    req.provider.as_str().into(),
                    req.name.as_str().into(),
                    req.endpoint.as_str().into(),
                    req.upstream_api_key_encrypted.as_str().into(),
                    req.upstream_api_key_preview.as_str().into(),
                    req.rpm_limit.unwrap_or(60).into(),
                    req.tpm_limit.unwrap_or(100000).into(),
                    req.priority.unwrap_or(0).into(),
                    req.models_supported.clone().into(),
                    req.api_capabilities.clone().into(),
                    req.visibility.as_deref().unwrap_or("tenant").into(),
                    req.pool_enabled.unwrap_or(true).into(),
                ],
            ))
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::Other("account insert returned no row".into()))?;
            audit(
                &tx,
                scope,
                &audit_ctx,
                "account.create",
                account.id,
                serde_json::json!({"tenant_id": account.tenant_id, "visibility": account.visibility}),
            )
            .await?;
            Ok::<_, DbError>(account)
        }
        .await;
        match result {
            Ok(account) => {
                tx.commit().await?;
                Ok(account)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn update_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        req: &UpdateAccountRequest,
        expected_config_version: chrono::DateTime<chrono::Utc>,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Account, DbError> {
        Self::update_scoped(
            db,
            WriteScope::Tenant(scope),
            id,
            req,
            expected_config_version,
            audit_ctx,
            snapshot,
        )
        .await
    }

    pub async fn update_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        id: Uuid,
        req: &UpdateAccountRequest,
        expected_config_version: chrono::DateTime<chrono::Utc>,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Account, DbError> {
        Self::update_scoped(
            db,
            WriteScope::Platform(scope),
            id,
            req,
            expected_config_version,
            audit_ctx,
            snapshot,
        )
        .await
    }

    async fn update_scoped(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: WriteScope,
        id: Uuid,
        req: &UpdateAccountRequest,
        expected_config_version: chrono::DateTime<chrono::Utc>,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Account, DbError> {
        validate_audit(scope, audit_ctx)?;
        if id.is_nil() {
            return Err(denied());
        }
        validate_priority(req.priority)?;
        validate_rate_limits(req.rpm_limit, req.tpm_limit)?;
        if let Some(endpoint) = req.endpoint.as_deref() {
            validate_endpoint(endpoint)?;
        }
        let tx = begin(db).await?;
        let result = async {
            let pre_tenant = owner_for_account(&tx, scope, id).await?;
            let requested_tenant = req.tenant_id.unwrap_or(pre_tenant);
            if requested_tenant.is_nil() {
                return Err(DbError::Other("account tenant must be valid".into()));
            }
            if matches!(scope, WriteScope::Tenant(_)) && requested_tenant != pre_tenant {
                return Err(DbError::Other(
                    "account ownership cannot be reassigned through an account update".into(),
                ));
            }
            let tenants = vec![pre_tenant, requested_tenant];
            let audit_ctx =
                current_actor(&tx, scope, &tenants, audit_ctx, snapshot).await?;
            verify_owner(scope, pre_tenant)?;
            ensure_active_tenants(&tx, &[requested_tenant]).await?;
            let current = account_for_update(&tx, id, pre_tenant).await?;
            if current.tenant_id != pre_tenant {
                return Err(DbError::OptimisticConflict {
                    entity: "account".into(),
                    id: id.to_string(),
                });
            }
            let transfer = requested_tenant != pre_tenant;
            if transfer && !matches!(scope, WriteScope::Platform(_)) {
                return Err(DbError::Other(
                    "account ownership cannot be reassigned by a tenant administrator".into(),
                ));
            }
            if let Some(visibility) = req.visibility.as_deref() {
                if !matches!(visibility, "tenant" | "global") {
                    return Err(DbError::Other("visibility must be tenant or global".into()));
                }
                if matches!(scope, WriteScope::Tenant(_)) && visibility != current.visibility {
                    return Err(DbError::Other(
                        "tenant administrators cannot change account visibility".into(),
                    ));
                }
            }
            if req.pool_enabled.is_some()
                && tx
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT 1 FROM passthrough_bindings WHERE account_id=$1 LIMIT 1",
                        [id.into()],
                    ))
                    .await?
                    .is_some()
            {
                return Err(DbError::Other(
                    "account pool participation is managed by passthrough bindings".into(),
                ));
            }
            let endpoint_changed = req
                .endpoint
                .as_deref()
                .is_some_and(|endpoint| endpoint != current.endpoint);
            let credential_changed = req
                .upstream_api_key_encrypted
                .as_deref()
                .is_some_and(|credential| credential != current.upstream_api_key_encrypted);
            let models_changed = req
                .models_supported
                .as_ref()
                .is_some_and(|models| models != &current.models_supported);
            let capabilities_changed = req
                .api_capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities != &current.api_capabilities);
            let connection_changed = endpoint_changed || credential_changed;
            if connection_changed || transfer {
                if ResponseAffinity::lock_account_routes_and_has_deletion_blocker(&tx, id).await? {
                    return Err(DbError::Other(
                        "account has pending Responses work".into(),
                    ));
                }
                if connection_changed {
                    ResponseAffinity::delete_settled_account_routes(&tx, id).await?;
                }
            }
            if transfer {
                tx.query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id FROM passthrough_bindings WHERE account_id=$1 ORDER BY id FOR UPDATE",
                    [id.into()],
                ))
                .await?;
            }
            let account = Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"UPDATE accounts
                   SET name=COALESCE($1,name),
                       endpoint=COALESCE($2,endpoint),
                       upstream_api_key_encrypted=COALESCE($3,upstream_api_key_encrypted),
                       upstream_api_key_preview=COALESCE($4,upstream_api_key_preview),
                       rpm_limit=COALESCE($5,rpm_limit),
                       tpm_limit=COALESCE($6,tpm_limit),
                       priority=COALESCE($7,priority),
                       enabled=COALESCE($8,enabled),
                       models_supported=COALESCE($9,models_supported),
                       api_capabilities=COALESCE($10,api_capabilities),
                       visibility=COALESCE($11,visibility),
                       pool_enabled=COALESCE($12,pool_enabled),
                       tenant_id=COALESCE($13,tenant_id),
                       health_updated_at=CASE WHEN
                         COALESCE($2,endpoint) IS DISTINCT FROM endpoint OR
                         COALESCE($3,upstream_api_key_encrypted) IS DISTINCT FROM upstream_api_key_encrypted OR
                         COALESCE($9,models_supported) IS DISTINCT FROM models_supported OR
                         COALESCE($10,api_capabilities) IS DISTINCT FROM api_capabilities
                         THEN GREATEST(health_updated_at,statement_timestamp())+INTERVAL '1 microsecond'
                         ELSE health_updated_at END,
                       health_generation=health_generation + CASE WHEN
                         COALESCE($2,endpoint) IS DISTINCT FROM endpoint OR
                         COALESCE($3,upstream_api_key_encrypted) IS DISTINCT FROM upstream_api_key_encrypted OR
                         COALESCE($9,models_supported) IS DISTINCT FROM models_supported OR
                         COALESCE($10,api_capabilities) IS DISTINCT FROM api_capabilities
                         THEN 1 ELSE 0 END,
                       upstream_config_version=CASE WHEN
                         COALESCE($2,endpoint) IS DISTINCT FROM endpoint OR
                         COALESCE($3,upstream_api_key_encrypted) IS DISTINCT FROM upstream_api_key_encrypted OR
                         COALESCE($9,models_supported) IS DISTINCT FROM models_supported OR
                         COALESCE($10,api_capabilities) IS DISTINCT FROM api_capabilities
                         THEN GREATEST(upstream_config_version,statement_timestamp())+INTERVAL '1 microsecond'
                         ELSE upstream_config_version END,
                       updated_at=GREATEST(updated_at,NOW())+INTERVAL '1 microsecond'
                   WHERE id=$14 AND tenant_id=$15 AND upstream_config_version=$16
                   RETURNING *"#,
                [
                    req.name.clone().into(),
                    req.endpoint.clone().into(),
                    req.upstream_api_key_encrypted.clone().into(),
                    req.upstream_api_key_preview.clone().into(),
                    req.rpm_limit.into(),
                    req.tpm_limit.into(),
                    req.priority.into(),
                    req.enabled.into(),
                    req.models_supported.clone().into(),
                    req.api_capabilities.clone().into(),
                    req.visibility.clone().into(),
                    req.pool_enabled.into(),
                    requested_tenant.into(),
                    id.into(),
                    pre_tenant.into(),
                    expected_config_version.into(),
                ],
            ))
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::OptimisticConflict {
                entity: "account".into(),
                id: id.to_string(),
            })?;
            PassthroughBinding::ensure_account_models_unambiguous(&tx, id).await?;
            audit(
                &tx,
                scope,
                &audit_ctx,
                "account.update",
                account.id,
                serde_json::json!({
                    "tenant_id": account.tenant_id,
                    "previous_tenant_id": pre_tenant,
                    "connection_changed": connection_changed,
                    "models_changed": models_changed,
                    "capabilities_changed": capabilities_changed,
                }),
            )
            .await?;
            Ok::<_, DbError>(account)
        }
        .await;
        match result {
            Ok(account) => {
                tx.commit().await?;
                Ok(account)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn delete_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<(), DbError> {
        Self::delete_scoped(db, WriteScope::Tenant(scope), id, audit_ctx, snapshot).await
    }

    pub async fn delete_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<(), DbError> {
        Self::delete_scoped(db, WriteScope::Platform(scope), id, audit_ctx, snapshot).await
    }

    async fn delete_scoped(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: WriteScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<(), DbError> {
        validate_audit(scope, audit_ctx)?;
        if id.is_nil() {
            return Err(denied());
        }
        let tx = begin(db).await?;
        let result = async {
            let owner_tenant = owner_for_account(&tx, scope, id).await?;
            let audit_ctx = current_actor(
                &tx,
                scope,
                &[owner_tenant],
                audit_ctx,
                snapshot,
            )
            .await?;
            verify_owner(scope, owner_tenant)?;
            let existing = account_for_update(&tx, id, owner_tenant).await?;
            if existing.tenant_id != owner_tenant {
                return Err(DbError::OptimisticConflict {
                    entity: "account".into(),
                    id: id.to_string(),
                });
            }
            if ResponseAffinity::lock_account_routes_and_has_deletion_blocker(&tx, id).await? {
                return Err(DbError::Other("account has pending Responses work".into()));
            }
            ResponseAffinity::delete_settled_account_routes(&tx, id).await?;
            if tx
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 FROM passthrough_bindings WHERE account_id=$1 LIMIT 1",
                    [id.into()],
                ))
                .await?
                .is_some()
            {
                return Err(DbError::Other(
                    "account is referenced by passthrough bindings".into(),
                ));
            }
            let deleted = tx
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "DELETE FROM accounts WHERE id=$1 AND tenant_id=$2 AND upstream_config_version=$3",
                    [
                        id.into(),
                        owner_tenant.into(),
                        existing.upstream_config_version.into(),
                    ],
                ))
                .await?
                .rows_affected();
            if deleted != 1 {
                return Err(DbError::OptimisticConflict {
                    entity: "account".into(),
                    id: id.to_string(),
                });
            }
            audit(
                &tx,
                scope,
                &audit_ctx,
                "account.delete",
                id,
                serde_json::json!({"tenant_id": owner_tenant}),
            )
            .await
        }
        .await;
        match result {
            Ok(()) => tx.commit().await.map_err(DbError::from),
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn reset_health_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<Account>, DbError> {
        Self::reset_health_scoped(db, WriteScope::Tenant(scope), id, audit_ctx, snapshot).await
    }

    pub async fn reset_health_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<Account>, DbError> {
        Self::reset_health_scoped(db, WriteScope::Platform(scope), id, audit_ctx, snapshot).await
    }

    async fn reset_health_scoped(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: WriteScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<Account>, DbError> {
        validate_audit(scope, audit_ctx)?;
        let tx = begin(db).await?;
        let result = async {
            let owner_tenant = owner_for_account(&tx, scope, id).await?;
            let audit_ctx = current_actor(&tx, scope, &[owner_tenant], audit_ctx, snapshot).await?;
            verify_owner(scope, owner_tenant)?;
            ensure_active_tenants(&tx, &[owner_tenant]).await?;
            let row = Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE accounts SET health_status='unknown',health_reason=NULL,
                 health_penalty=0,health_consecutive_failures=0,health_success_count=0,
                 health_failure_count=0,health_avg_latency_ms=NULL,
                 health_last_success_at=NULL,health_last_failure_at=NULL,
                 health_updated_at=GREATEST(health_updated_at,NOW())+INTERVAL '1 microsecond',
                 health_generation=health_generation+1
                 WHERE id=$1 AND tenant_id=$2 RETURNING *",
                [id.into(), owner_tenant.into()],
            ))
            .one(&tx)
            .await?;
            let Some(account) = row else {
                return Ok(None);
            };
            audit(
                &tx,
                scope,
                &audit_ctx,
                "account.health_reset",
                id,
                serde_json::json!({"tenant_id": owner_tenant}),
            )
            .await?;
            Ok(Some(account))
        }
        .await;
        match result {
            Ok(account) => {
                tx.commit().await?;
                Ok(account)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    /// Prepare a management probe. The transaction is committed before any
    /// upstream I/O, so no database lock is held over the network call.
    pub async fn prepare_probe(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: AccountManagementScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<Account>, DbError> {
        Self::prepare_management(db, scope, id, audit_ctx, snapshot, true).await
    }

    async fn prepare_management(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: AccountManagementScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
        probe_intent: bool,
    ) -> Result<Option<Account>, DbError> {
        let write = write_scope(scope);
        validate_audit(write, audit_ctx)?;
        let tx = begin(db).await?;
        let result = async {
            let owner_tenant = owner_for_account(&tx, write, id).await?;
            let actor = current_actor(&tx, write, &[owner_tenant], audit_ctx, snapshot).await?;
            verify_owner(write, owner_tenant)?;
            let account = account_for_key_share(&tx, id, owner_tenant).await?;
            if probe_intent {
                audit(
                    &tx,
                    write,
                    &actor,
                    "account.probe_requested",
                    id,
                    serde_json::json!({"tenant_id": owner_tenant}),
                )
                .await?;
            }
            Ok(Some(account))
        }
        .await;
        match result {
            Ok(account) => {
                tx.commit().await?;
                Ok(account)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn prepare_update(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: AccountManagementScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<Account>, DbError> {
        Self::prepare_management(db, scope, id, audit_ctx, snapshot, false).await
    }

    /// Fetch connection material and current authority in the same primary
    /// query. A separate Boolean check followed by a naked ID lookup could
    /// observe a different owner/configuration between statements.
    pub async fn load_authorized_probe(
        db: &impl ConnectionTrait,
        scope: AccountManagementScope,
        id: Uuid,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<Account>, DbError> {
        validate_read(read_scope(scope))?;
        let write = write_scope(scope);
        if matches!(write, WriteScope::Platform(_))
            && (snapshot.tenant_authz_version.is_some()
                || snapshot.membership_authz_version.is_some())
        {
            return Err(denied());
        }
        if matches!(write, WriteScope::Tenant(_))
            && (snapshot
                .tenant_authz_version
                .is_none_or(|version| version <= 0)
                || snapshot
                    .membership_authz_version
                    .is_none_or(|version| version <= 0))
        {
            return Err(denied());
        }
        let (tenant, actor, role, tenant_version, membership_version) = match write {
            WriteScope::Tenant(scope) => (
                Some(scope.tenant_id()),
                scope.user_id(),
                "admin",
                snapshot.tenant_authz_version,
                snapshot.membership_authz_version,
            ),
            WriteScope::Platform(scope) => (None, scope.user_id(), "root", None, None),
        };
        let row = Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT a.* FROM accounts a
                 JOIN tenants owner ON owner.id=a.tenant_id AND owner.status='active'
                 JOIN users u ON u.id=$2 AND u.status='active' AND u.token_version=$4
                 WHERE a.id=$1
                   AND ($3::uuid IS NULL OR a.tenant_id=$3)
                   AND (($5='root' AND u.platform_role='root')
                     OR ($5='admin' AND EXISTS(
                       SELECT 1 FROM tenant_memberships m JOIN tenants t ON t.id=m.tenant_id
                       WHERE m.tenant_id=a.tenant_id AND m.user_id=$2
                         AND m.tenant_role='admin' AND m.status='active'
                         AND t.status='active'
                         AND t.authz_version=$6
                         AND m.authz_version=$7)))",
            [
                id.into(),
                actor.into(),
                tenant.into(),
                snapshot.token_version.into(),
                role.into(),
                tenant_version.into(),
                membership_version.into(),
            ],
        ))
        .one(db)
        .await?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::validate_endpoint;

    #[test]
    fn management_endpoint_validation_rejects_stored_secret_components() {
        assert!(validate_endpoint("https://provider.example/v1").is_ok());
        assert!(validate_endpoint("https://user:secret@provider.example/v1").is_err());
        assert!(validate_endpoint("https://provider.example/v1?api_key=secret").is_err());
        assert!(validate_endpoint("https://provider.example/v1#secret").is_err());
    }
}
