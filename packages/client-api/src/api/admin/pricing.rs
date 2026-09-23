//! 定价管理相关类型

use serde::{Deserialize, Deserializer, Serialize, de::Error as DeError};

use super::ModelAccessMode;

/// Explicit platform-management target. Global pricing is not a nil tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope_type", rename_all = "snake_case")]
pub enum PricingTarget {
    Platform,
    Tenant { tenant_id: uuid::Uuid },
}
impl PricingTarget {
    pub fn validate(self) -> crate::Result<()> {
        if matches!(self, Self::Tenant { tenant_id } if tenant_id.is_nil()) {
            return Err(crate::ClientError::Config(
                "A nonzero pricing tenant is required".into(),
            ));
        }
        Ok(())
    }
    pub fn query(self) -> crate::Result<String> {
        self.validate()?;
        Ok(match self {
            Self::Platform => "scope_type=platform".into(),
            Self::Tenant { tenant_id } => format!("scope_type=tenant&tenant_id={tenant_id}"),
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingScopeType {
    Platform,
    Tenant,
}

pub(super) fn pricing_id(id: &str) -> crate::Result<uuid::Uuid> {
    uuid::Uuid::parse_str(id)
        .ok()
        .filter(|id| !id.is_nil())
        .ok_or_else(|| {
            crate::ClientError::Config("A nonzero pricing resource ID is required".into())
        })
}
pub(super) fn invalid_pricing() -> crate::ClientError {
    crate::ClientError::InvalidResponse(
        "Pricing response identity, scope or revision is invalid".into(),
    )
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
    pub scope_type: PricingScopeType,
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
    pub version: i64,
}

impl PricingInfo {
    pub fn target(&self) -> crate::Result<PricingTarget> {
        match (self.scope_type, self.tenant_id.as_deref()) {
            (PricingScopeType::Platform, None) => Ok(PricingTarget::Platform),
            (PricingScopeType::Tenant, Some(id)) => Ok(PricingTarget::Tenant {
                tenant_id: pricing_id(id).map_err(|_| invalid_pricing())?,
            }),
            _ => Err(invalid_pricing()),
        }
    }
    pub fn validate_for(&self, target: PricingTarget) -> crate::Result<()> {
        if self.target()? != target || self.version <= 0 || pricing_id(&self.id).is_err() {
            return Err(invalid_pricing());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PricingPage {
    pub pricing: Vec<PricingInfo>,
    pub total: u64,
    pub page: u64,
    pub page_size: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone)]
pub struct PricingQueryParams {
    pub target: PricingTarget,
    pub search: Option<String>,
    pub page: u64,
    pub page_size: u64,
}
impl PricingQueryParams {
    pub fn new(target: PricingTarget) -> Self {
        Self {
            target,
            search: None,
            page: 1,
            page_size: 20,
        }
    }
    pub fn with_search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }
    pub fn with_page(mut self, page: u64) -> Self {
        self.page = page;
        self
    }
    pub fn with_page_size(mut self, size: u64) -> Self {
        self.page_size = size;
        self
    }
    pub fn to_query_string(&self) -> crate::Result<String> {
        if !(1..=1_000_000).contains(&self.page)
            || !(1..=100).contains(&self.page_size)
            || self.search.as_ref().is_some_and(|s| s.len() > 255)
        {
            return Err(crate::ClientError::Config(
                "Invalid pricing pagination or search".into(),
            ));
        }
        let mut query = self.target.query()?;
        if let Some(s) = self.search.as_deref().filter(|s| !s.is_empty()) {
            query.push_str(&format!(
                "&search={}",
                crate::api::common::encode_query_value(s)
            ));
        }
        query.push_str(&format!("&page={}&page_size={}", self.page, self.page_size));
        Ok(query)
    }
}

/// 创建定价请求
#[derive(Debug, Clone, Serialize)]
pub struct CreatePricingRequest {
    pub model_name: String,
    #[serde(rename = "billing_dimension")]
    pub billing_dimension: String,
    #[serde(flatten)]
    pub target: PricingTarget,
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
    pub version: i64,
}

/// 更新定价响应
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePricingResponse {
    pub success: bool,
    pub message: String,
    pub pricing_id: String,
    pub version: i64,
}

impl CreatePricingRequest {
    pub fn new(
        target: PricingTarget,
        model_name: impl Into<String>,
        billing_dimension: impl Into<String>,
        input_price_per_1k: impl Into<String>,
        output_price_per_1k: impl Into<String>,
        currency: impl Into<String>,
    ) -> Self {
        Self {
            model_name: model_name.into(),
            billing_dimension: billing_dimension.into(),
            target,
            input_price_per_1k: input_price_per_1k.into(),
            output_price_per_1k: output_price_per_1k.into(),
            currency: currency.into(),
            is_default: false,
            effective_from: None,
            effective_until: None,
        }
    }

    pub fn validate(&self) -> crate::Result<()> {
        self.target.validate()?;
        if self.model_name.trim().is_empty()
            || self.model_name.chars().count() > 255
            || self.model_name.chars().any(char::is_control)
            || !matches!(self.billing_dimension.as_str(), "provideraccount" | "node")
            || self.currency.len() != 3
            || !self.currency.bytes().all(|c| c.is_ascii_uppercase())
        {
            return Err(crate::ClientError::Config(
                "Invalid pricing model, dimension or currency".into(),
            ));
        }
        crate::api::tenant_pricing::validate_decimal(&self.input_price_per_1k)?;
        crate::api::tenant_pricing::validate_decimal(&self.output_price_per_1k)?;
        Ok(())
    }
}

/// 更新定价请求
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdatePricingRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price_per_1k: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price_per_1k: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<String>,
    pub expected_version: i64,
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
        self.expected_version = version;
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
    pub version: i64,
}

/// A deletion retains the exact resource identity; platform rows cannot be deleted.
#[derive(Debug, Clone, Deserialize)]
pub struct DeletePricingResponse {
    pub success: bool,
    pub message: String,
    pub pricing_id: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct BatchDefaultPricingResponse {
    pub success: bool,
    pub message: String,
    pub pricing_ids: Vec<String>,
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
    use super::*;
    use serde_json::json;
    #[test]
    fn pricing_query_serializes_server_side_search_and_pagination() {
        let query = PricingQueryParams::new(PricingTarget::Platform)
            .with_search("gpt 4&tenant_id=other")
            .with_page(2)
            .with_page_size(50)
            .to_query_string()
            .unwrap();
        assert_eq!(
            query,
            "scope_type=platform&search=gpt%204%26tenant_id%3Dother&page=2&page_size=50"
        );
        assert!(
            PricingQueryParams::new(PricingTarget::Platform)
                .with_page_size(0)
                .to_query_string()
                .is_err()
        );
    }
    #[test]
    fn pricing_mutations_carry_concurrency_and_tenant_context() {
        let tenant_id = uuid::Uuid::new_v4();
        let target = PricingTarget::Tenant { tenant_id };
        let create =
            CreatePricingRequest::new(target, "gpt-4o", "provideraccount", "0.01", "0.02", "CNY");
        let value = serde_json::to_value(create).unwrap();
        assert_eq!(value["scope_type"], "tenant");
        assert_eq!(value["tenant_id"], json!(tenant_id));
        let global = serde_json::to_value(CreatePricingRequest::new(
            PricingTarget::Platform,
            "gpt-4o",
            "provideraccount",
            "0.01",
            "0.02",
            "CNY",
        ))
        .unwrap();
        assert_eq!(global["scope_type"], "platform");
        assert!(global.get("tenant_id").is_none());
        let patch = serde_json::to_value(
            UpdatePricingRequest::new()
                .with_input_price_per_1k("0.0000000001")
                .with_expected_version(7),
        )
        .unwrap();
        assert_eq!(patch["expected_version"], 7);
        assert!(patch.get("effective_until").is_none());
        let cost = CalculateCostRequest::new("model", 1, 2)
            .with_tenant_id(tenant_id.to_string())
            .with_mode(ModelAccessMode::NodeDispatch);
        let cost = serde_json::to_value(cost).unwrap();
        assert_eq!(cost["tenant_id"], tenant_id.to_string());
        assert_eq!(cost["mode"], "node_dispatch");
        assert!(
            serde_json::to_value(CalculateCostRequest::new("model", 1, 2))
                .unwrap()
                .get("mode")
                .is_none()
        );
    }
    #[test]
    fn cost_response_accepts_decimal_strings_and_numbers() {
        for cost in [json!("0.03"), json!(0.03)] {
            let response:CostCalculationResponse=serde_json::from_value(json!({"model":"m","input_cost":cost,"output_cost":cost,"total_cost":cost,"currency":"CNY"})).unwrap();
            assert!((response.total_cost - 0.03).abs() < f64::EPSILON);
        }
    }
    #[test]
    fn cost_response_rejects_non_finite_values_and_pricing_requires_real_versions() {
        assert!(serde_json::from_value::<CostCalculationResponse>(json!({"model":"m","input_cost":"NaN","output_cost":"0","total_cost":"0","currency":"CNY"})).is_err());
        let base = json!({"id":uuid::Uuid::new_v4(),"scope_type":"platform","tenant_id":null,"model_name":"m","billing_dimension":"provideraccount","input_price_per_1k":"1","output_price_per_1k":"2","currency":"CNY","is_default":false,"is_effective":true,"effective_from":"2026-01-01T00:00:00Z","effective_until":null,"created_at":"2026-01-01T00:00:00Z"});
        assert!(serde_json::from_value::<PricingInfo>(base.clone()).is_err());
        let mut with_version = base;
        with_version["version"] = json!(7);
        let valid: PricingInfo = serde_json::from_value(with_version.clone()).unwrap();
        valid.validate_for(PricingTarget::Platform).unwrap();
        with_version["version"] = json!(0);
        let invalid: PricingInfo = serde_json::from_value(with_version).unwrap();
        assert!(invalid.validate_for(PricingTarget::Platform).is_err());
    }
}
