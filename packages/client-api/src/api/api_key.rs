//! API Key 管理模块
//!
//! 处理用户 API Key 的创建、查询和删除

use crate::client::ApiClient;
use crate::error::Result;
use serde::{Deserialize, Serialize};

pub use super::common::MessageResponse;

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
    ///
    /// # 参数
    /// - `include_revoked`: 是否包含已撤销的 Key（默认 false）
    pub async fn list_my_api_keys(
        &self,
        include_revoked: bool,
        token: &str,
    ) -> Result<Vec<ApiKeyInfo>> {
        let path = if include_revoked {
            "/api/v1/keys?include_revoked=true"
        } else {
            "/api/v1/keys"
        };
        self.client.get_json_fresh(path, Some(token)).await
    }

    /// 分页获取我的 API Keys。显式传入分页参数时服务端返回分页对象。
    pub async fn list_my_api_keys_page(
        &self,
        params: &ApiKeyQueryParams,
        token: &str,
    ) -> Result<ApiKeyPage> {
        let mut query = params.to_query_string();
        if !query.is_empty() {
            query.insert(0, '?');
        }
        self.client
            .get_json_fresh(&format!("/api/v1/keys{query}"), Some(token))
            .await
    }

    /// 创建新的 API Key
    pub async fn create_api_key(
        &self,
        req: &CreateApiKeyRequest,
        token: &str,
    ) -> Result<CreateApiKeyResponse> {
        if req.name.trim().is_empty()
            || req.name.chars().count() > 255
            || req.name.chars().any(char::is_control)
        {
            return Err(crate::ClientError::Config(
                "Use a key name of 1–255 characters without control characters".into(),
            ));
        }
        let response: CreateApiKeyResponse = self
            .client
            .post_json("/api/v1/keys", req, Some(token))
            .await
            .map_err(super::common::one_time_key_error)?;
        if !response.success
            || response.name != req.name
            || response.never_expires != req.never_expires
            || uuid::Uuid::parse_str(&response.id)
                .ok()
                .is_none_or(|id| id.is_nil())
            || !super::common::valid_one_time_key(&response.api_key)
            || (req.never_expires && response.expires_at.is_some())
            || (!req.never_expires
                && response
                    .expires_at
                    .as_ref()
                    .is_none_or(|v| v.is_empty() || v.len() > 128))
        {
            return Err(super::common::one_time_key_error(
                crate::ClientError::InvalidResponse("Key creation response is inconsistent".into()),
            ));
        }
        Ok(response)
    }

    /// 删除 API Key
    pub async fn delete_api_key(&self, id: &str, token: &str) -> Result<MessageResponse> {
        self.client
            .delete_json(&format!("/api/v1/keys/{}", id), Some(token))
            .await
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApiKeyQueryParams {
    pub include_revoked: Option<bool>,
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl ApiKeyQueryParams {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_include_revoked(mut self, value: bool) -> Self {
        self.include_revoked = Some(value);
        self
    }
    pub fn with_page(mut self, value: i32) -> Self {
        self.page = Some(value);
        self
    }
    pub fn with_page_size(mut self, value: i32) -> Self {
        self.page_size = Some(value);
        self
    }
    pub fn to_query_string(&self) -> String {
        let mut values = Vec::new();
        if let Some(value) = self.include_revoked {
            values.push(format!("include_revoked={value}"));
        }
        if let Some(value) = self.page {
            values.push(format!("page={value}"));
        }
        if let Some(value) = self.page_size {
            values.push(format!("page_size={value}"));
        }
        if let Some(value) = self.limit {
            values.push(format!("limit={value}"));
        }
        if let Some(value) = self.offset {
            values.push(format!("offset={value}"));
        }
        values.join("&")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyPage {
    pub keys: Vec<ApiKeyInfo>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub total_pages: i64,
}

/// API Key 信息
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: String,
    pub key_preview: String,
    /// 是否活跃（后端返回 is_active，前端转换为 !revoked）
    #[serde(rename = "is_active")]
    pub is_active: bool,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
}

impl ApiKeyInfo {
    /// 返回是否已撤销（与 is_active 相反）
    pub fn revoked(&self) -> bool {
        !self.is_active
    }
}

/// 创建 API Key 请求
#[derive(Debug, Clone, Serialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    /// False retains the server's fixed 180-day policy. Custom dates belong to
    /// tenant issuance/metadata APIs, not this personal creation endpoint.
    pub never_expires: bool,
}

impl CreateApiKeyRequest {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            never_expires: false,
        }
    }

    pub fn with_never_expires(mut self, never_expires: bool) -> Self {
        self.never_expires = never_expires;
        self
    }
}

/// 创建 API Key 响应（包含完整 key，仅创建时返回一次）
#[derive(Clone, Deserialize)]
pub struct CreateApiKeyResponse {
    /// 是否成功
    pub success: bool,
    /// 消息
    pub message: Option<String>,
    /// API Key ID（后端字段名为 key_id）
    #[serde(rename = "key_id")]
    pub id: String,
    /// API Key 名称
    pub name: String,
    /// 完整的 API Key（后端字段名为 key）
    #[serde(rename = "key")]
    pub api_key: String,
    /// 过期时间
    pub expires_at: Option<String>,
    /// 创建时间
    pub created_at: String,
    /// 是否永不过期
    pub never_expires: bool,
}

impl std::fmt::Debug for CreateApiKeyResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateApiKeyResponse")
            .field("id", &self.id)
            .field("success", &self.success)
            .field("api_key", &"[REDACTED]")
            .field("message", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
