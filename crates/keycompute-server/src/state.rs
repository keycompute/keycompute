//! 应用状态
//!
//! AppState 定义（DB Pool, Redis, 各模块 Handle）

use std::sync::Arc;

/// 应用状态
#[derive(Debug, Clone)]
pub struct AppState {
    /// 限流服务
    pub rate_limiter: Arc<keycompute_ratelimit::RateLimitService>,
    // TODO: 添加其他模块服务
    // pub auth: Arc<keycompute_auth::AuthService>,
    // pub pricing: Arc<keycompute_pricing::PricingService>,
    // pub routing: Arc<keycompute_routing::RoutingEngine>,
    // pub runtime: Arc<keycompute_runtime::RuntimeManager>,
    // pub gateway: Arc<llm_gateway::GatewayExecutor>,
    // pub billing: Arc<keycompute_billing::BillingService>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new() -> Self {
        Self {
            rate_limiter: Arc::new(keycompute_ratelimit::RateLimitService::default_memory()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_state_new() {
        let state = AppState::new();
        // 基础测试，确保可以创建
        let _ = state;
    }
}
