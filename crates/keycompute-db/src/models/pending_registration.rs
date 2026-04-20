//! 待完成注册模型
//!
//! 管理邮箱验证码注册流程中的临时占位状态。
//!
//! pending 记录会一直保留，直到正式注册成功后才删除。

use crate::DbError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

/// 待完成注册记录
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
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
    pub async fn lock_email_slot(
        tx: &mut Transaction<'_, Postgres>,
        email: &str,
    ) -> Result<(), DbError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1)::bigint)")
            .bind(email)
            .execute(&mut **tx)
            .await?;

        Ok(())
    }

    /// 在事务中创建待完成注册记录。
    pub async fn create_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        req: &UpsertPendingRegistrationRequest,
    ) -> Result<PendingRegistration, DbError> {
        let pending = sqlx::query_as::<_, PendingRegistration>(
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
        )
        .bind(&req.email)
        .bind(req.referral_code)
        .bind(&req.verification_code_hash)
        .bind(req.expires_at)
        .bind(req.resend_count)
        .bind(req.last_sent_at)
        .bind(&req.requested_from_ip)
        .fetch_optional(&mut **tx)
        .await?;

        pending.ok_or_else(|| DbError::duplicate_key("pending_registrations", "email", &req.email))
    }

    /// 在事务中刷新验证码和冷却时间。
    ///
    /// 当前注册链路采用“记录发送尝试即进入冷却”，因此该方法会在发信前调用。
    pub async fn refresh_code_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        req: &UpsertPendingRegistrationRequest,
    ) -> Result<PendingRegistration, DbError> {
        let pending = sqlx::query_as::<_, PendingRegistration>(
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
        )
        .bind(self.id)
        .bind(&req.verification_code_hash)
        .bind(req.expires_at)
        .bind(req.last_sent_at)
        .bind(&req.requested_from_ip)
        .fetch_one(&mut **tx)
        .await?;

        Ok(pending)
    }

    /// 根据邮箱查找待完成注册记录。
    pub async fn find_by_email(
        pool: &sqlx::PgPool,
        email: &str,
    ) -> Result<Option<PendingRegistration>, DbError> {
        let pending = sqlx::query_as::<_, PendingRegistration>(
            "SELECT * FROM pending_registrations WHERE email = $1",
        )
        .bind(email)
        .fetch_optional(pool)
        .await?;

        Ok(pending)
    }

    /// 在事务中锁定并查询待完成注册记录。
    pub async fn find_by_email_for_update(
        tx: &mut Transaction<'_, Postgres>,
        email: &str,
    ) -> Result<Option<PendingRegistration>, DbError> {
        let pending = sqlx::query_as::<_, PendingRegistration>(
            "SELECT * FROM pending_registrations WHERE email = $1 FOR UPDATE",
        )
        .bind(email)
        .fetch_optional(&mut **tx)
        .await?;

        Ok(pending)
    }

    /// 在事务中增加验证码尝试次数。
    pub async fn increment_attempts(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<PendingRegistration, DbError> {
        let pending = sqlx::query_as::<_, PendingRegistration>(
            r#"
            UPDATE pending_registrations
            SET verify_attempts = verify_attempts + 1,
                updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(self.id)
        .fetch_one(&mut **tx)
        .await?;

        Ok(pending)
    }

    /// 在事务中删除待完成注册记录。
    pub async fn delete_in_tx(tx: &mut Transaction<'_, Postgres>, id: Uuid) -> Result<(), DbError> {
        sqlx::query("DELETE FROM pending_registrations WHERE id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await?;

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
