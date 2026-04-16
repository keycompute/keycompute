//! 定价管理相关类型

use serde::{Deserialize, Serialize};

/// 定价信息
#[derive(Debug, Clone, Deserialize)]
pub struct PricingInfo {
    pub id: String,
    pub model_name: String,
    pub provider: String,
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub currency: String,
    pub is_default: bool,
    pub is_effective: bool,
    pub created_at: String,
}

/// 创建定价请求
#[derive(Debug, Clone, Serialize)]
pub struct CreatePricingRequest {
    pub model_name: String,
    pub provider: String,
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
    pub provider: String,
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
        provider: impl Into<String>,
        input_price_per_1k: impl Into<String>,
        output_price_per_1k: impl Into<String>,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            model_name: model_name.into(),
            provider: provider.into(),
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
