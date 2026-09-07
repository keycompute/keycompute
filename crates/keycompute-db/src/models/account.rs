use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub models_supported: Vec<String>,
    pub api_capabilities: Vec<String>,
    /// 可见性：'tenant' = 仅本租户可见（默认），'global' = 所有租户可见
    pub visibility: String,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_probe_latency_ms: Option<i64>,
    pub last_probe_status: Option<String>,
    pub last_probe_error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
}

impl Account {
    /// 创建新账号
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateAccountRequest,
    ) -> Result<Account, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO accounts (
                tenant_id, provider, name, endpoint,
                upstream_api_key_encrypted, upstream_api_key_preview,
                rpm_limit, tpm_limit, priority, models_supported, api_capabilities, visibility
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
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
            "SELECT * FROM accounts WHERE id = $1 FOR KEY SHARE",
            [id.into()],
        );
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
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM accounts
            WHERE tenant_id = $1
              AND visibility = 'tenant'
              AND enabled = TRUE
              AND LOWER(provider) = LOWER($2)
              AND api_capabilities @> ARRAY[$3]::TEXT[]
            ORDER BY priority DESC, created_at ASC
            LIMIT $4
            "#,
            [
                tenant_id.into(),
                provider.into(),
                api_capability.into(),
                limit.into(),
            ],
        );
        Ok(Account::find_by_statement(stmt).all(db).await?)
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

    /// 查找租户启用的账号（含本租户 + 全局可见）
    pub async fn find_enabled_by_tenant(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
    ) -> Result<Vec<Account>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE (tenant_id = $1 OR visibility = 'global') AND enabled = TRUE ORDER BY priority DESC",
            [tenant_id.into()],
        );
        let accounts = Account::find_by_statement(stmt).all(db).await?;

        Ok(accounts)
    }

    /// 查找所有启用的账号（系统级，不限租户）
    pub async fn find_enabled_all(db: &impl ConnectionTrait) -> Result<Vec<Account>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM accounts WHERE enabled = TRUE ORDER BY priority DESC".to_string(),
        );
        let accounts = Account::find_by_statement(stmt).all(db).await?;

        Ok(accounts)
    }

    /// Persist a health probe only if the account configuration has not changed
    /// since the probe started.
    ///
    /// Probe telemetry deliberately does not modify `updated_at`: that column is
    /// the optimistic version for account configuration, while concurrent probes
    /// of the same version may safely use completion order for the latest health
    /// snapshot.
    pub async fn record_probe_snapshot_if_config_current(
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
                   WHERE id=$5 AND updated_at=$6"#,
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

    /// 查找支持指定模型的账号（含本租户 + 全局可见）
    pub async fn find_by_model(
        db: &impl ConnectionTrait,
        tenant_id: Uuid,
        model: &str,
        api_capability: &str,
    ) -> Result<Vec<Account>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            SELECT * FROM accounts
            WHERE (tenant_id = $1 OR visibility = 'global')
              AND enabled = TRUE
              AND $2 = ANY(models_supported)
              AND api_capabilities @> ARRAY[$3]::TEXT[]
            ORDER BY priority DESC
            "#,
            [tenant_id.into(), model.into(), api_capability.into()],
        );
        let accounts = Account::find_by_statement(stmt).all(db).await?;

        Ok(accounts)
    }

    /// 更新账号
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateAccountRequest,
    ) -> Result<Account, DbError> {
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
                models_supported = COALESCE($9, models_supported),
                api_capabilities = COALESCE($10, api_capabilities),
                visibility = COALESCE($11, visibility),
                tenant_id = COALESCE($12, tenant_id),
                updated_at = NOW()
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
