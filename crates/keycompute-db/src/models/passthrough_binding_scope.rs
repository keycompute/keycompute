//! Explicit management scopes for account-to-tenant bindings.
//!
//! A binding is not an ownership transfer. Tenant administrators can manage
//! only bindings for accounts owned by their tenant; root can create and
//! revoke foreign/global grants, which remain consumption-only for consumers.

use super::super::{
    account::{Account, AccountManagementScope, ProviderAuthzSnapshot},
    query::escape_like_pattern,
    tenant_audit_event::{AuditContext, TenantAuditEvent},
    upstream_access::lock_configuration,
};
use super::{
    AccountModelHealth, AccountModelHealthProbe, PassthroughBinding, PassthroughBindingCount,
    UpdatePassthroughBindingRequest,
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

#[derive(Clone, FromQueryResult, Serialize)]
pub struct PassthroughBindingManagementView {
    // Internal readiness inputs are never exposed through serialization or Debug.
    #[serde(skip)]
    connection_endpoint: String,
    #[serde(skip)]
    connection_secret: String,
    pub id: Uuid,
    pub account_id: Uuid,
    pub account_name: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub provider: String,
    pub is_global: bool,
    pub pool_enabled: bool,
    pub revision: i64,
    pub models_supported: Vec<String>,
    pub health_status: String,
    pub health_reason_code: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl std::fmt::Debug for PassthroughBindingManagementView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassthroughBindingManagementView")
            .field("id", &self.id)
            .field("tenant_id", &self.tenant_id)
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}
impl PassthroughBindingManagementView {
    /// Validate locally held connection metadata without copying secrets into
    /// the public DTO. No upstream request is made by this diagnostic check.
    pub fn validate_connection(&mut self, valid: impl FnOnce(&str, &str) -> bool) {
        if !valid(&self.connection_endpoint, &self.connection_secret) {
            self.health_status = "unavailable".into();
            self.health_reason_code = Some("invalid_connection_metadata".into());
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PassthroughBindingListFilter {
    pub tenant_id: Option<Uuid>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, FromQueryResult, Serialize)]
pub struct PassthroughAccountOption {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub pool_enabled: bool,
    pub models: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedPassthroughProbe {
    pub binding: PassthroughBinding,
    pub account: Account,
}

#[derive(Debug, Clone, Copy)]
enum Scope {
    Tenant(TenantScope),
    Platform(PlatformScope),
}

fn denied() -> DbError {
    DbError::Other("passthrough binding management authorization denied".into())
}

fn management_scope(scope: AccountManagementScope) -> Scope {
    match scope {
        AccountManagementScope::Tenant(scope) => Scope::Tenant(scope),
        AccountManagementScope::Platform(scope) => Scope::Platform(scope),
    }
}

fn validate_read(scope: Scope) -> Result<(), DbError> {
    match scope {
        Scope::Tenant(scope) if scope.tenant_role() == TenantRole::Admin => Ok(()),
        Scope::Platform(scope) if scope.platform_role() == PlatformRole::Root => Ok(()),
        _ => Err(denied()),
    }
}

fn validate_audit(scope: Scope, audit: &AuditContext) -> Result<(), DbError> {
    if audit.credential_kind != CredentialKind::Jwt || audit.actor_user_id.is_nil() {
        return Err(denied());
    }
    match scope {
        Scope::Tenant(scope)
            if scope.tenant_role() == TenantRole::Admin
                && scope.user_id() == audit.actor_user_id => {}
        Scope::Platform(scope)
            if scope.platform_role() == PlatformRole::Root
                && scope.user_id() == audit.actor_user_id => {}
        _ => return Err(denied()),
    }
    Ok(())
}

fn actor(scope: Scope) -> Uuid {
    match scope {
        Scope::Tenant(scope) => scope.user_id(),
        Scope::Platform(scope) => scope.user_id(),
    }
}

fn tenant_scope(scope: Scope) -> Option<Uuid> {
    match scope {
        Scope::Tenant(scope) => Some(scope.tenant_id()),
        Scope::Platform(_) => None,
    }
}

const VIEW_COLUMNS: &str = r#"
    pb.id,pb.account_id,a.name AS account_name,pb.tenant_id,t.name AS tenant_name,
    a.endpoint AS connection_endpoint,a.upstream_api_key_encrypted AS connection_secret,
    a.provider,pb.is_global,pb.pool_enabled,pb.revision,a.models_supported,
    CASE WHEN a.enabled
              AND ((a.provider='openai' AND a.api_capabilities && ARRAY['chat_completions','responses']::TEXT[])
                OR (a.provider='anthropic' AND 'messages'=ANY(a.api_capabilities)))
              AND owner.status='active' AND t.status='active'
         THEN a.health_status ELSE 'unavailable' END AS health_status,
    CASE WHEN a.enabled
              AND ((a.provider='openai' AND a.api_capabilities && ARRAY['chat_completions','responses']::TEXT[])
                OR (a.provider='anthropic' AND 'messages'=ANY(a.api_capabilities)))
              AND owner.status='active' AND t.status='active'
         THEN a.health_reason ELSE 'inactive_owner_or_binding_tenant' END AS health_reason_code,
    pb.created_at,pb.updated_at
"#;

fn query(
    scope: Scope,
    filter: &PassthroughBindingListFilter,
    id: Option<Uuid>,
    count: bool,
    page: Option<(i64, i64)>,
) -> Result<Statement, DbError> {
    validate_read(scope)?;
    let (tenant_filter, authority) = match scope {
        Scope::Tenant(scope) => (
            Some(scope.tenant_id()),
            "pb.tenant_id=$1 AND a.tenant_id=$1 AND NOT pb.is_global AND EXISTS(
                SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id
                JOIN tenants current_tenant ON current_tenant.id=m.tenant_id
                WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.tenant_role='admin'
                  AND m.status='active' AND u.status='active'
                  AND current_tenant.status='active'
            )",
        ),
        Scope::Platform(_) => (
            filter.tenant_id,
            "($1::uuid IS NULL OR pb.tenant_id=$1) AND EXISTS(
                SELECT 1 FROM users actor
                WHERE actor.id=$2 AND actor.platform_role='root' AND actor.status='active'
            )",
        ),
    };
    let search = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(escape_like_pattern);
    let projection = if count {
        "COUNT(*)::BIGINT AS total"
    } else {
        VIEW_COLUMNS
    };
    let mut sql = format!(
        "SELECT {projection}
         FROM passthrough_bindings pb
         JOIN accounts a ON a.id=pb.account_id
         JOIN tenants t ON t.id=pb.tenant_id
         JOIN tenants owner ON owner.id=a.tenant_id
         WHERE {authority}
           AND ($3::uuid IS NULL OR pb.id=$3)
           AND ($4::text IS NULL OR a.name ILIKE '%'||$4||'%' ESCAPE '\\'
                OR t.name ILIKE '%'||$4||'%' ESCAPE '\\')"
    );
    let mut values = vec![
        tenant_filter.into(),
        actor(scope).into(),
        id.into(),
        search.as_deref().into(),
    ];
    if let Some((limit, offset)) = page {
        sql.push_str(" ORDER BY a.name,t.name,pb.id LIMIT $5 OFFSET $6");
        values.extend([limit.clamp(1, 200).into(), offset.max(0).into()]);
    }
    Ok(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
}

async fn lock_authority(
    tx: &DatabaseTransaction,
    scope: Scope,
    tenants: &[Uuid],
    supplied: &AuditContext,
    snapshot: ProviderAuthzSnapshot,
) -> Result<AuditContext, DbError> {
    if supplied.credential_kind != CredentialKind::Jwt
        || supplied.actor_user_id != actor(scope)
        || snapshot.token_version < 0
    {
        return Err(denied());
    }
    if matches!(scope, Scope::Platform(_))
        && (snapshot.tenant_authz_version.is_some() || snapshot.membership_authz_version.is_some())
    {
        return Err(denied());
    }
    if matches!(scope, Scope::Tenant(_))
        && (snapshot
            .tenant_authz_version
            .is_none_or(|version| version <= 0)
            || snapshot
                .membership_authz_version
                .is_none_or(|version| version <= 0))
    {
        return Err(denied());
    }
    let mut sorted = tenants
        .iter()
        .copied()
        .filter(|id| !id.is_nil())
        .collect::<Vec<_>>();
    if let Scope::Tenant(scope) = scope {
        sorted.push(scope.tenant_id());
    }
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() {
        return Err(denied());
    }
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
    if let Scope::Tenant(scope) = scope {
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
    let user = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT platform_role,status,token_version FROM users WHERE id=$1 FOR UPDATE",
            [actor(scope).into()],
        ))
        .await?
        .ok_or_else(denied)?;
    let status: String = user.try_get("", "status")?;
    let role: String = user.try_get("", "platform_role")?;
    let token_version: i32 = user.try_get("", "token_version")?;
    if status != "active" {
        return Err(denied());
    }
    if token_version != snapshot.token_version {
        return Err(DbError::Other("console token is no longer current".into()));
    }
    let platform_role = role.parse().map_err(DbError::Other)?;
    let tenant_role = match scope {
        Scope::Platform(_) if platform_role == PlatformRole::Root => None,
        Scope::Tenant(scope)
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
            Some(
                member
                    .try_get::<String>("", "tenant_role")?
                    .parse()
                    .map_err(DbError::Other)?,
            )
        }
        _ => return Err(denied()),
    };
    Ok(AuditContext {
        actor_user_id: actor(scope),
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
    scope: Scope,
    ctx: &AuditContext,
    action: &str,
    id: Uuid,
    metadata: serde_json::Value,
) -> Result<(), DbError> {
    let (scope_type, tenant_id) = match scope {
        Scope::Tenant(scope) => (AuditScopeType::Tenant, Some(scope.tenant_id())),
        Scope::Platform(_) => (AuditScopeType::Platform, None),
    };
    TenantAuditEvent::append(
        tx,
        scope_type,
        tenant_id,
        ctx,
        action,
        "passthrough_binding",
        Some(&id.to_string()),
        AuditResult::Success,
        metadata,
    )
    .await?;
    Ok(())
}

#[derive(Debug, FromQueryResult)]
struct BindingMetadata {
    account_id: Uuid,
    binding_tenant_id: Uuid,
    owner_tenant_id: Uuid,
    revision: i64,
}

async fn authorized_binding_metadata(
    tx: &DatabaseTransaction,
    scope: Scope,
    id: Uuid,
) -> Result<BindingMetadata, DbError> {
    let (sql, values) = match scope {
        Scope::Tenant(scope) => (
            "SELECT pb.account_id,pb.tenant_id AS binding_tenant_id,
                    a.tenant_id AS owner_tenant_id,pb.revision
             FROM passthrough_bindings pb
             JOIN accounts a ON a.id=pb.account_id
             WHERE pb.id=$1 AND pb.tenant_id=$2 AND a.tenant_id=$2
               AND NOT pb.is_global
               AND EXISTS(
                   SELECT 1 FROM tenant_memberships m
                   JOIN users u ON u.id=m.user_id
                   JOIN tenants t ON t.id=m.tenant_id
                   WHERE m.tenant_id=$2 AND m.user_id=$3
                     AND m.tenant_role='admin' AND m.status='active'
                     AND u.status='active' AND t.status='active'
               )",
            vec![id.into(), scope.tenant_id().into(), scope.user_id().into()],
        ),
        Scope::Platform(scope) => (
            "SELECT pb.account_id,pb.tenant_id AS binding_tenant_id,
                    a.tenant_id AS owner_tenant_id,pb.revision
             FROM passthrough_bindings pb
             JOIN accounts a ON a.id=pb.account_id
             WHERE pb.id=$1
               AND EXISTS(
                   SELECT 1 FROM users u
                   WHERE u.id=$2 AND u.platform_role='root' AND u.status='active'
               )",
            vec![id.into(), scope.user_id().into()],
        ),
    };
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .await?
    .ok_or_else(|| DbError::not_found("passthrough binding", id))
    .and_then(|row| BindingMetadata::from_query_result(&row, "").map_err(DbError::from))
}

async fn authorized_account_owner(
    tx: &DatabaseTransaction,
    scope: Scope,
    account_id: Uuid,
) -> Result<Uuid, DbError> {
    let (sql, values) = match scope {
        Scope::Tenant(scope) => (
            "SELECT a.tenant_id
             FROM accounts a
             WHERE a.id=$1 AND a.tenant_id=$2
               AND EXISTS(
                   SELECT 1 FROM tenant_memberships m
                   JOIN users u ON u.id=m.user_id
                   JOIN tenants t ON t.id=m.tenant_id
                   WHERE m.tenant_id=$2 AND m.user_id=$3
                     AND m.tenant_role='admin' AND m.status='active'
                     AND u.status='active' AND t.status='active'
               )",
            vec![
                account_id.into(),
                scope.tenant_id().into(),
                scope.user_id().into(),
            ],
        ),
        Scope::Platform(scope) => (
            "SELECT a.tenant_id
             FROM accounts a
             WHERE a.id=$1
               AND EXISTS(
                   SELECT 1 FROM users u
                   WHERE u.id=$2 AND u.platform_role='root' AND u.status='active'
               )",
            vec![account_id.into(), scope.user_id().into()],
        ),
    };
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .await?
    .ok_or_else(|| DbError::not_found("account", account_id))
    .and_then(|row| row.try_get("", "tenant_id").map_err(DbError::from))
}

async fn binding_for_update(
    tx: &DatabaseTransaction,
    id: Uuid,
    binding_tenant: Uuid,
    account_id: Uuid,
) -> Result<PassthroughBinding, DbError> {
    PassthroughBinding::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM passthrough_bindings
         WHERE id=$1 AND tenant_id=$2 AND account_id=$3 FOR UPDATE",
        [id.into(), binding_tenant.into(), account_id.into()],
    ))
    .one(tx)
    .await?
    .ok_or_else(|| DbError::not_found("passthrough binding", id))
}

fn check_tenant_owner(
    scope: Scope,
    binding_tenant: Uuid,
    owner_tenant: Uuid,
    is_global: bool,
) -> Result<(), DbError> {
    if let Some(tenant) = tenant_scope(scope)
        && (tenant != binding_tenant || tenant != owner_tenant || is_global)
    {
        return Err(denied());
    }
    Ok(())
}

fn account_supported(account: &Account) -> bool {
    match account.provider.as_str() {
        "openai" => account
            .api_capabilities
            .iter()
            .any(|v| matches!(v.as_str(), "chat_completions" | "responses")),
        "anthropic" => account.api_capabilities.iter().any(|v| v == "messages"),
        _ => false,
    }
}

async fn validate_target(
    tx: &DatabaseTransaction,
    scope: Scope,
    account: Account,
    binding_tenant: Uuid,
    is_global: bool,
) -> Result<Account, DbError> {
    check_tenant_owner(scope, binding_tenant, account.tenant_id, is_global)?;
    let active = tx
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM tenants owner JOIN tenants target ON target.id=$2
             WHERE owner.id=$1 AND owner.status='active' AND target.status='active'",
            [account.tenant_id.into(), binding_tenant.into()],
        ))
        .await?
        .is_some();
    if !active || !account.enabled || !account_supported(&account) {
        return Err(DbError::Other(
            "account and binding tenant must be active and the account must declare a supported native API capability"
                .into(),
        ));
    }
    Ok(account)
}

