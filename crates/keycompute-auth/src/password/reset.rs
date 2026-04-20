//! 密码重置服务
//!
//! 提供密码重置功能，包括重置请求和执行重置

use crate::password::{EmailValidator, PasswordHasher, PasswordValidator};
use chrono::{Duration, Utc};
use keycompute_db::{
    CreatePasswordResetRequest, PasswordReset, UpdateUserCredentialRequest, User, UserCredential,
};
use keycompute_emailserver::EmailService;
use keycompute_types::{KeyComputeError, Result};
use rand::Rng;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// 密码重置请求
#[derive(Debug, Clone, Deserialize)]
pub struct ResetPasswordRequest {
    /// 重置令牌
    pub token: String,
    /// 新密码
    pub new_password: String,
}

/// 请求密码重置请求
#[derive(Debug, Clone, Deserialize)]
pub struct RequestPasswordResetRequest {
    /// 邮箱
    pub email: String,
    /// 客户端 IP（可选）
    pub client_ip: Option<String>,
    /// 对外公开的前端应用基础 URL
    pub public_base_url: String,
}

/// 密码重置服务
#[derive(Clone)]
pub struct PasswordResetService {
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
    /// 重置令牌有效期（小时）
    token_expiry_hours: i64,
}

impl std::fmt::Debug for PasswordResetService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordResetService")
            .field("token_expiry_hours", &self.token_expiry_hours)
            .finish()
    }
}

