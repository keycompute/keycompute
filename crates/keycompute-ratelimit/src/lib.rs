//! Rate Limit Module
//!
//! 限流模块，支持内存后端和 Redis 后端，按 user/tenant/key 多维度限流。

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

/// 限流参数常量（硬编码，不可通过配置修改）
pub(crate) const RPM_LIMIT: u32 = 60;
pub(crate) const WINDOW_SECS: u64 = 60;
/// 并发请求限制（供未来使用）
#[allow(dead_code)]
const CONCURRENCY_LIMIT: u32 = 10;

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
    /// 检查是否允许请求
    async fn check(&self, key: &RateLimitKey) -> Result<bool>;

    /// 记录请求（通过后调用）
    async fn record(&self, key: &RateLimitKey) -> Result<()>;

    /// 获取 RPM 限制
    fn rpm_limit(&self) -> u32;
}

/// 内存限流器
#[derive(Debug)]
pub struct MemoryRateLimiter {
    counters: DashMap<RateLimitKey, RateCounter>,
    window_size: Duration,
}

impl MemoryRateLimiter {
    /// 创建新的内存限流器
    pub fn new() -> Self {
        Self {
            counters: DashMap::new(),
            window_size: Duration::from_secs(WINDOW_SECS),
        }
    }

    /// 清理过期计数器
    pub fn cleanup(&self) {
        self.counters.retain(|_, counter| !counter.is_expired());
    }

    /// 获取计数器
    fn get_counter(&self, key: &RateLimitKey) -> RateCounter {
        self.counters
            .entry(key.clone())
            .or_insert_with(|| RateCounter::new(self.window_size))
            .clone()
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
        let counter = self.get_counter(key);

        // 检查是否过期，如果过期重置
        if counter.is_expired() {
            if let Some(mut entry) = self.counters.get_mut(key) {
                entry.reset();
            }
        }

        let count = counter.count();
        Ok(count < RPM_LIMIT as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let counter = self.get_counter(key);
        counter.increment();
        Ok(())
    }

    fn rpm_limit(&self) -> u32 {
        RPM_LIMIT
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

    /// 检查并记录请求
    pub async fn check_and_record(&self, key: &RateLimitKey) -> Result<()> {
        if !self.limiter.check(key).await? {
            return Err(KeyComputeError::RateLimitExceeded);
        }
        self.limiter.record(key).await
    }

    /// 仅检查不限流
    pub async fn check_only(&self, key: &RateLimitKey) -> Result<bool> {
        self.limiter.check(key).await
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
        assert_eq!(RPM_LIMIT, 60);
        assert_eq!(CONCURRENCY_LIMIT, 10);
        assert_eq!(WINDOW_SECS, 60);
    }

    #[tokio::test]
    async fn test_memory_rate_limiter() {
        let limiter = MemoryRateLimiter::default();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 第一次检查应该通过
        assert!(limiter.check(&key).await.unwrap());

        // 记录请求
        limiter.record(&key).await.unwrap();

        // 检查仍应通过（未达到限制）
        assert!(limiter.check(&key).await.unwrap());
    }

    #[tokio::test]
    async fn test_rate_limit_service() {
        let service = RateLimitService::default_memory();
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 第一次请求应该成功
        assert!(service.check_and_record(&key).await.is_ok());
    }
}
