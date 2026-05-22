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
use serde::{Deserialize, Serialize};

/// 定价查询请求
#[derive(Debug, Deserialize)]
pub struct PricingQuery {
    /// 模型名称
    pub model: String,
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
    // Node 模型（node:前缀）使用 empty provider，其他使用 openai
    let provider = keycompute_pricing::resolve_pricing_provider(&query.model);
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
    // Node 模型（node:前缀）使用 empty provider，其他使用 openai
    let provider = keycompute_pricing::resolve_pricing_provider(&request.model);
    let snapshot = state
        .pricing
        .create_snapshot(&request.model, &auth.tenant_id, Some(provider))
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to get pricing: {}", e)))?;

    let total_cost =
        state
            .pricing
            .calculate_cost(request.input_tokens, request.output_tokens, &snapshot);

    let input_cost = snapshot.input_price_per_1k
        * rust_decimal::Decimal::from(request.input_tokens)
        / rust_decimal::Decimal::from(1000);
    let output_cost = snapshot.output_price_per_1k
        * rust_decimal::Decimal::from(request.output_tokens)
        / rust_decimal::Decimal::from(1000);

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
}
