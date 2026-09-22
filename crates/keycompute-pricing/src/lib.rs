//! Pricing Module
//!
//! 定价模块，只读，生成 PricingSnapshot。
//! 架构约束：不写任何状态，不参与路由或执行。

use keycompute_cache::CacheService;
use keycompute_db::{DbRouter, PricingModel, PricingScopeType};
use keycompute_types::{KeyComputeError, ModelAccessMode, PricingSnapshot, Result};
use lru::LruCache;
use rust_decimal::Decimal;
use sea_orm::ConnectionTrait;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

/// Node 模型定价 provider 标识
pub const NODE_PRICING_PROVIDER: &str = "node";

/// ProviderAccount 定价 provider 标识
/// 用于所有非 Node 路径模型的定价查询
pub const DEFAULT_PRICING_PROVIDER: &str = "provideraccount";

/// Resolve the billing dimension from the explicit ingress access mode.
/// Model names are intentionally opaque and never infer trusted routing.
pub const fn resolve_pricing_provider(mode: ModelAccessMode) -> &'static str {
    match mode {
        ModelAccessMode::NodeDispatch => NODE_PRICING_PROVIDER,
        ModelAccessMode::AccountPool | ModelAccessMode::Passthrough => DEFAULT_PRICING_PROVIDER,
    }
}

/// 标记价格来源，用于优化缓存策略
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum PricingSource {
    /// 租户特定定价
    TenantSpecific,
    /// 数据库默认定价
    DatabaseDefault,
    /// 硬编码默认定价（兜底）
    HardcodedDefault,
}

/// 带来源标记的价格快照
#[derive(Debug, Clone)]
struct SnapshotWithSource {
    snapshot: PricingSnapshot,
    source: PricingSource,
}

fn find_global_default<'a>(
    defaults: &'a [PricingModel],
    model_name: &str,
    provider: &str,
) -> Option<&'a PricingModel> {
    defaults
        .iter()
        .find(|pricing| {
            pricing.scope_type == PricingScopeType::Platform
                && pricing.model_name == model_name
                && pricing.billing_dimension.as_str() == provider
        })
        .or_else(|| {
            defaults.iter().find(|pricing| {
                pricing.scope_type == PricingScopeType::Platform && pricing.model_name == model_name
            })
        })
}

/// 默认缓存 TTL（5 分钟）
const DEFAULT_CACHE_TTL_SECS: u64 = 300;

/// 默认缓存容量
const DEFAULT_CACHE_CAPACITY: usize = 10000;

/// 缓存条目
#[derive(Clone)]
struct CacheEntry {
    /// 价格快照
    snapshot: PricingSnapshot,
    /// 创建时间（用于 TTL 检查）
    created_at: Instant,
}

impl CacheEntry {
    fn new(snapshot: PricingSnapshot) -> Self {
        Self {
            snapshot,
            created_at: Instant::now(),
        }
    }

    /// 检查是否过期
    fn is_expired(&self, ttl_secs: u64) -> bool {
        // TTL 为 0 表示立即过期
        if ttl_secs == 0 {
            return true;
        }
        self.created_at.elapsed().as_secs() > ttl_secs
    }
}

/// 默认分布式锁 TTL（秒）——缓存防击穿时锁持有时间
/// 考虑到 pricing DB 查询可能涉及多次 SeaORM 查询（租户特定 + 默认扫描），
/// 5 秒为多副本场景提供安全窗口。
const DEFAULT_LOCK_TTL_SECS: u64 = 5;
/// 默认锁重试间隔（毫秒）
const DEFAULT_LOCK_RETRY_MS: u64 = 50;
/// 默认锁最大重试次数
const DEFAULT_LOCK_MAX_RETRIES: u32 = 5;

