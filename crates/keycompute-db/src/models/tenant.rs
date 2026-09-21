use super::query::escape_like_pattern;
use super::tenant_audit_event::{AuditContext, TenantAuditEvent, lock_identity_admin};
use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{AuditResult, AuditScopeType, CredentialKind, TenantStatus};
use sea_orm::{
    ConnectionTrait, DatabaseTransaction, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 租户模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct Tenant {
    pub id: Uuid,
    pub owner_user_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    /// 默认 RPM 限制
    pub default_rpm_limit: i32,
    /// 默认 TPM 限制
    pub default_tpm_limit: i32,
    /// Internal safety counter for permanent Responses idempotency identities.
    #[serde(skip)]
    pub responses_idempotency_claim_count: i64,
    pub authz_version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct EntityCount {
    total: i64,
}

#[derive(Debug, FromQueryResult)]
struct TenantCount {
    total: i64,
}

/// Durable Responses work that must finish before a tenant can be removed.
///
/// These rows are intentionally tenant-scoped rather than user-scoped. A
/// moved user can leave an in-flight Responses request, an execution
/// reservation, or a terminal billing outbox behind in the source tenant, and
/// the corresponding foreign keys are cascading by design. Deleting that
/// tenant while any count is non-zero would silently discard work the
/// background worker still needs.
#[derive(Debug, Clone, Copy, FromQueryResult, PartialEq, Eq)]
pub struct TenantDeletionBlockers {
    pub pending_response_settlements: i64,
    pub pending_response_reservations: i64,
    pub in_progress_response_claims: i64,
}

impl TenantDeletionBlockers {
    pub fn blocks_deletion(self) -> bool {
        self.pending_response_settlements > 0
            || self.pending_response_reservations > 0
            || self.in_progress_response_claims > 0
    }
}

/// Financial rows that would otherwise be erased by the tenant's cascading
/// foreign keys. They are retained as historical records when a user moves
/// between tenants, so deleting the now-empty source tenant must be explicit
/// rather than silently destroying the audit trail.
#[derive(Debug, Clone, Copy, FromQueryResult, PartialEq, Eq)]
pub struct TenantFinancialDeletionBlockers {
    pub payment_orders: i64,
    pub balance_transactions: i64,
    pub balance_reservations: i64,
}

impl TenantFinancialDeletionBlockers {
    pub fn blocks_deletion(self) -> bool {
        self.payment_orders > 0 || self.balance_transactions > 0 || self.balance_reservations > 0
    }
}

/// 创建租户请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTenantRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    /// 默认 RPM 限制
    #[serde(default)]
    pub default_rpm_limit: Option<i32>,
    /// 默认 TPM 限制
    #[serde(default)]
    pub default_tpm_limit: Option<i32>,
}

/// 更新租户请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTenantRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<TenantStatus>,
    pub default_rpm_limit: Option<i32>,
    pub default_tpm_limit: Option<i32>,
}

const TENANT_PRICING_COUNT_SQL: &str =
    "SELECT COUNT(*)::BIGINT AS total FROM pricing_models WHERE tenant_id = $1";

// Keep the blocker snapshot and its row locks in one statement.  Locking the
// tenant row alone prevents new foreign-key inserts, but an existing affinity
// can still be updated from `settlement IS NULL` to a durable settlement while
// the delete transaction is checking it.  Materialized CTEs force PostgreSQL
// to lock every existing child row before the counts are observed; callers
// hold those locks through the subsequent tenant DELETE.
const TENANT_DELETION_BLOCKERS_SQL: &str = r#"
WITH locked_affinities AS MATERIALIZED (
    SELECT tenant_id, response_id, settlement, is_reservation
    FROM response_affinities
    WHERE tenant_id = $1
    FOR UPDATE
), locked_claims AS MATERIALIZED (
    SELECT tenant_id, binding_id, execution_state
    FROM responses_idempotency_claims
    WHERE tenant_id = $1
    FOR UPDATE
)
SELECT
    (SELECT COUNT(*)::BIGINT FROM locked_affinities WHERE settlement IS NOT NULL)
        AS pending_response_settlements,
    (SELECT COUNT(*)::BIGINT FROM locked_affinities WHERE is_reservation)
        AS pending_response_reservations,
    (SELECT COUNT(*)::BIGINT FROM locked_claims WHERE execution_state = 'in_progress')
        AS in_progress_response_claims
"#;

