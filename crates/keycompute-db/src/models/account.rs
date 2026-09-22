use super::query::escape_like_pattern;
use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[path = "account_scope.rs"]
mod scope;
pub use scope::{
    AccountListFilter, AccountManagementScope, AccountManagementView, ProviderAuthzSnapshot,
};

pub const ACCOUNT_PRIORITY_MIN: i32 = 0;
pub const ACCOUNT_PRIORITY_MAX: i32 = 10;
pub const ACCOUNT_RATE_LIMIT_MIN: i32 = 1;

const ACTIVE_ACCOUNT_KEY_SHARE_SQL: &str = "SELECT accounts.* FROM accounts JOIN tenants ON tenants.id = accounts.tenant_id WHERE accounts.id = $1 AND tenants.status = 'active' FOR KEY SHARE OF accounts";
const ACCOUNT_KEY_SHARE_SQL: &str = "SELECT * FROM accounts WHERE id = $1 FOR KEY SHARE";

fn validate_priority(priority: Option<i32>) -> Result<(), DbError> {
    if let Some(priority) = priority
        && !(ACCOUNT_PRIORITY_MIN..=ACCOUNT_PRIORITY_MAX).contains(&priority)
    {
        return Err(DbError::Other(format!(
            "account priority must be between {ACCOUNT_PRIORITY_MIN} and {ACCOUNT_PRIORITY_MAX}"
        )));
    }
    Ok(())
}

fn validate_rate_limits(rpm_limit: Option<i32>, tpm_limit: Option<i32>) -> Result<(), DbError> {
    if let Some(rpm_limit) = rpm_limit
        && rpm_limit < ACCOUNT_RATE_LIMIT_MIN
    {
        return Err(DbError::Other(format!(
            "account rpm limit must be at least {ACCOUNT_RATE_LIMIT_MIN}"
        )));
    }
    if let Some(tpm_limit) = tpm_limit
        && tpm_limit < ACCOUNT_RATE_LIMIT_MIN
    {
        return Err(DbError::Other(format!(
            "account tpm limit must be at least {ACCOUNT_RATE_LIMIT_MIN}"
        )));
    }
    Ok(())
}

/// 上游 Provider 账号模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider: String,
    pub name: String,
    pub endpoint: String,
    pub upstream_api_key_encrypted: String,
    pub upstream_api_key_preview: String,
    pub rpm_limit: i32,
    pub tpm_limit: i32,
    pub priority: i32,
    pub enabled: bool,
    pub pool_enabled: bool,
    pub models_supported: Vec<String>,
    pub api_capabilities: Vec<String>,
    /// 可见性：'tenant' = 仅本租户可见（默认），'global' = 所有租户可见
    pub visibility: String,
    /// 账号级运行健康状态，独立于管理员 `enabled` 开关。
    pub health_status: String,
    pub health_reason: Option<String>,
    pub health_penalty: i32,
    pub health_consecutive_failures: i32,
    pub health_success_count: i64,
    pub health_failure_count: i64,
    pub health_avg_latency_ms: Option<i64>,
    pub health_last_success_at: Option<DateTime<Utc>>,
    pub health_last_failure_at: Option<DateTime<Utc>>,
    pub health_updated_at: DateTime<Utc>,
    pub health_generation: i64,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_probe_latency_ms: Option<i64>,
    pub last_probe_status: Option<String>,
    pub last_probe_error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub upstream_config_version: DateTime<Utc>,
}

#[derive(Debug, FromQueryResult)]
struct AccountCount {
    total: i64,
}

/// 创建账号请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateAccountRequest {
    pub tenant_id: Uuid,
    pub provider: String,
    pub name: String,
    pub endpoint: String,
    pub upstream_api_key_encrypted: String,
    pub upstream_api_key_preview: String,
    pub rpm_limit: Option<i32>,
    pub tpm_limit: Option<i32>,
    pub priority: Option<i32>,
    pub models_supported: Vec<String>,
    pub api_capabilities: Vec<String>,
    pub visibility: Option<String>,
    pub pool_enabled: Option<bool>,
}

