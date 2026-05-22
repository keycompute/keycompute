//! Billing 管理接口
//!
//! 用于查询计费记录和费用计算
//!
//! 注意：根据 MVP 架构约束，Billing 仅在 stream 结束后自动触发，
//! 不提供手动触发接口。

use crate::{error::Result, extractors::AuthExtractor, state::AppState};
use axum::{
    Json,
    extract::{Query, State},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 计费记录查询请求
#[derive(Debug, Deserialize)]
pub struct ListBillingQuery {
    /// 分页偏移
    #[serde(default)]
    pub offset: Option<i64>,
    /// 分页限制
    #[serde(default)]
    pub limit: Option<i64>,
    /// 开始时间
    #[serde(default)]
    pub start_time: Option<DateTime<Utc>>,
    /// 结束时间
    #[serde(default)]
    pub end_time: Option<DateTime<Utc>>,
}

/// 计费记录响应
#[derive(Debug, Serialize)]
pub struct BillingListResponse {
    /// 记录列表
    pub records: Vec<BillingRecord>,
    /// 总记录数
    pub total: i64,
}

/// 计费记录
#[derive(Debug, Serialize)]
pub struct BillingRecord {
    /// 记录 ID
    pub id: Uuid,
    /// 请求 ID
    pub request_id: Uuid,
    /// 模型名称
    pub model_name: String,
    /// Provider 名称
    pub provider_name: String,
    /// 输入 token 数
    pub input_tokens: i32,
    /// 输出 token 数
    pub output_tokens: i32,
    /// 总金额
    pub user_amount: Decimal,
    /// 货币
    pub currency: String,
    /// 状态
    pub status: String,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

/// 列出计费记录
pub async fn list_billing_records(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Query(query): Query<ListBillingQuery>,
) -> Result<Json<BillingListResponse>> {
    // 检查数据库是否配置
    let Some(pool) = &state.pool else {
        return Err(crate::error::ApiError::Internal(
            "Database not configured".to_string(),
        ));
    };

    // 分页参数
    let limit = query.limit.unwrap_or(20).min(100);
    let offset = query.offset.unwrap_or(0);

    // 获取总数
    let total = keycompute_db::UsageLog::count_by_tenant(pool, auth.tenant_id)
        .await
        .map_err(|e| {
            crate::error::ApiError::Internal(format!("Failed to count billing records: {}", e))
        })?;

    // 从数据库查询计费记录
    let logs = keycompute_db::UsageLog::find_by_tenant(pool, auth.tenant_id, limit, offset)
        .await
        .map_err(|e| {
            crate::error::ApiError::Internal(format!("Failed to query billing records: {}", e))
        })?;

    // 转换为响应格式
    let records: Vec<BillingRecord> = logs
        .into_iter()
        .map(|log| BillingRecord {
            id: log.id,
            request_id: log.request_id,
            model_name: log.model_name,
            provider_name: log.provider_name,
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            user_amount: bigdecimal_to_decimal(&log.user_amount),
            currency: log.currency,
            status: log.status,
            created_at: log.created_at,
        })
        .collect();

    Ok(Json(BillingListResponse { records, total }))
}

/// 将 BigDecimal 转换为 Decimal
fn bigdecimal_to_decimal(value: &bigdecimal::BigDecimal) -> Decimal {
    let s = value.to_string();
    s.parse().unwrap_or_default()
}

/// 计费统计查询请求
#[derive(Debug, Deserialize)]
pub struct BillingStatsQuery {
    /// 开始时间
    #[serde(default)]
    pub start_time: Option<DateTime<Utc>>,
    /// 结束时间
    #[serde(default)]
    pub end_time: Option<DateTime<Utc>>,
    /// 按模型分组
    #[serde(default)]
    pub group_by_model: Option<bool>,
}

/// 计费统计响应
#[derive(Debug, Serialize)]
pub struct BillingStatsResponse {
    /// 总请求数
    pub total_requests: i64,
    /// 总输入 tokens
    pub total_input_tokens: i64,
    /// 总输出 tokens
    pub total_output_tokens: i64,
    /// 总金额
    pub total_amount: Decimal,
    /// 货币
    pub currency: String,
    /// 按模型统计
    pub by_model: Vec<ModelStats>,
}

/// 模型统计
#[derive(Debug, Serialize)]
pub struct ModelStats {
    /// 模型名称
    pub model_name: String,
    /// 请求数
    pub request_count: i64,
    /// 输入 tokens
    pub input_tokens: i64,
    /// 输出 tokens
    pub output_tokens: i64,
    /// 金额
    pub amount: Decimal,
}

/// 获取计费统计
pub async fn get_billing_stats(
    State(state): State<AppState>,
    auth: AuthExtractor,
    Query(query): Query<BillingStatsQuery>,
) -> Result<Json<BillingStatsResponse>> {
    // 检查数据库是否配置
    let Some(pool) = &state.pool else {
        return Err(crate::error::ApiError::Internal(
            "Database not configured".to_string(),
        ));
    };

    // 时间范围
    let now = Utc::now();
    let start_time = query.start_time.unwrap_or(now - chrono::Duration::days(30));
    let end_time = query.end_time.unwrap_or(now);

    // 获取总体统计
    let stats =
        keycompute_db::UsageLog::get_stats_by_tenant(pool, auth.tenant_id, start_time, end_time)
            .await
            .map_err(|e| {
                crate::error::ApiError::Internal(format!("Failed to query billing stats: {}", e))
            })?;

    // 获取按模型分组的统计
    let model_stats = keycompute_db::UsageLog::get_stats_by_tenant_grouped_by_model(
        pool,
        auth.tenant_id,
        start_time,
        end_time,
    )
    .await
    .map_err(|e| crate::error::ApiError::Internal(format!("Failed to query model stats: {}", e)))?;

    // 转换为响应格式
    let by_model: Vec<ModelStats> = model_stats
        .into_iter()
        .map(|m| ModelStats {
            model_name: m.model_name,
            request_count: m.request_count,
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            amount: bigdecimal_to_decimal(&m.amount),
        })
        .collect();

    Ok(Json(BillingStatsResponse {
        total_requests: stats.total_requests,
        total_input_tokens: stats.total_input_tokens,
        total_output_tokens: stats.total_output_tokens,
        total_amount: bigdecimal_to_decimal(&stats.total_amount),
        currency: "CNY".to_string(),
        by_model,
    }))
}

/// 费用计算请求
#[derive(Debug, Deserialize)]
pub struct CalculateCostRequest {
    /// 模型名称
    pub model: String,
    /// 输入 token 数
    pub input_tokens: u32,
    /// 输出 token 数
    pub output_tokens: u32,
}

/// 费用计算响应
#[derive(Debug, Serialize)]
pub struct CalculateCostResponse {
    /// 模型名称
    pub model: String,
    /// 输入 token 数
    pub input_tokens: u32,
    /// 输出 token 数
    pub output_tokens: u32,
    /// 输入费用
    pub input_cost: Decimal,
    /// 输出费用
    pub output_cost: Decimal,
    /// 总费用
    pub total_cost: Decimal,
    /// 货币
    pub currency: String,
}

/// 计算费用（基于 PricingSnapshot）
pub async fn calculate_cost(
    State(state): State<AppState>,
    _auth: AuthExtractor,
    Json(request): Json<CalculateCostRequest>,
) -> Result<Json<CalculateCostResponse>> {
    // 使用默认租户 ID 创建价格快照
    let tenant_id = Uuid::nil();
    // Node 模型（node:前缀）使用 empty provider
    let provider = keycompute_pricing::resolve_pricing_provider(&request.model);
    let pricing = state
        .pricing
        .create_snapshot(&request.model, &tenant_id, Some(provider))
        .await
        .map_err(|e| crate::error::ApiError::Internal(format!("Failed to get pricing: {}", e)))?;

    // 计算费用
    let input_cost =
        Decimal::from(request.input_tokens) / Decimal::from(1000) * pricing.input_price_per_1k;
    let output_cost =
        Decimal::from(request.output_tokens) / Decimal::from(1000) * pricing.output_price_per_1k;
    let total_cost = input_cost + output_cost;

    Ok(Json(CalculateCostResponse {
        model: request.model,
        input_tokens: request.input_tokens,
        output_tokens: request.output_tokens,
        input_cost,
        output_cost,
        total_cost,
        currency: pricing.currency,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_billing_query_deserialize() {
        let json = r#"{"offset": 0, "limit": 10}"#;
        let query: ListBillingQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.offset, Some(0));
        assert_eq!(query.limit, Some(10));
    }

    #[test]
    fn test_calculate_cost_request_deserialize() {
        let json = r#"{"model": "gpt-4o", "input_tokens": 1000, "output_tokens": 500}"#;
        let req: CalculateCostRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.input_tokens, 1000);
        assert_eq!(req.output_tokens, 500);
    }
}
