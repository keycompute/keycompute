//! 定价管理处理器
//!
//! 管理模型定价的查询和计算

use crate::{
    error::{ApiError, Result},
    extractors::AuthExtractor,
    state::AppState,
};
use axum::{
    Json,
    extract::{Query, State},
};
use keycompute_types::ModelAccessMode;
use serde::{Deserialize, Serialize};

/// 定价查询请求
#[derive(Debug, Deserialize)]
pub struct PricingQuery {
    /// 模型名称
    pub model: String,
    #[serde(default)]
    pub mode: ModelAccessMode,
}

/// 定价响应
#[derive(Debug, Serialize)]
pub struct PricingResponse {
    /// 模型名称
    pub model: String,
    /// 货币
    pub currency: String,
    /// 输入价格（每 1K tokens）
    pub input_price_per_1k: String,
    /// 输出价格（每 1K tokens）
    pub output_price_per_1k: String,
}

/// 费用计算请求
#[derive(Debug, Deserialize)]
pub struct CostCalculationRequest {
    /// 模型名称
    pub model: String,
    #[serde(default)]
    pub mode: ModelAccessMode,
    /// 输入 token 数
    pub input_tokens: u32,
    /// 输出 token 数
    pub output_tokens: u32,
}

/// 费用计算响应
#[derive(Debug, Serialize)]
pub struct CostCalculationResponse {
    /// 模型名称
    pub model: String,
    /// 输入费用
    pub input_cost: String,
    /// 输出费用
    pub output_cost: String,
    /// 总费用
    pub total_cost: String,
    /// 货币
    pub currency: String,
}

/// 获取模型定价
pub async fn get_pricing(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Query(query): Query<PricingQuery>,
) -> Result<Json<PricingResponse>> {
    let provider = keycompute_pricing::resolve_pricing_provider(query.mode);
    let snapshot = state
        .pricing
        .create_snapshot(&query.model, &auth.tenant_id, Some(provider))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get pricing: {}", e)))?;

    Ok(Json(PricingResponse {
        model: snapshot.model_name,
        currency: snapshot.currency,
        input_price_per_1k: snapshot.input_price_per_1k.to_string(),
        output_price_per_1k: snapshot.output_price_per_1k.to_string(),
    }))
}

/// 计算请求费用
pub async fn calculate_cost(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Json(request): Json<CostCalculationRequest>,
) -> Result<Json<CostCalculationResponse>> {
    let provider = keycompute_pricing::resolve_pricing_provider(request.mode);
    let snapshot = state
        .pricing
        .create_snapshot(&request.model, &auth.tenant_id, Some(provider))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get pricing: {}", e)))?;

    let (input_cost, output_cost, total_cost) = keycompute_billing::calculate_breakdown(
        request.input_tokens,
        request.output_tokens,
        &snapshot,
    );

    Ok(Json(CostCalculationResponse {
        model: request.model,
        input_cost: input_cost.to_string(),
        output_cost: output_cost.to_string(),
        total_cost: total_cost.to_string(),
        currency: snapshot.currency,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pricing_query_deserialize() {
        let json = r#"{"model": "gpt-4o"}"#;
        let query: PricingQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.model, "gpt-4o");
    }

    #[test]
    fn test_cost_calculation_request_deserialize() {
        let json = r#"{"model": "gpt-4o", "input_tokens": 1000, "output_tokens": 500}"#;
        let req: CostCalculationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.input_tokens, 1000);
        assert_eq!(req.output_tokens, 500);
    }

    #[test]
    fn cost_calculation_response_preserves_decimal_string_wire_values() {
        let response = CostCalculationResponse {
            model: "gpt-4o".to_string(),
            input_cost: "0.01".to_string(),
            output_cost: "0.02".to_string(),
            total_cost: "0.03".to_string(),
            currency: "CNY".to_string(),
        };
        let value = serde_json::to_value(response).unwrap();
        assert!(value["input_cost"].is_string());
        assert_eq!(value["total_cost"], serde_json::json!("0.03"));
    }
}
