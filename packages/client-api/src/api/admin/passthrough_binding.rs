//! Account-to-tenant passthrough grants.
//!
//! A grant is deliberately not model-shaped: every model declared by the
//! account is exposed dynamically by the backend.  Keep this module separate
//! Account grants are distinct from diagnostic per-model health observations.

use serde::{Deserialize, Serialize};

use crate::api::common::encode_query_value;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PassthroughBindingInfo {
    pub id: String,
    pub account_id: String,
    pub account_name: String,
    pub tenant_id: String,
    pub tenant_name: String,
    pub provider: String,
    #[serde(default)]
    pub is_global: bool,
    #[serde(default)]
    pub pool_enabled: bool,
    pub revision: i64,
    #[serde(default)]
    pub models_supported: Vec<String>,
    #[serde(default)]
    pub health_status: Option<String>,
    #[serde(default)]
    pub health_reason_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PassthroughBindingQueryParams {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub search: Option<String>,
}

impl PassthroughBindingQueryParams {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }
    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = Some(page_size);
        self
    }
    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }
    pub fn to_query_string(&self) -> String {
        let mut values = Vec::new();
        if let Some(page) = self.page {
            values.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            values.push(format!("page_size={page_size}"));
        }
        if let Some(search) = &self.search
            && !search.is_empty()
        {
            values.push(format!("search={}", encode_query_value(search)));
        }
        values.join("&")
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PassthroughBindingPage {
    #[serde(default)]
    pub bindings: Vec<PassthroughBindingInfo>,
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
pub struct CreatePassthroughBindingRequest {
    pub account_id: String,
    pub tenant_id: String,
    #[serde(default)]
    pub is_global: bool,
    #[serde(default)]
    pub pool_enabled: bool,
}

impl CreatePassthroughBindingRequest {
    pub fn new(account_id: impl Into<String>, tenant_id: impl Into<String>) -> Self {
        Self {
            account_id: account_id.into(),
            tenant_id: tenant_id.into(),
            is_global: false,
            pool_enabled: false,
        }
    }
    pub fn with_is_global(mut self, value: bool) -> Self {
        self.is_global = value;
        self
    }
    pub fn with_pool_enabled(mut self, value: bool) -> Self {
        self.pool_enabled = value;
        self
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdatePassthroughBindingRequest {
    pub expected_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_global: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_enabled: Option<bool>,
}

impl UpdatePassthroughBindingRequest {
    pub fn new(expected_revision: i64) -> Self {
        Self {
            expected_revision,
            ..Self::default()
        }
    }
    pub fn with_account_id(mut self, value: impl Into<String>) -> Self {
        self.account_id = Some(value.into());
        self
    }
    pub fn with_tenant_id(mut self, value: impl Into<String>) -> Self {
        self.tenant_id = Some(value.into());
        self
    }
    pub fn with_is_global(mut self, value: bool) -> Self {
        self.is_global = Some(value);
        self
    }
    pub fn with_pool_enabled(mut self, value: bool) -> Self {
        self.pool_enabled = Some(value);
        self
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PassthroughBindingProbeRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PassthroughBindingProbeResponse {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub reason_code: Option<String>,
    #[serde(default)]
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PassthroughAccountOption {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub pool_enabled: bool,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PassthroughAccountOptions {
    #[serde(default)]
    pub accounts: Vec<PassthroughAccountOption>,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub total_pages: u32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PassthroughAccountOptionsQuery {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub search: Option<String>,
}
impl PassthroughAccountOptionsQuery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_page(mut self, v: u32) -> Self {
        self.page = Some(v);
        self
    }
    pub fn with_page_size(mut self, v: u32) -> Self {
        self.page_size = Some(v);
        self
    }
    pub fn with_search(mut self, v: impl Into<String>) -> Self {
        self.search = Some(v.into());
        self
    }
    pub fn to_query_string(&self) -> String {
        let mut v = Vec::new();
        if let Some(x) = self.page {
            v.push(format!("page={x}"));
        }
        if let Some(x) = self.page_size {
            v.push(format!("page_size={x}"));
        }
        if let Some(x) = &self.search
            && !x.is_empty()
        {
            v.push(format!("search={}", encode_query_value(x)));
        }
        v.join("&")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_do_not_emit_model_or_enabled_fields() {
        let value = serde_json::to_value(CreatePassthroughBindingRequest::new("a", "t")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"account_id":"a","tenant_id":"t","is_global":false,"pool_enabled":false})
        );
        assert!(!value.as_object().unwrap().contains_key("model"));
        assert!(!value.as_object().unwrap().contains_key("enabled"));
        let update = serde_json::to_value(UpdatePassthroughBindingRequest::new(9)).unwrap();
        assert_eq!(update, serde_json::json!({"expected_revision": 9}));
        assert!(!update.as_object().unwrap().contains_key("model"));
        assert!(!update.as_object().unwrap().contains_key("enabled"));
    }
}
