use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 上游 Provider 账号模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
}

/// 更新账号请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateAccountRequest {
    pub name: Option<String>,
    pub endpoint: Option<String>,
    pub upstream_api_key_encrypted: Option<String>,
    pub upstream_api_key_preview: Option<String>,
    pub rpm_limit: Option<i32>,
    pub tpm_limit: Option<i32>,
    pub priority: Option<i32>,
    pub enabled: Option<bool>,
    pub models_supported: Option<Vec<String>>,
}

impl Account {
    /// 创建新账号
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreateAccountRequest,
    ) -> Result<Account, sqlx::Error> {
        let account = sqlx::query_as::<_, Account>(
            r#"
            INSERT INTO accounts (
                tenant_id, provider, name, endpoint,
                upstream_api_key_encrypted, upstream_api_key_preview,
                rpm_limit, tpm_limit, priority, models_supported
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING *
            "#,
        )
        .bind(&req.tenant_id)
        .bind(&req.provider)
        .bind(&req.name)
        .bind(&req.endpoint)
        .bind(&req.upstream_api_key_encrypted)
        .bind(&req.upstream_api_key_preview)
        .bind(req.rpm_limit.unwrap_or(60))
        .bind(req.tpm_limit.unwrap_or(100000))
        .bind(req.priority.unwrap_or(0))
        .bind(&req.models_supported)
        .fetch_one(pool)
        .await?;

        Ok(account)
    }

    /// 根据 ID 查找账号
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<Account>, sqlx::Error> {
        let account = sqlx::query_as::<_, Account>("SELECT * FROM accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(account)
    }

    /// 查找租户的所有账号
    pub async fn find_by_tenant(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
    ) -> Result<Vec<Account>, sqlx::Error> {
        let accounts = sqlx::query_as::<_, Account>(
            "SELECT * FROM accounts WHERE tenant_id = $1 ORDER BY priority DESC, created_at ASC",
        )
        .bind(tenant_id)
        .fetch_all(pool)
        .await?;

        Ok(accounts)
    }

    /// 查找租户启用的账号
    pub async fn find_enabled_by_tenant(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
    ) -> Result<Vec<Account>, sqlx::Error> {
        let accounts = sqlx::query_as::<_, Account>(
            "SELECT * FROM accounts WHERE tenant_id = $1 AND enabled = TRUE ORDER BY priority DESC",
        )
        .bind(tenant_id)
        .fetch_all(pool)
        .await?;

        Ok(accounts)
    }

    /// 查找所有启用的账号（系统级，不限租户）
    pub async fn find_enabled_all(pool: &sqlx::PgPool) -> Result<Vec<Account>, sqlx::Error> {
        let accounts = sqlx::query_as::<_, Account>(
            "SELECT * FROM accounts WHERE enabled = TRUE ORDER BY priority DESC",
        )
        .fetch_all(pool)
        .await?;

        Ok(accounts)
    }

    /// 查找支持指定模型的账号
    pub async fn find_by_model(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
        model: &str,
    ) -> Result<Vec<Account>, sqlx::Error> {
        let accounts = sqlx::query_as::<_, Account>(
            r#"
            SELECT * FROM accounts
            WHERE tenant_id = $1
              AND enabled = TRUE
              AND $2 = ANY(models_supported)
            ORDER BY priority DESC
            "#,
        )
        .bind(tenant_id)
        .bind(model)
        .fetch_all(pool)
        .await?;

        Ok(accounts)
    }

    /// 更新账号
    pub async fn update(
        &self,
        pool: &sqlx::PgPool,
        req: &UpdateAccountRequest,
    ) -> Result<Account, sqlx::Error> {
        let account = sqlx::query_as::<_, Account>(
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
                updated_at = NOW()
            WHERE id = $10
            RETURNING *
            "#,
        )
        .bind(&req.name)
        .bind(&req.endpoint)
        .bind(&req.upstream_api_key_encrypted)
        .bind(&req.upstream_api_key_preview)
        .bind(req.rpm_limit)
        .bind(req.tpm_limit)
        .bind(req.priority)
        .bind(req.enabled)
        .bind(&req.models_supported)
        .bind(self.id)
        .fetch_one(pool)
        .await?;

        Ok(account)
    }

    /// 删除账号
    pub async fn delete(&self, pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM accounts WHERE id = $1")
            .bind(self.id)
            .execute(pool)
            .await?;

        Ok(())
    }
}