async fn ensure_no_overlap(
    db: &impl ConnectionTrait,
    account_id: Uuid,
    tenant_id: Uuid,
    is_global: bool,
    ignore_id: Option<Uuid>,
) -> Result<(), DbError> {
    let conflict = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM passthrough_bindings other
             JOIN accounts other_account ON other_account.id=other.account_id
             JOIN accounts selected ON selected.id=$1
             WHERE other.account_id<>$1 AND ($4::uuid IS NULL OR other.id<>$4)
               AND (other.is_global OR $3 OR other.tenant_id=$2)
               AND other_account.provider=selected.provider
               AND other_account.api_capabilities && selected.api_capabilities
               AND other_account.models_supported && selected.models_supported
             LIMIT 1",
            [
                account_id.into(),
                tenant_id.into(),
                is_global.into(),
                ignore_id.into(),
            ],
        ))
        .await?
        .is_some();
    if conflict {
        return Err(DbError::Other("passthrough_binding_ambiguous".into()));
    }
    Ok(())
}

async fn suppress_pool(db: &impl ConnectionTrait, account_id: Uuid) -> Result<(), DbError> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET pool_enabled=FALSE,
         updated_at=GREATEST(updated_at,statement_timestamp())+INTERVAL '1 microsecond'
         WHERE id=$1",
        [account_id.into()],
    ))
    .await?;
    Ok(())
}