// `delete_in_tx` holds a FOR UPDATE lock on the tenant before running this
// snapshot. The three tables all carry tenant foreign keys, so that parent
// lock prevents a new financial row from passing its FK check concurrently;
// unlike Responses rows, historical financial rows do not need child locks or
// a second lock order here.
const TENANT_FINANCIAL_DELETION_BLOCKERS_SQL: &str = r#"
SELECT
    (SELECT COUNT(*)::BIGINT FROM payment_orders WHERE tenant_id = $1)
        AS payment_orders,
    (SELECT COUNT(*)::BIGINT FROM balance_transactions WHERE tenant_id = $1)
        AS balance_transactions,
    (SELECT COUNT(*)::BIGINT FROM balance_reservations WHERE tenant_id = $1)
        AS balance_reservations
"#;

// Account rows use `ON DELETE RESTRICT` for their tenant foreign key, so the
// final tenant DELETE may need a key-share check on every account. Acquire
// those child locks immediately after the parent lock and before Responses
// affinity locks; account lifecycle operations use the same tenant -> account
// -> affinity order.
const TENANT_ACCOUNT_LOCK_SQL: &str =
    "SELECT id FROM accounts WHERE tenant_id = $1 ORDER BY id FOR UPDATE";

impl Tenant {
    /// Create an organization and its owner membership in one transaction.
    /// System credentials are only accepted for self-owned bootstrap/registration.
    pub async fn create_owned(
        tx: &DatabaseTransaction,
        req: &CreateTenantRequest,
        owner_user_id: Uuid,
        actor: &AuditContext,
    ) -> Result<Tenant, DbError> {
        lock_identity_admin(tx).await?;
        if owner_user_id.is_nil()
            || req.name.trim().is_empty()
            || req.name.len() > 255
            || req.slug.is_empty()
            || req.slug.len() > 100
            || !req
                .slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
            || req.default_rpm_limit.is_some_and(|limit| limit < 0)
            || req.default_tpm_limit.is_some_and(|limit| limit < 0)
        {
            return Err(DbError::Other(
                "invalid tenant owner, name, slug or limits".into(),
            ));
        }
        let actor = if actor.actor_user_id == owner_user_id
            && matches!(
                actor.credential_kind,
                CredentialKind::System | CredentialKind::Jwt
            ) {
            let owner = super::user::User::find_by_id(tx, owner_user_id)
                .await?
                .filter(|user| user.status == "active")
                .ok_or_else(|| DbError::Other("active tenant owner required".into()))?;
            AuditContext {
                actor_platform_role: owner.platform_role()?,
                actor_tenant_role: None,
                ..*actor
            }
        } else {
            actor.require_root(tx).await?
        };
        super::user::User::find_by_id_for_no_key_update(tx, owner_user_id)
            .await?
            .filter(|user| user.status == "active")
            .ok_or_else(|| DbError::Other("active tenant owner required".into()))?;
        let tenant = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO tenants(owner_user_id,name,slug,description,default_rpm_limit,default_tpm_limit) VALUES($1,$2,$3,$4,COALESCE($5,60),COALESCE($6,100000)) RETURNING *",
            [owner_user_id.into(), req.name.trim().into(), req.slug.as_str().into(),
                req.description.clone().into(), req.default_rpm_limit.into(), req.default_tpm_limit.into()],
        )).one(tx).await?.ok_or_else(|| DbError::Other("tenant insert returned no row".into()))?;
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO tenant_memberships(tenant_id,user_id,role,status) VALUES($1,$2,'admin','active')",
            [tenant.id.into(), owner_user_id.into()],
        )).await?;
        TenantAuditEvent::append(
            tx,
            AuditScopeType::Tenant,
            Some(tenant.id),
            &actor,
            "tenant.create",
            "tenant",
            Some(&tenant.id.to_string()),
            AuditResult::Success,
            serde_json::json!({"owner_user_id":owner_user_id}),
        )
        .await?;
        Ok(tenant)
    }

    /// The existing owner transfers ownership only to another active admin.
    pub async fn transfer_ownership(
        tx: &DatabaseTransaction,
        tenant_id: Uuid,
        new_owner: Uuid,
        actor: &AuditContext,
    ) -> Result<Tenant, DbError> {
        lock_identity_admin(tx).await?;
        let actor = actor.require_tenant_admin(tx, tenant_id).await?;
        let tenant = Self::find_by_id(tx, tenant_id)
            .await?
            .ok_or_else(|| DbError::not_found("Tenant", tenant_id))?;
        if tenant.owner_user_id != actor.actor_user_id {
            return Err(DbError::Other(
                "only the tenant owner may transfer ownership".into(),
            ));
        }
        super::tenant_membership::TenantMembership::find(tx, tenant_id, new_owner)
            .await?
            .filter(|member| member.role == "admin")
            .ok_or_else(|| DbError::Other("new owner must be an active administrator".into()))?;
        let updated = Self::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET owner_user_id=$2 WHERE id=$1 RETURNING *",
            [tenant_id.into(), new_owner.into()],
        ))
        .one(tx)
        .await?
        .ok_or_else(|| DbError::not_found("Tenant", tenant_id))?;
        TenantAuditEvent::append(tx, AuditScopeType::Tenant, Some(tenant_id), &actor,
            "tenant.transfer_owner", "tenant", Some(&tenant_id.to_string()), AuditResult::Success,
            serde_json::json!({"previous_owner_user_id":tenant.owner_user_id,"owner_user_id":new_owner})).await?;
        Ok(updated)
    }

    /// 根据 ID 查找租户
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Tenant>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenants WHERE id = $1",
            [id.into()],
        );
        let tenant = Tenant::find_by_statement(stmt).one(db).await?;

        Ok(tenant)
    }

    /// 根据 ID 查找并锁定租户，供租户级别的并发管理操作使用。
    pub async fn find_by_id_for_update(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Tenant>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenants WHERE id = $1 FOR UPDATE",
            [id.into()],
        );
        Ok(Tenant::find_by_statement(stmt).one(db).await?)
    }

    /// 根据 ID 查找并取得租户外键的 `KEY SHARE` 锁。
    ///
    /// Responses 预留和其它租户子表写入必须在锁定账号或 affinity 之前
    /// 取得该锁；这样租户删除（先锁父行再锁子行）不会与子表写入形成反向
    /// 等待环。`KEY SHARE` 允许普通租户配置更新，但会与删除冲突。
    pub async fn find_by_id_for_key_share(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Tenant>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenants WHERE id = $1 FOR KEY SHARE",
            [id.into()],
        );
        Ok(Tenant::find_by_statement(stmt).one(db).await?)
    }

    /// 统计租户下的用户数。
    pub async fn count_users(db: &impl ConnectionTrait, tenant_id: Uuid) -> Result<i64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT AS total FROM tenant_memberships WHERE tenant_id = $1 AND status = 'active'",
            [tenant_id.into()],
        );
        Ok(EntityCount::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| row.total)
            .unwrap_or(0))
    }

    /// 统计租户下的渠道账号数。
    pub async fn count_accounts(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<i64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT AS total FROM accounts WHERE tenant_id = $1",
            [tenant_id.into()],
        );
        Ok(EntityCount::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| row.total)
            .unwrap_or(0))
    }

    /// 统计租户下仍在使用的租户级定价配置。
    ///
    /// 定价模型不使用级联删除；删除租户前必须显式阻止该操作，避免
    /// 留下无法归属的配置记录。
    pub async fn count_pricing_models(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<i64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            TENANT_PRICING_COUNT_SQL,
            [tenant_id.into()],
        );
        Ok(EntityCount::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| row.total)
            .unwrap_or(0))
    }

    /// Count durable Responses work which would be lost by the tenant's
    /// cascading foreign keys. Callers that are deciding whether to delete a
    /// tenant should run this after acquiring a `FOR UPDATE` lock on the
    /// tenant row so new inserts cannot pass the check concurrently.
    pub async fn find_deletion_blockers(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<TenantDeletionBlockers, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            TENANT_DELETION_BLOCKERS_SQL,
            [tenant_id.into()],
        );
        TenantDeletionBlockers::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("tenant deletion blocker query returned no row".into()))
    }

    /// Count financial history which would be removed by a cascading tenant
    /// delete. Callers must already hold the tenant's `FOR UPDATE` lock; that
    /// parent lock serializes all new rows carrying the tenant foreign key.
    pub async fn find_financial_deletion_blockers(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<TenantFinancialDeletionBlockers, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            TENANT_FINANCIAL_DELETION_BLOCKERS_SQL,
            [tenant_id.into()],
        );
        TenantFinancialDeletionBlockers::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| {
                DbError::Other("tenant financial deletion blocker query returned no row".into())
            })
    }

    /// 根据 slug 查找租户
    pub async fn find_by_slug(
        db: &impl ConnectionTrait,
        slug: &str,
    ) -> Result<Option<Tenant>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenants WHERE slug = $1",
            [slug.into()],
        );
        let tenant = Tenant::find_by_statement(stmt).one(db).await?;

        Ok(tenant)
    }

    /// 查找所有租户
    pub async fn find_all(db: &impl ConnectionTrait) -> Result<Vec<Tenant>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM tenants ORDER BY created_at DESC".to_string(),
        );
        let tenants = Tenant::find_by_statement(stmt).all(db).await?;

        Ok(tenants)
    }

    /// 分页查找租户，供管理面使用。
    pub async fn find_all_filtered(
        db: &impl ConnectionTrait,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Tenant>, DbError> {
        let search = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(escape_like_pattern);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM tenants
            WHERE ($1::text IS NULL
                OR LOWER(name) LIKE '%' || LOWER($1) || '%' ESCAPE '\'
                OR LOWER(slug) LIKE '%' || LOWER($1) || '%' ESCAPE '\'
                OR LOWER(id::text) LIKE '%' || LOWER($1) || '%' ESCAPE '\')
            ORDER BY created_at DESC, id DESC
            LIMIT $2 OFFSET $3
            "#,
            [search.as_deref().into(), limit.into(), offset.into()],
        );
        Ok(Tenant::find_by_statement(stmt).all(db).await?)
    }

    /// 统计管理面过滤后的租户数量。
    pub async fn count_all_filtered(
        db: &impl ConnectionTrait,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        let search = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(escape_like_pattern);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT COUNT(*)::BIGINT AS total FROM tenants
            WHERE ($1::text IS NULL
                OR LOWER(name) LIKE '%' || LOWER($1) || '%' ESCAPE '\'
                OR LOWER(slug) LIKE '%' || LOWER($1) || '%' ESCAPE '\'
                OR LOWER(id::text) LIKE '%' || LOWER($1) || '%' ESCAPE '\')
            "#,
            [search.as_deref().into()],
        );
        Ok(TenantCount::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| row.total)
            .unwrap_or(0))
    }

    /// 查找激活的租户
    pub async fn find_active(db: &impl ConnectionTrait) -> Result<Vec<Tenant>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM tenants WHERE status = 'active' ORDER BY created_at DESC".to_string(),
        );
        let tenants = Tenant::find_by_statement(stmt).all(db).await?;

        Ok(tenants)
    }

    /// 更新租户
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateTenantRequest,
    ) -> Result<Tenant, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE tenants
            SET name = COALESCE($1, name),
                description = COALESCE($2, description),
                status = COALESCE($3, status),
                default_rpm_limit = COALESCE($4, default_rpm_limit),
                default_tpm_limit = COALESCE($5, default_tpm_limit),
                updated_at = NOW()
            WHERE id = $6
            RETURNING *
            "#,
            [
                req.name.clone().into(),
                req.description.clone().into(),
                req.status.map(|status| status.as_str()).into(),
                req.default_rpm_limit.into(),
                req.default_tpm_limit.into(),
                self.id.into(),
            ],
        );
        let tenant = Tenant::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("update failed to return row".to_string()))?;

        Ok(tenant)
    }

    /// Delete a tenant in its own writer transaction.
    ///
    /// The Responses blocker query takes row locks which must remain held
    /// until the `DELETE` statement. Starting a transaction here keeps direct
    /// model callers (including `DbRouter` callers) from releasing those locks
    /// between the check and the cascade. If the caller already owns a
    /// transaction, use [`Self::delete_in_tx`] so the lock and delete remain in
    /// that transaction instead of creating an unnecessary savepoint.
    pub async fn delete(
        &self,
        db: &(impl ConnectionTrait + TransactionTrait),
    ) -> Result<(), DbError> {
        let tx = db.begin().await?;
        match self.delete_in_tx(&tx).await {
            Ok(()) => tx.commit().await.map_err(DbError::from),
            Err(error) => {
                // Preserve the domain error (for example a pending-work
                // blocker) while still releasing locks before returning.
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    }

    /// Delete a tenant using an already-open transaction.
    ///
    /// The transaction must lock the tenant row before calling this method
    /// when concurrent inserts need to be excluded. This method acquires that
    /// lock itself, so callers cannot accidentally release the parent lock
    /// between the blocker check and the cascade. The child-row locks acquired
    /// by [`Self::find_deletion_blockers`] are then held through the delete.
    pub async fn delete_in_tx(&self, db: &DatabaseTransaction) -> Result<(), DbError> {
        lock_identity_admin(db).await?;
        // Serialize the existence check, child-row snapshot, and cascade with
        // tenant-scoped inserts that acquire a foreign-key key-share lock.
        // Keep the historical no-op behavior when the tenant row is already
        // absent; the DELETE below will simply affect zero rows.
        Self::find_by_id_for_update(db, self.id).await?;
        // Lock account children before the Responses blocker snapshot. An
        // account update/delete holds the account row before its affinity rows;
        // taking affinities first here would let the final tenant FK check wait
        // on the account while that updater waits on the affinity (a cycle).
        db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            TENANT_ACCOUNT_LOCK_SQL,
            [self.id.into()],
        ))
        .await?;
        let pricing_count = Self::count_pricing_models(db, self.id).await?;
        if pricing_count > 0 {
            return Err(DbError::TenantHasPricingModels {
                count: pricing_count,
            });
        }
        let financial = Self::find_financial_deletion_blockers(db, self.id).await?;
        if financial.blocks_deletion() {
            return Err(DbError::TenantHasFinancialHistory {
                payment_orders: financial.payment_orders,
                balance_transactions: financial.balance_transactions,
                balance_reservations: financial.balance_reservations,
            });
        }
        let blockers = Self::find_deletion_blockers(db, self.id).await?;
        if blockers.blocks_deletion() {
            return Err(DbError::TenantHasPendingResponsesWork {
                pending_settlements: blockers.pending_response_settlements,
                pending_reservations: blockers.pending_response_reservations,
                in_progress_claims: blockers.in_progress_response_claims,
            });
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM tenants WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 检查租户是否激活
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TENANT_DELETION_BLOCKERS_SQL, TENANT_FINANCIAL_DELETION_BLOCKERS_SQL,
        TENANT_PRICING_COUNT_SQL, TenantDeletionBlockers, TenantFinancialDeletionBlockers,
    };

    #[test]
    fn pricing_count_is_scoped_to_the_tenant() {
        assert!(TENANT_PRICING_COUNT_SQL.contains("pricing_models"));
        assert!(TENANT_PRICING_COUNT_SQL.contains("tenant_id = $1"));
    }

    #[test]
    fn deletion_blockers_detect_durable_responses_work() {
        assert!(
            !TenantDeletionBlockers {
                pending_response_settlements: 0,
                pending_response_reservations: 0,
                in_progress_response_claims: 0,
            }
            .blocks_deletion()
        );
        assert!(
            TenantDeletionBlockers {
                pending_response_settlements: 1,
                pending_response_reservations: 0,
                in_progress_response_claims: 0,
            }
            .blocks_deletion()
        );
        assert!(
            TenantDeletionBlockers {
                pending_response_settlements: 0,
                pending_response_reservations: 1,
                in_progress_response_claims: 0,
            }
            .blocks_deletion()
        );
        assert!(
            TenantDeletionBlockers {
                pending_response_settlements: 0,
                pending_response_reservations: 0,
                in_progress_response_claims: 1,
            }
            .blocks_deletion()
        );
    }

    #[test]
    fn financial_deletion_blockers_detect_retained_history() {
        assert!(
            !TenantFinancialDeletionBlockers {
                payment_orders: 0,
                balance_transactions: 0,
                balance_reservations: 0,
            }
            .blocks_deletion()
        );
        assert!(
            TenantFinancialDeletionBlockers {
                payment_orders: 1,
                balance_transactions: 0,
                balance_reservations: 0,
            }
            .blocks_deletion()
        );
        assert!(
            TenantFinancialDeletionBlockers {
                payment_orders: 0,
                balance_transactions: 1,
                balance_reservations: 0,
            }
            .blocks_deletion()
        );
        assert!(
            TenantFinancialDeletionBlockers {
                payment_orders: 0,
                balance_transactions: 0,
                balance_reservations: 1,
            }
            .blocks_deletion()
        );
    }

    #[test]
    fn financial_deletion_blocker_query_is_parent_lock_scoped() {
        assert!(TENANT_FINANCIAL_DELETION_BLOCKERS_SQL.contains("payment_orders"));
        assert!(TENANT_FINANCIAL_DELETION_BLOCKERS_SQL.contains("balance_transactions"));
        assert!(TENANT_FINANCIAL_DELETION_BLOCKERS_SQL.contains("balance_reservations"));
        assert!(!TENANT_FINANCIAL_DELETION_BLOCKERS_SQL.contains("FOR UPDATE"));
    }

    #[test]
    fn deletion_blocker_query_locks_existing_child_rows() {
        assert!(TENANT_DELETION_BLOCKERS_SQL.contains("MATERIALIZED"));
        assert_eq!(
            TENANT_DELETION_BLOCKERS_SQL.matches("FOR UPDATE").count(),
            2
        );
        assert!(TENANT_DELETION_BLOCKERS_SQL.contains("response_affinities"));
        assert!(TENANT_DELETION_BLOCKERS_SQL.contains("is_reservation"));
        assert!(TENANT_DELETION_BLOCKERS_SQL.contains("responses_idempotency_claims"));
    }
}
