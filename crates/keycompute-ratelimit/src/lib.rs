//! Rate Limit Module
//!
//! 限流模块，支持内存后端和 Redis 后端，按 user/tenant/key 多维度限流。
//! 支持从租户配置动态加载 RPM/TPM 限制。

use async_trait::async_trait;
use dashmap::DashMap;
use keycompute_types::{KeyComputeError, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[cfg(feature = "redis")]
pub mod redis;

#[cfg(feature = "redis")]
pub use redis::RedisRateLimiter;

/// 默认限流参数（当租户未配置时使用）
pub const DEFAULT_RPM_LIMIT: u32 = 60;
pub const DEFAULT_TPM_LIMIT: u32 = 100_000;
pub const WINDOW_SECS: u64 = 60;
/// 并发请求限制（供未来使用）
#[allow(dead_code)]
const CONCURRENCY_LIMIT: u32 = 10;

/// 限流配置（包含租户特定的限制）
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// RPM (Requests Per Minute) 限制
    pub rpm_limit: u32,
    /// TPM (Tokens Per Minute) 限制
    pub tpm_limit: u32,
    /// 窗口大小（秒）
    pub window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            rpm_limit: DEFAULT_RPM_LIMIT,
            tpm_limit: DEFAULT_TPM_LIMIT,
            window_secs: WINDOW_SECS,
        }
    }
}

impl RateLimitConfig {
    /// 创建新的限流配置
    pub fn new(rpm_limit: u32, tpm_limit: u32) -> Self {
        Self {
            rpm_limit,
            tpm_limit,
            window_secs: WINDOW_SECS,
        }
    }

    /// 从租户字段创建
    pub fn from_tenant(rpm_limit: i32, tpm_limit: i32) -> Self {
        Self {
            rpm_limit: rpm_limit.max(1) as u32,
            tpm_limit: tpm_limit.max(1) as u32,
            window_secs: WINDOW_SECS,
        }
    }
}

/// 限流键
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct RateLimitKey {
    /// 租户 ID
    pub tenant_id: Uuid,
    /// 用户 ID
    pub user_id: Uuid,
    /// API Key ID
    pub api_key_id: Uuid,
}

impl RateLimitKey {
    /// 创建新的限流键
    pub fn new(tenant_id: Uuid, user_id: Uuid, api_key_id: Uuid) -> Self {
        Self {
            tenant_id,
            user_id,
            api_key_id,
        }
    }
}

/// 限流计数器
#[derive(Debug)]
struct RateCounter {
    /// 当前计数
    count: AtomicU64,
    /// 窗口开始时间
    window_start: Instant,
    /// 窗口大小
    window_size: Duration,
}

impl Clone for RateCounter {
    fn clone(&self) -> Self {
        Self {
            count: AtomicU64::new(self.count.load(Ordering::Relaxed)),
            window_start: self.window_start,
            window_size: self.window_size,
        }
    }
}

impl RateCounter {
    fn new(window_size: Duration) -> Self {
        Self {
            count: AtomicU64::new(0),
            window_start: Instant::now(),
            window_size,
        }
    }

    fn is_expired(&self) -> bool {
        Instant::now().duration_since(self.window_start) > self.window_size
    }

    fn reset(&mut self) {
        self.count.store(0, Ordering::Relaxed);
        self.window_start = Instant::now();
    }

    fn increment(&self) -> u64 {
        self.count.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

/// 限流器 trait
#[async_trait]
pub trait RateLimiter: Send + Sync + std::fmt::Debug {
    /// 检查是否允许请求（使用默认限制）
    async fn check(&self, key: &RateLimitKey) -> Result<bool>;

    /// 检查是否允许请求（使用租户特定限制）
    async fn check_with_config(&self, key: &RateLimitKey, config: &RateLimitConfig)
    -> Result<bool>;

    /// 记录请求（通过后调用）
    async fn record(&self, key: &RateLimitKey) -> Result<()>;

    /// 原子地检查并记录请求（使用租户特定限制）
    ///
    /// 默认实现先检查再记录，非原子操作。分布式场景应覆盖此方法。
    async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<()> {
        if !self.check_with_config(key, config).await? {
            return Err(KeyComputeError::RateLimitExceeded);
        }
        self.record(key).await
    }

    /// 记录 Token 使用量
    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()>;

    /// 获取当前计数
    async fn get_count(&self, key: &RateLimitKey) -> Result<u64>;

    /// 获取当前 Token 使用量
    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64>;
}

/// 内存限流器
#[derive(Debug)]
pub struct MemoryRateLimiter {
    /// 请求计数器
    request_counters: DashMap<RateLimitKey, RateCounter>,
    /// Token 计数器
    token_counters: DashMap<RateLimitKey, RateCounter>,
    window_size: Duration,
}

impl MemoryRateLimiter {
    /// 创建新的内存限流器
    pub fn new() -> Self {
        Self {
            request_counters: DashMap::new(),
            token_counters: DashMap::new(),
            window_size: Duration::from_secs(WINDOW_SECS),
        }
    }

    /// 清理过期计数器
    pub fn cleanup(&self) {
        self.request_counters
            .retain(|_, counter| !counter.is_expired());
        self.token_counters
            .retain(|_, counter| !counter.is_expired());
    }

    /// 获取或创建请求计数器条目
    fn get_request_counter_entry(
        &self,
        key: &RateLimitKey,
    ) -> dashmap::mapref::one::Ref<'_, RateLimitKey, RateCounter> {
        self.request_counters
            .entry(key.clone())
            .or_insert_with(|| RateCounter::new(self.window_size))
            .downgrade()
    }

    /// 获取或创建 Token 计数器条目
    fn get_token_counter_entry(
        &self,
        key: &RateLimitKey,
    ) -> dashmap::mapref::one::Ref<'_, RateLimitKey, RateCounter> {
        self.token_counters
            .entry(key.clone())
            .or_insert_with(|| RateCounter::new(self.window_size))
            .downgrade()
    }