/// 定价服务
///
/// 负责从数据库加载模型价格，生成 PricingSnapshot
/// 使用两级缓存架构：
/// - L1: 本地 LRU 缓存（纳秒级读取）
/// - L2: Redis 分布式缓存（带 get_or_insert_with_lock 防击穿）
#[derive(Clone)]
pub struct PricingService {
    /// 数据库连接池（可选，用于测试时可以不提供）
    pool: Option<Arc<DbRouter>>,
    /// L1: 本地价格缓存：key = "tenant_id:model_name:provider"，使用 LRU 淘汰
    cache: Arc<RwLock<LruCache<String, CacheEntry>>>,
    /// L2: Redis 分布式缓存（带防击穿保护）
    dist_cache: Option<Arc<CacheService>>,
    /// 缓存 TTL（秒）
    cache_ttl_secs: u64,
    /// 缓存容量
    cache_capacity: usize,
}

impl std::fmt::Debug for PricingService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PricingService")
            .field("pool", &"DatabaseConnection")
            .field("cache", &"LruCache")
            .field(
                "dist_cache",
                &self.dist_cache.as_ref().map(|_| "<CacheService>"),
            )
            .field("cache_ttl_secs", &self.cache_ttl_secs)
            .field("cache_capacity", &self.cache_capacity)
            .finish()
    }
}

impl Default for PricingService {
    fn default() -> Self {
        Self::new()
    }
}

