use crate::DbError;
use chrono::{DateTime, Utc};
use keycompute_types::{AssignableUserRole, UserRole};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 用户模型
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建用户请求
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUserRequest {
    pub tenant_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: Option<UserRole>,
}

/// 更新用户请求
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUserRequest {
    pub name: Option<String>,
    pub role: Option<AssignableUserRole>,
}

impl User {
    /// 创建新用户
    pub async fn create(pool: &sqlx::PgPool, req: &CreateUserRequest) -> Result<User, DbError> {
        let user = sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (tenant_id, email, name, role)
            VALUES ($1, $2, $3, COALESCE($4, 'user'))
            RETURNING *
            "#,
        )
        .bind(req.tenant_id)
        .bind(&req.email)
        .bind(&req.name)
        .bind(req.role.as_ref().map(|role| role.as_str()))
        .fetch_one(pool)
        .await?;

        Ok(user)
    }

    /// 根据 ID 查找用户
    pub async fn find_by_id(pool: &sqlx::PgPool, id: Uuid) -> Result<Option<User>, DbError> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;

        Ok(user)
    }

    /// 根据邮箱查找用户
    pub async fn find_by_email(pool: &sqlx::PgPool, email: &str) -> Result<Option<User>, DbError> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE email = $1")
            .bind(email)
            .fetch_optional(pool)
            .await?;

        Ok(user)
    }

    /// 查找租户下的所有用户
    pub async fn find_by_tenant(
        pool: &sqlx::PgPool,
        tenant_id: Uuid,
    ) -> Result<Vec<User>, DbError> {
        let users = sqlx::query_as::<_, User>("SELECT * FROM users WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_all(pool)
            .await?;

        Ok(users)
    }

    /// 查找所有用户（Admin 全局查询）
    ///
    /// 支持分页，按创建时间倒序排列
    pub async fn find_all(
        pool: &sqlx::PgPool,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<User>, DbError> {
        let users = sqlx::query_as::<_, User>(
            r#"
            SELECT * FROM users
            ORDER BY created_at DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

        Ok(users)
    }

    /// 统计用户总数
    pub async fn count_all(pool: &sqlx::PgPool) -> Result<i64, DbError> {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
            .fetch_one(pool)
            .await?;

        Ok(count.0)
    }

    /// 更新用户
    pub async fn update(
        &self,
        pool: &sqlx::PgPool,
        req: &UpdateUserRequest,
    ) -> Result<User, DbError> {
        let user = sqlx::query_as::<_, User>(
            r#"
            UPDATE users
            SET name = COALESCE($1, name),
                role = COALESCE($2, role),
                updated_at = NOW()
            WHERE id = $3
            RETURNING *
            "#,
        )
        .bind(&req.name)
        .bind(req.role.as_ref().map(|role| role.as_str()))
        .bind(self.id)
        .fetch_one(pool)
        .await?;

        Ok(user)
    }

    /// 删除用户
    pub async fn delete(&self, pool: &sqlx::PgPool) -> Result<(), DbError> {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(self.id)
            .execute(pool)
            .await?;

        Ok(())
    }
}
