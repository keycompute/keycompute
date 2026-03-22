//! Routing Engine
//!
//! 路由引擎，双层路由，只读无副作用。
//! 架构约束：只读 Pricing 和 Runtime 状态快照，不写任何状态。

use keycompute_runtime::{AccountStateStore, ProviderHealthStore};
use keycompute_types::{ExecutionPlan, ExecutionTarget, KeyComputeError, PricingSnapshot, RequestContext, Result};
use std::sync::Arc;
use uuid::Uuid;

/// Provider 评分配置
///
/// 用于 Layer1 模型路由的评分权重
#[derive(Debug, Clone)]
pub struct RoutingConfig {
    /// 成本权重 (0.0 - 1.0)
    pub cost_weight: f64,
    /// 延迟权重 (0.0 - 1.0)
    pub latency_weight: f64,
    /// 成功率权重 (0.0 - 1.0)
    pub success_weight: f64,
    /// 健康评分权重 (0.0 - 1.0)
    pub health_weight: f64,
    /// 不健康 Provider 的惩罚分数
    pub unhealthy_penalty: f64,
    /// 高延迟阈值（毫秒）
    pub high_latency_threshold_ms: u64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            cost_weight: 0.3,
            latency_weight: 0.25,
            success_weight: 0.25,
            health_weight: 0.2,
            unhealthy_penalty: 100.0,
            high_latency_threshold_ms: 1000,
        }
    }
}

/// 路由引擎
///
/// 双层路由：Layer1 模型路由，Layer2 账号路由
/// 集成 ProviderHealthStore 进行健康评分路由
#[derive(Debug, Clone)]
pub struct RoutingEngine {
    /// 账号状态存储（只读）
    account_states: Arc<AccountStateStore>,
    /// Provider 健康状态存储（只读）
    provider_health: Arc<ProviderHealthStore>,
    /// 可用 Provider 列表
    providers: Vec<String>,
    /// 路由配置
    config: RoutingConfig,
}

impl RoutingEngine {
    /// 创建新的路由引擎
    pub fn new(
        account_states: Arc<AccountStateStore>,
        provider_health: Arc<ProviderHealthStore>,
    ) -> Self {
        Self {
            account_states,
            provider_health,
            providers: vec![
                "openai".to_string(),
                "claude".to_string(),
                "deepseek".to_string(),
            ],
            config: RoutingConfig::default(),
        }
    }

    /// 创建带自定义配置的路由引擎
    pub fn with_config(
        account_states: Arc<AccountStateStore>,
        provider_health: Arc<ProviderHealthStore>,
        config: RoutingConfig,
    ) -> Self {
        Self {
            account_states,
            provider_health,
            providers: vec![
                "openai".to_string(),
                "claude".to_string(),
                "deepseek".to_string(),
            ],
            config,
        }
    }

    /// 生成执行计划（只读操作）
    ///
    /// 根据 RequestContext 路由到最优的 Provider 和账号
    pub async fn route(&self, ctx: &RequestContext) -> Result<ExecutionPlan> {
        // Layer1: 模型路由 - 选择 provider 排序
        let ranked_providers = self.rank_providers(&ctx.model, &ctx.pricing_snapshot).await?;

        // Layer2: 账号路由 - 为每个 provider 选择最优账号
        let mut targets = Vec::new();
        for provider in ranked_providers {
            if let Some(target) = self.select_account(&provider).await? {
                targets.push(target);
            }
        }

        if targets.is_empty() {
            return Err(KeyComputeError::RoutingFailed);
        }

        Ok(ExecutionPlan {
            primary: targets.remove(0),
            fallback_chain: targets,
        })
    }