impl PricingService {
    /// 创建新的定价服务（无数据库连接，使用默认价格）
    pub fn new() -> Self {
        Self {
            pool: None,
            cache: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(DEFAULT_CACHE_CAPACITY).unwrap(),
            ))),
            dist_cache: None,
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    /// 创建带数据库连接的定价服务
    pub fn with_pool(pool: Arc<DbRouter>) -> Self {
        Self {
            pool: Some(pool),
            cache: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(DEFAULT_CACHE_CAPACITY).unwrap(),
            ))),
            dist_cache: None,
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    /// 创建带数据库连接和 Redis 分布式缓存的定价服务
    ///
    /// L2 分布式缓存提供跨实例防击穿保护：
    /// 缓存过期时仅一个实例回源查 DB，其他实例等待后从缓存读取。
    pub fn with_pool_and_cache(pool: Arc<DbRouter>, dist_cache: Arc<CacheService>) -> Self {
        Self {
            pool: Some(pool),
            cache: Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(DEFAULT_CACHE_CAPACITY).unwrap(),
            ))),
            dist_cache: Some(dist_cache),
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
            cache_capacity: DEFAULT_CACHE_CAPACITY,
        }
    }

    /// 设置缓存 TTL
    pub fn with_cache_ttl(mut self, ttl_secs: u64) -> Self {
        self.cache_ttl_secs = ttl_secs;
        self
    }

    /// 设置缓存容量
    pub fn with_cache_capacity(mut self, capacity: usize) -> Self {
        if capacity > 0 {
            self.cache_capacity = capacity;
            self.cache = Arc::new(RwLock::new(LruCache::new(
                NonZeroUsize::new(capacity).unwrap(),
            )));
        }
        self
    }

    /// 设置 Redis 分布式缓存（L2），提供跨实例防击穿保护
    pub fn with_dist_cache(mut self, dist_cache: Arc<CacheService>) -> Self {
        self.dist_cache = Some(dist_cache);
        self
    }

    /// 生成缓存 key
    ///
    /// 缓存键包含计费维度，因为同一个模型在不同计费维度下可能有不同定价
    /// 例如: gpt-4o 在 node 和 provideraccount 下可能有不同价格
    fn cache_key(tenant_id: &Uuid, model_name: &str, provider_type: &str) -> String {
        format!("{}:{}:{}", tenant_id, model_name, provider_type)
    }

    /// 创建价格快照（固化到 RequestContext）
    ///
    /// 从数据库或缓存加载指定模型的价格
    ///
    /// # 参数
    /// - `model_name`: 模型名称
    /// - `tenant_id`: 租户 ID
    /// - `provider`: 计费维度（"node" 或 "provideraccount"）
    ///
    /// # 缓存策略
    /// 每个缓存项保存该租户已解析的有效价格：
    /// `tenant_id:model:billing_dimension`，不能用其他租户预热的平台价格
    /// 跳过本租户定价查询。主库依次解析租户覆盖、平台价格和内置默认价格。
    pub async fn create_snapshot(
        &self,
        model_name: &str,
        tenant_id: &Uuid,
        provider: Option<&str>,
    ) -> Result<PricingSnapshot> {
        let provider = provider.unwrap_or(DEFAULT_PRICING_PROVIDER);
        let base_key = Self::cache_key(tenant_id, model_name, provider);
        // Revisions are part of cache identity, not a best-effort invalidation
        // message. A writer outage fails closed even when a stale L1 exists.
        let key = if let Some(pool) = &self.pool {
            let revision = PricingModel::runtime_cache_revision(
                pool.write_conn(),
                *tenant_id,
                model_name,
                provider,
            )
            .await
            .map_err(|error| KeyComputeError::DatabaseError(error.to_string()))?;
            format!("{base_key}:{revision}")
        } else {
            base_key
        };
        {
            let mut cache = self.cache.write().await;
            if let Some(entry) = cache.get(&key)
                && !entry.is_expired(self.cache_ttl_secs)
            {
                return Ok(entry.snapshot.clone());
            }
        }

        // Absence of a tenant cache entry does not prove absence of a tenant
        // pricing override. A platform cache warmed by another tenant must
        // never short-circuit this tenant's database resolution.
        if let (Some(dist_cache), Some(pool)) = (&self.dist_cache, &self.pool) {
            // Version the namespace to discard snapshots populated by the old
            // platform-first fallback. Only tenant-resolved entries are read.
            let dist_key = format!("pricing:v3:tenant:{key}");
            let result = dist_cache
                .get_or_insert_with_lock::<(PricingSnapshot, PricingSource), _, String>(
                    &dist_key,
                    Duration::from_secs(self.cache_ttl_secs),
                    DEFAULT_LOCK_TTL_SECS,
                    Duration::from_millis(DEFAULT_LOCK_RETRY_MS),
                    DEFAULT_LOCK_MAX_RETRIES,
                    async {
                        let resolved = self
                            .load_from_database_with_source(
                                pool.write_conn(),
                                model_name,
                                tenant_id,
                                provider,
                            )
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok((resolved.snapshot, resolved.source))
                    },
                )
                .await;
            match result {
                Ok((snapshot, _source)) => {
                    self.cache
                        .write()
                        .await
                        .put(key, CacheEntry::new(snapshot.clone()));
                    return Ok(snapshot);
                }
                Err(keycompute_cache::CacheError::FallbackFailed(error)) => {
                    // A database outage is not evidence that no custom price
                    // exists. Do not silently charge a hardcoded fallback.
                    return Err(KeyComputeError::DatabaseError(error));
                }
                Err(error) => {
                    tracing::warn!(%error, "Pricing cache unavailable; resolving on primary database");
                }
            }
        }

        let resolved = if let Some(pool) = &self.pool {
            self.load_from_database_with_source(pool.write_conn(), model_name, tenant_id, provider)
                .await?
        } else {
            SnapshotWithSource {
                snapshot: self.get_default_pricing(model_name),
                source: PricingSource::HardcodedDefault,
            }
        };
        let snapshot = resolved.snapshot;
        self.cache
            .write()
            .await
            .put(key, CacheEntry::new(snapshot.clone()));
        Ok(snapshot)
    }

    /// 更新 RequestContext 的定价快照（路由后调用）
    ///
    /// 当路由确定的 provider 与初始 provider 不同时，
    /// 重新获取定价并更新 RequestContext
    ///
    /// # 设计说明
    /// - 定价查找始终使用计费维度（"node" / "provideraccount"），而非真实 provider
    /// - ctx.provider 字段仍记录真实 provider（用于 UsageLog 和日志追踪）
    /// - 这样确保租户级定价（provider="provideraccount"）在路由后仍然生效
    ///
    /// # 参数
    /// - `ctx`: 可变引用的 RequestContext
    /// - `actual_provider`: 路由确定的实际 provider（如 "openai", "deepseek"）
    ///
    /// # 返回
    /// - `true`: 定价已更新
    /// - `false`: 定价未变化（provider 相同或获取失败）
    pub async fn update_context_pricing(
        &self,
        ctx: &mut keycompute_types::RequestContext,
        actual_provider: &str,
    ) -> bool {
        // 根据模型确定计费维度（而非使用真实 provider）
        let pricing_provider = resolve_pricing_provider(ctx.access_mode);

        let current_provider = ctx.provider.as_deref().unwrap_or(pricing_provider);

        // 如果计费维度相同，只需设置 provider 字段（如果尚未设置）
        if current_provider == pricing_provider {
            if ctx.provider.is_none() {
                ctx.set_provider(actual_provider);
            }
            return false;
        }

        // 获取新计费维度的定价
        match self
            .create_snapshot(&ctx.model, &ctx.tenant_id, Some(pricing_provider))
            .await
        {
            Ok(new_pricing) => {
                tracing::debug!(
                    request_id = %ctx.request_id,
                    model = %ctx.model,
                    old_provider = %current_provider,
                    new_provider = %actual_provider,
                    pricing_provider = %pricing_provider,
                    "Updated pricing for different provider"
                );
                ctx.set_provider(actual_provider);
                ctx.update_pricing(new_pricing);
                true
            }
            Err(e) => {
                tracing::warn!(
                    request_id = %ctx.request_id,
                    model = %ctx.model,
                    provider = %actual_provider,
                    error = %e,
                    "Failed to update pricing for provider, keeping original"
                );
                false
            }
        }
    }

    /// 从数据库加载价格（带来源标记）
    ///
    /// 定价仅按计费维度（"node" / "provideraccount"）区分，不按真实 provider 区分
    async fn load_from_database_with_source(
        &self,
        pool: &impl ConnectionTrait,
        model_name: &str,
        tenant_id: &Uuid,
        provider: &str,
    ) -> Result<SnapshotWithSource> {
        // 尝试按租户+模型名+计费维度查找
        let pricing =
            PricingModel::find_effective_for_runtime(pool, *tenant_id, model_name, provider)
                .await
                .map_err(|e| {
                    KeyComputeError::DatabaseError(format!("Failed to load pricing: {}", e))
                })?;

        if let Some(p) = pricing {
            // 判断结果是租户特定定价还是全局默认定价
            // The runtime lookup may return an explicit platform default.
            let is_global_default = p.scope_type == PricingScopeType::Platform;
            let source = if is_global_default {
                PricingSource::DatabaseDefault
            } else {
                PricingSource::TenantSpecific
            };

            return Ok(SnapshotWithSource {
                snapshot: PricingSnapshot {
                    model_name: p.model_name,
                    currency: p.currency,
                    input_price_per_1k: bigdecimal_to_decimal(&p.input_price_per_1k)?,
                    output_price_per_1k: bigdecimal_to_decimal(&p.output_price_per_1k)?,
                },
                source,
            });
        }

        // 尝试查找默认定价（按计费维度匹配）
        let defaults = PricingModel::find_runtime_defaults(pool)
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to load default pricing: {}", e))
            })?;

        if let Some(p) = find_global_default(&defaults, model_name, provider) {
            if p.billing_dimension.as_str() != provider {
                tracing::debug!(
                    model = %model_name,
                    requested_dimension = %provider,
                    fallback_dimension = %p.billing_dimension,
                    "Using default pricing from different billing dimension"
                );
            }
            return Ok(SnapshotWithSource {
                snapshot: PricingSnapshot {
                    model_name: p.model_name.clone(),
                    currency: p.currency.clone(),
                    input_price_per_1k: bigdecimal_to_decimal(&p.input_price_per_1k)?,
                    output_price_per_1k: bigdecimal_to_decimal(&p.output_price_per_1k)?,
                },
                source: PricingSource::DatabaseDefault,
            });
        }

        // 未找到，使用硬编码默认价格
        tracing::warn!(
            model = %model_name,
            tenant_id = %tenant_id,
            provider = %provider,
            "No pricing found in database, using hardcoded default"
        );
        Ok(SnapshotWithSource {
            snapshot: self.get_default_pricing(model_name),
            source: PricingSource::HardcodedDefault,
        })
    }

    /// 获取默认定价
    ///
    /// 统一使用默认价格，不再区分模型
    fn get_default_pricing(&self, model_name: &str) -> PricingSnapshot {
        PricingSnapshot {
            model_name: model_name.to_string(),
            currency: "CNY".to_string(),
            // 统一默认价格：输入 0.1 元/1k tokens，输出 0.3 元/1k tokens
            input_price_per_1k: Decimal::from(100) / Decimal::from(1000),
            output_price_per_1k: Decimal::from(300) / Decimal::from(1000),
        }
    }

    /// 清除缓存
    pub async fn clear_cache(&self) {
        // Invalidate L2 first. If L1 were cleared first, a concurrent request
        // could read the stale distributed entry and repopulate L1 after this
        // method had already cleared it.
        if let Some(dist_cache) = &self.dist_cache
            && let Err(error) = dist_cache.delete_matching("pricing:").await
        {
            tracing::warn!(%error, "Failed to clear distributed pricing cache");
        }
        let mut cache = self.cache.write().await;
        cache.clear();
        tracing::info!("Pricing cache cleared");
    }

    /// 清除过期缓存条目
    ///
    /// 由于 LruCache 不支持 retain，需要手动收集过期 key 后删除
    pub async fn clear_expired(&self) {
        let mut cache = self.cache.write().await;
        let before_len = cache.len();

        // 收集过期的 key
        let expired_keys: Vec<String> = cache
            .iter()
            .filter(|(_, entry)| entry.is_expired(self.cache_ttl_secs))
            .map(|(key, _)| key.clone())
            .collect();

        // 删除过期条目
        for key in expired_keys {
            cache.pop(&key);
        }

        let after_len = cache.len();
        if before_len != after_len {
            tracing::info!(
                removed = before_len - after_len,
                remaining = after_len,
                "Expired cache entries cleared"
            );
        }
    }

    /// 预热缓存（从数据库加载所有默认定价）
    ///
    /// 使用显式 platform scope 预热默认定价
    pub async fn warmup_cache(&self) -> Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let defaults = PricingModel::find_runtime_defaults(pool.write_conn())
            .await
            .map_err(|e| {
                KeyComputeError::DatabaseError(format!("Failed to load default pricing: {}", e))
            })?;

        let mut cache = self.cache.write().await;
        for p in defaults {
            let snapshot = PricingSnapshot {
                model_name: p.model_name.clone(),
                currency: p.currency.clone(),
                input_price_per_1k: bigdecimal_to_decimal(&p.input_price_per_1k)?,
                output_price_per_1k: bigdecimal_to_decimal(&p.output_price_per_1k)?,
            };
            // 使用 platform scope 和计费维度作为缓存 key
            let key = format!("platform:{}:{}", p.model_name, p.billing_dimension.as_str());
            cache.put(key, CacheEntry::new(snapshot));
        }

        tracing::info!(count = cache.len(), "Pricing cache warmed up");
        Ok(())
    }

    /// 计算请求费用
    pub fn calculate_cost(
        &self,
        input_tokens: u32,
        output_tokens: u32,
        pricing: &PricingSnapshot,
    ) -> Decimal {
        let input_cost = (Decimal::from(input_tokens) * pricing.input_price_per_1k
            / Decimal::from(1000))
        .round_dp(10);
        let output_cost = (Decimal::from(output_tokens) * pricing.output_price_per_1k
            / Decimal::from(1000))
        .round_dp(10);
        // Keep the service-level result aligned with billing breakdowns: each
        // displayed line item is rounded to database precision before summing.
        input_cost + output_cost
    }

    /// 检查是否已配置数据库连接
    ///
    /// 用于启动时验证配置
    pub fn has_pool(&self) -> bool {
        self.pool.is_some()
    }
}

