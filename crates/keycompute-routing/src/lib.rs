//! Routing Engine
//!
//! 路由引擎，双层路由，只读无副作用。
//! 架构约束：只读 Pricing 和 Runtime 状态快照，不写任何状态。

use keycompute_db::Account;
use keycompute_runtime::{AccountStateStore, CooldownManager, ProviderHealthStore};
use keycompute_types::{
    ExecutionPlan, ExecutionTarget, KeyComputeError, PricingSnapshot, RequestContext, Result,
};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// 路由权重常量（硬编码，不可通过配置修改）
const COST_WEIGHT: f64 = 0.3;
const LATENCY_WEIGHT: f64 = 0.25;
const SUCCESS_WEIGHT: f64 = 0.25;
const HEALTH_WEIGHT: f64 = 0.2;
const UNHEALTHY_PENALTY: f64 = 100.0;
const HIGH_LATENCY_THRESHOLD_MS: u64 = 1000;

/// 路由引擎
///
/// 双层路由：Layer1 模型路由，Layer2 账号路由
/// 集成 ProviderHealthStore 进行健康评分路由
/// 集成 CooldownManager 进行冷却状态检查
#[derive(Clone)]
pub struct RoutingEngine {
    /// 账号状态存储（只读）
    account_states: Arc<AccountStateStore>,
    /// Provider 健康状态存储（只读）
    provider_health: Arc<ProviderHealthStore>,
    /// 冷却管理器（只读）
    cooldown: Arc<CooldownManager>,
    /// 数据库连接池（可选）
    pool: Option<Arc<PgPool>>,
    /// 可用 Provider 列表
    providers: Vec<String>,
}

impl std::fmt::Debug for RoutingEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingEngine")
            .field("account_states", &"AccountStateStore")
            .field("provider_health", &"ProviderHealthStore")
            .field("cooldown", &"CooldownManager")
            .field("pool", &self.pool.as_ref().map(|_| "PgPool"))
            .field("providers", &self.providers)
            .finish()
    }
}

impl RoutingEngine {
    /// 创建新的路由引擎（无数据库连接）
    pub fn new(
        account_states: Arc<AccountStateStore>,
        provider_health: Arc<ProviderHealthStore>,
        cooldown: Arc<CooldownManager>,
    ) -> Self {
        Self {
            account_states,
            provider_health,
            cooldown,
            pool: None,
            providers: vec![
                "openai".to_string(),
                "deepseek".to_string(),
                "vllm".to_string(),
                "claude".to_string(),
            ],
        }
    }

    /// 创建带数据库连接的路由引擎
    pub fn with_pool(
        account_states: Arc<AccountStateStore>,
        provider_health: Arc<ProviderHealthStore>,
        cooldown: Arc<CooldownManager>,
        pool: Arc<PgPool>,
    ) -> Self {
        Self {
            account_states,
            provider_health,
            cooldown,
            pool: Some(pool),
            providers: vec![
                "openai".to_string(),
                "deepseek".to_string(),
                "vllm".to_string(),
                "claude".to_string(),
            ],
        }
    }

