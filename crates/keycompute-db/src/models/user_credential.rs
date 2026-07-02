//! 用户密码凭证模型
//!
//! 存储用户密码哈希和登录安全相关信息

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 用户密码凭证
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct UserCredential {
    pub id: Uuid,
    pub user_id: Uuid,
    pub password_hash: String,
    pub email_verified: bool,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub failed_login_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub last_login_ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建用户凭证请求
#[derive(Debug, Clone)]
pub struct CreateUserCredentialRequest {
    pub user_id: Uuid,
    pub password_hash: String,
}

/// 更新用户凭证请求
#[derive(Debug, Clone, Default)]
pub struct UpdateUserCredentialRequest {
    pub password_hash: Option<String>,
    pub email_verified: Option<bool>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub failed_login_attempts: Option<i32>,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub last_login_ip: Option<String>,
}

impl UserCredential {
    /// 创建新凭证
    pub async fn create(
        db: &impl ConnectionTrait,
        req: &CreateUserCredentialRequest,
    ) -> Result<UserCredential, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO user_credentials (user_id, password_hash) VALUES ($1, $2) RETURNING *"#,
            [req.user_id.into(), req.password_hash.as_str().into()],
        );
        let credential = UserCredential::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("create failed to return row".to_string()))?;

        Ok(credential)
    }

    /// 根据 ID 查找凭证
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: Uuid,
    ) -> Result<Option<UserCredential>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_credentials WHERE id = $1",
            [id.into()],
        );
        let credential = UserCredential::find_by_statement(stmt).one(db).await?;

        Ok(credential)
    }

    /// 根据用户 ID 查找凭证
    pub async fn find_by_user_id(
        db: &impl ConnectionTrait,
        user_id: Uuid,
    ) -> Result<Option<UserCredential>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_credentials WHERE user_id = $1",
            [user_id.into()],
        );
        let credential = UserCredential::find_by_statement(stmt).one(db).await?;

        Ok(credential)
    }

    /// 更新凭证
    pub async fn update(
        &self,
        db: &impl ConnectionTrait,
        req: &UpdateUserCredentialRequest,
    ) -> Result<UserCredential, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE user_credentials
            SET password_hash = COALESCE($1, password_hash),
                email_verified = COALESCE($2, email_verified),
                email_verified_at = COALESCE($3, email_verified_at),
                failed_login_attempts = COALESCE($4, failed_login_attempts),
                locked_until = COALESCE($5, locked_until),
                last_login_at = COALESCE($6, last_login_at),
                last_login_ip = COALESCE($7, last_login_ip),
                updated_at = NOW()
            WHERE id = $8
            RETURNING *
            "#,
            [
                req.password_hash.clone().into(),
                req.email_verified.into(),
                req.email_verified_at.into(),
                req.failed_login_attempts.into(),
                req.locked_until.into(),
                req.last_login_at.into(),
                req.last_login_ip.clone().into(),
                self.id.into(),
            ],
        );
        let credential = UserCredential::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::Other("update failed to return row".to_string()))?;

        Ok(credential)
    }

    /// 删除凭证
    pub async fn delete(&self, db: &impl ConnectionTrait) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM user_credentials WHERE id = $1",
            [self.id.into()],
        );
        db.execute(stmt).await?;

        Ok(())
    }

    /// 批量根据用户 ID 列表查找凭证，返回以 user_id 为 key 的 HashMap
    pub async fn find_by_user_ids(
        db: &impl ConnectionTrait,
        user_ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, UserCredential>, DbError> {
        if user_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM user_credentials WHERE user_id = ANY($1)",
            [user_ids.to_vec().into()],
        );
        let credentials = UserCredential::find_by_statement(stmt).all(db).await?;
        Ok(credentials.into_iter().map(|c| (c.user_id, c)).collect())
    }

    /// 增加失败登录次数
    pub async fn increment_failed_attempts(
        &self,
        db: &impl ConnectionTrait,
    ) -> Result<UserCredential, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_credentials SET failed_login_attempts = failed_login_attempts + 1, updated_at = NOW() WHERE id = $1 RETURNING *"#,
            [self.id.into()],
        );
        let credential = UserCredential::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("UserCredential", self.id.to_string()))?;

        Ok(credential)
    }

    /// 重置失败登录次数并更新登录信息
    pub async fn record_successful_login(
        &self,
        db: &impl ConnectionTrait,
        ip: Option<String>,
    ) -> Result<UserCredential, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_credentials SET failed_login_attempts = 0, locked_until = NULL, last_login_at = NOW(), last_login_ip = $1, updated_at = NOW() WHERE id = $2 RETURNING *"#,
            [ip.into(), self.id.into()],
        );
        let credential = UserCredential::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("UserCredential", self.id.to_string()))?;

        Ok(credential)
    }

    /// 锁定账户
    pub async fn lock(
        &self,
        db: &impl ConnectionTrait,
        duration_minutes: i64,
    ) -> Result<UserCredential, DbError> {
        let locked_until = Utc::now() + chrono::Duration::minutes(duration_minutes);
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE user_credentials SET locked_until = $1, updated_at = NOW() WHERE id = $2 RETURNING *"#,
            [locked_until.into(), self.id.into()],
        );
        let credential = UserCredential::find_by_statement(stmt)
            .one(db)
            .await?
            .ok_or_else(|| DbError::not_found("UserCredential", self.id.to_string()))?;

        Ok(credential)
    }

    /// 检查账户是否被锁定
    pub fn is_locked(&self) -> bool {
        match self.locked_until {
            Some(locked_until) => locked_until > Utc::now(),
            None => false,
        }
    }

    /// 获取剩余锁定时间（秒）
    pub fn remaining_lock_seconds(&self) -> i64 {
        match self.locked_until {
            Some(locked_until) => {
                let remaining = locked_until.signed_duration_since(Utc::now()).num_seconds();
                remaining.max(0)
            }
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_credential_is_locked() {
        let credential = UserCredential {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            password_hash: "hash".to_string(),
            email_verified: false,
            email_verified_at: None,
            failed_login_attempts: 0,
            locked_until: Some(Utc::now() + chrono::Duration::minutes(30)),
            last_login_at: None,
            last_login_ip: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(credential.is_locked());
    }

    #[test]
    fn test_user_credential_not_locked() {
        let credential = UserCredential {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            password_hash: "hash".to_string(),
            email_verified: true,
            email_verified_at: Some(Utc::now()),
            failed_login_attempts: 0,
            locked_until: None,
            last_login_at: None,
            last_login_ip: Some("192.168.1.1".to_string()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(!credential.is_locked());
    }

    #[test]
    fn test_user_credential_lock_expired() {
        let credential = UserCredential {
            id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            password_hash: "hash".to_string(),
            email_verified: false,
            email_verified_at: None,
            failed_login_attempts: 0,
            locked_until: Some(Utc::now() - chrono::Duration::minutes(1)),
            last_login_at: None,
            last_login_ip: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert!(!credential.is_locked());
        assert_eq!(credential.remaining_lock_seconds(), 0);
    }
}