/// 将 BigDecimal 转换为 Decimal（精确转换）
///
/// 对于价格数据，使用字符串中间格式足够精确，因为：
/// 1. 价格通常只有 2-6 位小数
/// 2. BigDecimal 和 Decimal 都支持任意精度
/// 3. 避免手动处理 BigInt 导致的溢出问题
///
/// # 返回
/// - `Ok(Decimal)`: 转换成功
/// - `Err(KeyComputeError)`: 转换失败，返回错误信息
fn bigdecimal_to_decimal(value: &bigdecimal::BigDecimal) -> Result<Decimal> {
    let s = value.to_string();
    s.parse::<Decimal>().map_err(|e| {
        tracing::error!(value = %s, error = %e, "Failed to convert BigDecimal to Decimal");
        KeyComputeError::Internal(format!(
            "Failed to convert BigDecimal '{}' to Decimal: {}",
            s, e
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use std::str::FromStr;

    /// 测试缓存 key 生成（包含计费维度）
    #[test]
    fn test_cache_key_generation() {
        let tenant_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let key = PricingService::cache_key(&tenant_id, "gpt-4o", "provideraccount");

        assert!(key.contains("gpt-4o"));
        assert!(key.contains("provideraccount"));
        assert!(key.contains("00000000-0000-0000-0000-000000000001"));
    }

    /// 测试 platform scope 的缓存 key
    #[test]
    fn test_cache_key_platform() {
        let key = format!("platform:{}:{}", "gpt-4o", "provideraccount");

        assert!(key.starts_with("platform:"));
    }

    #[test]
    fn tenant_defaults_are_never_selected_as_global_fallbacks() {
        let tenant_default = PricingModel {
            id: Uuid::new_v4(),
            scope_type: PricingScopeType::Tenant,
            tenant_id: Some(Uuid::new_v4()),
            model_name: "gpt-4o".to_string(),
            billing_dimension:
                keycompute_db::models::pricing_model::BillingDimension::ProviderAccount,
            currency: "CNY".to_string(),
            input_price_per_1k: BigDecimal::from(1),
            output_price_per_1k: BigDecimal::from(2),
            is_default: true,
            version: 1,
            effective_from: chrono::Utc::now(),
            effective_until: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert!(find_global_default(&[tenant_default], "gpt-4o", "provideraccount").is_none());
    }

    /// 测试成本计算
    #[test]
    fn test_calculate_cost() {
        let service = PricingService::new();
        let snapshot = PricingSnapshot {
            model_name: "test-model".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::from(100), // 0.1 元/token
            output_price_per_1k: Decimal::from(200), // 0.2 元/token
        };

        // 1000 input tokens + 500 output tokens
        // input_cost = 1000 * 100 / 1000 = 100
        // output_cost = 500 * 200 / 1000 = 100
        // total = 200
        let cost = service.calculate_cost(1000, 500, &snapshot);
        assert_eq!(cost, Decimal::from(200));
    }

    /// 测试成本计算 - 边界情况
    #[test]
    fn test_calculate_cost_zero_tokens() {
        let service = PricingService::new();
        let snapshot = PricingSnapshot {
            model_name: "test-model".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::from(100),
            output_price_per_1k: Decimal::from(200),
        };

        let cost = service.calculate_cost(0, 0, &snapshot);
        assert_eq!(cost, Decimal::ZERO);
    }

    #[test]
    fn test_calculate_cost_rounds_line_items_before_total() {
        let service = PricingService::new();
        let snapshot = PricingSnapshot {
            model_name: "tiny".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::new(6, 8),
            output_price_per_1k: Decimal::new(6, 8),
        };

        // Each raw line item is 6e-11 and rounds to 1e-10. Summing first would
        // incorrectly produce only 1e-10 after the final rounding step.
        assert_eq!(service.calculate_cost(1, 1, &snapshot), Decimal::new(2, 10));
    }

    /// 测试默认定价获取 - 统一使用相同默认价格
    #[test]
    fn test_get_default_pricing() {
        let service = PricingService::new();

        // 测试不同模型都返回相同的默认价格
        let models = [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
            "unknown-model",
        ];

        for model in models {
            let snapshot = service.get_default_pricing(model);

            assert_eq!(snapshot.model_name, model);
            assert_eq!(snapshot.currency, "CNY");
            // 统一默认价格: input = 0.1, output = 0.3
            assert_eq!(
                snapshot.input_price_per_1k,
                Decimal::from_str("0.1").unwrap()
            );
            assert_eq!(
                snapshot.output_price_per_1k,
                Decimal::from_str("0.3").unwrap()
            );
        }
    }

    /// 测试 BigDecimal 到 Decimal 转换 - 简单小数
    #[test]
    fn test_bigdecimal_to_decimal_simple() {
        let bd = BigDecimal::from_str("0.5").unwrap();
        let d = bigdecimal_to_decimal(&bd).unwrap();
        assert_eq!(d, Decimal::from_str("0.5").unwrap());
    }

    /// 测试 BigDecimal 到 Decimal 转换 - 整数
    #[test]
    fn test_bigdecimal_to_decimal_integer() {
        let bd = BigDecimal::from_str("100").unwrap();
        let d = bigdecimal_to_decimal(&bd).unwrap();
        assert_eq!(d, Decimal::from(100));
    }

    /// 测试 BigDecimal 到 Decimal 转换 - 多位小数
    #[test]
    fn test_bigdecimal_to_decimal_precision() {
        let bd = BigDecimal::from_str("0.123456789").unwrap();
        let d = bigdecimal_to_decimal(&bd).unwrap();
        assert_eq!(d, Decimal::from_str("0.123456789").unwrap());
    }

    /// 测试 BigDecimal 到 Decimal 转换 - 零
    #[test]
    fn test_bigdecimal_to_decimal_zero() {
        let bd = BigDecimal::from_str("0").unwrap();
        let d = bigdecimal_to_decimal(&bd).unwrap();
        assert_eq!(d, Decimal::ZERO);
    }

    /// 测试 BigDecimal 到 Decimal 转换 - 大数
    #[test]
    fn test_bigdecimal_to_decimal_large() {
        let bd = BigDecimal::from_str("12345.67").unwrap();
        let d = bigdecimal_to_decimal(&bd).unwrap();
        assert_eq!(d, Decimal::from_str("12345.67").unwrap());
    }

    /// 测试缓存条目过期检查
    #[test]
    fn test_cache_entry_expiry() {
        let snapshot = PricingSnapshot {
            model_name: "test".to_string(),
            currency: "CNY".to_string(),
            input_price_per_1k: Decimal::ONE,
            output_price_per_1k: Decimal::ONE,
        };
        let entry = CacheEntry::new(snapshot);

        // 新创建的条目不应过期
        assert!(!entry.is_expired(300));

        // TTL 为 0 时应立即过期
        assert!(entry.is_expired(0));
    }

    /// 测试 PricingService 创建
    #[test]
    fn test_pricing_service_new() {
        let service = PricingService::new();
        assert!(!service.has_pool());
    }

    /// 测试 PricingService 配置链式调用
    #[test]
    fn test_pricing_service_with_cache_ttl() {
        let service = PricingService::new().with_cache_ttl(600);
        assert_eq!(service.cache_ttl_secs, 600);
    }

    /// 测试无数据库时创建快照
    #[tokio::test]
    async fn test_create_snapshot_without_db() {
        let service = PricingService::new();
        let tenant_id = Uuid::new_v4();

        let snapshot = service
            .create_snapshot("gpt-4o", &tenant_id, None)
            .await
            .unwrap();

        assert_eq!(snapshot.model_name, "gpt-4o");
        assert_eq!(snapshot.currency, "CNY");
        assert!(snapshot.input_price_per_1k > Decimal::ZERO);
        assert!(snapshot.output_price_per_1k > Decimal::ZERO);
    }

    /// 即使有效价格相同，不同租户也保留独立的已解析缓存。
    #[tokio::test]
    async fn test_default_pricing_cache_is_resolved_per_tenant() {
        let service = PricingService::new();
        let tenant1 = Uuid::new_v4();
        let tenant2 = Uuid::new_v4();

        // 第一次请求，缓存未命中，使用默认价格
        let snapshot1 = service
            .create_snapshot("gpt-4o", &tenant1, None)
            .await
            .unwrap();

        // 第二次请求属于不同租户，必须独立解析，不能复用平台缓存。
        let snapshot2 = service
            .create_snapshot("gpt-4o", &tenant2, None)
            .await
            .unwrap();

        // 两个快照应该相同
        assert_eq!(snapshot1.input_price_per_1k, snapshot2.input_price_per_1k);
        assert_eq!(snapshot1.output_price_per_1k, snapshot2.output_price_per_1k);

        let cache = service.cache.write().await;
        assert_eq!(cache.len(), 2);
        for tenant in [&tenant1, &tenant2] {
            assert!(cache.contains(&PricingService::cache_key(
                tenant,
                "gpt-4o",
                "provideraccount"
            )));
        }
        assert!(!cache.contains(&"platform:gpt-4o:provideraccount".to_owned()));
    }

    /// 测试清除缓存
    #[tokio::test]
    async fn test_clear_cache() {
        let service = PricingService::new();
        let tenant_id = Uuid::new_v4();

        // 创建快照，填充缓存
        let _ = service
            .create_snapshot("gpt-4o", &tenant_id, None)
            .await
            .unwrap();

        // 清除缓存
        service.clear_cache().await;

        // 验证缓存已清空
        let cache = service.cache.write().await;
        assert!(cache.is_empty());
    }

    /// 测试不同计费维度请求的缓存拆分
    /// 验证缓存按计费维度区分，不同计费维度有独立缓存
    #[tokio::test]
    async fn test_cache_split_by_provider_dimension() {
        let service = PricingService::new();
        let tenant1 = Uuid::new_v4();
        let tenant2 = Uuid::new_v4();

        // 第一次请求：使用 provideraccount 计费维度
        let snapshot1 = service
            .create_snapshot("gpt-4o", &tenant1, Some("provideraccount"))
            .await
            .unwrap();

        // 第二次请求：使用 node 计费维度，不同租户
        let snapshot2 = service
            .create_snapshot("llama3", &tenant2, Some("node"))
            .await
            .unwrap();

        // 验证两个请求都返回相同的默认价格（硬编码）
        assert_eq!(snapshot1.input_price_per_1k, snapshot2.input_price_per_1k);
        assert_eq!(snapshot1.output_price_per_1k, snapshot2.output_price_per_1k);

        // 验证缓存有 2 个条目（不同计费维度独立缓存）
        {
            let cache = service.cache.write().await;
            // 应该有 2 个缓存条目：一个 provideraccount，一个 node
            assert_eq!(cache.len(), 2, "不同计费维度应该有独立的缓存条目");
        }
    }

    /// 测试 LRU 缓存容量限制
    #[tokio::test]
    async fn test_lru_cache_capacity() {
        // 创建一个容量为 3 的服务
        let service = PricingService::new().with_cache_capacity(3);
        let tenant = Uuid::new_v4();

        // 插入 4 个不同的模型
        for i in 0..4 {
            let model_name = format!("model-{}", i);
            let _ = service
                .create_snapshot(&model_name, &tenant, None)
                .await
                .unwrap();
        }

        // 验证缓存只有 3 个条目（容量限制）
        {
            let cache = service.cache.write().await;
            assert_eq!(cache.len(), 3, "缓存应受容量限制");
        }

        // 验证 model-0 被淘汰（最旧的条目）
        {
            let cache = service.cache.write().await;
            let key0 = PricingService::cache_key(&tenant, "model-0", "provideraccount");
            assert!(!cache.contains(&key0), "model-0 应该被 LRU 淘汰");

            // 验证最新的 3 个条目存在
            for i in 1..4 {
                let key =
                    PricingService::cache_key(&tenant, &format!("model-{i}"), "provideraccount");
                assert!(cache.contains(&key), "model-{} 应该在缓存中", i);
            }
        }
    }

    /// 测试 PricingService 配置链式调用 - 缓存容量
    #[test]
    fn test_pricing_service_with_cache_capacity() {
        let service = PricingService::new().with_cache_capacity(500);
        assert_eq!(service.cache_capacity, 500);
    }

    /// 测试 resolve_pricing_provider 函数
    #[test]
    fn test_resolve_pricing_provider() {
        use keycompute_types::ModelAccessMode;
        assert_eq!(
            resolve_pricing_provider(ModelAccessMode::NodeDispatch),
            "node"
        );
        assert_eq!(
            resolve_pricing_provider(ModelAccessMode::AccountPool),
            "provideraccount"
        );
        assert_eq!(
            resolve_pricing_provider(ModelAccessMode::Passthrough),
            "provideraccount"
        );
    }

    /// 测试常量值
    #[test]
    fn test_pricing_provider_constants() {
        assert_eq!(NODE_PRICING_PROVIDER, "node");
        assert_eq!(DEFAULT_PRICING_PROVIDER, "provideraccount");
    }
}
