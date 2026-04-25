//! 用户注册服务
//!
//! 新注册链路采用“先验证码、后开户”的两阶段流程：
//! 1. 请求邮箱验证码，只占位邮箱，不写正式用户
//! 2. 验证码通过后一次性完成正式开户和副作用写入
//!
//! `pending_registrations` 记录只会在正式注册成功后删除。
//! 邮件发送失败、验证码过期、验证失败等异常路径都会保留 pending 记录，
//! 方便后续同邮箱继续复用并刷新验证码。

use crate::password::{EmailValidator, PasswordHasher, PasswordValidator};
use chrono::{Duration, Utc};
use keycompute_db::{
    PendingRegistration, Tenant, UpsertPendingRegistrationRequest, User, UserCredential,
};
use keycompute_emailserver::EmailService;
use keycompute_types::{KeyComputeError, Result, UserRole};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use std::sync::Arc;
use uuid::Uuid;

/// 请求注册验证码
#[derive(Debug, Clone, Deserialize)]
pub struct RequestRegistrationCodeRequest {
    /// 邮箱
    pub email: String,
    /// 推荐码（推荐人的用户 ID）
    pub referral_code: Option<String>,
}

/// 请求注册验证码响应
#[derive(Debug, Clone, Serialize)]
pub struct RequestRegistrationCodeResponse {
    /// 邮箱
    pub email: String,
    /// 提示消息
    pub message: String,
    /// 验证码剩余有效秒数
    pub expires_in_seconds: i64,
}

/// 完成注册请求
#[derive(Debug, Clone, Deserialize)]
pub struct CompleteRegistrationRequest {
    /// 邮箱
    pub email: String,
    /// 6 位邮箱验证码
    pub code: String,
    /// 密码
    pub password: String,
    /// 用户名（可选）
    pub name: Option<String>,
}

/// 完成注册响应
#[derive(Debug, Clone, Serialize)]
pub struct CompleteRegistrationResponse {
    /// 用户 ID
    pub user_id: Uuid,
    /// 租户 ID
    pub tenant_id: Uuid,
    /// 邮箱
    pub email: String,
    /// 消息
    pub message: String,
}

/// 注册服务
#[derive(Clone)]
pub struct RegistrationService {
    /// 数据库连接池
    pool: Arc<PgPool>,
    /// 密码哈希器
    password_hasher: PasswordHasher,
    /// 密码验证器
    password_validator: PasswordValidator,
    /// 邮箱验证器
    email_validator: EmailValidator,
    /// 邮件服务
    email_service: Option<EmailService>,
    /// 注册验证码有效期（分钟）
    registration_code_expiry_minutes: i64,
    /// 注册验证码重发冷却（秒）
    registration_code_resend_cooldown_seconds: i64,
    /// 注册验证码最大错误次数
    max_registration_code_attempts: i32,
}

impl std::fmt::Debug for RegistrationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistrationService")
            .field(
                "registration_code_expiry_minutes",
                &self.registration_code_expiry_minutes,
            )
            .field(
                "registration_code_resend_cooldown_seconds",
                &self.registration_code_resend_cooldown_seconds,
            )
            .field(
                "max_registration_code_attempts",
                &self.max_registration_code_attempts,
            )
            .finish()
    }
}