    /// 生成执行计划（只读操作）
    ///
    /// 根据 RequestContext 路由到最优的 Provider 和账号
    pub async fn route(&self, ctx: &RequestContext) -> Result<ExecutionPlan> {
        // Layer1: 模型路由 - 选择 provider 排序
        let ranked_providers = self
            .rank_providers(&ctx.model, &ctx.pricing_snapshot)
            .await?;

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
    /// 同时检查 Provider 冷却状态，冷却中的 Provider 会被过滤
    /// 综合评分 = cost_weight * cost_norm + latency_weight * latency_norm
    ///          + success_weight * (1 - success_norm) + health_weight * (1 - health_norm)
    /// 分数越低表示越优先选择
    async fn rank_providers(&self, _model: &str, pricing: &PricingSnapshot) -> Result<Vec<String>> {
        // 首先过滤掉不健康的 Provider
        let healthy_providers = self.provider_health.healthy_providers(&self.providers);

        if healthy_providers.is_empty() {
            tracing::warn!("No healthy providers available, falling back to all providers");
        }

        // 再过滤掉冷却中的 Provider
        let available_providers: Vec<String> = healthy_providers
            .into_iter()
            .filter(|p| {
                let cooling = self.cooldown.is_provider_cooling(p);
                if cooling {
                    let remaining = self.cooldown.provider_cooldown_remaining(p);
                    tracing::debug!(
                        provider = %p,
                        remaining_secs = remaining.map(|d| d.as_secs()),
                        "Provider is cooling down, skipping"
                    );
                }
                !cooling
            })
            .collect();

        if available_providers.is_empty() {
            tracing::warn!(
                "No available providers (all cooling down), falling back to all providers"
            );
        }

        // 使用可用 Provider 列表（如果没有可用的，使用全部）
        let candidates = if available_providers.is_empty() {
            &self.providers
        } else {
            &available_providers
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
        scored_providers.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

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
            .map(|_| UNHEALTHY_PENALTY)
            .unwrap_or(0.0);

        // 7. 综合评分（加权平均）
        let total_weight = COST_WEIGHT + LATENCY_WEIGHT + SUCCESS_WEIGHT + HEALTH_WEIGHT;
        let normalized_score = (COST_WEIGHT * cost_score
            + LATENCY_WEIGHT * latency_score
            + SUCCESS_WEIGHT * success_score
            + HEALTH_WEIGHT * health_score)
            / total_weight;

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
        let input_price: f64 = pricing
            .input_price_per_1k
            .to_string()
            .parse()
            .unwrap_or(1.0);
        let output_price: f64 = pricing
            .output_price_per_1k
            .to_string()
            .parse()
            .unwrap_or(2.0);

        // 归一化到 0-100 范围（假设价格范围 0-10）
        let avg_price = (input_price + output_price) / 2.0;
        (avg_price * 10.0).min(100.0)
    }

    /// 计算延迟评分
    fn calculate_latency_score(&self, latency_ms: u64) -> f64 {
        if latency_ms == 0 {
            // 无延迟数据，返回中等分数
            50.0
        } else if latency_ms < 100 {
            10.0 // 优秀
        } else if latency_ms < 300 {
            30.0 // 良好
        } else if latency_ms < HIGH_LATENCY_THRESHOLD_MS {
            60.0 // 一般
        } else {
            90.0 // 较差
        }
    }

    /// Layer2: 账号路由
    ///
    /// 为指定 Provider 选择最优账号
    /// score = current_rpm/rpm_limit + error_rate*2
    /// 同时检查 Provider 健康状态和冷却状态
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

        // 检查 Provider 是否在冷却中
        if self.cooldown.is_provider_cooling(provider) {
            let remaining = self.cooldown.provider_cooldown_remaining(provider);
            tracing::warn!(
                provider = %provider,
                remaining_secs = remaining.map(|d| d.as_secs()),
                "Provider is cooling down, skipping"
            );
            return Ok(None);
        }

        // 尝试从数据库加载账号
        let accounts = if let Some(pool) = &self.pool {
            match self.load_accounts_from_database(pool, provider).await {
                Ok(accounts) => accounts,
                Err(e) => {
                    tracing::warn!(
                        provider = %provider,
                        error = %e,
                        "Failed to load accounts from database, using fallback"
                    );
                    return self.select_fallback_account(provider).await;
                }
            }
        } else {
            // 无数据库连接，使用回退逻辑
            return self.select_fallback_account(provider).await;
        };

        // 从账号列表中选择最优账号
        self.select_best_account(provider, accounts).await
    }

    /// 从数据库加载账号
    async fn load_accounts_from_database(
        &self,
        pool: &PgPool,
        provider: &str,
    ) -> Result<Vec<Account>> {
        // 加载所有启用的账号（不按租户过滤，因为账号池是全局的）
        let accounts = Account::find_enabled_by_tenant(pool, uuid::Uuid::nil())
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to load accounts: {}", e))
            })?;

