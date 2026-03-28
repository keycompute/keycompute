//! API Key 管理模块
//!
//! 处理用户 API Key 的创建、查询和删除

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

/// API Key API 客户端
#[derive(Debug, Clone)]
pub struct ApiKeyApi {
    client: ApiClient,
}

impl ApiKeyApi {
    /// 创建新的 API Key API 客户端
    pub fn new(client: &ApiClient) -> Self {
        Self {
            client: client.clone(),
        }
    }

    /// 获取我的 API Keys 列表
    pub async fn list_my_api_keys(&self, token: &str) -> Result<Vec<ApiKeyInfo>> {
        self.client.get_json("/api/v1/keys", Some(token)).await
    }

    /// 创建新的 API Key
    pub async fn create_api_key(
        &self,
        req: &CreateApiKeyRequest,
        token: &str,
    ) -> Result<CreateApiKeyResponse> {
        self.client
            .post_json("/api/v1/keys", req, Some(token))
            .await
    }

    /// 删除 API Key
    pub async fn delete_api_key(&self, id: &str, token: &str) -> Result<MessageResponse> {
        self.client
            .delete_json(&format!("/api/v1/keys/{}", id), Some(token))
            .await
    }
}

/// API Key 信息
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub key_preview: String,
    pub revoked: bool,
    pub revoked_at: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub created_at: String,
}

/// 创建 API Key 请求
#[derive(Debug, Clone, Serialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub expires_at: Option<String>,
}

impl CreateApiKeyRequest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            expires_at: None,
        }
    }

    pub fn with_expires_at(mut self, expires_at: impl Into<String>) -> Self {
        self.expires_at = Some(expires_at.into());
        self
    }
}

/// 创建 API Key 响应（包含完整 key，仅创建时返回一次）
#[derive(Debug, Clone, Deserialize)]
pub struct CreateApiKeyResponse {
    pub id: String,
    pub name: String,
    pub api_key: String,
    pub key_preview: String,
    pub expires_at: Option<String>,
    pub created_at: String,
}

/// 通用消息响应
#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    pub message: String,
}