impl RegistrationService {
    /// 创建新的注册服务
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self {
            pool,
            password_hasher: PasswordHasher::new(),
            password_validator: PasswordValidator::new(),
            email_validator: EmailValidator::new(),
            email_service: None,
            registration_code_expiry_minutes: 10,
            registration_code_resend_cooldown_seconds: 60,
            max_registration_code_attempts: 5,
        }
    }

    /// 设置邮件服务
    pub fn with_email_service(mut self, email_service: EmailService) -> Self {
        self.email_service = Some(email_service);
        self
    }

    /// 设置注册验证码有效期
    pub fn with_registration_code_expiry_minutes(mut self, minutes: i64) -> Self {
        self.registration_code_expiry_minutes = minutes;
        self
    }

    /// 请求注册验证码。
    ///
    /// 仅占位邮箱，不写入正式用户信息。
    /// 同邮箱请求会被串行化，只有邮件成功发送后才会刷新验证码/过期时间。
    pub async fn request_registration_code(
        &self,
        req: &RequestRegistrationCodeRequest,
        client_ip: Option<String>,
    ) -> Result<RequestRegistrationCodeResponse> {
        let email = self.email_validator.normalize(&req.email);
        self.email_validator.validate(&email)?;

        let mut tx = self.pool.begin().await.map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to start transaction: {}", e))
        })?;

        PendingRegistration::lock_email_slot(&mut tx, &email)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to lock pending registration slot: {}",
                    e
                ))
            })?;

        if self.user_exists_by_email_in_tx(&mut tx, &email).await? {
            return Err(KeyComputeError::ValidationError(
                "该邮箱已被注册，请直接登录".to_string(),
            ));
        }

        let existing_pending = PendingRegistration::find_by_email_for_update(&mut tx, &email)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to check pending registration: {}",
                    e
                ))
            })?;

        let referral_code = if let Some(existing_pending) = existing_pending.as_ref() {
            existing_pending.referral_code
        } else {
            self.validate_referral_code(req.referral_code.as_deref())
                .await?
        };

        if let Some(existing_pending) = existing_pending.as_ref()
            && !existing_pending.is_expired()
        {
            let elapsed = (Utc::now() - existing_pending.last_sent_at).num_seconds();
            if elapsed < self.registration_code_resend_cooldown_seconds {
                return Err(KeyComputeError::RateLimitExceeded(format!(
                    "验证码发送过于频繁，请在 {} 秒后重试",
                    self.registration_code_resend_cooldown_seconds - elapsed
                )));
            }
        }

        let code = self.generate_registration_code();
        let code_hash = self.password_hasher.hash(&code)?;
        let expires_at = Utc::now() + Duration::minutes(self.registration_code_expiry_minutes);
        let last_sent_at = Utc::now();
        let save_req = UpsertPendingRegistrationRequest {
            email: email.clone(),
            referral_code,
            verification_code_hash: code_hash,
            expires_at,
            requested_from_ip: client_ip.clone(),
            resend_count: 1,
            last_sent_at,
        };

        if let Some(existing_pending) = existing_pending.as_ref() {
            existing_pending
                .refresh_code_in_tx(&mut tx, &save_req)
                .await
                .map_err(|e| {
                    KeyComputeError::DatabaseError(format!(
                        "Failed to refresh pending registration: {}",
                        e
                    ))
                })?;
        } else {
            PendingRegistration::create_in_tx(&mut tx, &save_req)
                .await
                .map_err(|e| {
                    KeyComputeError::DatabaseError(format!(
                        "Failed to create pending registration: {}",
                        e
                    ))
                })?;
        }

        tx.commit().await.map_err(|e| {
            KeyComputeError::DatabaseError(format!(
                "Failed to commit pending registration update: {}",
                e
            ))
        })?;

        let Some(email_service) = &self.email_service else {
            return Err(KeyComputeError::ServiceUnavailable(format!(
                "注册验证码暂时无法发送，请在 {} 秒后重试",
                self.registration_code_resend_cooldown_seconds
            )));
        };

        if let Err(e) = email_service
            .send_registration_code_email(&email, &code, self.registration_code_expiry_minutes)
            .await
        {
            tracing::error!(
                email = %email,
                error = %e,
                "Failed to send registration code email"
            );

            return Err(KeyComputeError::ServiceUnavailable(format!(
                "注册验证码发送失败，请在 {} 秒后重试",
                self.registration_code_resend_cooldown_seconds
            )));
        }

        Ok(RequestRegistrationCodeResponse {
            email,
            message: format!(
                "验证码已发送，请在 {} 分钟内完成验证",
                self.registration_code_expiry_minutes
            ),
            expires_in_seconds: self.registration_code_expiry_minutes * 60,
        })
    }

    /// 验证邮箱验证码并完成正式注册。
    ///
    /// 只有在验证码校验通过后，才会一次性写入正式用户、凭证、余额和推荐关系。
    pub async fn complete_registration(
        &self,
        req: &CompleteRegistrationRequest,
        default_quota: f64,
    ) -> Result<CompleteRegistrationResponse> {
        let email = self.email_validator.normalize(&req.email);
        self.email_validator.validate(&email)?;
        self.validate_registration_code(&req.code)?;
        self.password_validator.validate(&req.password)?;

        let mut tx = self.pool.begin().await.map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to start transaction: {}", e))
        })?;

        PendingRegistration::lock_email_slot(&mut tx, &email)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to lock pending registration slot: {}",
                    e
                ))
            })?;

        if self.user_exists_by_email_in_tx(&mut tx, &email).await? {
            return Err(KeyComputeError::ValidationError(
                "该邮箱已被注册，请直接登录".to_string(),
            ));
        }

        let pending = PendingRegistration::find_by_email_for_update(&mut tx, &email)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to load pending registration: {}",
                    e
                ))
            })?
            .ok_or_else(|| {
                KeyComputeError::VerificationError("验证码不存在或已失效，请重新获取".to_string())
            })?;

        if pending.is_expired() {
            return Err(KeyComputeError::VerificationError(
                "验证码已过期，请重新获取".to_string(),
            ));
        }

        if pending.verify_attempts >= self.max_registration_code_attempts {
            return Err(KeyComputeError::VerificationError(
                "验证码错误次数过多，请重新获取".to_string(),
            ));
        }

        let code_matches = self
            .password_hasher
            .verify(&req.code, &pending.verification_code_hash)?;

        if !code_matches {
            let updated_pending = pending.increment_attempts(&mut tx).await.map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to record code attempt: {}", e))
            })?;

            if updated_pending.verify_attempts >= self.max_registration_code_attempts {
                tx.commit().await.map_err(|e| {
                    KeyComputeError::DatabaseError(format!("Failed to commit attempts: {}", e))
                })?;
                return Err(KeyComputeError::VerificationError(
                    "验证码错误次数过多，请重新获取".to_string(),
                ));
            }

            tx.commit().await.map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to commit attempts: {}", e))
            })?;
            return Err(KeyComputeError::VerificationError("验证码错误".to_string()));
        }

        let tenant = self.get_or_create_default_tenant_in_tx(&mut tx).await?;
        let password_hash = self.password_hasher.hash(&req.password)?;
        let user = self
            .create_user_in_tx(&mut tx, tenant.id, &email, req.name.clone())
            .await?;
        self.create_verified_credential_in_tx(&mut tx, user.id, &password_hash)
            .await?;

        if default_quota > 0.0 {
            self.initialize_user_balance_in_tx(&mut tx, user.id, tenant.id, default_quota)
                .await?;
        }

        self.create_referral_in_tx(&mut tx, user.id, pending.referral_code)
            .await?;

        PendingRegistration::delete_in_tx(&mut tx, pending.id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to delete pending registration: {}",
                    e
                ))
            })?;

        tx.commit().await.map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to commit registration: {}", e))
        })?;

        if let Some(email_service) = &self.email_service
            && let Err(e) = email_service
                .send_welcome_email(&email, req.name.as_deref())
                .await
        {
            tracing::warn!(
                user_id = %user.id,
                email = %email,
                error = %e,
                "Failed to send welcome email"
            );
        }

        tracing::info!(
            user_id = %user.id,
            tenant_id = %tenant.id,
            email = %email,
            "User registration completed after code verification"
        );

        Ok(CompleteRegistrationResponse {
            user_id: user.id,
            tenant_id: tenant.id,
            email,
            message: "注册成功，您现在可以登录了".to_string(),
        })
    }

    /// 获取或创建默认租户
    async fn get_or_create_default_tenant_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Tenant> {
        let default_slug = "default";
        let tenant = sqlx::query_as::<_, Tenant>(
            r#"
            INSERT INTO tenants (name, slug, description)
            VALUES ($1, $2, $3)
            ON CONFLICT (slug) DO UPDATE
            SET slug = EXCLUDED.slug
            RETURNING *
            "#,
        )
        .bind("Default Tenant")
        .bind(default_slug)
        .bind(Some("Default tenant for new users"))
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to create default tenant: {}", e))
        })?;

        Ok(tenant)
    }

    fn validate_registration_code(&self, code: &str) -> Result<()> {
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err(KeyComputeError::ValidationError(
                "验证码必须为 6 位数字".to_string(),
            ));
        }

        Ok(())
    }

    fn generate_registration_code(&self) -> String {
        let mut rng = rand::thread_rng();
        format!("{:06}", rng.gen_range(0..1_000_000))
    }

    async fn user_exists_by_email_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        email: &str,
    ) -> Result<bool> {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)")
            .bind(email)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to check email existence: {}", e))
            })
    }

    async fn create_user_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: Uuid,
        email: &str,
        name: Option<String>,
    ) -> Result<User> {
        sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (tenant_id, email, name, role)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(tenant_id)
        .bind(email)
        .bind(name)
        .bind(UserRole::User.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to create user: {}", e)))
    }

    async fn create_verified_credential_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        password_hash: &str,
    ) -> Result<UserCredential> {
        sqlx::query_as::<_, UserCredential>(
            r#"
            INSERT INTO user_credentials (
                user_id,
                password_hash,
                email_verified,
                email_verified_at
            )
            VALUES ($1, $2, TRUE, NOW())
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(password_hash)
        .fetch_one(&mut **tx)
        .await
        .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to create credential: {}", e)))
    }

    async fn initialize_user_balance_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        tenant_id: Uuid,
        initial_balance: f64,
    ) -> Result<()> {
        use rust_decimal::Decimal;

        let amount = Decimal::from_f64_retain(initial_balance).unwrap_or(Decimal::ZERO);

        sqlx::query(
            r#"
            INSERT INTO user_balances (tenant_id, user_id, available_balance, total_recharged)
            VALUES ($1, $2, $3, $3)
            ON CONFLICT (user_id) DO UPDATE SET
                available_balance = user_balances.available_balance + $3,
                total_recharged = user_balances.total_recharged + $3,
                updated_at = NOW()
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(amount)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to initialize balance: {}", e))
        })?;

        sqlx::query(
            r#"
            INSERT INTO balance_transactions (
                tenant_id,
                user_id,
                transaction_type,
                amount,
                balance_before,
                balance_after,
                description
            )
            VALUES ($1, $2, 'recharge', $3, 0, $3, 'Initial quota from system')
            "#,
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(amount)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            KeyComputeError::DatabaseError(format!(
                "Failed to record initial balance transaction: {}",
                e
            ))
        })?;

        Ok(())
    }

    async fn create_referral_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: Uuid,
        referral_code: Option<Uuid>,
    ) -> Result<()> {
        let Some(level1_referrer_id) = referral_code else {
            return Ok(());
        };

        if level1_referrer_id == user_id {
            return Err(KeyComputeError::ValidationError(
                "不能使用自己的推荐码".to_string(),
            ));
        }

        let referrer_exists =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM users WHERE id = $1)")
                .bind(level1_referrer_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(|e| {
                    KeyComputeError::DatabaseError(format!(
                        "Failed to validate referral code: {}",
                        e
                    ))
                })?;

        if !referrer_exists {
            return Err(KeyComputeError::ValidationError("推荐码无效".to_string()));
        }

        let level2_referrer_id = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT level1_referrer_id FROM user_referrals WHERE user_id = $1",
        )
        .bind(level1_referrer_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to load referral chain: {}", e))
        })?
        .flatten();

        sqlx::query(
            r#"
            INSERT INTO user_referrals (
                user_id,
                level1_referrer_id,
                level2_referrer_id,
                source
            )
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(user_id)
        .bind(level1_referrer_id)
        .bind(level2_referrer_id)
        .bind("referral_code")
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to create referral relationship: {}", e))
        })?;

        Ok(())
    }

    async fn validate_referral_code(&self, referral_code: Option<&str>) -> Result<Option<Uuid>> {
        let Some(referral_code) = referral_code else {
            return Ok(None);
        };

        let referrer_id = Uuid::parse_str(referral_code)
            .map_err(|_| KeyComputeError::ValidationError("推荐码无效".to_string()))?;

        let referrer = User::find_by_id(&self.pool, referrer_id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to validate referral code: {}", e))
            })?;

        if referrer.is_none() {
            return Err(KeyComputeError::ValidationError("推荐码无效".to_string()));
        }

        Ok(Some(referrer_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_registration_code() {
        let pool = PgPool::connect_lazy("postgres://localhost/test").expect("lazy pool");
        let service = RegistrationService::new(Arc::new(pool));
        let code = service.generate_registration_code();

        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[tokio::test]
    async fn test_validate_registration_code() {
        let pool = PgPool::connect_lazy("postgres://localhost/test").expect("lazy pool");
        let service = RegistrationService::new(Arc::new(pool));

        assert!(service.validate_registration_code("123456").is_ok());
        assert!(service.validate_registration_code("12345").is_err());
        assert!(service.validate_registration_code("12ab56").is_err());
    }
}
