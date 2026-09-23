//! Platform identity/lifecycle entrypoints with current signed root authority.
use crate::{
    AuditContext, CreateTenantRequest, DbError, Tenant, TenantAuditEvent, UpdateTenantRequest,
    User,
    models::{financial_scope::FinancialScope, tenant_audit_event::lock_identity_admin},
};
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType, PlatformRole, UserStatus};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromQueryResult)]
pub struct PlatformUserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub platform_role: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformTenantInfo {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub user_count: i64,
    pub account_count: i64,
    pub status: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Default)]
pub struct PlatformUserPatch {
    pub name: Option<String>,
    pub platform_role: Option<PlatformRole>,
    pub status: Option<UserStatus>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformPage<T> {
    pub items: Vec<T>,
    pub total: i64,
}
pub struct PlatformIdentity;
const USER_COLUMNS: &str = "u.id,u.email,u.name,u.platform_role,u.status,u.created_at,u.updated_at,(SELECT c.last_login_at FROM user_credentials c WHERE c.user_id=u.id) AS last_login_at";
const TENANT_COLUMNS: &str = "t.id,t.name,t.slug,t.description,t.status,(t.status='active') AS is_active,t.created_at,t.updated_at,(SELECT COUNT(*)::bigint FROM tenant_memberships m WHERE m.tenant_id=t.id) AS user_count,(SELECT COUNT(*)::bigint FROM accounts a WHERE a.tenant_id=t.id) AS account_count";
fn invalid() -> DbError {
    DbError::Other("platform_identity_request_invalid".into())
}
fn denied() -> DbError {
    DbError::Other("financial_authority_invalid".into())
}
fn page(limit: i64, offset: i64, search: Option<&str>) -> Result<&str, DbError> {
    if !(1..=1000).contains(&limit)
        || !(0..=100_000_000).contains(&offset)
        || search.is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(invalid());
    }
    Ok(search.unwrap_or("").trim())
}
fn reason(value: &str) -> Result<&str, DbError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 1000 || value.chars().any(char::is_control) {
        Err(invalid())
    } else {
        Ok(value)
    }
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
async fn begin(
    db: &(impl ConnectionTrait + TransactionTrait),
) -> Result<DatabaseTransaction, DbError> {
    let tx = db.begin().await?;
    tx.execute_unprepared("SET LOCAL lock_timeout='3s'; SET LOCAL statement_timeout='10s'")
        .await?;
    Ok(tx)
}
async fn change_audit(
    tx: &DatabaseTransaction,
    actor: &AuditContext,
    action: &str,
    resource: &str,
    id: Uuid,
    why: &str,
    metadata: serde_json::Value,
) -> Result<(), DbError> {
    let mut metadata = metadata;
    metadata["reason"] = why.into();
    TenantAuditEvent::append(
        tx,
        AuditScopeType::Platform,
        None,
        actor,
        action,
        resource,
        Some(&id.to_string()),
        AuditResult::Success,
        metadata,
    )
    .await?;
    Ok(())
}
impl PlatformIdentity {
    pub async fn users(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        role: Option<PlatformRole>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<PlatformPage<PlatformUserInfo>, DbError> {
        scope.require_root_global()?;
        let search = page(limit, offset, search)?;
        let mut values = scope.values();
        values.extend([
            role.map(|r| r.as_str()).into(),
            search.into(),
            limit.into(),
            offset.into(),
        ]);
        let sql = format!(
            "WITH authority AS MATERIALIZED(SELECT 1 WHERE {}),visible AS MATERIALIZED(SELECT {USER_COLUMNS} FROM users u CROSS JOIN authority WHERE ($10::text IS NULL OR u.platform_role=$10) AND ($11='' OR strpos(lower(u.email||' '||COALESCE(u.name,'')),lower($11))>0)),page AS (SELECT * FROM visible ORDER BY created_at DESC,id DESC LIMIT $12 OFFSET $13) SELECT (SELECT COUNT(*)::bigint FROM visible) AS total,(SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.created_at DESC,p.id DESC),'[]'::jsonb) FROM page p) AS items FROM authority",
            scope.predicate()
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(denied)?;
        Ok(PlatformPage {
            items: serde_json::from_value(row.try_get("", "items")?).map_err(|_| invalid())?,
            total: row.try_get("", "total")?,
        })
    }
    pub async fn user(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        id: Uuid,
    ) -> Result<PlatformUserInfo, DbError> {
        scope.require_root_global()?;
        if id.is_nil() {
            return Err(invalid());
        }
        let mut values = scope.values();
        values.push(id.into());
        let sql = format!(
            "SELECT {USER_COLUMNS} FROM users u WHERE u.id=$10 AND {}",
            scope.predicate()
        );
        PlatformUserInfo::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            values,
        ))
        .one(db)
        .await?
        .ok_or_else(|| DbError::not_found("User", id))
    }
    pub async fn tenants(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<PlatformPage<PlatformTenantInfo>, DbError> {
        scope.require_root_global()?;
        let search = page(limit, offset, search)?;
        let mut values = scope.values();
        values.extend([search.into(), limit.into(), offset.into()]);
        let sql = format!(
            "WITH authority AS MATERIALIZED(SELECT 1 WHERE {}),visible AS MATERIALIZED(SELECT t.id,t.name,t.slug,t.description,t.status,t.created_at,t.updated_at FROM tenants t CROSS JOIN authority WHERE ($10='' OR strpos(lower(t.name||' '||t.slug),lower($10))>0)),selected AS (SELECT * FROM visible ORDER BY created_at DESC,id DESC LIMIT $11 OFFSET $12),page AS (SELECT {TENANT_COLUMNS} FROM selected t) SELECT (SELECT COUNT(*)::bigint FROM visible) AS total,(SELECT COALESCE(jsonb_agg(to_jsonb(p) ORDER BY p.created_at DESC,p.id DESC),'[]'::jsonb) FROM page p) AS items FROM authority",
            scope.predicate()
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(denied)?;
        Ok(PlatformPage {
            items: serde_json::from_value(row.try_get("", "items")?).map_err(|_| invalid())?,
            total: row.try_get("", "total")?,
        })
    }
    pub async fn tenant(
        db: &impl ConnectionTrait,
        scope: FinancialScope,
        id: Uuid,
    ) -> Result<PlatformTenantInfo, DbError> {
        scope.require_root_global()?;
        if id.is_nil() {
            return Err(invalid());
        }
        let mut values = scope.values();
        values.push(id.into());
        let sql = format!(
            "WITH target AS (SELECT {TENANT_COLUMNS} FROM tenants t WHERE t.id=$10 AND {}) SELECT to_jsonb(target) AS item FROM target",
            scope.predicate()
        );
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await?
            .ok_or_else(|| DbError::not_found("Tenant", id))?;
        serde_json::from_value(row.try_get("", "item")?).map_err(|_| invalid())
    }
    pub async fn update_user(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        id: Uuid,
        patch: &PlatformUserPatch,
        why: &str,
    ) -> Result<PlatformUserInfo, DbError> {
        scope.require_root_global()?;
        let why = reason(why)?;
        if id.is_nil()
            || patch
                .name
                .as_ref()
                .is_some_and(|n| n.len() > 255 || n.contains('\0'))
        {
            return Err(invalid());
        }
        let tx = begin(db).await?;
        let result=async {
            lock_identity_admin(&tx).await?;scope.current_actor(&tx).await?;
            let parents=tx.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id FROM tenants WHERE owner_user_id=$1 ORDER BY id",[id.into()])).await?.into_iter().map(|r|r.try_get("","id").map_err(DbError::from)).collect::<Result<Vec<Uuid>,_>>()?;
            let actor=scope.lock_related(&tx,audit,&parents,&[id]).await?;
            let before=Self::user(&tx,scope,id).await?;
            let role=patch.platform_role.unwrap_or(before.platform_role.parse().map_err(DbError::Other)?);
            let status=patch.status.unwrap_or(before.status.parse().map_err(DbError::Other)?);
            let user=User::set_security(&tx,id,role,status,&actor).await?;
            let user=user.update_in_tx(&tx,&crate::UpdateUserRequest{name:patch.name.clone()}).await?;
            change_audit(&tx,&actor,"user.update","user",id,why,serde_json::json!({"before":{"platform_role":before.platform_role,"status":before.status},"after":{"platform_role":user.platform_role,"status":user.status},"name_changed":patch.name.is_some()})).await?;
            // This command may intentionally invalidate its own selected context
            // or role. All authority rows remain locked; only wall-clock expiry
            // can change independently after the authorized mutation starts.
            scope.check_expiry()?;
            Ok(PlatformUserInfo{id:user.id,email:user.email,name:user.name,platform_role:user.platform_role,status:user.status,created_at:user.created_at,updated_at:user.updated_at,last_login_at:before.last_login_at})
        }.await;
        finish(tx, result).await
    }
    pub async fn delete_user(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        id: Uuid,
    ) -> Result<(), DbError> {
        scope.require_root_global()?;
        if id.is_nil() || id == scope.user_id() {
            return Err(invalid());
        }
        let tx = begin(db).await?;
        let result = async {
            lock_identity_admin(&tx).await?;
            scope.current_actor(&tx).await?;
            let parents = tx
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id FROM tenants WHERE owner_user_id=$1 ORDER BY id",
                    [id.into()],
                ))
                .await?
                .into_iter()
                .map(|r| r.try_get("", "id").map_err(DbError::from))
                .collect::<Result<Vec<Uuid>, _>>()?;
            let actor = scope.lock_related(&tx, audit, &parents, &[id]).await?;
            let current = Self::user(&tx, scope, id).await?;
            let user = User::set_security(
                &tx,
                id,
                current.platform_role.parse().map_err(DbError::Other)?,
                UserStatus::Suspended,
                &actor,
            )
            .await?;
            user.delete(&tx).await?;
            change_audit(
                &tx,
                &actor,
                "user.delete",
                "user",
                id,
                "root requested identity deletion",
                serde_json::json!({}),
            )
            .await?;
            scope.check_expiry()?;
            Ok(())
        }
        .await;
        finish(tx, result).await
    }
    pub async fn create_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        request: &CreateTenantRequest,
        owner: Uuid,
    ) -> Result<PlatformTenantInfo, DbError> {
        scope.require_root_global()?;
        if owner.is_nil() {
            return Err(invalid());
        }
        let tx = begin(db).await?;
        let result = async {
            let actor = scope.lock_related(&tx, audit, &[], &[owner]).await?;
            let tenant = Tenant::create_owned(&tx, request, owner, &actor).await?;
            change_audit(
                &tx,
                &actor,
                "tenant.create",
                "tenant",
                tenant.id,
                "root created tenant",
                serde_json::json!({"owner_user_id":owner}),
            )
            .await?;
            let result = Self::tenant(&tx, scope, tenant.id).await?;
            scope.check_expiry()?;
            Ok(result)
        }
        .await;
        finish(tx, result).await
    }
    pub async fn update_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        id: Uuid,
        request: &UpdateTenantRequest,
    ) -> Result<PlatformTenantInfo, DbError> {
        scope.require_root_global()?;
        if id.is_nil() {
            return Err(invalid());
        }
        let tx = begin(db).await?;
        let result=async {
            let actor=scope.lock_related(&tx,audit,&[id],&[]).await?;
            let before=Tenant::find_by_id_for_update(&tx,id).await?.ok_or_else(||DbError::not_found("Tenant",id))?;
            if before.slug=="default" && request.status.is_some_and(|s|s.as_str()=="inactive"){return Err(DbError::Other("protected_default_tenant".into()));}
            let current=before.update(&tx,request).await?;
            change_audit(&tx,&actor,"tenant.update","tenant",id,"root updated tenant",serde_json::json!({"previous_status":before.status,"status":current.status,"authz_version":current.authz_version})).await?;
            // The authorized target may be the selected tenant; construct its
            // safe result directly rather than trying to renew an invalid token.
            let result=PlatformTenantInfo{id:current.id,name:current.name,slug:current.slug,description:current.description,user_count:Tenant::count_users(&tx,id).await?,account_count:Tenant::count_accounts(&tx,id).await?,is_active:current.status=="active",status:current.status,created_at:current.created_at,updated_at:current.updated_at};
            scope.check_expiry()?;Ok(result)
        }.await;
        finish(tx, result).await
    }
    pub async fn delete_tenant(
        db: &(impl ConnectionTrait + TransactionTrait),
        scope: FinancialScope,
        audit: &AuditContext,
        id: Uuid,
    ) -> Result<(), DbError> {
        scope.require_root_global()?;
        if id.is_nil() {
            return Err(invalid());
        }
        let tx = begin(db).await?;
        let result = async {
            let actor = scope.lock_related(&tx, audit, &[id], &[]).await?;
            let tenant = Tenant::find_by_id_for_update(&tx, id)
                .await?
                .ok_or_else(|| DbError::not_found("Tenant", id))?;
            if tenant.slug == "default" {
                return Err(DbError::Other("protected_default_tenant".into()));
            }
            if Tenant::count_users(&tx, id).await? > 1 || Tenant::count_accounts(&tx, id).await? > 0
            {
                return Err(DbError::Other("tenant_retained_members_or_accounts".into()));
            }
            tenant.delete_in_tx(&tx).await?;
            change_audit(
                &tx,
                &actor,
                "tenant.delete",
                "tenant",
                id,
                "root deleted empty tenant",
                serde_json::json!({}),
            )
            .await?;
            scope.check_expiry()?;
            Ok(())
        }
        .await;
        finish(tx, result).await
    }
}
