//! 认证模块
//!
//! 处理用户注册、登录、密码重置等认证相关 API

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// 认证 API 客户端
#[derive(Debug, Clone)]
pub struct AuthApi {
    client: ApiClient,
}

impl AuthApi {
    /// 创建新的认证 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 用户注册
    pub async fn register(&self, req: &RegisterRequest) -> Result<AuthResponse> {
        self.client.post_json("/auth/register", req, None).await
    }

    /// 用户登录
    pub async fn login(&self, req: &LoginRequest) -> Result<AuthResponse> {
        self.client.post_json("/auth/login", req, None).await
    }

    /// 验证邮箱
    pub async fn verify_email(&self, token: &str) -> Result<MessageResponse> {
        self.client
            .get_json(&format!("/auth/verify-email/{}", token), None)
            .await
    }

    /// 忘记密码
    pub async fn forgot_password(&self, req: &ForgotPasswordRequest) -> Result<MessageResponse> {
        self.client
            .post_json("/auth/forgot-password", req, None)
            .await
    }

    /// 重置密码
    pub async fn reset_password(&self, req: &ResetPasswordRequest) -> Result<MessageResponse> {
        self.client
            .post_json("/auth/reset-password", req, None)
            .await
    }

    /// 验证重置令牌
    pub async fn verify_reset_token(&self, token: &str) -> Result<MessageResponse> {
        self.client
            .get_json(&format!("/auth/verify-reset-token/{}", token), None)
            .await
    }

    /// 刷新令牌
    pub async fn refresh_token(&self, req: &RefreshTokenRequest) -> Result<AuthResponse> {
        self.client
            .post_json("/auth/refresh-token", req, None)
            .await
    }

    /// 重发验证邮件
    pub async fn resend_verification(
        &self,
        req: &ResendVerificationRequest,
    ) -> Result<MessageResponse> {
        self.client
            .post_json("/auth/resend-verification", req, None)
            .await
    }
}

/// 注册请求
#[derive(Debug, Clone, Serialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: Option<String>,
}

impl RegisterRequest {
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            password: password.into(),
            name: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// 登录请求
#[derive(Debug, Clone, Serialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

impl LoginRequest {
    pub fn new(email: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            password: password.into(),
        }
    }
}

/// 认证响应
#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub user: UserInfo,
}

/// 用户信息
#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
}

/// 忘记密码请求
#[derive(Debug, Clone, Serialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

impl ForgotPasswordRequest {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
        }
    }
}

/// 重置密码请求
#[derive(Debug, Clone, Serialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

impl ResetPasswordRequest {
    pub fn new(token: impl Into<String>, new_password: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            new_password: new_password.into(),
        }
    }
}

/// 刷新令牌请求
#[derive(Debug, Clone, Serialize)]
pub struct RefreshTokenRequest {
    pub refresh_token: String,
}

impl RefreshTokenRequest {
    pub fn new(refresh_token: impl Into<String>) -> Self {
        Self {
            refresh_token: refresh_token.into(),
        }
    }
}

/// 重发验证邮件请求
#[derive(Debug, Clone, Serialize)]
pub struct ResendVerificationRequest {
    pub email: String,
}

impl ResendVerificationRequest {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
        }
    }
}

/// 通用消息响应
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub message: String,
}