    /// Layer1: 模型路由
    ///
    /// 根据模型、价格、延迟、成功率、健康评分对 Provider 排序
    /// 综合评分 = cost_weight * cost_norm + latency_weight * latency_norm
    ///          + success_weight * (1 - success_norm) + health_weight * (1 - health_norm)
    /// 分数越低表示越优先选择
    async fn rank_providers(
        &self,
        _model: &str,
        pricing: &PricingSnapshot,
    ) -> Result<Vec<String>> {
        // 首先过滤掉不健康的 Provider
        let healthy_providers = self.provider_health.healthy_providers(&self.providers);
        
        if healthy_providers.is_empty() {
            tracing::warn!("No healthy providers available, falling back to all providers");
        }
        
        // 使用健康 Provider 列表（如果没有健康的，使用全部）
        let candidates = if healthy_providers.is_empty() {
            &self.providers
        } else {
            &healthy_providers
        };

        // 计算每个 Provider 的综合评分
        let mut scored_providers: Vec<(String, f64)> = candidates
            .iter()
            .map(|p| {
                let score = self.score_provider(p, pricing);
                (p.clone(), score)
            })
            .collect();

        // 按分数排序（分数越低越好）
        scored_providers.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        tracing::debug!(
            provider_scores = ?scored_providers,
            "Provider ranking completed"
        );

        Ok(scored_providers.into_iter().map(|(p, _)| p).collect())
    }

    /// 计算 Provider 综合评分
    ///
    /// 基于架构公式：score = 0.3*cost + 0.25*latency + 0.25*(1-success) + 0.2*(1-health)
    /// 同时考虑 ProviderHealthStore 中的实时健康状态
    fn score_provider(&self, provider: &str, pricing: &PricingSnapshot) -> f64 {
        let cfg = &self.config;
        
        // 1. 成本评分 (0-100，越低越好)
        let cost_score = self.calculate_cost_score(pricing);
        
        // 2. 从 ProviderHealthStore 获取健康状态
        let health = self.provider_health.get_health(provider);
        
        // 3. 延迟评分 (0-100，越低越好)
        let latency_score = health
            .as_ref()
            .map(|h| self.calculate_latency_score(h.avg_latency_ms))
            .unwrap_or(50.0); // 默认中等延迟
        
        // 4. 成功率评分 (0-100，越高越好，所以用 100 - success_rate)
        let success_score = health
            .as_ref()
            .map(|h| 100.0 - h.success_rate)
            .unwrap_or(0.0); // 默认 100% 成功率
        
        // 5. 健康评分 (0-100，越高越好，所以用 100 - health_score)
        let health_score = health
            .as_ref()
            .map(|h| 100.0 - h.health_score() as f64)
            .unwrap_or(50.0); // 默认中等健康
        
        // 6. 不健康惩罚
        let unhealthy_penalty = health
            .as_ref()
            .filter(|h| !h.healthy)
            .map(|_| cfg.unhealthy_penalty)
            .unwrap_or(0.0);
        
        // 7. 综合评分（加权平均）
        let total_weight = cfg.cost_weight + cfg.latency_weight + cfg.success_weight + cfg.health_weight;
        let normalized_score = (
            cfg.cost_weight * cost_score +
            cfg.latency_weight * latency_score +
            cfg.success_weight * success_score +
            cfg.health_weight * health_score
        ) / total_weight;
        
        let final_score = normalized_score + unhealthy_penalty;
        
        tracing::debug!(
            provider = %provider,
            cost_score = cost_score,
            latency_score = latency_score,
            success_score = success_score,
            health_score = health_score,
            unhealthy_penalty = unhealthy_penalty,
            final_score = final_score,
            "Provider scored"
        );
        
        final_score
    }
    
    /// 计算成本评分
    fn calculate_cost_score(&self, pricing: &PricingSnapshot) -> f64 {
        // 将价格转换为 f64，价格越高分数越高（越不优先）
        let input_price: f64 = pricing.input_price_per_1k.to_string()
            .parse()
            .unwrap_or(1.0);
        let output_price: f64 = pricing.output_price_per_1k.to_string()
            .parse()
            .unwrap_or(2.0);
        
        // 归一化到 0-100 范围（假设价格范围 0-10）
        let avg_price = (input_price + output_price) / 2.0;
        (avg_price * 10.0).min(100.0)
    }
    
