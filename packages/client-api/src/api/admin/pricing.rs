//! 定价管理相关类型

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

use super::ModelAccessMode;

const fn default_pricing_version() -> i64 {
    1
}

fn deserialize_f64_or_string<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(number) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| D::Error::custom("expected a finite number")),
        serde_json::Value::String(raw) => {
            let value = raw
                .parse::<f64>()
                .map_err(|_| D::Error::custom("expected a decimal number"))?;
            if value.is_finite() {
                Ok(value)
            } else {
                Err(D::Error::custom("expected a finite number"))
            }
        }
        _ => Err(D::Error::custom("expected a decimal number")),
    }
}

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
    #[serde(default = "default_pricing_version")]
    pub version: i64,
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
    #[serde(default = "default_pricing_version")]
    pub version: i64,
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
    #[serde(default = "default_pricing_version")]
    pub version: i64,
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

    /// Set the tenant that owns this pricing model. Global pricing rows are
    /// managed by the server and cannot be created through the admin API.
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }
}

/// 更新定价请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdatePricingRequest {
    pub input_price_per_1k: Option<String>,
    pub output_price_per_1k: Option<String>,
    pub effective_until: Option<String>,
    pub expected_version: Option<i64>,
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

    pub fn with_expected_version(mut self, version: i64) -> Self {
        self.expected_version = Some(version);
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
    #[serde(default = "default_pricing_version")]
    pub version: i64,
}

/// 计算费用请求
#[derive(Debug, Clone, Default, Serialize)]
pub struct CalculateCostRequest {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Optional for wire compatibility; omitted requests use AccountPool on the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModelAccessMode>,
}

impl CalculateCostRequest {
    pub fn new(model: impl Into<String>, input_tokens: i64, output_tokens: i64) -> Self {
        Self {
            model: model.into(),
            input_tokens,
            output_tokens,
            tenant_id: None,
            mode: None,
        }
    }

    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    pub fn with_mode(mut self, mode: ModelAccessMode) -> Self {
        self.mode = Some(mode);
        self
    }
}

/// 费用计算响应
#[derive(Debug, Clone, Deserialize)]
pub struct CostCalculationResponse {
    pub model: String,
    #[serde(deserialize_with = "deserialize_f64_or_string")]
    pub input_cost: f64,
    #[serde(deserialize_with = "deserialize_f64_or_string")]
    pub output_cost: f64,
    #[serde(deserialize_with = "deserialize_f64_or_string")]
    pub total_cost: f64,
    pub currency: String,
}

#[cfg(test)]
mod tests {
    use super::{
        CalculateCostRequest, CostCalculationResponse, ModelAccessMode, PricingInfo,
        PricingQueryParams, UpdatePricingRequest,
    };

    #[test]
    fn pricing_query_serializes_server_side_search_and_pagination() {
        let query = PricingQueryParams::default()
            .with_search("gpt 4")
            .with_page(2)
            .with_page_size(50)
            .to_query_string();
        assert_eq!(query, "search=gpt%204&page=2&page_size=50");
    }

    #[test]
    fn pricing_mutations_carry_concurrency_and_tenant_context() {
        let create =
            super::CreatePricingRequest::new("gpt-4o", "provideraccount", "0.01", "0.02", "CNY")
                .with_tenant_id("11111111-1111-1111-1111-111111111111");
        assert_eq!(
            serde_json::to_value(create).unwrap()["tenant_id"],
            "11111111-1111-1111-1111-111111111111"
        );

        let update = UpdatePricingRequest::new().with_expected_version(7);
        assert_eq!(serde_json::to_value(update).unwrap()["expected_version"], 7);

        let calculate = CalculateCostRequest {
            model: "gpt-4o".to_string(),
            input_tokens: 1,
            output_tokens: 2,
            tenant_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
            mode: Some(ModelAccessMode::NodeDispatch),
        };
        let calculate = serde_json::to_value(calculate).unwrap();
        assert_eq!(
            calculate["tenant_id"],
            "11111111-1111-1111-1111-111111111111"
        );
        assert_eq!(calculate["mode"], "node_dispatch");
        assert_eq!(
            serde_json::to_value(CalculateCostRequest::new("gemma3:270m", 1, 2))
                .unwrap()
                .get("mode"),
            None
        );
    }

    #[test]
    fn cost_response_accepts_decimal_strings_and_numbers() {
        let from_string: super::CostCalculationResponse =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-4o",
                "input_cost": "0.01",
                "output_cost": "0.02",
                "total_cost": "0.03",
                "currency": "CNY"
            }))
            .unwrap();
        assert!((from_string.total_cost - 0.03).abs() < f64::EPSILON);

        let from_number: super::CostCalculationResponse =
            serde_json::from_value(serde_json::json!({
                "model": "gpt-4o",
                "input_cost": 0.01,
                "output_cost": 0.02,
                "total_cost": 0.03,
                "currency": "CNY"
            }))
            .unwrap();
        assert!((from_number.total_cost - 0.03).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_response_rejects_non_finite_values_and_defaults_missing_versions() {
        let non_finite = serde_json::from_value::<CostCalculationResponse>(serde_json::json!({
            "model": "gpt-4o",
            "input_cost": "NaN",
            "output_cost": "0.02",
            "total_cost": "0.02",
            "currency": "CNY"
        }));
        assert!(non_finite.is_err());

        let pricing: PricingInfo = serde_json::from_value(serde_json::json!({
            "id": "pricing-1",
            "tenant_id": null,
            "model_name": "gpt-4o",
            "billing_dimension": "provideraccount",
            "input_price_per_1k": "0.01",
            "output_price_per_1k": "0.02",
            "currency": "CNY",
            "is_default": true,
            "is_effective": true,
            "effective_from": "2026-01-01T00:00:00Z",
            "effective_until": null,
            "created_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(pricing.version, 1);
    }
}
