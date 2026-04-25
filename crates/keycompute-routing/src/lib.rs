//! Routing Engine
//!
//! 路由引擎，双层路由，只读无副作用。
//! 架构约束：只读 Pricing 和状态快照，不写任何状态。
//! 包含 Provider 健康状态管理和账号状态管理。

pub mod account_state;
pub mod provider_health;

pub use account_state::{AccountState, AccountStateStore};
use keycompute_db::Account;
use keycompute_runtime::{CryptoError, EncryptedApiKey, decrypt_api_key};
use keycompute_types::{
    ExecutionPlan, ExecutionTarget, KeyComputeError, PricingSnapshot, RequestContext, Result,
};
pub use provider_health::{ProviderHealth, ProviderHealthStore};
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
/// 集成 AccountStateStore 进行账号冷却状态检查
#[derive(Clone)]
pub struct RoutingEngine {
    /// 账号状态存储（只读）
    account_states: Arc<AccountStateStore>,
    /// Provider 健康状态存储（只读）
    provider_health: Arc<ProviderHealthStore>,
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
            .field("pool", &self.pool.as_ref().map(|_| "PgPool"))
            .field("providers", &self.providers)
            .finish()
    }
}

impl RoutingEngine {
    /// 创建新的路由引擎（无数据库连接）
    ///
    /// # 参数
    /// - `account_states`: 账号状态存储
    /// - `provider_health`: Provider 健康状态存储
    /// - `providers`: Provider 名称列表（从外部传入，确保与 Gateway 一致）
    pub fn new(
        account_states: Arc<AccountStateStore>,
        provider_health: Arc<ProviderHealthStore>,
        providers: Vec<String>,
    ) -> Self {
        Self {
            account_states,
            provider_health,
            pool: None,
            providers,
        }
    }

    /// 创建带数据库连接的路由引擎
    ///
    /// # 参数
    /// - `account_states`: 账号状态存储
    /// - `provider_health`: Provider 健康状态存储
    /// - `pool`: 数据库连接池
    /// - `providers`: Provider 名称列表（从外部传入，确保与 Gateway 一致）
    pub fn with_pool(
        account_states: Arc<AccountStateStore>,
        provider_health: Arc<ProviderHealthStore>,
        pool: Arc<PgPool>,
        providers: Vec<String>,
    ) -> Self {
        Self {
            account_states,
            provider_health,
            pool: Some(pool),
            providers,
        }
    }

    /// 生成执行计划（只读操作）
    ///
    /// 根据 RequestContext 路由到最优的 Provider 和账号
    /// 使用租户专属账号池进行路由
    pub async fn route(&self, ctx: &RequestContext) -> Result<ExecutionPlan> {
        tracing::info!(
            request_id = %ctx.request_id,
            model = %ctx.model,
            tenant_id = %ctx.tenant_id,
            "route: starting"
        );

        // Layer1: 模型路由 - 选择 provider 排序
        let ranked_providers = self
            .rank_providers(&ctx.model, &ctx.pricing_snapshot)
            .await?;

        tracing::info!(
            request_id = %ctx.request_id,
            ranked_providers = ?ranked_providers,
            "route: providers ranked"
        );

        // Layer2: 账号路由 - 为每个 provider 选择租户专属的最优账号
        // 关键改进：只选择支持请求模型的账号
        let mut targets = Vec::new();
        for provider in ranked_providers {
            // 传入 tenant_id 和 model 确保使用租户专属账号池且账号支持该模型
            tracing::info!(
                request_id = %ctx.request_id,
                provider = %provider,
                "route: selecting account"
            );
            if let Some(target) = self
                .select_account_for_model(&provider, ctx.tenant_id, &ctx.model)
                .await?
            {
                tracing::info!(
                    request_id = %ctx.request_id,
                    provider = %provider,
                    endpoint = %target.endpoint,
                    "route: account selected"
                );
                targets.push(target);
            } else {
                tracing::info!(
                    request_id = %ctx.request_id,
                    provider = %provider,
                    "route: no account found"
                );
            }
        }

        if targets.is_empty() {
            tracing::error!(
                request_id = %ctx.request_id,
                "route: no targets found, routing failed"
            );
            return Err(KeyComputeError::RoutingFailed(ctx.model.clone()));
        }

        tracing::info!(
            request_id = %ctx.request_id,
            primary_provider = %targets[0].provider,
            targets_count = targets.len(),
            "route: completed"
        );

        Ok(ExecutionPlan {
            primary: targets.remove(0),
            fallback_chain: targets,
        })
    }

