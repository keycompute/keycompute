//! 用户登录服务
//!
//! 提供用户登录功能，包括账户锁定保护

use crate::jwt::JwtValidator;
use crate::password::{EmailValidator, PasswordHasher};
use keycompute_db::{DbRouter, User, UserCredential};
use keycompute_types::{KeyComputeError, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// 登录请求
#[derive(Debug, Clone, Deserialize)]
pub struct LoginRequest {
    /// 邮箱
    pub email: String,
    /// 密码
    pub password: String,
    /// 客户端 IP（可选，用于安全审计）
    pub client_ip: Option<String>,
}

/// 登录响应
#[derive(Debug, Clone, Serialize)]
pub struct LoginResponse {
    /// 用户 ID
    pub user_id: Uuid,
    /// 租户 ID
    pub tenant_id: Uuid,
    /// 邮箱
    pub email: String,
    /// 角色
    pub role: String,
    /// JWT Token
    pub jwt_token: String,
    /// Token 有效期（秒）
    pub expires_in: i64,
}

/// 登录服务
#[derive(Clone)]
pub struct LoginService {
    /// 数据库连接池
    pool: Arc<DbRouter>,
    /// 密码哈希器
    password_hasher: PasswordHasher,
    /// 邮箱验证器
    email_validator: EmailValidator,
    /// JWT 验证器
    jwt_validator: JwtValidator,
    /// 最大登录失败次数
    max_failed_attempts: i32,
    /// 账户锁定时长（分钟）
    lock_duration_minutes: i64,
}

impl std::fmt::Debug for LoginService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginService")
            .field("max_failed_attempts", &self.max_failed_attempts)
            .field("lock_duration_minutes", &self.lock_duration_minutes)
            .finish()
    }
}

impl LoginService {
    /// 创建新的登录服务
    pub fn new(pool: Arc<DbRouter>, jwt_validator: JwtValidator) -> Self {
        Self {
            pool,
            password_hasher: PasswordHasher::new(),
            email_validator: EmailValidator::new(),
            jwt_validator,
            max_failed_attempts: 5,
            lock_duration_minutes: 30,
        }
    }

    /// 设置最大失败次数
    pub fn with_max_failed_attempts(mut self, max: i32) -> Self {
        self.max_failed_attempts = max;
        self
    }

    /// 设置锁定时长
    pub fn with_lock_duration(mut self, minutes: i64) -> Self {
        self.lock_duration_minutes = minutes;
        self
    }

    /// 用户登录
    ///
    /// # 流程
    /// 1. 验证邮箱格式
    /// 2. 查找用户
    /// 3. 检查账户锁定状态
    /// 4. 验证密码
    /// 5. 检查邮箱验证状态
    /// 6. 生成 JWT Token
    /// 7. 更新登录信息
    pub async fn login(&self, req: &LoginRequest) -> Result<LoginResponse> {
        // 1. 规范化并验证邮箱
        let email = self.email_validator.normalize(&req.email);
        self.email_validator.validate(&email)?;

        // 2. 查找用户
        let user = User::find_by_email(self.pool.as_ref(), &email)
            .await
            .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to find user: {}", e)))?
            .ok_or_else(|| {
                // 用户不存在时返回统一错误，防止邮箱枚举
                KeyComputeError::AuthError("Email or password is incorrect".to_string())
            })?;

        // 3. 获取凭证
        let credential = UserCredential::find_by_user_id(self.pool.as_ref(), user.id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to find credential: {}", e))
            })?
            .ok_or_else(|| {
                KeyComputeError::AuthError("Email or password is incorrect".to_string())
            })?;

        // 4. 检查账户锁定状态
        if credential.is_locked() {
            let remaining = credential.remaining_lock_seconds();
            tracing::warn!(
                user_id = %user.id,
                remaining_seconds = remaining,
                "Account is locked"
            );
            return Err(KeyComputeError::AuthError(format!(
                "账户已被锁定，请在 {} 秒后重试",
                remaining
            )));
        }

        // 5. 验证密码
        let password_valid = self
            .password_hasher
            .verify(&req.password, &credential.password_hash)?;

        if !password_valid {
            // 记录失败登录
            self.record_failed_login(&credential).await?;

            tracing::warn!(
                user_id = %user.id,
                email = %email,
                "Failed login attempt"
            );

            return Err(KeyComputeError::AuthError(
                "Email or password is incorrect".to_string(),
            ));
        }

