//! 待完成注册模型
//!
//! 管理邮箱验证码注册流程中的临时占位状态。
//!
//! pending 记录会一直保留，直到正式注册成功后才删除。

use crate::DbError;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, FromQueryResult, Statement,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 待完成注册记录
#[derive(Debug, Clone, FromQueryResult, Serialize, Deserialize)]
pub struct PendingRegistration {
    pub id: Uuid,
    pub email: String,
    pub referral_code: Option<Uuid>,
    pub verification_code_hash: String,
    pub expires_at: DateTime<Utc>,
    pub verify_attempts: i32,
    pub resend_count: i32,
    pub last_sent_at: DateTime<Utc>,
    pub requested_from_ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 创建或更新待完成注册请求
#[derive(Debug, Clone)]
pub struct UpsertPendingRegistrationRequest {
    pub email: String,
    pub referral_code: Option<Uuid>,
    pub verification_code_hash: String,
    pub expires_at: DateTime<Utc>,
    pub requested_from_ip: Option<String>,
    pub resend_count: i32,
    pub last_sent_at: DateTime<Utc>,
}

impl PendingRegistration {
    /// 对同一个邮箱加事务级 advisory lock，串行化注册验证码请求。
    pub async fn lock_email_slot(txn: &DatabaseTransaction, email: &str) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtext($1)::bigint)",
            [email.into()],
        );
        txn.execute(stmt).await?;

        Ok(())
    }

    /// 在事务中创建待完成注册记录。
    pub async fn create_in_tx(
        txn: &DatabaseTransaction,
        req: &UpsertPendingRegistrationRequest,
    ) -> Result<PendingRegistration, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO pending_registrations (
                email,
                referral_code,
                verification_code_hash,
                expires_at,
                resend_count,
                last_sent_at,
                requested_from_ip
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (email) DO NOTHING
            RETURNING *
            "#,
            [
                req.email.as_str().into(),
                req.referral_code.into(),
                req.verification_code_hash.as_str().into(),
                req.expires_at.into(),
                req.resend_count.into(),
                req.last_sent_at.into(),
                req.requested_from_ip.clone().into(),
            ],
        );
        let pending = PendingRegistration::find_by_statement(stmt)
            .one(txn)
            .await?;

        pending.ok_or_else(|| DbError::duplicate_key("pending_registrations", "email", &req.email))
    }

    /// 在事务中刷新验证码和冷却时间。
    ///
    /// 当前注册链路采用"记录发送尝试即进入冷却"，因此该方法会在发信前调用。
    pub async fn refresh_code_in_tx(
        &self,
        txn: &DatabaseTransaction,
        req: &UpsertPendingRegistrationRequest,
    ) -> Result<PendingRegistration, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE pending_registrations
            SET verification_code_hash = $2,
                expires_at = $3,
                verify_attempts = 0,
                resend_count = resend_count + 1,
                last_sent_at = $4,
                requested_from_ip = $5,
                updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
            [
                self.id.into(),
                req.verification_code_hash.as_str().into(),
                req.expires_at.into(),
                req.last_sent_at.into(),
                req.requested_from_ip.clone().into(),
            ],
        );
        let pending = PendingRegistration::find_by_statement(stmt)
            .one(txn)
            .await?
            .ok_or_else(|| DbError::not_found("PendingRegistration", self.id.to_string()))?;

        Ok(pending)
    }

    /// 根据邮箱查找待完成注册记录。
    pub async fn find_by_email(
        db: &DatabaseConnection,
        email: &str,
    ) -> Result<Option<PendingRegistration>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM pending_registrations WHERE email = $1",
            [email.into()],
        );
        let pending = PendingRegistration::find_by_statement(stmt).one(db).await?;

        Ok(pending)
    }

    /// 在事务中锁定并查询待完成注册记录。
    pub async fn find_by_email_for_update(
        txn: &DatabaseTransaction,
        email: &str,
    ) -> Result<Option<PendingRegistration>, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM pending_registrations WHERE email = $1 FOR UPDATE",
            [email.into()],
        );
        let pending = PendingRegistration::find_by_statement(stmt)
            .one(txn)
            .await?;

        Ok(pending)
    }

    /// 在事务中增加验证码尝试次数。
    pub async fn increment_attempts(
        &self,
        txn: &DatabaseTransaction,
    ) -> Result<PendingRegistration, DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE pending_registrations
            SET verify_attempts = verify_attempts + 1,
                updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
            [self.id.into()],
        );
        let pending = PendingRegistration::find_by_statement(stmt)
            .one(txn)
            .await?
            .ok_or_else(|| DbError::not_found("PendingRegistration", self.id.to_string()))?;

        Ok(pending)
    }

    /// 在事务中删除待完成注册记录。
    pub async fn delete_in_tx(txn: &DatabaseTransaction, id: Uuid) -> Result<(), DbError> {
        let stmt = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM pending_registrations WHERE id = $1",
            [id.into()],
        );
        txn.execute(stmt).await?;

        Ok(())
    }

    /// 检查验证码是否已过期。
    pub fn is_expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }

    /// 获取剩余有效秒数。
    pub fn remaining_seconds(&self) -> i64 {
        (self.expires_at - Utc::now()).num_seconds().max(0)
    }
}
