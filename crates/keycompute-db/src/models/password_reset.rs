//! 密码重置模型
//!
//! 管理用户密码重置流程

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 密码重置记录
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
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
        db: &DatabaseConnection,
        req: &CreatePasswordResetRequest,
    ) -> Result<PasswordReset, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
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
            [
                req.user_id.into(),
                req.token.as_str().into(),
                req.expires_at.into(),
                req.requested_from_ip.clone().into(),
            ],
        );
        let reset = PasswordReset::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(reset)
    }

    /// 根据 ID 查找
    pub async fn find_by_id(
        db: &DatabaseConnection,
        id: Uuid,
    ) -> Result<Option<PasswordReset>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, user_id, token, expires_at, used, used_at, requested_from_ip::TEXT AS requested_from_ip, created_at FROM password_resets WHERE id = $1",
            [id.into()],
        );
        let reset = PasswordReset::find_by_statement(stmt).one(db).await?;

        Ok(reset)
    }

    /// 根据令牌查找
    pub async fn find_by_token(
        db: &DatabaseConnection,
        token: &str,
    ) -> Result<Option<PasswordReset>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, user_id, token, expires_at, used, used_at, requested_from_ip::TEXT AS requested_from_ip, created_at FROM password_resets WHERE token = $1",
            [token.into()],
        );
        let reset = PasswordReset::find_by_statement(stmt).one(db).await?;

        Ok(reset)
    }

    /// 查找用户的有效重置记录
    pub async fn find_valid_by_user(
        db: &DatabaseConnection,
        user_id: Uuid,
    ) -> Result<Option<PasswordReset>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
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
            [user_id.into()],
        );
        let reset = PasswordReset::find_by_statement(stmt).one(db).await?;

        Ok(reset)
    }

    /// 标记为已使用
    pub async fn mark_used(&self, db: &DatabaseConnection) -> Result<PasswordReset, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
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
            [self.id.into()],
        );
        let reset = PasswordReset::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("PasswordReset", self.id.to_string()))?;

        Ok(reset)
    }

    /// 删除重置记录
    pub async fn delete(&self, db: &DatabaseConnection) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM password_resets WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 删除用户的所有重置记录
    pub async fn delete_all_by_user(
        db: &DatabaseConnection,
        user_id: Uuid,
    ) -> Result<u64, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM password_resets WHERE user_id = $1",
            [user_id.into()],
        );
        let result = db.execute(stmt).await?;

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
