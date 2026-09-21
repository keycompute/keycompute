//! 认证模块
//!
//! 处理用户注册、登录、密码重置等认证相关 API

use crate::client::ApiClient;
use crate::error::Result;
use keycompute_types::{PlatformRole, TenantRole};
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

    /// Selects the active tenant for a console session.
    ///
    /// The request contains only the tenant selector. Authority is refreshed
    /// by the server and returned in the session response.
    pub async fn select_tenant(
        &self,
        req: &SelectTenantRequest,
        token: &str,
    ) -> Result<AuthResponse> {
        self.client
            .post_json(&super::routes::me("tenant"), req, Some(token))
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

/// Explicit tenant selector used when switching a console session.
#[derive(Debug, Clone, Serialize)]
pub struct SelectTenantRequest {
    pub tenant_id: Option<String>,
}

impl SelectTenantRequest {
    pub fn new(tenant_id: impl Into<String>) -> Self {
        Self {
            tenant_id: Some(tenant_id.into()),
        }
    }

    pub fn global() -> Self {
        Self { tenant_id: None }
    }
}

/// 认证响应
#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub user_id: String,
    pub name: Option<String>,
    pub status: Option<keycompute_types::UserStatus>,
    pub email: String,
    /// The server may omit this for a global session or before membership
    /// lookup. The client never invents a role when it is absent.
    #[serde(default)]
    pub platform_role: Option<PlatformRole>,
    #[serde(default)]
    pub selected_tenant: Option<SelectedTenant>,
    #[serde(default)]
    pub memberships: Vec<TenantMembership>,
    #[serde(default)]
    pub capabilities: SessionCapabilities,
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
}

/// The tenant selected for this session, if any.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SelectedTenant {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub role: Option<TenantRole>,
    #[serde(default)]
    pub authz_version: Option<i64>,
    #[serde(default)]
    pub membership_version: Option<i64>,
}

/// A membership is an identity relationship, not a global user role.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TenantMembership {
    pub tenant_id: String,
    #[serde(default)]
    pub tenant_name: Option<String>,
    pub role: TenantRole,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub version: Option<i64>,
}

/// Returned permissions are authoritative presentation data for the console.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct SessionCapabilities {
    #[serde(default)]
    pub platform: Vec<String>,
    #[serde(default)]
    pub tenant: Vec<String>,
}

/// 忘记密码请求
#[derive(Debug, Clone, Serialize)]
pub struct ForgotPasswordRequest {
    pub name: String,
    pub email: String,
}

impl ForgotPasswordRequest {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
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