    /// 计算延迟评分
    fn calculate_latency_score(&self, latency_ms: u64) -> f64 {
        let threshold = self.config.high_latency_threshold_ms;
        
        if latency_ms == 0 {
            // 无延迟数据，返回中等分数
            50.0
        } else if latency_ms < 100 {
            10.0 // 优秀
        } else if latency_ms < 300 {
            30.0 // 良好
        } else if latency_ms < threshold {
            60.0 // 一般
        } else {
            90.0 // 较差
        }
    }

    /// Layer2: 账号路由
    ///
    /// 为指定 Provider 选择最优账号
    /// score = current_rpm/rpm_limit + error_rate*2
    /// 同时检查 Provider 健康状态，不健康的 Provider 直接跳过
    async fn select_account(&self, provider: &str) -> Result<Option<ExecutionTarget>> {
        // 首先检查 Provider 是否健康
        if !self.provider_health.is_healthy(provider) {
            tracing::warn!(
                provider = %provider,
                health_score = self.provider_health.get_score(provider),
                "Provider is unhealthy, skipping"
            );
            return Ok(None);
        }

        // TODO: 从数据库加载该 Provider 的可用账号
        // 这里简化处理，返回模拟数据

        let account_id = Uuid::new_v4();

        // 检查账号是否在冷却中
        if self.account_states.is_cooling_down(&account_id) {
            tracing::debug!(provider = %provider, "Account is cooling down, skipping");
            return Ok(None);
        }

        // 构建执行目标
        let target = ExecutionTarget {
            provider: provider.to_string(),
            account_id,
            endpoint: format!("https://api.{}.com/v1/chat/completions", provider),
            api_key: "mock-api-key".to_string(),
        };

        Ok(Some(target))
    }
    
    /// 获取 Provider 健康状态存储（只读访问）
    pub fn provider_health(&self) -> &Arc<ProviderHealthStore> {
        &self.provider_health
    }
    
    /// 获取指定 Provider 的健康评分
    pub fn get_provider_health_score(&self, provider: &str) -> u64 {
        self.provider_health.get_score(provider)
    }
    
    /// 检查 Provider 是否健康
    pub fn is_provider_healthy(&self, provider: &str) -> bool {
        self.provider_health.is_healthy(provider)
    }

    /// 获取配置的所有 Provider 列表
    pub fn configured_providers(&self) -> &[String] {
        &self.providers
    }
    
    /// 获取当前健康的 Provider 列表
    pub fn healthy_providers(&self) -> Vec<String> {
        self.provider_health.healthy_providers(&self.providers)
    }

    /// 添加 Provider
    pub fn add_provider(&mut self, provider: impl Into<String>) {
        let provider = provider.into();
        if !self.providers.contains(&provider) {
            self.providers.push(provider);
        }
    }
    
    /// 移除 Provider
    pub fn remove_provider(&mut self, provider: &str) {
        self.providers.retain(|p| p != provider);
    }
    
    /// 获取路由配置
    pub fn config(&self) -> &RoutingConfig {
        &self.config
    }
    
