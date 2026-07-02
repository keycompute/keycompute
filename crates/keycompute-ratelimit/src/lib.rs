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

/// 限流计数器（单一 key 的完整状态）
///
/// 包含请求计数、Token 计数和共享的窗口时间戳。
/// 共享窗口确保 RPM 和 TPM 始终同步。
#[derive(Debug)]
struct RateLimitEntry {
    /// 请求计数
    request_count: AtomicU64,
    /// Token 计数
    token_count: AtomicU64,
    /// 窗口开始时间（共享）
    window_start: std::sync::Mutex<Instant>,
    /// 窗口大小
    window_size: Duration,
}

impl RateLimitEntry {
    fn new(window_size: Duration) -> Self {
        Self {
            request_count: AtomicU64::new(0),
            token_count: AtomicU64::new(0),
            window_start: std::sync::Mutex::new(Instant::now()),
            window_size,
        }
    }

    fn is_expired(&self) -> bool {
        let start = self.window_start.lock().unwrap();
        Instant::now().duration_since(*start) > self.window_size
    }

    /// 重置计数器（原子操作）
    ///
    /// 返回 true 表示执行了重置，false 表示窗口未过期
    fn reset_if_expired(&self) -> bool {
        let mut start = self.window_start.lock().unwrap();
        if Instant::now().duration_since(*start) > self.window_size {
            self.request_count.store(0, Ordering::Relaxed);
            self.token_count.store(0, Ordering::Relaxed);
            *start = Instant::now();
            true
        } else {
            false
        }
    }

    fn increment_request(&self) -> u64 {
        self.request_count.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn add_tokens(&self, tokens: u64) {
        self.token_count.fetch_add(tokens, Ordering::Relaxed);
    }

    fn request_count(&self) -> u64 {
        self.request_count.load(Ordering::Relaxed)
    }

    fn token_count(&self) -> u64 {
        self.token_count.load(Ordering::Relaxed)
    }

    fn decrement_request(&self) {
        self.request_count.fetch_sub(1, Ordering::Relaxed);
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
            return Err(KeyComputeError::RateLimitExceeded(format!(
                "RPM limit exceeded for tenant {}",
                key.tenant_id
            )));
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
    /// 限流条目（包含请求计数、Token计数和共享窗口）
    entries: DashMap<RateLimitKey, RateLimitEntry>,
    window_size: Duration,
}

impl MemoryRateLimiter {
    /// 创建新的内存限流器
    pub fn new() -> Self {
        Self {
            entries: DashMap::new(),
            window_size: Duration::from_secs(WINDOW_SECS),
        }
    }

    /// 清理过期计数器
    pub fn cleanup(&self) {
        self.entries.retain(|_, entry| !entry.is_expired());
    }

    /// 获取或创建限流条目
    fn get_or_create_entry(
        &self,
        key: &RateLimitKey,
    ) -> dashmap::mapref::one::Ref<'_, RateLimitKey, RateLimitEntry> {
        self.entries
            .entry(key.clone())
            .or_insert_with(|| RateLimitEntry::new(self.window_size))
            .downgrade()
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
        let entry = self.get_or_create_entry(key);
        entry.reset_if_expired();
        Ok(entry.request_count() < config.rpm_limit as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        entry.increment_request();
        Ok(())
    }

    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        let entry = self.get_or_create_entry(key);
        entry.reset_if_expired();
        entry.add_tokens(tokens as u64);
        Ok(())
    }

    async fn get_count(&self, key: &RateLimitKey) -> Result<u64> {
        let entry = self.get_or_create_entry(key);
        entry.reset_if_expired();
        Ok(entry.request_count())
    }

    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64> {
        let entry = self.get_or_create_entry(key);
        entry.reset_if_expired();
        Ok(entry.token_count())
    }

    /// 原子地检查并记录请求（使用租户特定限制）
    ///
    /// 实现策略：先原子增加，再检查是否超限，超限则回滚。
    /// 使用单一 DashMap 条目保证窗口同步和操作原子性。
    async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &RateLimitConfig,
    ) -> Result<()> {
        let entry = self.get_or_create_entry(key);

        // 原子地检查并重置过期窗口
        entry.reset_if_expired();

        // 原子地增加计数
        let new_count = entry.increment_request();

        // 检查是否超限
        if new_count > config.rpm_limit as u64 {
            // 超限，回滚计数
            entry.decrement_request();
            return Err(KeyComputeError::RateLimitExceeded(format!(
                "RPM limit exceeded for tenant {} (limit: {}, current: {}) ",
                key.tenant_id, config.rpm_limit, new_count
            )));
        }

        Ok(())
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
            return Err(KeyComputeError::RateLimitExceeded(format!(
                "RPM limit exceeded for tenant {}",
                key.tenant_id
            )));
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
                if matches!(e, KeyComputeError::RateLimitExceeded(_)) {
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
    /// 通过 URL 创建 Redis 限流服务（内部创建连接池）
    pub fn new_redis(redis_url: &str) -> Result<Self> {
        let cfg = deadpool_redis::Config::from_url(redis_url);
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| {
                KeyComputeError::Internal(format!("Failed to create Redis pool: {}", e))
            })?;
        let limiter = RedisRateLimiter::new(pool);
        Ok(Self::new(
            std::sync::Arc::new(limiter),
            RateLimitBackend::Redis,
        ))
    }

    /// 通过 URL 创建带前缀的 Redis 限流服务（内部创建连接池）
    pub fn new_redis_with_prefix(redis_url: &str, prefix: impl Into<String>) -> Result<Self> {
        let cfg = deadpool_redis::Config::from_url(redis_url);
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| {
                KeyComputeError::Internal(format!("Failed to create Redis pool: {}", e))
            })?;
        let limiter = RedisRateLimiter::with_prefix(pool, prefix);
        Ok(Self::new(
            std::sync::Arc::new(limiter),
            RateLimitBackend::Redis,
        ))
    }

