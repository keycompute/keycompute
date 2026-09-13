//! Gateway 调试接口
//!
//! 用于调试 Gateway 执行状态和 Provider 健康情况

use crate::{error::Result, state::AppState};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Gateway 状态响应
#[derive(Debug, Serialize)]
pub struct GatewayStatusResponse {
    /// Gateway 是否可用
    pub available: bool,
    /// 已加载的 Provider 列表
    pub providers: Vec<ProviderInfo>,
    /// 配置信息
    pub config: GatewayConfigInfo,
}

/// Provider 信息
#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    /// Provider 名称
    pub name: String,
    /// 支持的模型列表
    pub supported_models: Vec<String>,
    /// 健康状态
    pub healthy: bool,
}

/// Gateway 配置信息
#[derive(Debug, Serialize)]
pub struct GatewayConfigInfo {
    /// 最大重试次数
    pub max_retries: u32,
    /// 超时时间（秒）
    pub timeout_secs: u64,
    /// 是否启用 fallback
    pub enable_fallback: bool,
}

impl Default for GatewayConfigInfo {
    fn default() -> Self {
        Self {
            max_retries: 3,
            timeout_secs: 120,
            enable_fallback: true,
        }
    }
}

/// 从渠道账号一次性聚合各 Provider（协议）支持的模型列表
///
/// 协议层不再维护模型白名单，模型由各账号的 models_supported 声明，
/// 此处全量加载启用账号后在内存中按 provider 分组去重，
/// 避免逐 provider 重复查库（N+1）。
///
/// 注意：本模块是 Admin 调试接口（路由层有 admin_auth_middleware 保护），
/// 故聚合不限租户；若未来复用到用户侧接口，必须改为按租户 +
/// visibility 过滤（与路由选账号的可见性规则对齐）
async fn aggregate_models_by_provider(state: &AppState) -> HashMap<String, Vec<String>> {
    let Some(pool) = state.pool.as_deref() else {
        return HashMap::new();
    };
    match keycompute_db::Account::find_enabled_all(pool).await {
        Ok(accounts) => {
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            for account in accounts {
                map.entry(account.provider)
                    .or_default()
                    .extend(account.models_supported);
            }
            for models in map.values_mut() {
                models.sort();
                models.dedup();
            }
            map
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to aggregate models from accounts"
            );
            HashMap::new()
        }
    }
}

/// 聚合单个 Provider（协议）支持的模型列表
async fn aggregate_provider_models(state: &AppState, provider: &str) -> Vec<String> {
    aggregate_models_by_provider(state)
        .await
        .remove(provider)
        .unwrap_or_default()
}

/// Aggregate runtime health from concrete enabled accounts. Protocol names are
/// only adapters; they do not own routing health or penalties.
async fn provider_account_health(
    state: &AppState,
    provider: &str,
) -> (bool, Option<u64>, Option<String>) {
    let Some(pool) = state.pool.as_deref() else {
        return (state.gateway.has_provider(provider), None, None);
    };
    let accounts = match keycompute_db::Account::find_enabled_all(pool.write_conn()).await {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(%error, %provider, "Failed to load account health summary");
            return (false, None, Some("Account health unavailable".to_string()));
        }
    };
    let accounts = accounts
        .into_iter()
        .filter(|account| account.provider.eq_ignore_ascii_case(provider))
        .collect::<Vec<_>>();
    if accounts.is_empty() {
        return (
            false,
            None,
            Some("No enabled accounts configured".to_string()),
        );
    }

    let mut routable = 0usize;
    let mut latency_sum = 0u64;
    let mut latency_samples = 0u64;
    for account in &accounts {
        let health = state.provider_health.account_health_for(account);
        if health.is_routable() && !state.account_states.is_cooling_down(&account.id) {
            routable += 1;
        }
        if let Some(latency) = health.avg_latency_ms {
            latency_sum = latency_sum.saturating_add(latency.max(0) as u64);
            latency_samples += 1;
        }
    }
    let healthy = routable > 0;
    let latency_ms = (latency_samples > 0).then(|| latency_sum / latency_samples);
    let error = (!healthy).then(|| "No routable accounts".to_string());
    (healthy, latency_ms, error)
}

/// 获取 Gateway 状态
pub async fn get_gateway_status(
    State(state): State<AppState>,
) -> Result<Json<GatewayStatusResponse>> {
    // 从 GatewayExecutor 获取 Provider 列表，模型列表从渠道账号一次性聚合
    let mut models_by_provider = aggregate_models_by_provider(&state).await;
    let mut providers = Vec::new();
    for name in state.gateway.list_providers() {
        let (healthy, _, _) = provider_account_health(&state, &name).await;
        let supported_models = models_by_provider.remove(&name).unwrap_or_default();
        providers.push(ProviderInfo {
            name: name.clone(),
            supported_models,
            healthy,
        });
    }

    Ok(Json(GatewayStatusResponse {
        available: !providers.is_empty(),
        providers,
        config: GatewayConfigInfo::default(),
    }))
}