    /// 获取请求计数器的可变条目
    fn get_request_counter_mut(
        &self,
        key: &RateLimitKey,
    ) -> dashmap::mapref::one::RefMut<'_, RateLimitKey, RateCounter> {
        self.request_counters
            .entry(key.clone())
            .or_insert_with(|| RateCounter::new(self.window_size))
    }

    /// 获取 Token 计数器的可变条目
    fn get_token_counter_mut(
        &self,
        key: &RateLimitKey,
    ) -> dashmap::mapref::one::RefMut<'_, RateLimitKey, RateCounter> {
        self.token_counters
            .entry(key.clone())
            .or_insert_with(|| RateCounter::new(self.window_size))
    }
}

impl Default for MemoryRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RateLimiter for MemoryRateLimiter {
    async fn check(&self, key: &RateLimitKey) -> Result<bool> {
        self.check_with_config(key, &RateLimitConfig::default())
            .await
    }

    async fn check_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<bool> {
        // 检查是否过期，如果过期重置
        if let Some(counter) = self.request_counters.get(key)
            && counter.is_expired()
        {
            drop(counter);
            if let Some(mut entry) = self.request_counters.get_mut(key) {
                entry.reset();
            }
            // 同时重置 Token 计数器
            if let Some(mut entry) = self.token_counters.get_mut(key) {
                entry.reset();
            }
        }

        // 获取计数并检查
        let counter = self.get_request_counter_entry(key);
        let count = counter.count();
        Ok(count < config.rpm_limit as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let counter = self.get_request_counter_mut(key);
        counter.increment();
        Ok(())
    }

    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        let counter = self.get_token_counter_mut(key);
        counter.count.fetch_add(tokens as u64, Ordering::Relaxed);
        Ok(())
    }

    async fn get_count(&self, key: &RateLimitKey) -> Result<u64> {
        let counter = self.get_request_counter_entry(key);
        Ok(counter.count())
    }

    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64> {
        let counter = self.get_token_counter_entry(key);
        Ok(counter.count())
    }
}

/// 限流服务
pub struct RateLimitService {
    limiter: std::sync::Arc<dyn RateLimiter>,
    backend: RateLimitBackend,
}

/// 限流后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitBackend {
    /// 内存后端
    Memory,
    /// Redis 后端
    Redis,
}

impl std::fmt::Debug for RateLimitService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitService")
            .field("backend", &self.backend)
            .finish()
    }
}

impl Clone for RateLimitService {
    fn clone(&self) -> Self {
        Self {
            limiter: Arc::clone(&self.limiter),
            backend: self.backend,
        }
    }
}

impl RateLimitService {
    /// 创建新的限流服务
    pub fn new(limiter: std::sync::Arc<dyn RateLimiter>, backend: RateLimitBackend) -> Self {
        Self { limiter, backend }
    }

    /// 创建默认的内存限流服务
    pub fn default_memory() -> Self {
        Self::new(
            std::sync::Arc::new(MemoryRateLimiter::default()),
            RateLimitBackend::Memory,
        )
    }

    /// 获取后端类型
    pub fn backend(&self) -> RateLimitBackend {
        self.backend
    }

    /// 检查并记录请求（使用默认限制）
    pub async fn check_and_record(&self, key: &RateLimitKey) -> Result<()> {
        if !self.limiter.check(key).await? {
            return Err(KeyComputeError::RateLimitExceeded);
        }
        self.limiter.record(key).await
    }

