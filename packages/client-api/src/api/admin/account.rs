//! 渠道账号管理相关类型

use serde::{Deserialize, Serialize};

use crate::api::common::encode_query_value;

/// A tenant-scoped model binding.  The server deliberately returns only
/// non-secret account metadata; credentials and endpoints never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelBindingInfo {
    pub id: String,
    pub tenant_id: String,
    pub api_capability: String,
    pub model: String,
    pub account_id: String,
    #[serde(default)]
    pub account_name: Option<String>,
    pub enabled: bool,
    pub revision: i64,
    #[serde(default)]
    pub health_status: Option<String>,
    #[serde(default)]
    pub health_reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ModelBindingQueryParams {
    pub tenant_id: Option<String>,
    pub model: Option<String>,
    pub api_capability: Option<String>,
    pub enabled: Option<bool>,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

impl ModelBindingQueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_api_capability(mut self, capability: impl Into<String>) -> Self {
        self.api_capability = Some(capability.into());
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
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

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(value) = &self.tenant_id {
            params.push(format!("tenant_id={}", encode_query_value(value)));
        }
        if let Some(value) = &self.model {
            params.push(format!("model={}", encode_query_value(value)));
        }
        if let Some(value) = &self.api_capability {
            params.push(format!("api_capability={}", encode_query_value(value)));
        }
        if let Some(value) = self.enabled {
            params.push(format!("enabled={value}"));
        }
        if let Some(value) = self.page {
            params.push(format!("page={value}"));
        }
        if let Some(value) = self.page_size {
            params.push(format!("page_size={value}"));
        }
        params.join("&")
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelBindingPage {
    #[serde(default)]
    pub bindings: Vec<ModelBindingInfo>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub total_pages: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateModelBindingRequest {
    pub tenant_id: String,
    pub api_capability: String,
    pub model: String,
    pub account_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

impl CreateModelBindingRequest {
    pub fn new(
        tenant_id: impl Into<String>,
        model: impl Into<String>,
        account_id: impl Into<String>,
    ) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            api_capability: "chat_completions".to_string(),
            model: model.into(),
            account_id: account_id.into(),
            enabled: None,
        }
    }

    pub fn with_api_capability(mut self, capability: impl Into<String>) -> Self {
        self.api_capability = capability.into();
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateModelBindingRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Required by the server for optimistic-concurrency updates.
    pub expected_revision: i64,
}

impl UpdateModelBindingRequest {
    pub fn new(expected_revision: i64) -> Self {
        Self {
            expected_revision,
            ..Self::default()
        }
    }

    pub fn with_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ModelBindingProbeRequest {
    /// Explicitly supplied even when probing a binding, preventing accidental
    /// fallback to an account's first configured model.
    pub model: String,
    #[serde(default = "default_chat_capability")]
    pub api_capability: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

fn default_chat_capability() -> String {
    "chat_completions".to_string()
}

impl ModelBindingProbeRequest {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_capability: default_chat_capability(),
            timeout_ms: None,
        }
    }

    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ModelBindingProbeResponse {
    #[serde(default)]
    pub binding_id: Option<String>,
    pub account_id: String,
    pub model: String,
    pub status: String,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub checked_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub generation: Option<i64>,
}

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
    /// 所属租户是否处于活跃状态
    #[serde(default = "default_tenant_active")]
    pub tenant_active: bool,
    pub name: String,
    pub provider: String,
    pub api_key_preview: String,
    /// 自定义 Base URL（Provider 端点地址）
    pub api_base: Option<String>,
    pub models: Vec<String>,
    pub api_capabilities: Vec<String>,
    pub rpm_limit: i32,
    /// 账号 TPM 上限。旧服务端响应缺少该字段时使用数据库默认值。
    #[serde(default = "default_tpm_limit")]
    pub tpm_limit: i32,
    pub current_rpm: i32,
    pub is_active: bool,
    pub is_healthy: bool,
    #[serde(default = "default_health_status")]
    pub health_status: String,
    #[serde(default)]
    pub health_penalty: i32,
    #[serde(default)]
    pub health_reason: Option<String>,
    #[serde(default)]
    pub routing_eligible: bool,
    #[serde(default)]
    pub last_probe_at: Option<String>,
    #[serde(default)]
    pub last_probe_status: Option<String>,
    #[serde(default)]
    pub last_probe_error_code: Option<String>,
    pub priority: i32,
    /// 可见性：'tenant' = 仅本租户可见，'global' = 所有租户可见
    pub visibility: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
}

fn default_health_status() -> String {
    "unknown".to_string()
}

fn default_tenant_active() -> bool {
    true
}

fn default_tpm_limit() -> i32 {
    100_000
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<i32>,
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
            priority: None,
            rpm_limit: None,
            tpm_limit: None,
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

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = Some(priority);
        self
    }

    pub fn with_rpm_limit(mut self, rpm_limit: i32) -> Self {
        self.rpm_limit = Some(rpm_limit);
        self
    }

    pub fn with_tpm_limit(mut self, tpm_limit: i32) -> Self {
        self.tpm_limit = Some(tpm_limit);
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tpm_limit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
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

    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = Some(priority);
        self
    }

    pub fn with_api_capabilities(mut self, api_capabilities: Vec<String>) -> Self {
        self.api_capabilities = Some(api_capabilities);
        self
    }

    pub fn with_models(mut self, models: Vec<String>) -> Self {
        self.models = Some(models);
        self
    }

    pub fn with_rpm_limit(mut self, rpm_limit: i32) -> Self {
        self.rpm_limit = Some(rpm_limit);
        self
    }

    pub fn with_tpm_limit(mut self, tpm_limit: i32) -> Self {
        self.tpm_limit = Some(tpm_limit);
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
        AccountInfo, AccountQueryParams, AccountRefreshResponse, AccountTestResponse,
        CreateAccountRequest, CreateModelBindingRequest, ModelBindingInfo,
        ModelBindingProbeRequest, ModelBindingQueryParams, UpdateAccountRequest,
        UpdateModelBindingRequest,
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
            ])
            .with_priority(10);

        let value = serde_json::to_value(request).unwrap();
        assert_eq!(
            value["api_capabilities"],
            serde_json::json!(["chat_completions", "responses"])
        );
        assert_eq!(value["priority"], serde_json::json!(10));
    }

    #[test]
    fn account_requests_serialize_models_and_rate_limits() {
        let create = CreateAccountRequest::new("OpenAI", "openai", "sk-test")
            .with_models(vec!["gpt-test".to_string()])
            .with_rpm_limit(12)
            .with_tpm_limit(34);
        let create_value = serde_json::to_value(create).unwrap();
        assert_eq!(create_value["rpm_limit"], serde_json::json!(12));
        assert_eq!(create_value["tpm_limit"], serde_json::json!(34));

        let update = UpdateAccountRequest::new()
            .with_models(vec!["gpt-test".to_string(), "gpt-mini".to_string()])
            .with_rpm_limit(56)
            .with_tpm_limit(78);
        let update_value = serde_json::to_value(update).unwrap();
        assert_eq!(
            update_value["models"],
            serde_json::json!(["gpt-test", "gpt-mini"])
        );
        assert_eq!(update_value["rpm_limit"], serde_json::json!(56));
        assert_eq!(update_value["tpm_limit"], serde_json::json!(78));
    }

    #[test]
    fn account_info_defaults_tpm_for_older_responses() {
        let value = serde_json::json!({
            "id": "account_001",
            "tenant_id": "tenant_001",
            "name": "OpenAI",
            "provider": "openai",
            "api_key_preview": "sk-...",
            "api_base": null,
            "models": ["gpt-test"],
            "api_capabilities": ["chat_completions"],
            "rpm_limit": 60,
            "current_rpm": 0,
            "is_active": true,
            "is_healthy": true,
            "priority": 0,
            "visibility": "tenant",
            "created_at": "2024-01-01T00:00:00Z",
            "last_used_at": null
        });
        let account: AccountInfo = serde_json::from_value(value).unwrap();
        assert_eq!(account.tpm_limit, 100_000);
    }

    #[test]
    fn model_binding_query_escapes_filters_and_bounds_pages() {
        let query = ModelBindingQueryParams::new()
            .with_model("gpt/mini & test")
            .with_api_capability("chat_completions")
            .with_enabled(true)
            .with_page(2)
            .with_page_size(25)
            .to_query_string();
        assert_eq!(
            query,
            "model=gpt%2Fmini%20%26%20test&api_capability=chat_completions&enabled=true&page=2&page_size=25"
        );
    }

    #[test]
    fn model_binding_requests_keep_optimistic_revision_and_explicit_model_probe() {
        let create =
            CreateModelBindingRequest::new("tenant-1", "gpt-mini", "account-a").with_enabled(true);
        let create_value = serde_json::to_value(create).unwrap();
        assert_eq!(create_value["api_capability"], "chat_completions");
        assert_eq!(create_value["model"], "gpt-mini");

        let update = UpdateModelBindingRequest::new(7)
            .with_account_id("account-b")
            .with_enabled(false);
        let update_value = serde_json::to_value(update).unwrap();
        assert_eq!(update_value["expected_revision"], 7);
        assert_eq!(update_value["account_id"], "account-b");

        let probe = ModelBindingProbeRequest::new("gpt-mini").with_timeout_ms(2_000);
        let probe_value = serde_json::to_value(probe).unwrap();
        assert_eq!(probe_value["model"], "gpt-mini");
        assert_eq!(probe_value["api_capability"], "chat_completions");
        assert_eq!(probe_value["timeout_ms"], 2_000);
    }

    #[test]
    fn model_binding_info_accepts_optional_health_metadata() {
        let binding: ModelBindingInfo = serde_json::from_value(serde_json::json!({
            "id": "binding-1",
            "revision": 1,
            "tenant_id": "tenant-1",
            "api_capability": "chat_completions",
            "model": "gpt-mini",
            "account_id": "account-a",
            "enabled": true,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(binding.revision, 1);
        assert!(binding.health_status.is_none());
    }
    #[test]
    fn model_binding_info_requires_authoritative_revision() {
        let value = serde_json::json!({
            "id":"binding-1", "tenant_id":"tenant-1", "model":"gpt-mini",
            "api_capability":"chat_completions", "account_id":"account-a", "enabled":true,
            "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z"
        });
        assert!(serde_json::from_value::<ModelBindingInfo>(value).is_err());
    }
}
