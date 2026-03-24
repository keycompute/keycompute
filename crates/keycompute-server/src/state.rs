//! 应用状态
//!
//! AppState 定义（DB Pool, Redis, 各模块 Handle）

use keycompute_auth::{ApiKeyValidator, AuthService};
use keycompute_billing::BillingService;
use keycompute_provider_trait::ProviderAdapter;
use keycompute_routing::RoutingEngine;
use keycompute_runtime::{AccountStateStore, CooldownManager, ProviderHealthStore};
use llm_gateway::{GatewayBuilder, GatewayExecutor, HttpProxy, ProxyConfig as HttpProxyConfig};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// 限流后端配置
#[derive(Debug, Clone)]
pub enum RateLimitBackendConfig {
    /// 内存后端
    Memory,
    /// Redis 后端
    Redis { url: String },
}

impl Default for RateLimitBackendConfig {
    fn default() -> Self {
        Self::Memory
    }
}

/// 应用状态配置
#[derive(Debug, Clone)]
pub struct AppStateConfig {
    /// 限流后端配置
    pub rate_limit: RateLimitBackendConfig,
    /// API Key 验证密钥
    pub api_key_secret: String,
    /// Gateway 配置
    pub gateway: keycompute_config::GatewayConfig,
}

impl Default for AppStateConfig {
    fn default() -> Self {
        Self {
            rate_limit: RateLimitBackendConfig::default(),
            api_key_secret: "default-secret".to_string(),
            gateway: keycompute_config::GatewayConfig::default(),
        }
    }
}

impl AppStateConfig {
    /// 从 keycompute_config::AppConfig 创建
    pub fn from_config(config: &keycompute_config::AppConfig) -> Self {
        Self {
            rate_limit: if let Some(redis) = &config.redis {
                RateLimitBackendConfig::Redis {
                    url: redis.url.clone(),
                }
            } else {
                RateLimitBackendConfig::Memory
            },
            api_key_secret: config.auth.api_key_secret.clone(),
            gateway: config.gateway.clone(),
        }
    }
}

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
    /// Internal HTTP Proxy（统一上游连接管理）
    pub http_proxy: Arc<HttpProxy>,
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
            .field("http_proxy", &"<HttpProxy>")
            .field("billing", &"<BillingService>")
            .finish()
    }
}

impl AppState {
    /// 创建新的应用状态（无数据库连接，使用默认配置）
    pub fn new() -> Self {
        Self::with_config(AppStateConfig::default())
    }

    /// 创建带配置的应用状态（无数据库连接）
    pub fn with_config(config: AppStateConfig) -> Self {
        // 创建 API Key 验证器
        let api_key_validator = ApiKeyValidator::new(&config.api_key_secret);
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

        // 创建 Internal HTTP Proxy（统一上游连接管理，支持配置）
        let http_proxy = Arc::new(Self::create_http_proxy(
            config.gateway.proxy.as_ref(),
            &config.gateway,
        ));

        // 创建 Gateway 执行器，注册所有 Provider，集成 HTTP Proxy
        let gateway = Arc::new(
            GatewayBuilder::new()
                .add_provider("openai", Arc::new(keycompute_openai::OpenAIProvider::new()))
                .add_provider(
                    "deepseek",
                    Arc::new(keycompute_deepseek::DeepSeekProvider::new()),
                )
                .add_provider("vllm", Arc::new(keycompute_vllm::VllmProvider::new()))
                .with_http_proxy(Arc::clone(&http_proxy))
                .build(),
        );

        // 创建计费服务
        let billing = Arc::new(BillingService::new());

        // 根据配置创建限流服务
        let rate_limiter = Self::create_rate_limiter(&config.rate_limit);

        Self {
            pool: None,
            auth: Arc::new(auth_service),
            rate_limiter: Arc::new(rate_limiter),
            pricing: Arc::new(pricing_service),
            account_states: Arc::clone(&account_states),
            provider_health,
            cooldown,
            routing: routing_engine,
            gateway,
            http_proxy,
            billing,
        }
    }

    /// 根据配置创建限流服务
    fn create_rate_limiter(
        config: &RateLimitBackendConfig,
    ) -> keycompute_ratelimit::RateLimitService {
        match config {
            RateLimitBackendConfig::Memory => {
                keycompute_ratelimit::RateLimitService::default_memory()
            }
            #[cfg(feature = "redis")]
            RateLimitBackendConfig::Redis { url } => {
                keycompute_ratelimit::RateLimitService::new_redis(url)
                    .expect("Failed to create Redis rate limiter")
            }
            #[cfg(not(feature = "redis"))]
            RateLimitBackendConfig::Redis { .. } => {
                panic!("Redis backend requested but redis feature is not enabled")
            }
        }
    }

