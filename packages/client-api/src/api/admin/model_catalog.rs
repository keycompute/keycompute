//! Product model-management catalog and binding setup options.
//!
//! These types are shared with the server so the console can render availability
//! and next actions without inventing provider credentials or health claims.

use keycompute_types::{BindingAccountOption, BindingAccountOptions, ModelCatalogPage};
use serde::Serialize;

use crate::api::common::encode_query_value;

pub use keycompute_types::{
    BindingAccountOption as SharedBindingAccountOption,
    BindingAccountOptions as SharedBindingAccountOptions, ModelAccessMode,
    ModelAccessMode as SharedModelAccessMode, ModelAvailability, ModelCatalogEntry,
    ModelCatalogPage as SharedModelCatalogPage,
};

#[derive(Debug, Clone, Serialize, Default)]
pub struct ModelCatalogQuery {
    pub mode: ModelAccessMode,
    pub tenant_id: Option<String>,
    pub protocol: Option<String>,
    pub capability: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub q: Option<String>,
}

impl ModelCatalogQuery {
    pub fn new(mode: ModelAccessMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    pub fn with_tenant_id(mut self, value: impl Into<String>) -> Self {
        self.tenant_id = Some(value.into());
        self
    }
    pub fn with_protocol(mut self, value: impl Into<String>) -> Self {
        self.protocol = Some(value.into());
        self
    }
    pub fn with_capability(mut self, value: impl Into<String>) -> Self {
        self.capability = Some(value.into());
        self
    }
    pub fn with_page(mut self, value: u64) -> Self {
        self.page = Some(value);
        self
    }
    pub fn with_page_size(mut self, value: u64) -> Self {
        self.page_size = Some(value);
        self
    }
    pub fn with_query(mut self, value: impl Into<String>) -> Self {
        self.q = Some(value.into());
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut values = vec![format!("mode={}", self.mode.as_str())];
        if let Some(value) = &self.tenant_id {
            values.push(format!("tenant_id={}", encode_query_value(value)));
        }
        if let Some(value) = &self.protocol {
            values.push(format!("protocol={}", encode_query_value(value)));
        }
        if let Some(value) = &self.capability {
            values.push(format!("capability={}", encode_query_value(value)));
        }
        if let Some(value) = self.page {
            values.push(format!("page={value}"));
        }
        if let Some(value) = self.page_size {
            values.push(format!("page_size={value}"));
        }
        if let Some(value) = &self.q {
            values.push(format!("q={}", encode_query_value(value)));
        }
        values.join("&")
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BindingOptionsQuery {
    pub tenant_id: Option<String>,
    pub q: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
}

impl BindingOptionsQuery {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_tenant_id(mut self, value: impl Into<String>) -> Self {
        self.tenant_id = Some(value.into());
        self
    }
    pub fn with_query(mut self, value: impl Into<String>) -> Self {
        self.q = Some(value.into());
        self
    }
    pub fn with_page(mut self, value: u64) -> Self {
        self.page = Some(value);
        self
    }
    pub fn with_page_size(mut self, value: u64) -> Self {
        self.page_size = Some(value);
        self
    }
    pub fn to_query_string(&self) -> String {
        let mut values = Vec::new();
        if let Some(value) = &self.tenant_id {
            values.push(format!("tenant_id={}", encode_query_value(value)));
        }
        if let Some(value) = &self.q {
            values.push(format!("q={}", encode_query_value(value)));
        }
        if let Some(value) = self.page {
            values.push(format!("page={value}"));
        }
        if let Some(value) = self.page_size {
            values.push(format!("page_size={value}"));
        }
        values.join("&")
    }
}

pub type ModelCatalog = ModelCatalogPage;
pub type BindingOptions = BindingAccountOptions;
pub type BindingOption = BindingAccountOption;
