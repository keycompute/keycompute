use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 租户模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub status: String,
    /// 默认 RPM 限制
    pub default_rpm_limit: i32,
    /// 默认 TPM 限制
    pub default_tpm_limit: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    pub status: Option<String>,
    pub default_rpm_limit: Option<i32>,
    pub default_tpm_limit: Option<i32>,
}

impl Tenant {
    /// 创建新租户
    pub async fn create(pool: &sqlx::PgPool, req: &CreateTenantRequest) -> Result<Tenant, DbError> {
        let tenant = sqlx::query_as::<_, Tenant>(
            r#"
            INSERT INTO tenants (name, slug, description, default_rpm_limit, default_tpm_limit)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(&req.name)
        .bind(&req.slug)
        .bind(&req.description)
        .bind(req.default_rpm_limit.unwrap_or(60))
        .bind(req.default_tpm_limit.unwrap_or(100000))
        .fetch_one(pool)
        .await?;

        Ok(tenant)
    }

    /// 根据 ID 查找租户
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<Tenant>, DbError> {
        let tenant = sqlx::query_as::<_, Tenant>("SELECT * FROM tenants WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(tenant)
    }

    /// 根据 slug 查找租户
    pub async fn find_by_slug(pool: &sqlx::PgPool, slug: &str) -> Result<Option<Tenant>, DbError> {
        let tenant = sqlx::query_as::<_, Tenant>("SELECT * FROM tenants WHERE slug = $1")
            .bind(slug)
            .fetch_optional(pool)
            .await?;

        Ok(tenant)
    }

    /// 查找所有租户
    pub async fn find_all(pool: &sqlx::PgPool) -> Result<Vec<Tenant>, DbError> {
        let tenants = sqlx::query_as::<_, Tenant>("SELECT * FROM tenants ORDER BY created_at DESC")
            .fetch_all(pool)
            .await?;

        Ok(tenants)
    }

    /// 查找激活的租户
    pub async fn find_active(pool: &sqlx::PgPool) -> Result<Vec<Tenant>, DbError> {
        let tenants = sqlx::query_as::<_, Tenant>(
            "SELECT * FROM tenants WHERE status = 'active' ORDER BY created_at DESC",
        )
        .fetch_all(pool)
        .await?;

        Ok(tenants)
    }

    /// 更新租户
    pub async fn update(
        &self,
        pool: &sqlx::PgPool,
        req: &UpdateTenantRequest,
    ) -> Result<Tenant, DbError> {
        let tenant = sqlx::query_as::<_, Tenant>(
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
        )
        .bind(&req.name)
        .bind(&req.description)
        .bind(&req.status)
        .bind(req.default_rpm_limit)
        .bind(req.default_tpm_limit)
        .bind(self.id)
        .fetch_one(pool)
        .await?;

        Ok(tenant)
    }

    /// 删除租户
    pub async fn delete(&self, pool: &sqlx::PgPool) -> Result<(), DbError> {
        sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(self.id)
            .execute(pool)
            .await?;

        Ok(())
    }

    /// 检查租户是否激活
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}