    /// 创建 HTTP Proxy（支持从配置读取代理设置）
    fn create_http_proxy(
        proxy_config: Option<&keycompute_config::ProxyConfig>,
        gateway_config: &keycompute_config::GatewayConfig,
    ) -> HttpProxy {
        // 创建 HTTP Proxy 配置
        let http_proxy_config = HttpProxyConfig::default()
            .with_request_timeout(Duration::from_secs(gateway_config.request_timeout_secs))
            .with_stream_timeout(Duration::from_secs(gateway_config.stream_timeout_secs));

        if let Some(proxy) = proxy_config {
            // 有代理配置，创建带代理的 HttpProxy
            let mut proxies = proxy.providers.clone();

            // 添加通配符规则
            if let Some(patterns) = &proxy.patterns {
                for (pattern, url) in patterns {
                    // 通配符规则以 pattern: 为前缀存储
                    proxies.insert(format!("pattern:{}", pattern), url.clone());
                }
            }

            // 添加账号级代理
            if let Some(accounts) = &proxy.accounts {
                for (key, url) in accounts {
                    // 账号级代理以 account: 为前缀存储
                    proxies.insert(format!("account:{}", key), url.clone());
                }
            }

            HttpProxy::with_proxies(http_proxy_config, proxies)
        } else {
            // 无代理配置，使用默认 HttpProxy
            HttpProxy::new(http_proxy_config)
        }
    }

    /// 创建带数据库连接的应用状态（使用默认配置）
    pub fn with_pool(pool: Arc<PgPool>) -> Self {
        Self::with_pool_and_config(pool, AppStateConfig::default())
    }

    /// 创建带数据库连接和自定义配置的应用状态
    pub fn with_pool_and_config(pool: Arc<PgPool>, config: AppStateConfig) -> Self {
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

        // 创建 Internal HTTP Proxy（统一上游连接管理，支持配置）
        let http_proxy = Arc::new(Self::create_http_proxy(
            config.gateway.proxy.as_ref(),
            &config.gateway,
        ));

        // 创建 Gateway 执行器，注册所有 Provider，集成 HTTP Proxy
        let gateway = Arc::new(
            GatewayBuilder::new()
                .add_provider("openai", Arc::new(keycompute_openai::OpenAIProvider::new()))
                .add_provider(
                    "deepseek",
                    Arc::new(keycompute_deepseek::DeepSeekProvider::new()),
                )
                .add_provider("vllm", Arc::new(keycompute_vllm::VllmProvider::new()))
                .with_http_proxy(Arc::clone(&http_proxy))
                .build(),
        );

        // 创建带数据库连接的计费服务
        let billing = Arc::new(BillingService::with_pool(Arc::clone(&pool)));

        // 根据配置创建限流服务
        let rate_limiter = Self::create_rate_limiter(&config.rate_limit);

        Self {
            pool: Some(pool),
            auth: Arc::new(auth_service),
            rate_limiter: Arc::new(rate_limiter),
            pricing: Arc::new(pricing_service),
            account_states: Arc::clone(&account_states),
            provider_health,
            cooldown,
            routing: routing_engine,
            gateway,
            http_proxy,
            billing,
        }
    }

    /// 创建用于测试的应用状态，使用自定义 Provider（默认配置）
    pub fn with_providers(providers: HashMap<String, Arc<dyn ProviderAdapter>>) -> Self {
        Self::with_providers_and_config(providers, AppStateConfig::default())
    }

    /// 创建用于测试的应用状态，使用自定义 Provider和配置
    pub fn with_providers_and_config(
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        config: AppStateConfig,
    ) -> Self {
        // 创建 API Key 验证器
        let api_key_validator = ApiKeyValidator::new(&config.api_key_secret);
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

        // 创建 Internal HTTP Proxy（支持配置）
        let http_proxy = Arc::new(Self::create_http_proxy(
            config.gateway.proxy.as_ref(),
            &config.gateway,
        ));

        // 创建 Gateway 执行器，使用自定义 Provider
        let mut builder = GatewayBuilder::new().with_http_proxy(Arc::clone(&http_proxy));
        for (name, provider) in providers {
            builder = builder.add_provider(name, provider);
        }
        let gateway = Arc::new(builder.build());

        // 创建计费服务
        let billing = Arc::new(BillingService::new());

        // 根据配置创建限流服务
        let rate_limiter = Self::create_rate_limiter(&config.rate_limit);

        Self {
            pool: None,
            auth: Arc::new(auth_service),
            rate_limiter: Arc::new(rate_limiter),
            pricing: Arc::new(pricing_service),
            account_states: Arc::clone(&account_states),
            provider_health,
            cooldown,
            routing: routing_engine,
            gateway,
            http_proxy,
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
