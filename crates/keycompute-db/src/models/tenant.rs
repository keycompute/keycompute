use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 租户模型
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
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
    pub async fn create(
        db: &DatabaseConnection,
        req: &CreateTenantRequest,
    ) -> Result<Tenant, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO tenants (name, slug, description, default_rpm_limit, default_tpm_limit)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
            [
                req.name.as_str().into(),
                req.slug.as_str().into(),
                req.description.clone().into(),
                req.default_rpm_limit.unwrap_or(60).into(),
                req.default_tpm_limit.unwrap_or(100000).into(),
            ],
        );
        let tenant = Tenant::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(tenant)
    }

    /// 根据 ID 查找租户
    pub async fn find_by_id(db: &DatabaseConnection, id: Uuid) -> Result<Option<Tenant>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM tenants WHERE id = $1",
            [id.into()],
        );
        let tenant = Tenant::find_by_statement(stmt).one(db).await?;

        Ok(tenant)
    }

    /// 根据 slug 查找租户
    pub async fn find_by_slug(
        db: &DatabaseConnection,
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
    pub async fn find_all(db: &DatabaseConnection) -> Result<Vec<Tenant>, DbError> {
        let stmt = Statement::from_string(
            DbBackend::Postgres,
            "SELECT * FROM tenants ORDER BY created_at DESC".to_string(),
        );
        let tenants = Tenant::find_by_statement(stmt).all(db).await?;

        Ok(tenants)
    }

    /// 查找激活的租户
    pub async fn find_active(db: &DatabaseConnection) -> Result<Vec<Tenant>, DbError> {
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
        db: &DatabaseConnection,
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
                req.status.clone().into(),
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

    /// 删除租户
    pub async fn delete(&self, db: &DatabaseConnection) -> Result<(), DbError> {
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