/// Provider 健康检查请求
#[derive(Debug, Deserialize)]
pub struct ProviderHealthRequest {
    /// Provider 名称
    pub provider: String,
    /// 测试用的 API Key（可选）
    pub api_key: Option<String>,
}

/// Provider 健康检查结果
#[derive(Debug, Serialize)]
pub struct ProviderHealthResponse {
    /// Provider 名称
    pub provider: String,
    /// 是否健康
    pub healthy: bool,
    /// 延迟（毫秒）
    pub latency_ms: Option<u64>,
    /// 错误信息（如果不健康）
    pub error: Option<String>,
    /// 支持的模型
    pub models: Vec<String>,
}

/// 检查 Provider 健康状态
pub async fn check_provider_health(
    State(state): State<AppState>,
    Json(request): Json<ProviderHealthRequest>,
) -> Result<Json<ProviderHealthResponse>> {
    if !state.gateway.has_provider(&request.provider) {
        return Ok(Json(ProviderHealthResponse {
            provider: request.provider,
            healthy: false,
            latency_ms: None,
            error: Some("Provider not configured".to_string()),
            models: Vec::new(),
        }));
    }
    let (healthy, latency_ms, error) = provider_account_health(&state, &request.provider).await;
    let models = aggregate_provider_models(&state, &request.provider).await;
    Ok(Json(ProviderHealthResponse {
        provider: request.provider,
        healthy,
        latency_ms,
        error,
        models,
    }))
}

/// 执行统计信息
#[derive(Debug, Serialize)]
pub struct ExecutionStats {
    /// 总请求数
    pub total_requests: u64,
    /// 成功请求数
    pub successful_requests: u64,
    /// 失败请求数
    pub failed_requests: u64,
    /// Fallback 次数
    pub fallback_count: u64,
    /// 平均延迟（毫秒）
    pub avg_latency_ms: u64,
    /// Provider 统计
    pub provider_stats: HashMap<String, ProviderStats>,
}

/// Provider 统计
#[derive(Debug, Serialize)]
pub struct ProviderStats {
    /// 请求数
    pub requests: u64,
    /// 成功数
    pub successes: u64,
    /// 失败数
    pub failures: u64,
    /// 平均延迟
    pub avg_latency_ms: u64,
}

/// 获取执行统计
pub async fn get_execution_stats(State(state): State<AppState>) -> Result<Json<ExecutionStats>> {
    // 从 ProviderHealthStore 获取真实统计数据
    let all_health = state.provider_health.all_health();

    let mut total_requests = 0u64;
    let mut successful_requests = 0u64;
    let mut failed_requests = 0u64;
    let mut total_latency = 0u64;
    let mut latency_count = 0u64;
    let mut provider_stats = HashMap::new();

    for health in all_health {
        total_requests += health.total_requests;
        successful_requests += health.success_requests;
        failed_requests += health.failed_requests;

        if health.avg_latency_ms > 0 {
            total_latency += health.avg_latency_ms;
            latency_count += 1;
        }

        provider_stats.insert(
            health.name.clone(),
            ProviderStats {
                requests: health.total_requests,
                successes: health.success_requests,
                failures: health.failed_requests,
                avg_latency_ms: health.avg_latency_ms,
            },
        );
    }

    let avg_latency_ms = total_latency.checked_div(latency_count).unwrap_or(0);

    Ok(Json(ExecutionStats {
        total_requests,
        successful_requests,
        failed_requests,
        fallback_count: state.provider_health.get_fallback_count(),
        avg_latency_ms,
        provider_stats,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gateway_config_info_default() {
        let config = GatewayConfigInfo::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.timeout_secs, 120);
        assert!(config.enable_fallback);
    }

    #[test]
    fn test_provider_health_request_deserialize() {
        let json = r#"{"provider": "openai", "api_key": "test-key"}"#;
        let req: ProviderHealthRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.provider, "openai");
        assert_eq!(req.api_key, Some("test-key".to_string()));
    }

    #[tokio::test]
    async fn unconfigured_provider_health_keeps_legacy_error_contract() {
        let response = check_provider_health(
            State(AppState::new()),
            Json(ProviderHealthRequest {
                provider: "not-configured".to_string(),
                api_key: None,
            }),
        )
        .await
        .expect("health check should return a response")
        .0;

        assert!(!response.healthy);
        assert_eq!(response.error.as_deref(), Some("Provider not configured"));
        assert!(response.models.is_empty());
    }
}
