//! 定价管理相关类型

use serde::{Deserialize, Serialize};

/// 定价信息
#[derive(Debug, Clone, Deserialize)]
pub struct PricingInfo {
    pub id: String,
    pub tenant_id: Option<String>,
    pub model_name: String,
    pub billing_dimension: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub currency: String,
    pub is_default: bool,
    pub is_effective: bool,
    pub effective_from: String,
    pub effective_until: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PricingPage {
    pub pricing: Vec<PricingInfo>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PricingQueryParams {
    pub search: Option<String>,
    pub page: Option<u64>,
    pub page_size: Option<u64>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

impl PricingQueryParams {
    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn with_page(mut self, page: u64) -> Self {
        self.page = Some(page);
        self
    }

    pub fn with_page_size(mut self, page_size: u64) -> Self {
        self.page_size = Some(page_size);
        self
    }

    pub fn to_query_string(&self) -> String {
        let mut params = Vec::new();
        if let Some(search) = self.search.as_deref().filter(|value| !value.is_empty()) {
            params.push(format!(
                "search={}",
                crate::api::common::encode_query_value(search)
            ));
        }
        if let Some(page) = self.page {
            params.push(format!("page={page}"));
        }
        if let Some(page_size) = self.page_size {
            params.push(format!("page_size={page_size}"));
        }
        if let Some(limit) = self.limit {
            params.push(format!("limit={limit}"));
        }
        if let Some(offset) = self.offset {
            params.push(format!("offset={offset}"));
        }
        params.join("&")
    }
}

/// 创建定价请求
#[derive(Debug, Clone, Serialize)]
pub struct CreatePricingRequest {
    pub model_name: String,
    #[serde(rename = "billing_dimension")]
    pub billing_dimension: String,
    #[serde(rename = "tenant_id")]
    pub tenant_id: Option<String>,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub currency: String,
    pub is_default: bool,
    pub effective_from: Option<String>,
    pub effective_until: Option<String>,
}

/// 创建定价响应
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePricingResponse {
    pub success: bool,
    pub message: String,
    pub pricing_id: String,
    pub model_name: String,
    pub billing_dimension: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub is_default: bool,
}

/// 更新定价响应
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub pricing_id: String,
}

impl CreatePricingRequest {
    pub fn new(
        model_name: impl Into<String>,
        billing_dimension: impl Into<String>,
        input_price_per_1k: impl Into<String>,
        output_price_per_1k: impl Into<String>,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            model_name: model_name.into(),
            billing_dimension: billing_dimension.into(),
            tenant_id: None,
            input_price_per_1k: input_price_per_1k.into(),
            output_price_per_1k: output_price_per_1k.into(),
            currency: currency.into(),
            is_default: false,
            effective_from: None,
            effective_until: None,
        }
    }
}

/// 更新定价请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdatePricingRequest {
    pub input_price_per_1k: Option<String>,
    pub output_price_per_1k: Option<String>,
    pub effective_until: Option<String>,
}

impl UpdatePricingRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_input_price_per_1k(mut self, price: impl Into<String>) -> Self {
        self.input_price_per_1k = Some(price.into());
        self
    }

    pub fn with_output_price_per_1k(mut self, price: impl Into<String>) -> Self {
        self.output_price_per_1k = Some(price.into());
        self
    }
}

/// 设置默认定价请求
#[derive(Debug, Clone, Serialize)]
pub struct SetDefaultPricingRequest {
    pub model_ids: Vec<String>,
}

/// 设为默认定价响应
#[derive(Debug, Clone, Deserialize)]
pub struct MakeDefaultPricingResponse {
    pub success: bool,
    pub message: String,
    pub pricing_id: String,
}

/// 计算费用请求
#[derive(Debug, Clone, Serialize)]
pub struct CalculateCostRequest {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// 费用计算响应
#[derive(Debug, Clone, Deserialize)]
pub struct CostCalculationResponse {
    pub model: String,
    pub input_cost: f64,
    pub output_cost: f64,
    pub total_cost: f64,
    pub currency: String,
}

#[cfg(test)]
mod tests {
    use super::PricingQueryParams;

    #[test]
    fn pricing_query_serializes_server_side_search_and_pagination() {
        let query = PricingQueryParams::default()
            .with_search("gpt 4")
            .with_page(2)
            .with_page_size(50)
            .to_query_string();
        assert_eq!(query, "search=gpt%204&page=2&page_size=50");
    }
}
