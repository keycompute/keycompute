//! 渠道账号管理相关类型

use serde::{Deserialize, Serialize};

use crate::api::common::encode_query_value;

/// 账号查询参数
#[derive(Debug, Clone, Serialize, Default)]
pub struct AccountQueryParams {
    pub search: Option<String>,
    pub provider: Option<String>,
    pub status: Option<String>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
}

impl AccountQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn with_limit(mut self, limit: i32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: i32) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    pub(crate) fn has_explicit_pagination(&self) -> bool {
        self.page.is_some()
            || self.page_size.is_some()
            || self.limit.is_some()
            || self.offset.is_some()
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(ref search) = self.search {
            params.push(format!("search={}", encode_query_value(search)));
        }
        if let Some(ref provider) = self.provider {
            params.push(format!("provider={}", encode_query_value(provider)));
        }
        if let Some(ref status) = self.status {
            params.push(format!("status={}", encode_query_value(status)));
        }
        if let Some(page) = self.page {
            params.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={page_size}"));
        }
        if let Some(limit) = self.limit {
            params.push(format!("limit={}", limit));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={}", offset));
        }
        params.join("&")
    }
}

/// 账号信息
#[derive(Debug, Clone, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    /// 所属租户 ID
    pub tenant_id: String,
    pub name: String,
    pub provider: String,
    pub api_key_preview: String,
    /// 自定义 Base URL（Provider 端点地址）
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub api_capabilities: Vec<String>,
    pub rpm_limit: i32,
    pub current_rpm: i32,
    pub is_active: bool,
    pub is_healthy: bool,
    pub priority: i32,
    /// 可见性：'tenant' = 仅本租户可见，'global' = 所有租户可见
    pub visibility: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AccountPage {
    pub accounts: Vec<AccountInfo>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub total_pages: u32,
}

/// 创建账号请求
#[derive(Debug, Clone, Serialize)]
pub struct CreateAccountRequest {
    pub name: String,
    pub provider: String,
    pub api_key: String,
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub api_capabilities: Option<Vec<String>>,
}

impl CreateAccountRequest {
    pub fn new(
        name: impl Into<String>,
        provider: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            provider: provider.into(),
            api_key: api_key.into(),
            api_base: None,
            models: Vec::new(),
            api_capabilities: None,
        }
    }

    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = Some(api_base.into());
        self
    }

    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = models;
        self
    }

    pub fn with_api_capabilities(mut self, api_capabilities: Vec<String>) -> Self {
        self.api_capabilities = Some(api_capabilities);
        self
    }
}

/// 更新账号请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateAccountRequest {
    pub tenant_id: Option<String>,
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub api_capabilities: Option<Vec<String>>,
    pub is_active: Option<bool>,
    /// 可见性：'tenant' = 仅本租户可见，'global' = 所有租户可见
    pub visibility: Option<String>,
}

impl UpdateAccountRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_is_active(mut self, is_active: bool) -> Self {
        self.is_active = Some(is_active);
        self
    }

    pub fn with_api_capabilities(mut self, api_capabilities: Vec<String>) -> Self {
        self.api_capabilities = Some(api_capabilities);
        self
    }

    pub fn with_visibility(mut self, visibility: impl Into<String>) -> Self {
        self.visibility = Some(visibility.into());
        self
    }
}

/// 账号测试响应
#[derive(Debug, Clone, Deserialize)]
pub struct AccountTestResponse {
    pub success: bool,
    pub message: String,
    pub latency_ms: Option<i64>,
}

/// 账号模型刷新响应
#[derive(Debug, Clone, Deserialize)]
pub struct AccountRefreshResponse {
    pub success: bool,
    pub message: String,
    pub account_id: String,
    pub refreshed_by: String,
    pub previous_models: Vec<String>,
    pub updated_models: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        AccountQueryParams, AccountRefreshResponse, AccountTestResponse, CreateAccountRequest,
    };

    #[test]
    fn account_query_serializes_server_side_search_and_pagination() {
        let query = AccountQueryParams::new()
            .with_search("OpenAI & team")
            .with_page(3)
            .with_page_size(20)
            .to_query_string();
        assert_eq!(query, "search=OpenAI%20%26%20team&page=3&page_size=20");
    }

    #[test]
    fn account_test_response_reads_top_level_latency() {
        let response: AccountTestResponse = serde_json::from_str(
            r#"{
                "success": false,
                "message": "Account connection test failed",
                "latency_ms": 123,
                "test_result": {"is_healthy": false}
            }"#,
        )
        .unwrap();

        assert!(!response.success);
        assert_eq!(response.latency_ms, Some(123));
    }

    #[test]
    fn account_refresh_response_matches_server_payload() {
        let response: AccountRefreshResponse = serde_json::from_str(
            r#"{
                "success": true,
                "message": "Account refreshed",
                "account_id": "account-id",
                "refreshed_by": "admin-id",
                "previous_models": ["model-a"],
                "updated_models": ["model-b"]
            }"#,
        )
        .unwrap();

        assert!(response.success);
        assert_eq!(response.account_id, "account-id");
        assert_eq!(response.refreshed_by, "admin-id");
        assert_eq!(response.previous_models, ["model-a"]);
        assert_eq!(response.updated_models, ["model-b"]);
    }

    #[test]
    fn create_account_serializes_explicit_api_capabilities() {
        let request = CreateAccountRequest::new("OpenAI", "openai", "sk-test")
            .with_models(vec!["gpt-test".to_string()])
            .with_api_capabilities(vec![
                "chat_completions".to_string(),
                "responses".to_string(),
            ]);

        let value = serde_json::to_value(request).unwrap();
        assert_eq!(
            value["api_capabilities"],
            serde_json::json!(["chat_completions", "responses"])
        );
    }
}