impl PasswordResetService {
    /// 创建新的密码重置服务
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self {
            pool,
            password_hasher: PasswordHasher::new(),
            password_validator: PasswordValidator::new(),
            email_validator: EmailValidator::new(),
            email_service: None,
            token_expiry_hours: 1, // 默认 1 小时有效期
        }
    }

    /// 设置邮件服务
    pub fn with_email_service(mut self, email_service: EmailService) -> Self {
        self.email_service = Some(email_service);
        self
    }

    /// 设置令牌有效期
    pub fn with_token_expiry(mut self, hours: i64) -> Self {
        self.token_expiry_hours = hours;
        self
    }

    /// 请求密码重置
    ///
    /// # 流程
    /// 1. 验证邮箱格式
    /// 2. 查找用户（即使用户不存在也返回成功，防止邮箱枚举）
    /// 3. 生成重置令牌
    /// 4. 保存令牌
    /// 5. 返回令牌（实际场景中应发送邮件）
    ///
    /// # 安全考虑
    /// 即使用户不存在也返回成功，防止邮箱枚举攻击
    pub async fn request_reset(&self, req: &RequestPasswordResetRequest) -> Result<Option<String>> {
        // 1. 规范化并验证邮箱
        let email = self.email_validator.normalize(&req.email);

        if self.email_validator.validate(&email).is_err() {
            // 邮箱格式无效时静默返回
            tracing::debug!("Invalid email format in password reset request");
            return Ok(None);
        }

        // 2. 查找用户
        let user = match User::find_by_email(&self.pool, &email).await {
            Ok(Some(user)) => user,
            Ok(None) => {
                // 用户不存在时返回成功但不发送邮件
                tracing::info!(
                    email = %email,
                    "Password reset requested for non-existent email"
                );
                return Ok(None);
            }
            Err(e) => {
                return Err(KeyComputeError::DatabaseError(format!(
                    "Failed to find user: {}",
                    e
                )));
            }
        };

        // 3. 生成重置令牌
        let token = self.generate_reset_token();
        let expires_at = Utc::now() + Duration::hours(self.token_expiry_hours);

        // 4. 保存令牌
        PasswordReset::create(
            &self.pool,
            &CreatePasswordResetRequest {
                user_id: user.id,
                token: token.clone(),
                expires_at,
                requested_from_ip: req.client_ip.clone(),
            },
        )
        .await
        .map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to create password reset: {}", e))
        })?;

        // 5. 发送密码重置邮件
        if let Some(email_service) = &self.email_service
            && let Err(e) = email_service
                .send_password_reset_email(&email, &token, &req.public_base_url)
                .await
        {
            tracing::error!(
                user_id = %user.id,
                email = %email,
                error = %e,
                "Failed to send password reset email"
            );
            return Err(KeyComputeError::AuthError(
                "发送重置邮件失败，请稍后重试".to_string(),
            ));
        }

        tracing::info!(
            user_id = %user.id,
            email = %email,
            expiry_hours = self.token_expiry_hours,
            "Password reset token created"
        );

        Ok(Some(token))
    }

    /// 执行密码重置
    ///
    /// # 流程
    /// 1. 验证密码强度
    /// 2. 查找重置令牌
    /// 3. 验证令牌有效性
    /// 4. 哈希新密码
    /// 5. 更新密码
    /// 6. 标记令牌已使用
    /// 7. 清除账户锁定状态
    pub async fn reset_password(&self, req: &ResetPasswordRequest) -> Result<Uuid> {
        // 1. 验证密码强度
        self.password_validator.validate(&req.new_password)?;

        // 2. 查找重置令牌
        let reset = PasswordReset::find_by_token(&self.pool, &req.token)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to find reset token: {}", e))
            })?
            .ok_or_else(|| KeyComputeError::AuthError("无效的重置链接".to_string()))?;

        // 3. 验证令牌有效性
        if !reset.is_valid() {
            if reset.used {
                return Err(KeyComputeError::AuthError("该重置链接已使用".to_string()));
            }
            return Err(KeyComputeError::AuthError(
                "重置链接已过期，请重新申请".to_string(),
            ));
        }

        // 4. 哈希新密码
        let new_hash = self.password_hasher.hash(&req.new_password)?;

        // 5. 更新密码
        let credential = UserCredential::find_by_user_id(&self.pool, reset.user_id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to find credential: {}", e))
            })?
            .ok_or_else(|| KeyComputeError::AuthError("用户凭证不存在".to_string()))?;

        credential
            .update(
                &self.pool,
                &UpdateUserCredentialRequest {
                    password_hash: Some(new_hash),
                    failed_login_attempts: Some(0),
                    locked_until: None, // 清除锁定
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to update password: {}", e))
            })?;

        // 6. 标记令牌已使用
        reset.mark_used(&self.pool).await.map_err(|e| {
            KeyComputeError::DatabaseError(format!("Failed to mark reset token as used: {}", e))
        })?;

        // 7. 清除该用户的其他重置令牌
        PasswordReset::delete_all_by_user(&self.pool, reset.user_id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to cleanup reset tokens: {}", e))
            })?;

        tracing::info!(
            user_id = %reset.user_id,
            "Password reset successfully"
        );

        Ok(reset.user_id)
    }

    /// 验证重置令牌
    ///
    /// 检查令牌是否有效，用于前端验证
    pub async fn validate_token(&self, token: &str) -> Result<bool> {
        let reset = PasswordReset::find_by_token(&self.pool, token)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to find reset token: {}", e))
            })?
            .ok_or_else(|| KeyComputeError::AuthError("无效的重置链接".to_string()))?;

        Ok(reset.is_valid())
    }

    /// 验证重置令牌（别名方法）
    pub async fn verify_token(&self, token: &str) -> Result<bool> {
        self.validate_token(token).await
    }

    /// 生成安全的重置令牌
    fn generate_reset_token(&self) -> String {
        let mut rng = rand::thread_rng();
        let token: String = (0..64)
            .map(|_| {
                let idx = rng.gen_range(0..62);
                if idx < 10 {
                    (b'0' + idx as u8) as char
                } else if idx < 36 {
                    (b'a' + idx as u8 - 10) as char
                } else {
                    (b'A' + idx as u8 - 36) as char
                }
            })
            .collect();
        token
    }

    /// 清理过期令牌
    ///
    /// 删除所有已过期或已使用的重置令牌
    pub async fn cleanup_expired_tokens(&self) -> Result<u64> {
        let result =
            sqlx::query("DELETE FROM password_resets WHERE expires_at < NOW() OR used = TRUE")
                .execute(&*self.pool)
                .await
                .map_err(|e| {
                    KeyComputeError::DatabaseError(format!(
                        "Failed to cleanup expired tokens: {}",
                        e
                    ))
                })?;

        let deleted = result.rows_affected();
        if deleted > 0 {
            tracing::info!(
                deleted_count = deleted,
                "Cleaned up expired password reset tokens"
            );
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_reset_token() {
        // 测试令牌生成逻辑，不需要数据库连接
        let mut rng = rand::thread_rng();
        let token: String = (0..64)
            .map(|_| {
                let idx = rng.gen_range(0..62);
                if idx < 10 {
                    (b'0' + idx as u8) as char
                } else if idx < 36 {
                    (b'a' + idx as u8 - 10) as char
                } else {
                    (b'A' + idx as u8 - 36) as char
                }
            })
            .collect();

        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_reset_password_request_fields() {
        // 测试请求结构
        let req = ResetPasswordRequest {
            token: "abc123token456".to_string(),
            new_password: "NewSecurePass123!".to_string(),
        };

        assert_eq!(req.token, "abc123token456");
        assert_eq!(req.new_password, "NewSecurePass123!");
    }
}