    /// 检查并记录请求（使用租户特定限制）
    ///
    /// 这是主要入口，用于应用租户的配额限制
    pub async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<()> {
        // 使用 limiter 的原子方法（分布式后端会覆盖实现）
        self.limiter
            .check_and_record_with_config(key, config)
            .await
            .map_err(|e| {
                if matches!(e, KeyComputeError::RateLimitExceeded) {
                    tracing::warn!(
                        tenant_id = %key.tenant_id,
                        user_id = %key.user_id,
                        rpm_limit = config.rpm_limit,
                        "RPM limit exceeded"
                    );
                }
                e
            })
    }

    /// 仅检查不限流
    pub async fn check_only(&self, key: &RateLimitKey) -> Result<bool> {
        self.limiter.check(key).await
    }

    /// 仅检查不限流（使用租户特定限制）
    pub async fn check_only_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<bool> {
        self.limiter.check_with_config(key, config).await
    }

    /// 记录 Token 使用量（用于 TPM 限制）
    pub async fn record_token_usage(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        self.limiter.record_tokens(key, tokens).await
    }

    /// 检查 TPM 限制
    pub async fn check_tpm(&self, key: &RateLimitKey, config: &RateLimitConfig) -> Result<bool> {
        let current_tokens = self.limiter.get_token_count(key).await?;
        Ok(current_tokens < config.tpm_limit as u64)
    }

    /// 获取当前 RPM 计数
    pub async fn get_rpm_count(&self, key: &RateLimitKey) -> Result<u64> {
        self.limiter.get_count(key).await
    }

    /// 获取当前 TPM 计数
    pub async fn get_tpm_count(&self, key: &RateLimitKey) -> Result<u64> {
        self.limiter.get_token_count(key).await
    }
}

#[cfg(feature = "redis")]
impl RateLimitService {
    /// 创建 Redis 限流服务
    pub fn new_redis(redis_url: &str) -> Result<Self> {
        let limiter = RedisRateLimiter::new(redis_url)?;
        Ok(Self::new(
            std::sync::Arc::new(limiter),
            RateLimitBackend::Redis,
        ))
    }

    /// 创建带前缀的 Redis 限流服务
    pub fn new_redis_with_prefix(redis_url: &str, prefix: impl Into<String>) -> Result<Self> {
        let limiter = RedisRateLimiter::with_prefix(redis_url, prefix)?;
        Ok(Self::new(
            std::sync::Arc::new(limiter),
            RateLimitBackend::Redis,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_key() {
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        assert!(!key.tenant_id.is_nil());
    }

    #[test]
    fn test_rate_limit_constants() {
        assert_eq!(DEFAULT_RPM_LIMIT, 60);
        assert_eq!(DEFAULT_TPM_LIMIT, 100_000);
        assert_eq!(CONCURRENCY_LIMIT, 10);
        assert_eq!(WINDOW_SECS, 60);
    }

    #[test]
    fn test_rate_limit_config() {
        let config = RateLimitConfig::default();
        assert_eq!(config.rpm_limit, 60);
        assert_eq!(config.tpm_limit, 100_000);

        let config = RateLimitConfig::new(100, 200_000);
        assert_eq!(config.rpm_limit, 100);
        assert_eq!(config.tpm_limit, 200_000);

        let config = RateLimitConfig::from_tenant(120, 150_000);
        assert_eq!(config.rpm_limit, 120);
        assert_eq!(config.tpm_limit, 150_000);
    }

    #[tokio::test]
    async fn test_memory_rate_limiter() {
        let limiter = MemoryRateLimiter::default();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::default();

        // 第一次检查应该通过
        assert!(limiter.check_with_config(&key, &config).await.unwrap());

        // 记录请求
        limiter.record(&key).await.unwrap();

        // 检查仍应通过（未达到限制）
        assert!(limiter.check_with_config(&key, &config).await.unwrap());
    }

    #[tokio::test]
    async fn test_rate_limit_service() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = RateLimitConfig::default();

        // 第一次请求应该成功
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn test_rate_limit_service_with_custom_config() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 使用低限制配置
        let config = RateLimitConfig::new(2, 1000);

        // 前两次请求应该成功
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_ok()
        );
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_ok()
        );

        // 第三次请求应该被拒绝
        assert!(
            service
                .check_and_record_with_config(&key, &config)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_token_tracking() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 记录 Token 使用量
        service.record_token_usage(&key, 100).await.unwrap();
        service.record_token_usage(&key, 50).await.unwrap();

        // 检查 Token 计数
        let count = service.get_tpm_count(&key).await.unwrap();
        assert_eq!(count, 150);
    }
}
