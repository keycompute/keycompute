//! 应用状态
//!
//! AppState 定义（DB Pool, Redis, 各模块 Handle）

use keycompute_auth::{ApiKeyValidator, AuthService};
use keycompute_billing::BillingService;
use keycompute_provider_trait::ProviderAdapter;
use keycompute_routing::RoutingEngine;
use keycompute_runtime::{AccountStateStore, CooldownManager, ProviderHealthStore};
use llm_gateway::{GatewayBuilder, GatewayExecutor};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;

/// 应用状态
#[derive(Clone)]
pub struct AppState {
    /// 数据库连接池（可选）
    pub pool: Option<Arc<PgPool>>,
    /// 认证服务
    pub auth: Arc<AuthService>,
    /// 限流服务
    pub rate_limiter: Arc<keycompute_ratelimit::RateLimitService>,
    /// 定价服务
    pub pricing: Arc<keycompute_pricing::PricingService>,
    /// 运行时状态存储（账号状态）
    pub account_states: Arc<AccountStateStore>,
    /// Provider 健康状态存储
    pub provider_health: Arc<ProviderHealthStore>,
    /// 冷却管理器
    pub cooldown: Arc<CooldownManager>,
    /// 路由引擎
    pub routing: Arc<RoutingEngine>,
    /// Gateway 执行器（唯一执行层）
    pub gateway: Arc<GatewayExecutor>,
    /// 计费服务
    pub billing: Arc<BillingService>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("pool", &self.pool.as_ref().map(|_| "PgPool"))
            .field("auth", &"<AuthService>")
            .field("rate_limiter", &"<RateLimitService>")
            .field("pricing", &"<PricingService>")
            .field("account_states", &self.account_states)
            .field("provider_health", &"<ProviderHealthStore>")
            .field("cooldown", &"<CooldownManager>")
            .field("routing", &"<RoutingEngine>")
            .field("gateway", &"<GatewayExecutor>")
            .field("billing", &"<BillingService>")
            .finish()
    }
}

impl AppState {
    /// 创建新的应用状态（无数据库连接）
    pub fn new() -> Self {
        // 创建 API Key 验证器
        let api_key_validator = ApiKeyValidator::new("default-secret");
        let auth_service = AuthService::new(api_key_validator);

        // 创建定价服务
        let pricing_service = keycompute_pricing::PricingService::new();

        // 创建运行时状态存储
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let cooldown = Arc::new(CooldownManager::new());

        // 创建路由引擎（集成 ProviderHealthStore 和 CooldownManager）
        let routing_engine = Arc::new(RoutingEngine::new(
            Arc::clone(&account_states),
            Arc::clone(&provider_health),
            Arc::clone(&cooldown),
        ));

        // 创建 Gateway 执行器，注册所有 Provider
        let gateway = Arc::new(
            GatewayBuilder::new()
                .add_provider("openai", Arc::new(keycompute_openai::OpenAIProvider::new()))
                .add_provider("deepseek", Arc::new(keycompute_deepseek::DeepSeekProvider::new()))
                .build(),
        );

        // 创建计费服务
        let billing = Arc::new(BillingService::new());

        Self {
            pool: None,
            auth: Arc::new(auth_service),
            rate_limiter: Arc::new(keycompute_ratelimit::RateLimitService::default_memory()),
            pricing: Arc::new(pricing_service),
            account_states: Arc::clone(&account_states),
            provider_health,
            cooldown,
            routing: routing_engine,
            gateway,
            billing,
        }
    }

    /// 创建带数据库连接的应用状态
    pub fn with_pool(pool: Arc<PgPool>) -> Self {
        // 创建带数据库连接的 API Key 验证器
        let api_key_validator = ApiKeyValidator::with_pool(Arc::clone(&pool));
        let auth_service = AuthService::new(api_key_validator);

        // 创建带数据库连接的定价服务
        let pricing_service = keycompute_pricing::PricingService::with_pool(Arc::clone(&pool));

        // 创建运行时状态存储
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let cooldown = Arc::new(CooldownManager::new());

        // 创建带数据库连接的路由引擎
        let routing_engine = Arc::new(RoutingEngine::with_pool(
            Arc::clone(&account_states),
            Arc::clone(&provider_health),
            Arc::clone(&cooldown),
            Arc::clone(&pool),
        ));

        // 创建 Gateway 执行器，注册所有 Provider
        let gateway = Arc::new(
            GatewayBuilder::new()
                .add_provider("openai", Arc::new(keycompute_openai::OpenAIProvider::new()))
                .add_provider("deepseek", Arc::new(keycompute_deepseek::DeepSeekProvider::new()))
                .build(),
        );

        // 创建带数据库连接的计费服务
        let billing = Arc::new(BillingService::with_pool(Arc::clone(&pool)));

        Self {
            pool: Some(pool),
            auth: Arc::new(auth_service),
            rate_limiter: Arc::new(keycompute_ratelimit::RateLimitService::default_memory()),
            pricing: Arc::new(pricing_service),
            account_states: Arc::clone(&account_states),
            provider_health,
            cooldown,
            routing: routing_engine,
            gateway,
            billing,
        }
    }

    /// 创建用于测试的应用状态，使用自定义 Provider
    pub fn with_providers(providers: HashMap<String, Arc<dyn ProviderAdapter>>) -> Self {
        // 创建 API Key 验证器
        let api_key_validator = ApiKeyValidator::new("default-secret");
        let auth_service = AuthService::new(api_key_validator);

        // 创建定价服务
        let pricing_service = keycompute_pricing::PricingService::new();

        // 创建运行时状态存储
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let cooldown = Arc::new(CooldownManager::new());

        // 创建路由引擎（集成 ProviderHealthStore 和 CooldownManager）
        let routing_engine = Arc::new(RoutingEngine::new(
            Arc::clone(&account_states),
            Arc::clone(&provider_health),
            Arc::clone(&cooldown),
        ));

        // 创建 Gateway 执行器，使用自定义 Provider
        let mut builder = GatewayBuilder::new();
        for (name, provider) in providers {
            builder = builder.add_provider(name, provider);
        }
        let gateway = Arc::new(builder.build());

        // 创建计费服务
        let billing = Arc::new(BillingService::new());

        Self {
            pool: None,
            auth: Arc::new(auth_service),
            rate_limiter: Arc::new(keycompute_ratelimit::RateLimitService::default_memory()),
            pricing: Arc::new(pricing_service),
            account_states: Arc::clone(&account_states),
            provider_health,
            cooldown,
            routing: routing_engine,
            gateway,
            billing,
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