/// 更新账号请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAccountRequest {
    pub tenant_id: Option<Uuid>,
    pub name: Option<String>,
    pub endpoint: Option<String>,
    pub upstream_api_key_encrypted: Option<String>,
    pub upstream_api_key_preview: Option<String>,
    pub rpm_limit: Option<i32>,
    pub tpm_limit: Option<i32>,
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub models_supported: Option<Vec<String>>,
    pub api_capabilities: Option<Vec<String>>,
    pub visibility: Option<String>,
    pub pool_enabled: Option<bool>,
}

impl Account {
    /// 创建新账号
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateAccountRequest,
    ) -> Result<Account, DbError> {
        validate_priority(req.priority)?;
        validate_rate_limits(req.rpm_limit, req.tpm_limit)?;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO accounts (
                tenant_id, provider, name, endpoint,
                upstream_api_key_encrypted, upstream_api_key_preview,
                rpm_limit, tpm_limit, priority, models_supported, api_capabilities, visibility, pool_enabled
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING *
            "#,
            [
                req.tenant_id.into(),
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
        );
        let account = Account::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(account)
    }

    /// 根据 ID 查找账号
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Account>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE id = $1",
            [id.into()],
        );
        let account = Account::find_by_statement(stmt).one(db).await?;

        Ok(account)
    }

    /// Load and lock an account on the writer for a destructive operation.
    /// The lock prevents a new Responses affinity from acquiring its foreign-
    /// key key-share lock while account deletion drains existing routes.
    pub async fn find_by_id_for_update(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Account>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE id = $1 FOR UPDATE",
            [id.into()],
        );
        Ok(Account::find_by_statement(stmt).one(db).await?)
    }

    /// Load an account from the writer for an authorization- or
    /// ownership-sensitive operation without taking an exclusive row lock.
    pub async fn find_by_id_for_key_share(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Account>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            ACTIVE_ACCOUNT_KEY_SHARE_SQL,
            [id.into()],
        );
        Ok(Account::find_by_statement(stmt).one(db).await?)
    }

    /// Load and lock an account for a background settlement that was already
    /// authorized and dispatched before the owning tenant became inactive.
    ///
    /// This deliberately does not apply the active-tenant filter used by
    /// [`find_by_id_for_key_share`].  The account row is still key-share
    /// locked so account deletion cannot race with settlement finalization.
    pub async fn find_by_id_for_key_share_any_tenant(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Account>, DbError> {
        let stmt =
            Statement::from_sql_and_values(DbBackend::Postgres, ACCOUNT_KEY_SHARE_SQL, [id.into()]);
        Ok(Account::find_by_statement(stmt).one(db).await?)
    }

    /// 查找租户的所有账号（仅本租户，管理面使用）
    pub async fn find_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<Account>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE tenant_id = $1 ORDER BY priority DESC, created_at ASC",
            [tenant_id.into()],
        );
        let accounts = Account::find_by_statement(stmt).all(db).await?;

        Ok(accounts)
    }

    /// Load a bounded set of tenant-private accounts that can be probed to
    /// discover the owner of an imported protocol resource.
    ///
    /// Shared/global accounts are deliberately excluded: probing them with a
    /// tenant-supplied resource ID could expose or attach another tenant's
    /// upstream resource. The caller may request one extra row to determine
    /// whether its probe budget truncated the eligible set.
    pub async fn find_tenant_discovery_candidates(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        provider: &str,
        api_capability: &str,
        limit: u64,
    ) -> Result<Vec<Account>, DbError> {
        let policy = super::upstream_access::non_pt_predicate("a", "$1");
        // Discovery is limited to consumer-owned private accounts: it probes
        // opaque resources, not global accounts belonging to another consumer.
        Ok(Account::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            format!("SELECT a.* FROM accounts a WHERE a.tenant_id=$1 AND a.visibility='tenant' AND NOT EXISTS (SELECT 1 FROM passthrough_bindings shared_grant WHERE shared_grant.account_id=a.id AND (shared_grant.is_global OR shared_grant.tenant_id<>$1)) AND {policy} AND LOWER(a.provider)=LOWER($2) AND $3=ANY(a.api_capabilities) ORDER BY a.priority DESC,a.id LIMIT $4"),
            [tenant_id.into(),provider.into(),api_capability.into(),i64::try_from(limit).unwrap_or(i64::MAX).into()])).all(db).await?)
    }

    /// 查找所有账号（不限租户，Admin 管理面使用）
    pub async fn find_all(db: &impl ConnectionTrait) -> Result<Vec<Account>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM accounts ORDER BY priority DESC, created_at ASC".to_string(),
        );
        let accounts = Account::find_by_statement(stmt).all(db).await?;

        Ok(accounts)
    }

    /// 分页查找所有租户的账号，供 Admin 管理面使用。
    pub async fn find_all_filtered(
        db: &impl ConnectionTrait,
        provider: Option<&str>,
        enabled: Option<bool>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Account>, DbError> {
        let search = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(escape_like_pattern);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM accounts
            WHERE ($1::text IS NULL OR LOWER(provider) = LOWER($1))
              AND ($2::boolean IS NULL OR enabled = $2)
              AND ($3::text IS NULL
                OR LOWER(name) LIKE '%' || LOWER($3) || '%' ESCAPE '\'
                OR LOWER(provider) LIKE '%' || LOWER($3) || '%' ESCAPE '\'
                OR LOWER(id::text) LIKE '%' || LOWER($3) || '%' ESCAPE '\'
                OR LOWER(tenant_id::text) LIKE '%' || LOWER($3) || '%' ESCAPE '\')
            ORDER BY priority DESC, created_at ASC, id ASC
            LIMIT $4 OFFSET $5
            "#,
            [
                provider.into(),
                enabled.into(),
                search.as_deref().into(),
                limit.into(),
                offset.into(),
            ],
        );
        Ok(Account::find_by_statement(stmt).all(db).await?)
    }

    /// 统计 Admin 管理面过滤后的账号数量。
    pub async fn count_all_filtered(
        db: &impl ConnectionTrait,
        provider: Option<&str>,
        enabled: Option<bool>,
        search: Option<&str>,
    ) -> Result<i64, DbError> {
        let search = search
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(escape_like_pattern);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT COUNT(*)::BIGINT AS total FROM accounts
            WHERE ($1::text IS NULL OR LOWER(provider) = LOWER($1))
              AND ($2::boolean IS NULL OR enabled = $2)
              AND ($3::text IS NULL
                OR LOWER(name) LIKE '%' || LOWER($3) || '%' ESCAPE '\'
                OR LOWER(provider) LIKE '%' || LOWER($3) || '%' ESCAPE '\'
                OR LOWER(id::text) LIKE '%' || LOWER($3) || '%' ESCAPE '\'
                OR LOWER(tenant_id::text) LIKE '%' || LOWER($3) || '%' ESCAPE '\')
            "#,
            [provider.into(), enabled.into(), search.as_deref().into()],
        );
        Ok(AccountCount::find_by_statement(stmt)
            .one(db)
            .await?
            .map(|row| row.total)
            .unwrap_or(0))
    }

    /// 批量统计各租户的渠道账号数量，供租户管理列表使用。
    pub async fn count_by_tenants(
        db: &impl ConnectionTrait,
        tenant_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, i64>, DbError> {
        #[derive(FromQueryResult)]
        struct TenantAccountCount {
            tenant_id: Uuid,
            count: i64,
        }

        if tenant_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT tenant_id, COUNT(*)::BIGINT AS count
               FROM accounts
               WHERE tenant_id = ANY($1)
               GROUP BY tenant_id"#,
            [tenant_ids.to_vec().into()],
        );
        let rows: Vec<TenantAccountCount> =
            TenantAccountCount::find_by_statement(stmt).all(db).await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.tenant_id, row.count))
            .collect())
    }

    /// 查找租户启用的账号（含本租户 + 全局可见）
    pub async fn find_enabled_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<Account>, DbError> {
        let policy = super::upstream_access::non_pt_predicate("a", "$1");
        Ok(Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT a.* FROM accounts a WHERE {policy} ORDER BY a.priority DESC,a.id"),
            [tenant_id.into()],
        ))
        .all(db)
        .await?)
    }

    /// 查找所有启用的账号（系统级，不限租户）
    pub async fn find_enabled_all(db: &impl ConnectionTrait) -> Result<Vec<Account>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT accounts.* FROM accounts JOIN tenants ON tenants.id = accounts.tenant_id WHERE tenants.status = 'active' AND accounts.enabled = TRUE ORDER BY accounts.priority DESC".to_string(),
        );
        let accounts = Account::find_by_statement(stmt).all(db).await?;

        Ok(accounts)
    }

    /// Persist a health probe only if the account configuration has not changed
    /// since the probe started.
    ///
    /// Probe telemetry does not modify the dedicated `upstream_config_version`.
    /// Ordinary labels and pool options do not invalidate it. The expected health
    /// timestamp is a second compare-and-swap token, so a late probe cannot
    /// overwrite a newer live transition or administrator reset.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_probe_snapshot_if_config_current(
        db: &impl ConnectionTrait,
        id: Uuid,
        expected_updated_at: DateTime<Utc>,
        expected_health_updated_at: DateTime<Utc>,
        expected_health_generation: i64,
        probed_at: DateTime<Utc>,
        latency_ms: i64,
        status: &str,
        error_code: Option<&str>,
        health_status: &str,
        health_reason: Option<&str>,
        health_penalty: i32,
        health_consecutive_failures: i32,
        health_success_count: i64,
        health_failure_count: i64,
        health_avg_latency_ms: Option<i64>,
        health_last_success_at: Option<DateTime<Utc>>,
        health_last_failure_at: Option<DateTime<Utc>>,
    ) -> Result<bool, DbError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"UPDATE accounts
                   SET last_probe_at=$1,last_probe_latency_ms=$2,
                       last_probe_status=$3,last_probe_error_code=$4,
                       health_status=$5,health_reason=$6,health_penalty=$7,
                       health_consecutive_failures=$8,health_success_count=$9,
                       health_failure_count=$10,health_avg_latency_ms=$11,
                       health_last_success_at=$12,health_last_failure_at=$13,
                       health_updated_at=GREATEST(health_updated_at,$1),
                       health_generation=health_generation+1
                   WHERE id=$14 AND upstream_config_version=$15 AND health_updated_at=$16
                     AND health_generation=$17"#,
                [
                    probed_at.into(),
                    latency_ms.into(),
                    status.into(),
                    error_code.into(),
                    health_status.into(),
                    health_reason.into(),
                    health_penalty.into(),
                    health_consecutive_failures.into(),
                    health_success_count.into(),
                    health_failure_count.into(),
                    health_avg_latency_ms.into(),
                    health_last_success_at.into(),
                    health_last_failure_at.into(),
                    id.into(),
                    expected_updated_at.into(),
                    expected_health_updated_at.into(),
                    expected_health_generation.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Persist probe telemetry without changing the operational health
    /// snapshot. This is used when a probe finishes after a live transition.
    pub async fn record_probe_telemetry_if_config_current(
        db: &impl ConnectionTrait,
        id: Uuid,
        expected_updated_at: DateTime<Utc>,
        probed_at: DateTime<Utc>,
        latency_ms: i64,
        status: &str,
        error_code: Option<&str>,
    ) -> Result<bool, DbError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"UPDATE accounts
                   SET last_probe_at=$1,last_probe_latency_ms=$2,
                       last_probe_status=$3,last_probe_error_code=$4
                   WHERE id=$5 AND upstream_config_version=$6"#,
                [
                    probed_at.into(),
                    latency_ms.into(),
                    status.into(),
                    error_code.into(),
                    id.into(),
                    expected_updated_at.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Atomically persist one successful runtime request.
    ///
    /// Runtime health is emitted as an event rather than as a complete
    /// in-memory snapshot. This lets concurrent server replicas increment the
    /// counters against the current row instead of overwriting each other's
    /// counters with stale values. The health generation fences events that
    /// were queued before a newer probe/reset, while all events in the same
    /// generation are merged atomically.
    pub async fn record_runtime_success(
        db: &impl ConnectionTrait,
        id: Uuid,
        expected_health_generation: i64,
        latency_ms: i64,
        occurred_at: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"UPDATE accounts
                   SET health_status='healthy',health_reason=NULL,health_penalty=0,
                       health_consecutive_failures=0,
                       health_success_count=health_success_count+1,
                       health_avg_latency_ms=CASE
                           WHEN health_avg_latency_ms IS NULL THEN $2
                           ELSE ((health_avg_latency_ms * 7) + ($2 * 3)) / 10
                       END,
                       health_last_success_at=CASE
                           WHEN health_last_success_at IS NULL OR health_last_success_at < $3
                               THEN $3 ELSE health_last_success_at END,
                       health_updated_at=GREATEST(health_updated_at, $3) + INTERVAL '1 microsecond'
                   WHERE id=$1 AND health_generation=$4"#,
                [
                    id.into(),
                    latency_ms.max(0).into(),
                    occurred_at.into(),
                    expected_health_generation.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Atomically persist one failed runtime request.
    ///
    /// Counter increments and status transitions are atomic within the current
    /// health generation. Events queued before a newer probe/reset are ignored
    /// by the generation fence.
    pub async fn record_runtime_failure(
        db: &impl ConnectionTrait,
        id: Uuid,
        expected_health_generation: i64,
        reason: &str,
        hard_failure: bool,
        occurred_at: DateTime<Utc>,
    ) -> Result<bool, DbError> {
        let result = db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"UPDATE accounts
                   SET health_status=CASE
                           WHEN $3 OR health_consecutive_failures + 1 >= 3
                               THEN 'unhealthy'
                           ELSE 'degraded'
                       END,
                       health_reason=$2,
                       health_penalty=CASE
                           WHEN $3 OR health_consecutive_failures + 1 >= 3
                               THEN 100
                           ELSE LEAST(health_penalty + 35, 99)
                       END,
                       health_consecutive_failures=health_consecutive_failures + 1,
                       health_failure_count=health_failure_count+1,
                       health_last_failure_at=CASE
                           WHEN health_last_failure_at IS NULL OR health_last_failure_at < $4
                               THEN $4 ELSE health_last_failure_at END,
                       health_updated_at=GREATEST(health_updated_at, $4) + INTERVAL '1 microsecond'
                   WHERE id=$1 AND health_generation=$5"#,
                [
                    id.into(),
                    reason.into(),
                    hard_failure.into(),
                    occurred_at.into(),
                    expected_health_generation.into(),
                ],
            ))
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Reset one account's persisted operational health without changing its
    /// administrator-controlled enabled flag or priority.
    pub async fn reset_health(db: &impl ConnectionTrait, id: Uuid) -> Result<(), DbError> {
        Self::reset_health_snapshot(db, id).await.map(|_| ())
    }

    /// Reset one account's persisted operational health and return the writer
    /// snapshot that was actually committed. Callers that keep an in-memory
    /// mirror must use this value rather than reconstructing `NOW()` locally:
    /// the health CAS token is an exact database timestamp.
    pub async fn reset_health_snapshot(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<Account>, DbError> {
        let account = Account::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE accounts
               SET health_status='unknown',health_reason=NULL,health_penalty=0,
                   health_consecutive_failures=0,health_success_count=0,
                   health_failure_count=0,health_avg_latency_ms=NULL,
                   health_last_success_at=NULL,health_last_failure_at=NULL,
                   health_updated_at=GREATEST(health_updated_at, NOW()) + INTERVAL '1 microsecond',
                   health_generation=health_generation+1
               WHERE id=$1
               RETURNING *"#,
            [id.into()],
        ))
        .one(db)
        .await?;
        Ok(account)
    }

    /// Reset all persisted account health snapshots and return the exact
    /// writer snapshots produced by PostgreSQL.
    pub async fn reset_all_health_snapshot(
        db: &impl ConnectionTrait,
    ) -> Result<Vec<Account>, DbError> {
        let accounts = Account::find_by_statement(Statement::from_string(
            DbBackend::Postgres,
            r#"UPDATE accounts
               SET health_status='unknown',health_reason=NULL,health_penalty=0,
                   health_consecutive_failures=0,health_success_count=0,
                   health_failure_count=0,health_avg_latency_ms=NULL,
                   health_last_success_at=NULL,health_last_failure_at=NULL,
                   health_updated_at=GREATEST(health_updated_at, NOW()) + INTERVAL '1 microsecond',
                   health_generation=health_generation+1
               RETURNING *"#
                .to_string(),
        ))
        .all(db)
        .await?;
        Ok(accounts)
    }

    /// Reset all persisted account health snapshots without returning rows.
    pub async fn reset_all_health(db: &impl ConnectionTrait) -> Result<(), DbError> {
        db.execute(Statement::from_string(
            DbBackend::Postgres,
            r#"UPDATE accounts
               SET health_status='unknown',health_reason=NULL,health_penalty=0,
                   health_consecutive_failures=0,health_success_count=0,
                   health_failure_count=0,health_avg_latency_ms=NULL,
                   health_last_success_at=NULL,health_last_failure_at=NULL,
                   health_updated_at=GREATEST(health_updated_at, NOW()) + INTERVAL '1 microsecond',
                   health_generation=health_generation+1"#
                .to_string(),
        ))
        .await?;
        Ok(())
    }

    /// 查找支持指定模型的账号（含本租户 + 全局可见）
    pub async fn find_by_model(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        model: &str,
        api_capability: &str,
    ) -> Result<Vec<Account>, DbError> {
        let policy = super::upstream_access::non_pt_predicate("a", "$1");
        Ok(Account::find_by_statement(Statement::from_sql_and_values(DbBackend::Postgres,
            format!("SELECT a.* FROM accounts a WHERE {policy} AND $2=ANY(a.models_supported) AND $3=ANY(a.api_capabilities) ORDER BY a.priority DESC,a.id"),
            [tenant_id.into(),model.into(),api_capability.into()])).all(db).await?)
    }

    /// Shared authorization predicate for all non-passthrough account paths,
    /// including Responses affinity/resource/background work.
    pub async fn authorize_non_pt(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        account_id: Uuid,
    ) -> Result<bool, DbError> {
        let policy = super::upstream_access::non_pt_predicate("a", "$1");
        let row = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            db.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!("SELECT 1 FROM accounts a WHERE a.id=$2 AND {policy}"),
                [tenant_id.into(), account_id.into()],
            )),
        )
        .await
        .map_err(|_| DbError::Other("account access lookup timed out".into()))??;
        Ok(row.is_some())
    }

    /// 更新账号
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateAccountRequest,
    ) -> Result<Account, DbError> {
        if req.pool_enabled.is_some()
            && db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT 1 FROM passthrough_bindings WHERE account_id=$1 LIMIT 1",
                    [self.id.into()],
                ))
                .await?
                .is_some()
        {
            return Err(DbError::Other(
                "account pool participation is managed by passthrough bindings".into(),
            ));
        }
        validate_priority(req.priority)?;
        validate_rate_limits(req.rpm_limit, req.tpm_limit)?;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE accounts
            SET name = COALESCE($1, name),
                endpoint = COALESCE($2, endpoint),
                upstream_api_key_encrypted = COALESCE($3, upstream_api_key_encrypted),
                upstream_api_key_preview = COALESCE($4, upstream_api_key_preview),
                rpm_limit = COALESCE($5, rpm_limit),
                tpm_limit = COALESCE($6, tpm_limit),
                priority = COALESCE($7, priority),
                enabled = COALESCE($8, enabled),
                pool_enabled = COALESCE($14, pool_enabled),
                models_supported = COALESCE($9, models_supported),
                api_capabilities = COALESCE($10, api_capabilities),
                visibility = COALESCE($11, visibility),
                -- Fence runtime health events that were started against the
                -- previous account configuration. The health snapshot itself
                -- is preserved until a probe or runtime result replaces it.
                health_updated_at = CASE WHEN COALESCE($2,endpoint) IS DISTINCT FROM endpoint
                    OR COALESCE($3,upstream_api_key_encrypted) IS DISTINCT FROM upstream_api_key_encrypted
                    OR COALESCE($9,models_supported) IS DISTINCT FROM models_supported
                    OR COALESCE($10,api_capabilities) IS DISTINCT FROM api_capabilities
                    THEN GREATEST(health_updated_at,statement_timestamp())+INTERVAL '1 microsecond'
                    ELSE health_updated_at END,
                health_generation = health_generation + CASE WHEN COALESCE($2,endpoint) IS DISTINCT FROM endpoint
                    OR COALESCE($3,upstream_api_key_encrypted) IS DISTINCT FROM upstream_api_key_encrypted
                    OR COALESCE($9,models_supported) IS DISTINCT FROM models_supported
                    OR COALESCE($10,api_capabilities) IS DISTINCT FROM api_capabilities THEN 1 ELSE 0 END,
                tenant_id = COALESCE($12, tenant_id),
                upstream_config_version = CASE WHEN
                    COALESCE($2,endpoint) IS DISTINCT FROM endpoint
                    OR COALESCE($3,upstream_api_key_encrypted) IS DISTINCT FROM upstream_api_key_encrypted
                    OR COALESCE($9,models_supported) IS DISTINCT FROM models_supported
                    OR COALESCE($10,api_capabilities) IS DISTINCT FROM api_capabilities
                    THEN GREATEST(upstream_config_version,statement_timestamp())+INTERVAL '1 microsecond'
                    ELSE upstream_config_version END,
                updated_at = GREATEST(updated_at, NOW()) + INTERVAL '1 microsecond'
            WHERE id = $13
            RETURNING *
            "#,
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
                req.tenant_id.into(),
                self.id.into(),
                req.pool_enabled.into(),
            ],
        );
        let account = Account::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("update failed to return row".to_string()))?;

        Ok(account)
    }

    /// 删除账号
    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM accounts WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_priority_accepts_only_zero_through_ten() {
        assert!(validate_priority(None).is_ok());
        assert!(validate_priority(Some(ACCOUNT_PRIORITY_MIN)).is_ok());
        assert!(validate_priority(Some(ACCOUNT_PRIORITY_MAX)).is_ok());
        assert!(validate_priority(Some(-1)).is_err());
        assert!(validate_priority(Some(11)).is_err());
    }

    #[test]
    fn background_key_share_lookup_is_not_blocked_by_tenant_status() {
        assert!(ACTIVE_ACCOUNT_KEY_SHARE_SQL.contains("tenants.status = 'active'"));
        assert!(!ACCOUNT_KEY_SHARE_SQL.contains("tenants.status"));
        assert!(ACCOUNT_KEY_SHARE_SQL.contains("FOR KEY SHARE"));
    }
}