    /// 更新路由配置
    pub fn set_config(&mut self, config: RoutingConfig) {
        self.config = config;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keycompute_types::PricingSnapshot;
    use rust_decimal::Decimal;

    fn create_test_context() -> RequestContext {
        RequestContext {
            request_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            api_key_id: Uuid::new_v4(),
            model: "gpt-4o".to_string(),
            messages: vec![],
            stream: true,
            pricing_snapshot: PricingSnapshot {
                model_name: "gpt-4o".to_string(),
                currency: "CNY".to_string(),
                input_price_per_1k: Decimal::from(1),
                output_price_per_1k: Decimal::from(2),
            },
            usage: Default::default(),
            started_at: chrono::Utc::now(),
        }
    }

    fn create_test_engine() -> RoutingEngine {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        RoutingEngine::new(account_states, provider_health)
    }

    #[tokio::test]
    async fn test_routing_engine_new() {
        let engine = create_test_engine();

        assert_eq!(engine.configured_providers().len(), 3);
    }

    #[tokio::test]
    async fn test_route() {
        let engine = create_test_engine();
        let ctx = create_test_context();

        let plan = engine.route(&ctx).await;
        assert!(plan.is_ok());

        let plan = plan.unwrap();
        assert!(!plan.primary.provider.is_empty());
    }

    #[test]
    fn test_score_provider() {
        let engine = create_test_engine();
        let pricing = PricingSnapshot {
            model_name: "test".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::from(1),
            output_price_per_1k: Decimal::from(2),
        };

        let openai_score = engine.score_provider("openai", &pricing);
        let other_score = engine.score_provider("other", &pricing);

        // 两者应该都有合理的分数（0-200 范围）
        assert!(openai_score >= 0.0 && openai_score <= 200.0);
        assert!(other_score >= 0.0 && other_score <= 200.0);
    }

    #[test]
    fn test_provider_health_integration() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        
        // 模拟一些请求数据
        provider_health.record_success("openai", 100);
        provider_health.record_success("openai", 150);
        provider_health.record_failure("claude");
        
        let engine = RoutingEngine::new(account_states, provider_health);
        
        // 检查健康状态
        assert!(engine.is_provider_healthy("openai"));
        // claude 只有一次失败，仍然健康（成功率 0%，但没有达到 10 次阈值）
        assert!(engine.is_provider_healthy("claude"));
        
        // 检查评分
        let openai_score = engine.get_provider_health_score("openai");
        assert!(openai_score > 50, "OpenAI should have good health score");
    }

    #[test]
    fn test_unhealthy_provider_filtering() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        
        // 让 claude 多次失败变得不健康
        for _ in 0..10 {
            provider_health.record_failure("claude");
        }
        
        let engine = RoutingEngine::new(account_states, provider_health);
        
        // claude 应该被标记为不健康
        assert!(!engine.is_provider_healthy("claude"));
        
        // 健康列表应该不包含 claude
        let healthy = engine.healthy_providers();
        assert!(!healthy.contains(&"claude".to_string()));
    }

    #[test]
    fn test_routing_config() {
        let config = RoutingConfig {
            cost_weight: 0.4,
            latency_weight: 0.3,
            success_weight: 0.2,
            health_weight: 0.1,
            unhealthy_penalty: 50.0,
            high_latency_threshold_ms: 500,
        };
        
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let mut engine = RoutingEngine::new(account_states, provider_health);
        
        engine.set_config(config.clone());
        
        assert_eq!(engine.config().cost_weight, 0.4);
        assert_eq!(engine.config().high_latency_threshold_ms, 500);
    }

    #[test]
    fn test_calculate_cost_score() {
        let engine = create_test_engine();
        
        let cheap_pricing = PricingSnapshot {
            model_name: "test".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::from(1),
            output_price_per_1k: Decimal::from(2),
        };
        
        let expensive_pricing = PricingSnapshot {
            model_name: "test".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::from(5),
            output_price_per_1k: Decimal::from(10),
        };
        
        let cheap_score = engine.calculate_cost_score(&cheap_pricing);
        let expensive_score = engine.calculate_cost_score(&expensive_pricing);
        
        // 贵的应该分数更高（越不优先）
        assert!(expensive_score > cheap_score);
    }

    #[test]
    fn test_calculate_latency_score() {
        let engine = create_test_engine();
        
        assert!(engine.calculate_latency_score(50) < engine.calculate_latency_score(200));
        assert!(engine.calculate_latency_score(200) < engine.calculate_latency_score(500));
        assert!(engine.calculate_latency_score(500) < engine.calculate_latency_score(1500));
    }
}
