//! 用户自服务模块
//!
//! 处理当前用户信息获取、资料更新、密码修改等

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

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
        self.client.get_json("/api/v1/me", Some(token)).await
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
    pub role: String,
    pub tenant_id: String,
    pub created_at: String,
    pub updated_at: String,
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

/// 通用消息响应
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub message: String,
}