    /// Layer1: 模型路由
    ///
    /// 根据模型、价格、延迟、失败率、不健康度对 Provider 排序
    /// 注意：暂时不过滤不健康的 Provider，所有 Provider 都参与路由
    /// 评分规则：所有指标统一为"越高越不优先"，最终分数越低越优先
    /// 综合评分 = weighted_average + unhealthy_penalty
    async fn rank_providers(&self, _model: &str, pricing: &PricingSnapshot) -> Result<Vec<String>> {
        // 注意：暂时不过滤不健康的 Provider，所有 Provider 都参与路由
        // 健康状态仅用于评分排序，不用于过滤
        let _healthy_providers = self.provider_health.healthy_providers(&self.providers);

        // 使用所有 Provider 参与路由
        let candidates = &self.providers;

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
    /// 评分规则：所有指标统一为"越高越不优先"
    /// - 成本越高 → 分数越高
    /// - 延迟越高 → 分数越高
    /// - 成功率越低 → 分数越高
    /// - 健康度越低 → 分数越高
    /// - 不健康 → 额外惩罚分
    ///   最终分数越低越优先选择
    fn score_provider(&self, provider: &str, pricing: &PricingSnapshot) -> f64 {
        // 1. 成本评分 (0-100，越高越不优先)
        let cost_score = self.calculate_cost_score(pricing);

        // 2. 从 ProviderHealthStore 获取健康状态
        let health = self.provider_health.get_health(provider);

        // 3. 延迟评分 (0-100，越高越不优先)
        let latency_score = health
            .as_ref()
            .map(|h| self.calculate_latency_score(h.avg_latency_ms))
            .unwrap_or(50.0); // 默认中等延迟

        // 4. 失败率评分 (0-100，越高越不优先)
        // 成功率越高 → 失败率越低 → 分数越低（越好）
        let failure_score = health
            .as_ref()
            .map(|h| 100.0 - h.success_rate)
            .unwrap_or(0.0); // 默认 0 失败率

        // 5. 不健康度评分 (0-100，越高越不健康)
        // health_score() 越高 → 越健康 → 不健康度越低（越好）
        let unhealthiness_score = health
            .as_ref()
            .map(|h| 100.0 - h.health_score() as f64)
            .unwrap_or(50.0); // 默认中等

        // 6. 不健康额外惩罚
        let unhealthy_penalty = health
            .as_ref()
            .filter(|h| !h.healthy)
            .map(|_| UNHEALTHY_PENALTY)
            .unwrap_or(0.0);

        // 7. 综合评分（加权平均）
        let total_weight = COST_WEIGHT + LATENCY_WEIGHT + SUCCESS_WEIGHT + HEALTH_WEIGHT;
        let weighted_score = (COST_WEIGHT * cost_score
            + LATENCY_WEIGHT * latency_score
            + SUCCESS_WEIGHT * failure_score
            + HEALTH_WEIGHT * unhealthiness_score)
            / total_weight;

        let final_score = weighted_score + unhealthy_penalty;

        tracing::debug!(
            provider = %provider,
            cost_score = cost_score,
            latency_score = latency_score,
            failure_score = failure_score,
            unhealthiness_score = unhealthiness_score,
            unhealthy_penalty = unhealthy_penalty,
            final_score = final_score,
            "Provider scored (lower is better)"
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

    /// Layer2: 账号路由（带模型过滤）
    ///
    /// 为指定 Provider 选择支持特定模型的账号
    /// 按优先级排序，选择第一个未冷却的账号
    /// 注意：暂时不检查 Provider 健康状态，所有 Provider 都可以选择账号
    ///
    /// # 参数
    /// - `provider`: Provider 名称
    /// - `tenant_id`: 租户 ID，用于选择租户专属账号池
    /// - `model`: 请求的模型名称，用于过滤支持该模型的账号
    async fn select_account_for_model(
        &self,
        provider: &str,
        tenant_id: Uuid,
        model: &str,
    ) -> Result<Option<ExecutionTarget>> {
        // 注意：暂时不检查 Provider 健康状态
        // 即使 Provider 不健康，仍然尝试选择其下的账号
        // 健康状态仅影响 Layer1 的路由排序
        let _is_healthy = self.provider_health.is_healthy(provider);

        // 尝试从数据库加载租户专属账号
        let accounts = if let Some(pool) = &self.pool {
            // 根据是否指定模型选择不同的加载方式
            let result = if model.is_empty() {
                self.load_accounts_from_database(pool, provider, tenant_id)
                    .await
            } else {
                self.load_accounts_for_model(pool, provider, tenant_id, model)
                    .await
            };

            match result {
                Ok(accounts) => accounts,
                Err(e) => {
                    tracing::warn!(
                        provider = %provider,
                        tenant_id = %tenant_id,
                        model = %model,
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

    /// 从数据库加载租户专属账号
    ///
    /// # 参数
    /// - `pool`: 数据库连接池
    /// - `provider`: Provider 名称
    /// - `tenant_id`: 租户 ID
    ///
    /// # 返回
    /// 返回该租户专属的、支持指定 provider 的启用账号列表
    async fn load_accounts_from_database(
        &self,
        pool: &PgPool,
        provider: &str,
        tenant_id: Uuid,
    ) -> Result<Vec<Account>> {
        // 加载租户专属的启用账号
        let accounts = Account::find_enabled_by_tenant(pool, tenant_id)
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
            tenant_id = %tenant_id,
            count = provider_accounts.len(),
            "Loaded tenant-specific accounts from database"
        );

        Ok(provider_accounts)
    }

    /// 从数据库加载支持指定模型的租户专属账号
    ///
    /// # 参数
    /// - `pool`: 数据库连接池
    /// - `provider`: Provider 名称
    /// - `tenant_id`: 租户 ID
    /// - `model`: 请求的模型名称
    ///
    /// # 返回
    /// 返回该租户专属的、支持指定模型和 provider 的启用账号列表
    async fn load_accounts_for_model(
        &self,
        pool: &PgPool,
        provider: &str,
        tenant_id: Uuid,
        model: &str,
    ) -> Result<Vec<Account>> {
        // 直接使用模型查询，更高效
        let accounts = Account::find_by_model(pool, tenant_id, model)
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
            tenant_id = %tenant_id,
            model = %model,
            count = provider_accounts.len(),
            "Loaded tenant-specific accounts for model from database"
        );

        Ok(provider_accounts)
    }

    /// 选择账号
    ///
    /// 按优先级排序，选择第一个未冷却的账号
    async fn select_best_account(
        &self,
        provider: &str,
        accounts: Vec<Account>,
    ) -> Result<Option<ExecutionTarget>> {
        tracing::info!(
            provider = %provider,
            accounts_count = accounts.len(),
            "select_best_account: starting"
        );

        if accounts.is_empty() {
            tracing::warn!(provider = %provider, "No accounts available");
            return Ok(None);
        }

        // 按优先级排序
        let mut sorted_accounts: Vec<_> = accounts.into_iter().collect();
        sorted_accounts.sort_by_key(|account| std::cmp::Reverse(account.priority));

        for account in sorted_accounts {
            // 检查账号是否在冷却中
            if self.account_states.is_cooling_down(&account.id) {
                let remaining = self.account_states.get(&account.id).cooldown_remaining();
                tracing::warn!(
                    provider = %provider,
                    account_id = %account.id,
                    remaining_secs = remaining.map(|d| d.as_secs()),
                    "Account is cooling down, skipping"
                );
                continue;
            }

            // 解密上游 API Key
            let upstream_api_key =
                Self::decrypt_upstream_api_key(&account.upstream_api_key_encrypted)?;

            let target = ExecutionTarget {
                provider: provider.to_string(),
                account_id: account.id,
                endpoint: account.endpoint,
                upstream_api_key: upstream_api_key.into(),
            };

            tracing::info!(
                provider = %provider,
                account_id = %account.id,
                "Account selected"
            );

            return Ok(Some(target));
        }

        Ok(None)
    }

    /// 解密上游 API Key
    ///
    /// 尝试解密存储的 API Key。如果全局加密密钥未设置，
    /// 说明系统可能还在使用明文存储，此时回退使用原始值。
    fn decrypt_upstream_api_key(encrypted_value: &str) -> Result<String> {
        // 尝试使用全局密钥解密
        match decrypt_api_key(&EncryptedApiKey::from(encrypted_value)) {
            Ok(decrypted) => {
                tracing::trace!("Successfully decrypted upstream API key");
                Ok(decrypted)
            }
            Err(CryptoError::InvalidKey(msg)) if msg.contains("Global crypto key not set") => {
                // 全局密钥未设置，回退使用原始值（可能存储的是明文）
                tracing::warn!(
                    "Global crypto key not set, using stored value as plaintext. \n\
                     This is acceptable for development but should be fixed in production."
                );
                Ok(encrypted_value.to_string())
            }
            Err(e) => {
                // 其他解密错误
                tracing::error!(error = %e, "Failed to decrypt upstream API key");
                Err(KeyComputeError::Internal(format!(
                    "Failed to decrypt upstream API key: {}",
                    e
                )))
            }
        }
    }

    /// 回退账号选择（无数据库时使用）
    async fn select_fallback_account(&self, provider: &str) -> Result<Option<ExecutionTarget>> {
        let account_id = Uuid::new_v4();

        // 检查账号是否在冷却中
        if self.account_states.is_cooling_down(&account_id) {
            let remaining = self.account_states.get(&account_id).cooldown_remaining();
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
            upstream_api_key: "mock-api-key".into(),
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

    /// 获取账号状态存储（只读访问）
    pub fn account_states(&self) -> &Arc<AccountStateStore> {
        &self.account_states
    }

    /// 检查账号是否在冷却中
    pub fn is_account_cooling(&self, account_id: &Uuid) -> bool {
        self.account_states.is_cooling_down(account_id)
    }

    /// 获取账号冷却剩余时间
    pub fn account_cooldown_remaining(&self, account_id: &Uuid) -> Option<std::time::Duration> {
        self.account_states.get(account_id).cooldown_remaining()
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
        RequestContext::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "gpt-4o",
            vec![],
            true,
            PricingSnapshot {
                model_name: "gpt-4o".to_string(),
                currency: "CNY".to_string(),
                input_price_per_1k: Decimal::from(1),
                output_price_per_1k: Decimal::from(2),
            },
        )
    }

    fn create_test_engine() -> RoutingEngine {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());
        let providers = vec![
            "openai".to_string(),
            "deepseek".to_string(),
            "claude".to_string(),
            "gemini".to_string(),
        ];
        RoutingEngine::new(account_states, provider_health, providers)
    }

    #[tokio::test]
    async fn test_routing_engine_new() {
        let engine = create_test_engine();

        // 验证 Provider 数量与传入的测试列表一致（4个）
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
        assert!((0.0..=200.0).contains(&openai_score));
        assert!((0.0..=200.0).contains(&other_score));
    }

    #[test]
    fn test_provider_health_integration() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());

        // 模拟一些请求数据
        provider_health.record_success("openai", 100);
        provider_health.record_success("openai", 150);
        provider_health.record_failure("claude");

        let providers = vec!["openai".to_string(), "claude".to_string()];
        let engine = RoutingEngine::new(account_states, provider_health, providers);

        // 检查健康状态
        assert!(engine.is_provider_healthy("openai"));
        // claude 只有一次失败，仍然健康（成功率 0%，但没有达到 10 次阈值）
        assert!(engine.is_provider_healthy("claude"));

        // 检查评分
        let openai_score = engine.get_provider_health_score("openai");
        assert!(openai_score > 50, "OpenAI should have good health score");
    }

    #[test]
    fn test_unhealthy_provider_marking() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());

        // 让 claude 多次失败变得不健康
        for _ in 0..10 {
            provider_health.record_failure("claude");
        }

        let providers = vec!["openai".to_string(), "claude".to_string()];
        let engine = RoutingEngine::new(account_states, provider_health, providers);

        // claude 应该被标记为不健康
        assert!(!engine.is_provider_healthy("claude"));

        // 健康列表应该不包含 claude（但路由时不会过滤，只是评分靠后）
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
    fn test_account_cooldown_check() {
        let account_states = Arc::new(AccountStateStore::new());
        let provider_health = Arc::new(ProviderHealthStore::new());

        let providers = vec!["test".to_string()];
        let engine = RoutingEngine::new(account_states.clone(), provider_health, providers);

        let account_id = Uuid::new_v4();

        // 初始状态
        assert!(!engine.is_account_cooling(&account_id));

        // 设置账号冷却
        account_states.set_cooldown(account_id, 30);

        // 现在应该在冷却中
        assert!(engine.is_account_cooling(&account_id));
        assert!(engine.account_cooldown_remaining(&account_id).is_some());
    }

    #[test]
    fn test_decrypt_upstream_api_key_without_global_key() {
        // 当全局密钥未设置时，应该回退使用原始值
        // 注意：如果其他测试先设置了全局密钥，此测试跳过
        if keycompute_runtime::global_crypto().is_some() {
            // 全局密钥已被其他测试设置，跳过此测试
            return;
        }
        let result = RoutingEngine::decrypt_upstream_api_key("test-api-key");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test-api-key");
    }

    #[test]
    fn test_decrypt_upstream_api_key_with_global_key() {
        // 设置全局密钥
        let key = keycompute_runtime::ApiKeyCrypto::generate_key();
        keycompute_runtime::set_global_crypto(&key).expect("Failed to set global crypto");

        // 加密一个 API Key
        let plaintext = "sk-test-secret-key-123";
        let encrypted = keycompute_runtime::encrypt_api_key(plaintext).expect("Failed to encrypt");

        // 解密应该返回原始值
        let result = RoutingEngine::decrypt_upstream_api_key(encrypted.as_str());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), plaintext);
    }
}