    /// 使用已有连接池创建 Redis 限流服务
    ///
    /// 与 `new_redis` 的区别在于接受外部创建的连接池，
    /// 用于与其他 Redis 消费者共享同一连接池。
    pub fn with_redis_pool(pool: deadpool_redis::Pool) -> Self {
        let limiter = RedisRateLimiter::new(pool);
        Self::new(std::sync::Arc::new(limiter), RateLimitBackend::Redis)
    }

    /// 使用已有连接池 + 前缀创建 Redis 限流服务
    pub fn with_redis_pool_and_prefix(
        pool: deadpool_redis::Pool,
        prefix: impl Into<String>,
    ) -> Self {
        let limiter = RedisRateLimiter::with_prefix(pool, prefix);
        Self::new(std::sync::Arc::new(limiter), RateLimitBackend::Redis)
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

    /// 测试并发场景下的原子限流
    /// 验证在高并发情况下，限流计数准确，不会超出限制
    #[tokio::test]
    async fn test_concurrent_atomic_rate_limit() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use tokio::task::JoinSet;

        let service = Arc::new(RateLimitService::default_memory());
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 设置限制为 5，并发 20 个请求
        let config = Arc::new(RateLimitConfig::new(5, 1000));
        let concurrent_requests = 20;

        let success_count = Arc::new(AtomicU32::new(0));
        let reject_count = Arc::new(AtomicU32::new(0));

        let mut tasks = JoinSet::new();

        for _ in 0..concurrent_requests {
            let service = Arc::clone(&service);
            let key = key.clone();
            let config = Arc::clone(&config);
            let success_count = Arc::clone(&success_count);
            let reject_count = Arc::clone(&reject_count);

            tasks.spawn(async move {
                let result = service.check_and_record_with_config(&key, &config).await;
                match result {
                    Ok(()) => success_count.fetch_add(1, Ordering::Relaxed),
                    Err(_) => reject_count.fetch_add(1, Ordering::Relaxed),
                };
            });
        }

        // 等待所有任务完成
        while tasks.join_next().await.is_some() {}

        let success = success_count.load(Ordering::Relaxed);
        let reject = reject_count.load(Ordering::Relaxed);

        // 验证：成功数应恰好等于限制数 5，拒绝数应为 15
        assert_eq!(
            success, 5,
            "Expected exactly 5 successful requests, got {}",
            success
        );
        assert_eq!(
            reject, 15,
            "Expected exactly 15 rejected requests, got {}",
            reject
        );

        // 验证最终计数准确
        let final_count = service.get_rpm_count(&key).await.unwrap();
        assert_eq!(
            final_count, 5,
            "Final count should be exactly 5, got {}",
            final_count
        );
    }
}
