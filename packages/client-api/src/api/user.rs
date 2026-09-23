//! 用户自服务模块
//!
//! 处理当前用户信息获取、资料更新、密码修改等

use crate::client::ApiClient;
use crate::error::Result;
use keycompute_types::{PlatformRole, UserStatus};
use serde::{Deserialize, Serialize};

use super::auth::{SelectedTenant, SessionCapabilities, TenantMembership};

pub use super::common::MessageResponse;

/// 用户 API 客户端
#[derive(Debug, Clone)]
pub struct UserApi {
    client: ApiClient,
}

impl UserApi {
    /// 创建新的用户 API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取当前用户信息
    pub async fn get_current_user(&self, token: &str) -> Result<CurrentUserResponse> {
        self.client.get_json_fresh("/api/v1/me", Some(token)).await
    }

    /// 更新个人资料
    pub async fn update_profile(
        &self,
        req: &UpdateProfileRequest,
        token: &str,
    ) -> Result<CurrentUserResponse> {
        self.client
            .put_json("/api/v1/me/profile", req, Some(token))
            .await
    }

    /// 修改密码
    pub async fn change_password(
        &self,
        req: &ChangePasswordRequest,
        token: &str,
    ) -> Result<MessageResponse> {
        self.client
            .put_json("/api/v1/me/password", req, Some(token))
            .await
    }
}

/// 当前用户响应
#[derive(Debug, Clone, Deserialize)]
pub struct CurrentUserResponse {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    #[serde(default)]
    pub platform_role: Option<PlatformRole>,
    #[serde(default)]
    pub status: Option<UserStatus>,
    #[serde(default)]
    pub token_version: Option<i32>,
    #[serde(default)]
    pub memberships: Vec<TenantMembership>,
    #[serde(default)]
    pub selected_tenant: Option<SelectedTenant>,
    #[serde(default)]
    pub capabilities: SessionCapabilities,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// 更新资料请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
}

impl UpdateProfileRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// 修改密码请求
#[derive(Debug, Clone, Serialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

impl ChangePasswordRequest {
    pub fn new(current_password: impl Into<String>, new_password: impl Into<String>) -> Self {
        Self {
            current_password: current_password.into(),
            new_password: new_password.into(),
        }
    }
}