        // 6. 检查邮箱验证状态
        if !credential.email_verified {
            return Err(KeyComputeError::AuthError(
                "Email is not verified".to_string(),
            ));
        }

        // 7. 重置失败计数并更新登录信息
        let _updated_credential = credential
            .record_successful_login(self.pool.as_ref(), req.client_ip.clone())
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to update login info: {}", e))
            })?;

        // 8. 生成 JWT Token
        let token = self
            .jwt_validator
            .generate_token(user.id, user.tenant_id, &user.role)?;

        tracing::info!(
            user_id = %user.id,
            tenant_id = %user.tenant_id,
            email = %email,
            "User logged in successfully"
        );

        Ok(LoginResponse {
            user_id: user.id,
            tenant_id: user.tenant_id,
            email: user.email.clone(),
            role: user.role.clone(),
            jwt_token: token,
            expires_in: self.jwt_validator.default_expiration(),
        })
    }

    /// 记录失败登录
    ///
    /// 超过最大失败次数后锁定账户
    async fn record_failed_login(&self, credential: &UserCredential) -> Result<()> {
        let updated = credential
            .increment_failed_attempts(self.pool.as_ref())
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!(
                    "Failed to increment failed attempts: {}",
                    e
                ))
            })?;

        // 检查是否需要锁定
        if updated.failed_login_attempts >= self.max_failed_attempts {
            updated
                .lock(self.pool.as_ref(), self.lock_duration_minutes)
                .await
                .map_err(|e| {
                    KeyComputeError::DatabaseError(format!("Failed to lock account: {}", e))
                })?;

            tracing::warn!(
                user_id = %credential.user_id,
                failed_attempts = updated.failed_login_attempts,
                lock_duration_minutes = self.lock_duration_minutes,
                "Account locked due to too many failed attempts"
            );
        }

        Ok(())
    }

    /// 刷新 Token
    ///
    /// 验证当前 Token 并生成新 Token
    pub async fn refresh_token(&self, token: &str) -> Result<LoginResponse> {
        // 验证当前 Token
        let claims = self.jwt_validator.validate(token)?;

        // 获取用户信息
        let user = User::find_by_id(self.pool.as_ref(), claims.user_id)
            .await
            .map_err(|e| KeyComputeError::DatabaseError(format!("Failed to find user: {}", e)))?
            .ok_or_else(|| KeyComputeError::AuthError("User does not exist".to_string()))?;

        // 检查凭证状态
        let credential = UserCredential::find_by_user_id(self.pool.as_ref(), user.id)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to find credential: {}", e))
            })?
            .ok_or_else(|| {
                KeyComputeError::AuthError("User credential does not exist".to_string())
            })?;

        if credential.is_locked() {
            return Err(KeyComputeError::AuthError("Account is locked".to_string()));
        }

        if !credential.email_verified {
            return Err(KeyComputeError::AuthError(
                "Email is not verified".to_string(),
            ));
        }

        // 生成新 Token
        let new_token = self
            .jwt_validator
            .generate_token(user.id, user.tenant_id, &user.role)?;

        Ok(LoginResponse {
            user_id: user.id,
            tenant_id: user.tenant_id,
            email: user.email.clone(),
            role: user.role.clone(),
            jwt_token: new_token,
            expires_in: self.jwt_validator.default_expiration(),
        })
    }

    /// 登出（可选：将 Token 加入黑名单）
    ///
    /// 当前实现不维护 Token 黑名单，此方法仅用于日志记录
    pub async fn logout(&self, user_id: Uuid) -> Result<()> {
        tracing::info!(user_id = %user_id, "User logged out");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：登录服务的完整测试需要数据库连接
    // 这里只测试基础功能

    #[test]
    fn test_login_request_fields() {
        // 测试请求结构
        let req = LoginRequest {
            email: "test@example.com".to_string(),
            password: "SecurePass123!".to_string(),
            client_ip: Some("192.168.1.1".to_string()),
        };

        assert_eq!(req.email, "test@example.com");
        assert_eq!(req.password, "SecurePass123!");
        assert_eq!(req.client_ip, Some("192.168.1.1".to_string()));
    }

    #[test]
    fn test_login_response_fields() {
        // 测试响应结构
        let response = LoginResponse {
            user_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            email: "test@example.com".to_string(),
            role: "user".to_string(),
            jwt_token: "token123".to_string(),
            expires_in: 3600,
        };

        assert_eq!(response.email, "test@example.com");
        assert_eq!(response.role, "user");
        assert_eq!(response.expires_in, 3600);
    }
}
