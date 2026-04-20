//! 认证模块
//!
//! 处理用户注册、登录、密码重置等认证相关 API

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

pub use super::common::MessageResponse;

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

    /// 请求注册验证码
    pub async fn request_registration_code(
        &self,
        req: &RequestRegistrationCodeRequest,
    ) -> Result<RequestRegistrationCodeResponse> {
        self.client
            .post_json("/api/v1/auth/register", req, None)
            .await
    }

    /// 完成注册
    pub async fn complete_registration(
        &self,
        req: &CompleteRegistrationRequest,
    ) -> Result<CompleteRegistrationResponse> {
        self.client
            .post_json("/api/v1/auth/register/complete", req, None)
            .await
    }

    /// 用户登录
    pub async fn login(&self, req: &LoginRequest) -> Result<AuthResponse> {
        self.client.post_json("/api/v1/auth/login", req, None).await
    }

    /// 忘记密码
    pub async fn forgot_password(&self, req: &ForgotPasswordRequest) -> Result<MessageResponse> {
        self.client
            .post_json("/api/v1/auth/forgot-password", req, None)
            .await
    }

    /// 重置密码
    pub async fn reset_password(&self, req: &ResetPasswordRequest) -> Result<MessageResponse> {
        self.client
            .post_json("/api/v1/auth/reset-password", req, None)
            .await
    }

    /// 验证重置令牌
    pub async fn verify_reset_token(&self, token: &str) -> Result<MessageResponse> {
        self.client
            .get_json(&format!("/api/v1/auth/verify-reset-token/{}", token), None)
            .await
    }

    /// 刷新令牌
    pub async fn refresh_token(&self, req: &RefreshTokenRequest) -> Result<AuthResponse> {
        self.client
            .post_json("/api/v1/auth/refresh-token", req, None)
            .await
    }
}

/// 请求注册验证码
#[derive(Debug, Clone, Serialize)]
pub struct RequestRegistrationCodeRequest {
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub referral_code: Option<String>,
}

impl RequestRegistrationCodeRequest {
    pub fn new(email: impl Into<String>) -> Self {
        Self {
            email: email.into(),
            referral_code: None,
        }
    }

    pub fn with_referral_code(mut self, referral_code: impl Into<String>) -> Self {
        self.referral_code = Some(referral_code.into());
        self
    }
}

/// 请求注册验证码响应
#[derive(Debug, Clone, Deserialize)]
pub struct RequestRegistrationCodeResponse {
    pub email: String,
    pub message: String,
    pub expires_in_seconds: i64,
}

/// 完成注册请求
#[derive(Debug, Clone, Serialize)]
pub struct CompleteRegistrationRequest {
    pub email: String,
    pub code: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl CompleteRegistrationRequest {
    pub fn new(
        email: impl Into<String>,
        code: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            email: email.into(),
            code: code.into(),
            password: password.into(),
            name: None,
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// 完成注册响应
#[derive(Debug, Clone, Deserialize)]
pub struct CompleteRegistrationResponse {
    pub user_id: String,
    pub tenant_id: String,
    pub email: String,
    pub message: String,
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
    pub user_id: String,
    pub tenant_id: String,
    pub email: String,
    pub role: String,
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
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