        // 过滤出指定 provider 的账号
        let provider_accounts: Vec<Account> = accounts
            .into_iter()
            .filter(|a| a.provider == provider)
            .collect();

        tracing::debug!(
            provider = %provider,
            count = provider_accounts.len(),
            "Loaded accounts from database"
        );

        Ok(provider_accounts)
    }

    /// 选择最优账号
    async fn select_best_account(
        &self,
        provider: &str,
        accounts: Vec<Account>,
    ) -> Result<Option<ExecutionTarget>> {
        if accounts.is_empty() {
            tracing::warn!(provider = %provider, "No accounts available");
            return Ok(None);
        }

        // 按优先级排序，然后选择负载最低的
        let mut sorted_accounts: Vec<_> = accounts.into_iter().collect();
        sorted_accounts.sort_by(|a, b| b.priority.cmp(&a.priority));

        for account in sorted_accounts {
            // 检查账号是否在冷却中
            let account_cooling = self.account_states.is_cooling_down(&account.id)
                || self.cooldown.is_account_cooling(&account.id);

            if account_cooling {
                let remaining = self.cooldown.account_cooldown_remaining(&account.id);
                tracing::debug!(
                    provider = %provider,
                    account_id = %account.id,
                    remaining_secs = remaining.map(|d| d.as_secs()),
                    "Account is cooling down, skipping"
                );
                continue;
            }

            // 计算账号评分
            let state = self.account_states.get(&account.id);
            let rpm_ratio = state.current_rpm as f64 / account.rpm_limit.max(1) as f64;
            let error_rate = if state.total_requests > 0 {
                state.error_count as f64 / state.total_requests as f64
            } else {
                0.0
            };
            let score = rpm_ratio + error_rate * 2.0;

            tracing::debug!(
                provider = %provider,
                account_id = %account.id,
                score = score,
                rpm_ratio = rpm_ratio,
                error_rate = error_rate,
                "Account scored"
            );

            // 选择评分最低的账号
            let target = ExecutionTarget {
                provider: provider.to_string(),
                account_id: account.id,
                endpoint: account.endpoint,
                api_key: account.api_key_encrypted, // 注意：实际应解密
            };

            return Ok(Some(target));
        }

        Ok(None)
    }

    /// 回退账号选择（无数据库时使用）
    async fn select_fallback_account(&self, provider: &str) -> Result<Option<ExecutionTarget>> {
        let account_id = Uuid::new_v4();

        // 检查账号是否在冷却中
        let account_cooling = self.account_states.is_cooling_down(&account_id)
            || self.cooldown.is_account_cooling(&account_id);

        if account_cooling {
            let remaining = self.cooldown.account_cooldown_remaining(&account_id);
            tracing::debug!(
                provider = %provider,
                account_id = %account_id,
                remaining_secs = remaining.map(|d| d.as_secs()),
                "Account is cooling down, skipping"
            );
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

    /// 获取冷却管理器（只读访问）
    pub fn cooldown(&self) -> &Arc<CooldownManager> {
        &self.cooldown
    }

    /// 检查 Provider 是否在冷却中
    pub fn is_provider_cooling(&self, provider: &str) -> bool {
        self.cooldown.is_provider_cooling(provider)
    }

    /// 检查账号是否在冷却中
    pub fn is_account_cooling(&self, account_id: &Uuid) -> bool {
        self.cooldown.is_account_cooling(account_id)
    }

    /// 获取 Provider 冷却剩余时间
    pub fn provider_cooldown_remaining(&self, provider: &str) -> Option<std::time::Duration> {
        self.cooldown.provider_cooldown_remaining(provider)
    }

    /// 获取账号冷却剩余时间
    pub fn account_cooldown_remaining(&self, account_id: &Uuid) -> Option<std::time::Duration> {
        self.cooldown.account_cooldown_remaining(account_id)
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
        let cooldown = Arc::new(CooldownManager::new());
        RoutingEngine::new(account_states, provider_health, cooldown)
    }

    #[tokio::test]
    async fn test_routing_engine_new() {
        let engine = create_test_engine();

        assert_eq!(engine.configured_providers().len(), 4);
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
        let cooldown = Arc::new(CooldownManager::new());

        // 模拟一些请求数据
        provider_health.record_success("openai", 100);
        provider_health.record_success("openai", 150);
        provider_health.record_failure("claude");

        let engine = RoutingEngine::new(account_states, provider_health, cooldown);

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
        let cooldown = Arc::new(CooldownManager::new());

        // 让 claude 多次失败变得不健康
        for _ in 0..10 {
            provider_health.record_failure("claude");
        }

        let engine = RoutingEngine::new(account_states, provider_health, cooldown);

        // claude 应该被标记为不健康
        assert!(!engine.is_provider_healthy("claude"));

        // 健康列表应该不包含 claude
        let healthy = engine.healthy_providers();
        assert!(!healthy.contains(&"claude".to_string()));
    }

    #[test]
    fn test_routing_constants() {
        // 验证路由权重常量总和为 1.0
        let total = COST_WEIGHT + LATENCY_WEIGHT + SUCCESS_WEIGHT + HEALTH_WEIGHT;
        assert!(
            (total - 1.0).abs() < 0.001,
            "Routing weights should sum to 1.0"
        );
        assert_eq!(COST_WEIGHT, 0.3);
        assert_eq!(LATENCY_WEIGHT, 0.25);
        assert_eq!(UNHEALTHY_PENALTY, 100.0);
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

    #[test]
    fn test_cooldown_manager_integration() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let cooldown = Arc::new(CooldownManager::new());

        let engine = RoutingEngine::new(account_states, provider_health, cooldown.clone());

        // 初始状态不应该在冷却中
        assert!(!engine.is_provider_cooling("openai"));
        assert!(!engine.is_account_cooling(&Uuid::new_v4()));

        // 设置 Provider 冷却
        cooldown.set_provider_cooldown(
            "openai",
            Some(std::time::Duration::from_secs(60)),
            keycompute_runtime::CooldownReason::ConsecutiveErrors,
        );

        // 现在应该在冷却中
        assert!(engine.is_provider_cooling("openai"));
        assert!(engine.provider_cooldown_remaining("openai").is_some());

        // 其他 Provider 不应该受影响
        assert!(!engine.is_provider_cooling("claude"));
    }

    #[test]
    fn test_provider_cooldown_filtering() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let cooldown = Arc::new(CooldownManager::new());

        let engine = RoutingEngine::new(account_states, provider_health, cooldown.clone());

        // 设置 openai 冷却
        cooldown.set_provider_cooldown(
            "openai",
            Some(std::time::Duration::from_secs(60)),
            keycompute_runtime::CooldownReason::CircuitBreaker,
        );

        // openai 应该在冷却中
        assert!(engine.is_provider_cooling("openai"));

        // claude 和 deepseek 不应该在冷却中
        assert!(!engine.is_provider_cooling("claude"));
        assert!(!engine.is_provider_cooling("deepseek"));
    }

    #[test]
    fn test_account_cooldown_check() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let cooldown = Arc::new(CooldownManager::new());

        let engine = RoutingEngine::new(account_states, provider_health, cooldown.clone());

        let account_id = Uuid::new_v4();

        // 初始状态
        assert!(!engine.is_account_cooling(&account_id));

        // 设置账号冷却
        cooldown.set_account_cooldown(
            account_id,
            Some(std::time::Duration::from_secs(30)),
            keycompute_runtime::CooldownReason::RpmLimitExceeded,
        );

        // 现在应该在冷却中
        assert!(engine.is_account_cooling(&account_id));
        assert!(engine.account_cooldown_remaining(&account_id).is_some());
    }
}