impl PassthroughBinding {
    pub async fn find_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        id: Uuid,
    ) -> Result<Option<PassthroughBindingManagementView>, DbError> {
        Ok(PassthroughBindingManagementView::find_by_statement(query(
            Scope::Tenant(scope),
            &PassthroughBindingListFilter::default(),
            Some(id),
            false,
            None,
        )?)
        .one(db)
        .await?)
    }

    pub async fn list_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        filter: &PassthroughBindingListFilter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PassthroughBindingManagementView>, DbError> {
        Ok(PassthroughBindingManagementView::find_by_statement(query(
            Scope::Tenant(scope),
            filter,
            None,
            false,
            Some((limit, offset)),
        )?)
        .all(db)
        .await?)
    }

    pub async fn count_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        filter: &PassthroughBindingListFilter,
    ) -> Result<i64, DbError> {
        Ok(PassthroughBindingCount::find_by_statement(query(
            Scope::Tenant(scope),
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
    ) -> Result<Option<PassthroughBindingManagementView>, DbError> {
        Ok(PassthroughBindingManagementView::find_by_statement(query(
            Scope::Platform(scope),
            &PassthroughBindingListFilter::default(),
            Some(id),
            false,
            None,
        )?)
        .one(db)
        .await?)
    }

    pub async fn list_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        filter: &PassthroughBindingListFilter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<PassthroughBindingManagementView>, DbError> {
        Ok(PassthroughBindingManagementView::find_by_statement(query(
            Scope::Platform(scope),
            filter,
            None,
            false,
            Some((limit, offset)),
        )?)
        .all(db)
        .await?)
    }

    pub async fn count_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        filter: &PassthroughBindingListFilter,
    ) -> Result<i64, DbError> {
        Ok(PassthroughBindingCount::find_by_statement(query(
            Scope::Platform(scope),
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

    pub async fn options_in_tenant(
        db: &impl ConnectionTrait,
        scope: TenantScope,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PassthroughAccountOption>, i64), DbError> {
        options(db, Scope::Tenant(scope), search, limit, offset).await
    }

    pub async fn options_platform(
        db: &impl ConnectionTrait,
        scope: PlatformScope,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<PassthroughAccountOption>, i64), DbError> {
        options(db, Scope::Platform(scope), search, limit, offset).await
    }

    pub async fn create_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        req: &super::CreatePassthroughBindingRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Self, DbError> {
        create_scoped(db, Scope::Tenant(scope), req, audit_ctx, snapshot).await
    }

    pub async fn create_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        req: &super::CreatePassthroughBindingRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Self, DbError> {
        create_scoped(db, Scope::Platform(scope), req, audit_ctx, snapshot).await
    }

    pub async fn update_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        req: &UpdatePassthroughBindingRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Self, DbError> {
        update_scoped(db, Scope::Tenant(scope), id, req, audit_ctx, snapshot).await
    }

    pub async fn update_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        id: Uuid,
        req: &UpdatePassthroughBindingRequest,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Self, DbError> {
        update_scoped(db, Scope::Platform(scope), id, req, audit_ctx, snapshot).await
    }

    pub async fn delete_in_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: TenantScope,
        id: Uuid,
        revision: i64,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<(), DbError> {
        delete_scoped(db, Scope::Tenant(scope), id, revision, audit_ctx, snapshot).await
    }

    pub async fn delete_platform(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: PlatformScope,
        id: Uuid,
        revision: i64,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<(), DbError> {
        delete_scoped(
            db,
            Scope::Platform(scope),
            id,
            revision,
            audit_ctx,
            snapshot,
        )
        .await
    }

    pub async fn prepare_probe(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: AccountManagementScope,
        id: Uuid,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<PreparedPassthroughProbe>, DbError> {
        let scope = management_scope(scope);
        validate_audit(scope, audit_ctx)?;
        let tx = begin(db).await?;
        let result = async {
            let metadata = authorized_binding_metadata(&tx, scope, id).await?;
            let audit_ctx = lock_authority(
                &tx,
                scope,
                &[metadata.binding_tenant_id, metadata.owner_tenant_id],
                audit_ctx,
                snapshot,
            )
            .await?;
            let account = Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
                [metadata.account_id.into(), metadata.owner_tenant_id.into()],
            ))
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("account", metadata.account_id))?;
            let binding =
                binding_for_update(&tx, id, metadata.binding_tenant_id, metadata.account_id)
                    .await?;
            check_tenant_owner(
                scope,
                binding.tenant_id,
                account.tenant_id,
                binding.is_global,
            )?;
            if binding.revision != metadata.revision {
                return Err(DbError::OptimisticConflict {
                    entity: "passthrough binding".into(),
                    id: id.to_string(),
                });
            }
            let account =
                validate_target(&tx, scope, account, binding.tenant_id, binding.is_global).await?;
            audit(
                &tx,
                scope,
                &audit_ctx,
                "passthrough_binding.probe_requested",
                binding.id,
                serde_json::json!({"account_id": account.id, "tenant_id": binding.tenant_id}),
            )
            .await?;
            Ok(Some(PreparedPassthroughProbe { binding, account }))
        }
        .await;
        match result {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    pub async fn record_probe(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: AccountManagementScope,
        binding: &PassthroughBinding,
        probe: &AccountModelHealthProbe,
        audit_ctx: &AuditContext,
        snapshot: ProviderAuthzSnapshot,
    ) -> Result<Option<AccountModelHealth>, DbError> {
        let scope = management_scope(scope);
        validate_audit(scope, audit_ctx)?;
        let tx = begin(db).await?;
        let result = async {
            let metadata = authorized_binding_metadata(&tx, scope, binding.id).await?;
            let audit_ctx =
                lock_authority(
                    &tx,
                    scope,
                    &[metadata.binding_tenant_id, metadata.owner_tenant_id],
                    audit_ctx,
                    snapshot,
                )
                    .await?;
            let account = Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
                [metadata.account_id.into(), metadata.owner_tenant_id.into()],
            ))
            .one(&tx)
                .await?
                .ok_or_else(|| DbError::not_found("account", metadata.account_id))?;
            let current = binding_for_update(
                &tx,
                binding.id,
                metadata.binding_tenant_id,
                metadata.account_id,
            )
            .await?;
            check_tenant_owner(
                scope,
                current.tenant_id,
                account.tenant_id,
                current.is_global,
            )?;
            if current.revision != binding.revision || current.account_id != binding.account_id {
                return Err(DbError::OptimisticConflict {
                    entity: "passthrough binding".into(),
                    id: binding.id.to_string(),
                });
            }
            if current.revision != metadata.revision {
                return Err(DbError::OptimisticConflict {
                    entity: "passthrough binding".into(),
                    id: binding.id.to_string(),
                });
            }
            let active_tenants = tx
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 FROM tenants owner
                     JOIN tenants target ON target.id=$2
                     WHERE owner.id=$1
                       AND owner.status='active' AND target.status='active'",
                    [account.tenant_id.into(), current.tenant_id.into()],
                ))
                .await?
                .is_some();
            if !active_tenants {
                return Ok(None);
            }
            if !account.enabled || !account_supported(&account) {
                return Ok(None);
            }
            let saved = AccountModelHealth::upsert_binding_probe_if_current(&tx, probe, &current).await?;
            let Some(saved) = saved else {
                return Ok(None);
            };
            audit(
                &tx,
                scope,
                &audit_ctx,
                "passthrough_binding.probe",
                binding.id,
                serde_json::json!({"account_id": current.account_id, "model": probe.model, "status": probe.status}),
            )
            .await?;
            Ok(Some(saved))
        }
        .await;
        match result {
            Ok(value) => {
                tx.commit().await?;
                Ok(value)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }
}

async fn create_scoped(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: Scope,
    req: &super::CreatePassthroughBindingRequest,
    audit_ctx: &AuditContext,
    snapshot: ProviderAuthzSnapshot,
) -> Result<PassthroughBinding, DbError> {
    validate_audit(scope, audit_ctx)?;
    if req.account_id.is_nil() || req.tenant_id.is_nil() {
        return Err(denied());
    }
    if matches!(scope, Scope::Tenant(_)) && req.is_global {
        return Err(DbError::Other(
            "global passthrough grants require root platform authorization".into(),
        ));
    }
    if let Scope::Tenant(scope) = scope
        && req.tenant_id != scope.tenant_id()
    {
        return Err(denied());
    }
    let tx = begin(db).await?;
    let result = async {
        let owner_tenant = authorized_account_owner(&tx, scope, req.account_id).await?;
        let audit_ctx =
            lock_authority(
                &tx,
                scope,
                &[owner_tenant, req.tenant_id],
                audit_ctx,
                snapshot,
            )
            .await?;
        let account = Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
            [req.account_id.into(), owner_tenant.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("account", req.account_id))?;
        let account = validate_target(
            &tx,
            scope,
            account,
            req.tenant_id,
            req.is_global,
        )
        .await?;
        ensure_no_overlap(&tx, req.account_id, req.tenant_id, req.is_global, None).await?;
        let binding = PassthroughBinding::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO passthrough_bindings(account_id,tenant_id,is_global,pool_enabled)
             VALUES($1,$2,$3,$4) RETURNING *",
            [
                req.account_id.into(),
                req.tenant_id.into(),
                req.is_global.into(),
                req.pool_enabled.into(),
            ],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::Other("passthrough binding insert returned no row".into()))?;
        suppress_pool(&tx, account.id).await?;
        audit(
            &tx,
            scope,
            &audit_ctx,
            "passthrough_binding.create",
            binding.id,
            serde_json::json!({"account_id": binding.account_id, "tenant_id": binding.tenant_id, "is_global": binding.is_global}),
        )
        .await?;
        Ok::<_, DbError>(binding)
    }
    .await;
    match result {
        Ok(binding) => {
            tx.commit().await?;
            Ok(binding)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn update_scoped(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: Scope,
    id: Uuid,
    req: &UpdatePassthroughBindingRequest,
    audit_ctx: &AuditContext,
    snapshot: ProviderAuthzSnapshot,
) -> Result<PassthroughBinding, DbError> {
    validate_audit(scope, audit_ctx)?;
    if id.is_nil() || req.expected_revision <= 0 {
        return Err(DbError::OptimisticConflict {
            entity: "passthrough binding".into(),
            id: id.to_string(),
        });
    }
    if matches!(scope, Scope::Tenant(_)) && req.is_global == Some(true) {
        return Err(DbError::Other(
            "global passthrough grants require root platform authorization".into(),
        ));
    }
    let tx = begin(db).await?;
    let result = async {
        let metadata = authorized_binding_metadata(&tx, scope, id).await?;
        let requested_account = req.account_id.unwrap_or(metadata.account_id);
        let requested_tenant = req.tenant_id.unwrap_or(metadata.binding_tenant_id);
        if requested_account.is_nil() || requested_tenant.is_nil() {
            return Err(denied());
        }
        if let Scope::Tenant(scope) = scope
            && requested_tenant != scope.tenant_id()
        {
            return Err(denied());
        }
        let requested_owner = authorized_account_owner(&tx, scope, requested_account).await?;
        let audit_ctx = lock_authority(
            &tx,
            scope,
            &[
                metadata.owner_tenant_id,
                metadata.binding_tenant_id,
                requested_owner,
                requested_tenant,
            ],
            audit_ctx,
            snapshot,
        )
        .await?;
        let mut account_ids = vec![metadata.account_id, requested_account];
        account_ids.sort_unstable();
        account_ids.dedup();
        for account_id in account_ids {
            let owner_tenant = if account_id == metadata.account_id {
                metadata.owner_tenant_id
            } else {
                requested_owner
            };
            Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
                [account_id.into(), owner_tenant.into()],
            ))
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("account", account_id))?;
        }
        let current = binding_for_update(
            &tx,
            id,
            metadata.binding_tenant_id,
            metadata.account_id,
        )
        .await?;
        if current.revision != metadata.revision {
            return Err(DbError::OptimisticConflict {
                entity: "passthrough binding".into(),
                id: id.to_string(),
            });
        }
        let current_account = Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
            [current.account_id.into(), metadata.owner_tenant_id.into()],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::not_found("account", current.account_id))?;
        let requested_global = req.is_global.unwrap_or(current.is_global);
        let requested_pool = req.pool_enabled.unwrap_or(current.pool_enabled);
        let requested_account = req.account_id.unwrap_or(current.account_id);
        let requested_tenant = req.tenant_id.unwrap_or(current.tenant_id);
        let requested_account_row = if requested_account == current.account_id {
            current_account.clone()
        } else {
            Account::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
                [requested_account.into(), requested_owner.into()],
            ))
            .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("account", requested_account))?
        };
        check_tenant_owner(
            scope,
            current.tenant_id,
            current_account.tenant_id,
            current.is_global,
        )?;
        let only_restricting = requested_account == current.account_id
            && requested_tenant == current.tenant_id
            && (!requested_global || current.is_global)
            && (!requested_pool || current.pool_enabled);
        if !only_restricting {
            validate_target(
                &tx,
                scope,
                requested_account_row,
                requested_tenant,
                requested_global,
            )
            .await?;
            ensure_no_overlap(
                &tx,
                requested_account,
                requested_tenant,
                requested_global,
                Some(id),
            )
            .await?;
        } else {
            check_tenant_owner(
                scope,
                requested_tenant,
                current_account.tenant_id,
                requested_global,
            )?;
        }
        let binding = PassthroughBinding::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE passthrough_bindings
             SET account_id=$1,tenant_id=$2,is_global=$3,pool_enabled=$4,
                 revision=revision+1,
                 updated_at=GREATEST(updated_at,statement_timestamp())+INTERVAL '1 microsecond'
             WHERE id=$5 AND tenant_id=$6 AND account_id=$7 AND revision=$8
             RETURNING *",
            [
                requested_account.into(),
                requested_tenant.into(),
                requested_global.into(),
                requested_pool.into(),
                id.into(),
                current.tenant_id.into(),
                current.account_id.into(),
                req.expected_revision.into(),
            ],
        ))
        .one(&tx)
        .await?
        .ok_or_else(|| DbError::OptimisticConflict {
            entity: "passthrough binding".into(),
            id: id.to_string(),
        })?;
        suppress_pool(&tx, current.account_id).await?;
        suppress_pool(&tx, requested_account).await?;
        audit(
            &tx,
            scope,
            &audit_ctx,
            "passthrough_binding.update",
            id,
            serde_json::json!({"account_id": binding.account_id, "tenant_id": binding.tenant_id, "revision": binding.revision}),
        )
        .await?;
        Ok::<_, DbError>(binding)
    }
    .await;
    match result {
        Ok(binding) => {
            tx.commit().await?;
            Ok(binding)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

async fn delete_scoped(
    db: &(impl ConnectionTrait + TransactionTrait),
    scope: Scope,
    id: Uuid,
    revision: i64,
    audit_ctx: &AuditContext,
    snapshot: ProviderAuthzSnapshot,
) -> Result<(), DbError> {
    validate_audit(scope, audit_ctx)?;
    if id.is_nil() || revision <= 0 {
        return Err(DbError::OptimisticConflict {
            entity: "passthrough binding".into(),
            id: id.to_string(),
        });
    }
    let tx = begin(db).await?;
    let result = async {
        let metadata = authorized_binding_metadata(&tx, scope, id).await?;
        let audit_ctx = lock_authority(
            &tx,
            scope,
            &[metadata.binding_tenant_id, metadata.owner_tenant_id],
            audit_ctx,
            snapshot,
        )
        .await?;
        let account = Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
            [metadata.account_id.into(), metadata.owner_tenant_id.into()],
        ))
        .one(&tx)
            .await?
            .ok_or_else(|| DbError::not_found("account", metadata.account_id))?;
        let current = binding_for_update(
            &tx,
            id,
            metadata.binding_tenant_id,
            metadata.account_id,
        )
        .await?;
        check_tenant_owner(
            scope,
            current.tenant_id,
            account.tenant_id,
            current.is_global,
        )?;
        if current.revision != metadata.revision {
            return Err(DbError::OptimisticConflict {
                entity: "passthrough binding".into(),
                id: id.to_string(),
            });
        }
        suppress_pool(&tx, current.account_id).await?;
        let deleted = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM passthrough_bindings
                 WHERE id=$1 AND tenant_id=$2 AND account_id=$3 AND revision=$4",
                [
                    id.into(),
                    current.tenant_id.into(),
                    current.account_id.into(),
                    revision.into(),
                ],
            ))
            .await?
            .rows_affected();
        if deleted != 1 {
            return Err(DbError::OptimisticConflict {
                entity: "passthrough binding".into(),
                id: id.to_string(),
            });
        }
        audit(
            &tx,
            scope,
            &audit_ctx,
            "passthrough_binding.delete",
            id,
            serde_json::json!({"account_id": current.account_id, "tenant_id": current.tenant_id, "revision": revision}),
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

async fn options(
    db: &impl ConnectionTrait,
    scope: Scope,
    search: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<(Vec<PassthroughAccountOption>, i64), DbError> {
    validate_read(scope)?;
    let search = search
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(escape_like_pattern);
    let (tenant, actor, authority) = match scope {
        Scope::Tenant(scope) => (
            Some(scope.tenant_id()),
            scope.user_id(),
            "a.tenant_id=$1 AND EXISTS(
                SELECT 1 FROM tenant_memberships m JOIN users u ON u.id=m.user_id
                JOIN tenants t ON t.id=m.tenant_id
                WHERE m.tenant_id=$1 AND m.user_id=$2 AND m.tenant_role='admin'
                  AND m.status='active' AND u.status='active' AND t.status='active')",
        ),
        Scope::Platform(scope) => (
            None,
            scope.user_id(),
            "EXISTS(SELECT 1 FROM users actor
                    WHERE actor.id=$2 AND actor.platform_role='root' AND actor.status='active')",
        ),
    };
    let from = format!(
        "FROM accounts a JOIN tenants t ON t.id=a.tenant_id
         WHERE a.enabled AND t.status='active' AND {authority}
           AND ((a.provider='openai' AND a.api_capabilities && ARRAY['chat_completions','responses']::TEXT[])
             OR (a.provider='anthropic' AND 'messages'=ANY(a.api_capabilities))
           ) AND ($3::text IS NULL OR a.name ILIKE '%'||$3||'%' ESCAPE '\\')"
    );
    #[derive(FromQueryResult)]
    struct Total {
        total: i64,
    }
    let total = Total::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!("SELECT COUNT(*)::BIGINT AS total {from}"),
        [tenant.into(), actor.into(), search.clone().into()],
    ))
    .one(db)
    .await?
    .map(|row| row.total)
    .unwrap_or(0);
    let rows = PassthroughAccountOption::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "SELECT a.id,a.name,a.provider,a.pool_enabled,a.models_supported AS models
             {from} ORDER BY a.name,a.id LIMIT $4 OFFSET $5"
        ),
        [
            tenant.into(),
            actor.into(),
            search.into(),
            limit.clamp(1, 200).into(),
            offset.max(0).into(),
        ],
    ))
    .all(db)
    .await?;
    Ok((rows, total))
}
