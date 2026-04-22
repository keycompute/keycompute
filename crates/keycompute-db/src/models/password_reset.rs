//! 密码重置模型
//!
//! 管理用户密码重置流程

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// 密码重置记录
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PasswordReset {
    pub id: Uuid,
    pub user_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub used: bool,
    pub used_at: Option<DateTime<Utc>>,
    pub requested_from_ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// 创建密码重置请求
#[derive(Debug, Clone)]
pub struct CreatePasswordResetRequest {
    pub user_id: Uuid,
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub requested_from_ip: Option<String>,
}

impl PasswordReset {
    /// 创建新重置记录
    pub async fn create(
        pool: &sqlx::PgPool,
        req: &CreatePasswordResetRequest,
    ) -> Result<PasswordReset, DbError> {
        let reset = sqlx::query_as::<_, PasswordReset>(
            r#"
            INSERT INTO password_resets (user_id, token, expires_at, requested_from_ip)
            VALUES ($1, $2, $3, $4::INET)
            RETURNING
                id,
                user_id,
                token,
                expires_at,
                used,
                used_at,
                requested_from_ip::TEXT AS requested_from_ip,
                created_at
            "#,
        )
        .bind(req.user_id)
        .bind(&req.token)
        .bind(req.expires_at)
        .bind(&req.requested_from_ip)
        .fetch_one(pool)
        .await?;

        Ok(reset)
    }

    /// 根据 ID 查找
    pub async fn find_by_id(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> Result<Option<PasswordReset>, DbError> {
        let reset = sqlx::query_as::<_, PasswordReset>(
            r#"
                SELECT
                    id,
                    user_id,
                    token,
                    expires_at,
                    used,
                    used_at,
                    requested_from_ip::TEXT AS requested_from_ip,
                    created_at
                FROM password_resets
                WHERE id = $1
                "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;

        Ok(reset)
    }

    /// 根据令牌查找
    pub async fn find_by_token(
        pool: &sqlx::PgPool,
        token: &str,
    ) -> Result<Option<PasswordReset>, DbError> {
        let reset = sqlx::query_as::<_, PasswordReset>(
            r#"
                SELECT
                    id,
                    user_id,
                    token,
                    expires_at,
                    used,
                    used_at,
                    requested_from_ip::TEXT AS requested_from_ip,
                    created_at
                FROM password_resets
                WHERE token = $1
                "#,
        )
        .bind(token)
        .fetch_optional(pool)
        .await?;

        Ok(reset)
    }

    /// 查找用户的有效重置记录
    pub async fn find_valid_by_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<Option<PasswordReset>, DbError> {
        let reset = sqlx::query_as::<_, PasswordReset>(
            r#"
            SELECT
                id,
                user_id,
                token,
                expires_at,
                used,
                used_at,
                requested_from_ip::TEXT AS requested_from_ip,
                created_at
            FROM password_resets
            WHERE user_id = $1 
            AND used = FALSE 
            AND expires_at > NOW()
            ORDER BY created_at DESC 
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await?;

        Ok(reset)
    }

    /// 标记为已使用
    pub async fn mark_used(&self, pool: &sqlx::PgPool) -> Result<PasswordReset, DbError> {
        let reset = sqlx::query_as::<_, PasswordReset>(
            r#"
            UPDATE password_resets
            SET used = TRUE,
                used_at = NOW()
            WHERE id = $1
            RETURNING
                id,
                user_id,
                token,
                expires_at,
                used,
                used_at,
                requested_from_ip::TEXT AS requested_from_ip,
                created_at
            "#,
        )
        .bind(self.id)
        .fetch_one(pool)
        .await?;

        Ok(reset)
    }

    /// 删除重置记录
    pub async fn delete(&self, pool: &sqlx::PgPool) -> Result<(), DbError> {
        sqlx::query("DELETE FROM password_resets WHERE id = $1")
            .bind(self.id)
            .execute(pool)
            .await?;

        Ok(())
    }

    /// 删除用户的所有重置记录
    pub async fn delete_all_by_user(pool: &sqlx::PgPool, user_id: Uuid) -> Result<u64, DbError> {
        let result = sqlx::query("DELETE FROM password_resets WHERE user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await?;

        Ok(result.rows_affected())
    }

    /// 检查令牌是否有效（未使用且未过期）
    pub fn is_valid(&self) -> bool {
        !self.used && self.expires_at > Utc::now()
    }

    /// 检查令牌是否过期
    pub fn is_expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_password_reset_is_valid() {
        let reset = PasswordReset {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            token: "reset_token".to_string(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            used: false,
            used_at: None,
            requested_from_ip: Some("192.168.1.1".to_string()),
            created_at: Utc::now(),
        };

        assert!(reset.is_valid());
        assert!(!reset.is_expired());
    }

    #[test]
    fn test_password_reset_used() {
        let reset = PasswordReset {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            token: "reset_token".to_string(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            used: true,
            used_at: Some(Utc::now()),
            requested_from_ip: None,
            created_at: Utc::now(),
        };

        assert!(!reset.is_valid());
    }

    #[test]
    fn test_password_reset_expired() {
        let reset = PasswordReset {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            token: "reset_token".to_string(),
            expires_at: Utc::now() - chrono::Duration::minutes(1),
            used: false,
            used_at: None,
            requested_from_ip: Some("192.168.1.1".to_string()),
            created_at: Utc::now(),
        };

        assert!(!reset.is_valid());
        assert!(reset.is_expired());
    }
}
