//! # Cache Service
//!
//! Unified Redis caching layer with `deadpool-redis` connection pooling.
//!
//! ## Design
//!
//! - Uses `deadpool-redis` connection pool (shared across the application).
//! - Graceful degradation: when Redis is unavailable, all operations silently
//!   no-op. The application never fails to start or crash due to cache errors.
//! - `get_or_insert` pattern: auto-populates cache on miss.
//! - `get_or_insert_with_lock`: cache stampede protection via distributed lock.
//! - Negative caching via `set_null` to prevent cache penetration.
//!
//! ## Graceful Degradation
//!
//! When Redis is unavailable (`pool` is `None`), all public methods immediately
//! return `Ok(None)` or `Ok(())`. The service never propagates Redis errors
//! to callers in normal operations.

pub mod lock;

use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;

/// Redis connection pool type alias.
type RedisPool = deadpool_redis::Pool;

/// Default key prefix for cache entries.
const DEFAULT_KEY_PREFIX: &str = "keycompute:cache:";

/// Negative-cache marker suffix.
///
/// Uses a double-colon + word separator that cannot appear in any
/// programmatically generated cache key (UUID:model:provider format).
/// This guarantees build_key(X) can never equal build_null_key(Y)
/// for any valid pair of keys in the actual usage.
const NULL_MARKER_SUFFIX: &str = "::null_marker";

// ── Error Types ─────────────────────────────────────────────────────────

/// Cache service error.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Redis connection pool error.
    #[error("Redis pool error: {0}")]
    Pool(#[from] deadpool_redis::PoolError),

    /// Redis command error.
    #[error("Redis command error: {0}")]
    Redis(#[from] deadpool_redis::redis::RedisError),

    /// JSON serialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// JSON deserialization error.
    #[error("Deserialization error: {0}")]
    Deserialization(String),

    /// Cache not available (Redis not configured or unreachable).
    #[error("Cache not available (Redis not configured)")]
    NotAvailable,

    /// Fallback computation failed.
    #[error("Fallback computation failed: {0}")]
    FallbackFailed(String),
}

/// Result type alias for cache operations.
pub type CacheResult<T> = std::result::Result<T, CacheError>;

// ── Cache Service ───────────────────────────────────────────────────────

/// Unified cache service, shared across the application.
///
/// When Redis is unavailable (`pool` is `None`), all operations silently
/// no-op — the service never returns errors from `get()`, `set()`, etc.
/// when the pool is absent.
#[derive(Clone)]
pub struct CacheService {
    pool: Option<RedisPool>,
    key_prefix: String,
}

impl std::fmt::Debug for CacheService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheService")
            .field("pool", &self.pool.as_ref().map(|_| "<deadpool pool>"))
            .field("key_prefix", &self.key_prefix)
            .finish()
    }
}

impl Default for CacheService {
    fn default() -> Self {
        Self::disabled()
    }
}

impl CacheService {
    /// Create a new cache service with a deadpool-redis connection pool.
    ///
    /// # Graceful degradation
    ///
    /// If `redis_url` is empty, the service logs a warning and runs in
    /// **no-op mode** — all operations silently succeed without doing
    /// any Redis work. The server never fails to start due to a Redis issue.
    ///
    /// # Panics
    ///
    /// Panics if `pool_size` is 0.
    pub async fn new(redis_url: &str, pool_size: usize) -> Self {
        if redis_url.is_empty() {
            tracing::info!(
                "CacheService: Redis URL is empty, running in no-op mode. \
                 Set KEYCOMPUTE_REDIS_URL to enable caching."
            );
            return Self::disabled();
        }

        let mut cfg = deadpool_redis::Config::from_url(redis_url);
        cfg.pool = Some(deadpool_redis::PoolConfig {
            max_size: pool_size,
            ..Default::default()
        });
        let pool = match cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1)) {
            Ok(p) => {
                tracing::info!(
                    "CacheService: deadpool established (pool_size={}).",
                    pool_size
                );
                p
            }
            Err(e) => {
                tracing::warn!(
                    "CacheService: Failed to create pool from '{}': {:?}. \
                     Running in no-op mode.",
                    redis_url,
                    e
                );
                return Self::disabled();
            }
        };

        Self {
            pool: Some(pool),
            key_prefix: DEFAULT_KEY_PREFIX.to_string(),
        }
    }

    /// Create a cache service that reuses an existing `deadpool_redis::Pool`.
    ///
    /// This is the preferred constructor when the application already has
    /// a shared Redis pool (e.g., created by `RedisRuntimeStore`).
    pub fn with_pool(pool: RedisPool) -> Self {
        Self {
            pool: Some(pool),
            key_prefix: DEFAULT_KEY_PREFIX.to_string(),
        }
    }

    /// Create a cache service with a custom key prefix.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Create a cache service in **no-op mode** — all operations silently no-op.
    ///
    /// Useful for testing or when Redis is intentionally disabled.
    pub fn disabled() -> Self {
        Self {
            pool: None,
            key_prefix: DEFAULT_KEY_PREFIX.to_string(),
        }
    }

    /// Returns `true` if a Redis pool is available.
    pub fn is_available(&self) -> bool {
        self.pool.is_some()
    }

    /// Return a reference to the inner `deadpool_redis::Pool` for sharing
    /// with the lock service or other components.
    pub fn pool(&self) -> Option<&RedisPool> {
        self.pool.as_ref()
    }

    // ── Key Management ──────────────────────────────────────────────────

    /// Build a full cache key with the configured prefix.
    fn build_key(&self, key: &str) -> String {
        format!("{}{}", self.key_prefix, key)
    }

    /// Build a negative-cache marker key.
    fn build_null_key(&self, key: &str) -> String {
        format!("{}{}{}", self.key_prefix, key, NULL_MARKER_SUFFIX)
    }

    /// Check directly in Redis whether a negative-cache marker exists.
    ///
    /// This uses the raw null key (not `exists()`) to avoid double-prefixing.
    async fn has_null_marker(&self, key: &str) -> CacheResult<bool> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(false),
        };
        let null_key = self.build_null_key(key);
        let count: i32 = deadpool_redis::redis::cmd("EXISTS")
            .arg(&null_key)
            .query_async(&mut conn)
            .await?;
        Ok(count > 0)
    }

    // ── Core Operations ─────────────────────────────────────────────────

    /// Retrieve and deserialize a cached value.
    ///
    /// Returns `Ok(None)` when:
    /// - The key does not exist in Redis.
    /// - Redis is not available (no-op mode).
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> CacheResult<Option<T>> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(None),
        };

        let full_key = self.build_key(key);
        let raw: Option<String> = deadpool_redis::redis::cmd("GET")
            .arg(&full_key)
            .query_async(&mut conn)
            .await?;

        match raw {
            Some(s) => {
                let val: T = serde_json::from_str(&s)
                    .map_err(|e| CacheError::Deserialization(e.to_string()))?;
                Ok(Some(val))
            }
            None => Ok(None),
        }
    }

    /// Store a value with TTL.
    ///
    /// Returns `Ok(())` in no-op mode.
    pub async fn set<T: Serialize>(&self, key: &str, val: &T, ttl: Duration) -> CacheResult<()> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(()),
        };

        let json =
            serde_json::to_string(val).map_err(|e| CacheError::Serialization(e.to_string()))?;
        let full_key = self.build_key(key);

        deadpool_redis::redis::cmd("SETEX")
            .arg(&full_key)
            .arg(ttl.as_secs())
            .arg(&json)
            .query_async::<()>(&mut conn)
            .await?;

        Ok(())
    }

    /// Delete a cache key.
    ///
    /// Returns `Ok(())` in no-op mode.
    pub async fn delete(&self, key: &str) -> CacheResult<()> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(()),
        };

        let full_key = self.build_key(key);
        deadpool_redis::redis::cmd("DEL")
            .arg(&full_key)
            .query_async::<()>(&mut conn)
            .await?;

        Ok(())
    }

    /// Check if a key exists in cache.
    ///
    /// Returns `Ok(false)` in no-op mode.
    pub async fn exists(&self, key: &str) -> CacheResult<bool> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(false),
        };

        let full_key = self.build_key(key);
        let count: i32 = deadpool_redis::redis::cmd("EXISTS")
            .arg(&full_key)
            .query_async(&mut conn)
            .await?;

        Ok(count > 0)
    }

    // ── High-level Patterns ─────────────────────────────────────────────

    /// Retrieve from cache, or compute-and-store on miss.
    ///
    /// On cache miss, runs `f` to compute the value, stores it with `ttl`,
    /// and returns it.
    ///
    /// # Negative cache
    ///
    /// This method does **not** automatically store a negative-cache marker
    /// when `f` fails. Call [`Self::set_null`] explicitly in your fallback
    /// path if you need cache-penetration protection.
    ///
    /// # Errors
    ///
    /// Returns `CacheError::FallbackFailed` when `f` returns an error.
    pub async fn get_or_insert<T, F, E>(
        &self,
        key: &str,
        ttl: Duration,
        f: F,
    ) -> std::result::Result<T, CacheError>
    where
        T: Serialize + DeserializeOwned + Send,
        F: std::future::Future<Output = std::result::Result<T, E>> + Send,
        E: std::fmt::Display,
    {
        // 1. Try cache hit
        if self.is_available() {
            if let Some(val) = self.get::<T>(key).await? {
                return Ok(val);
            }
            // 2. Check negative-cache marker (entity does not exist)
            if self.has_null_marker(key).await? {
                return Err(CacheError::FallbackFailed(
                    "Entity does not exist (negative cache)".to_string(),
                ));
            }
        }

        // 3. Fallback computation
        let val = f
            .await
            .map_err(|e| CacheError::FallbackFailed(format!("Fallback failed: {}", e)))?;

        // 4. Populate cache (best-effort — failures are logged, not propagated)
        if let Err(e) = self.set(key, &val, ttl).await {
            tracing::warn!("Cache set failed for key '{}': {:?}", key, e);
        }

        Ok(val)
    }

    /// Cache-stampede-protected variant of [`get_or_insert`](Self::get_or_insert).
    ///
    /// # 适用场景：热点缓存 key 击穿保护
    ///
    /// 在 K8s 多副本部署下，当高频访问的缓存 key 过期时，所有 pod 可能
    /// 同时回源查询 DB，导致负载瞬增。本方法使用分布式锁确保仅一个 pod
    /// 回源计算，其他 pod 等待后从缓存读取。
    ///
    /// 典型的适用场景：
    /// - 全局共享配置（功能开关、定价表）
    /// - 高并发公共统计数据
    ///
    /// # 算法
    ///
    /// 1. 快速路径：尝试缓存命中
    /// 2. 检查负缓存标记
    /// 3. 尝试获取分布式锁
    ///    - 获取成功 → 双检缓存 → 仍 Miss → 回源计算 → 写入缓存
    ///    - 获取失败 → Spin-wait 轮询缓存 → 最终仍 Miss → 回源计算（无锁保护）
    ///    - Redis 不可用 → 回源计算（fail-open）
    /// 4. LockGuard Drop 自动释放锁
    ///
    /// # 何时使用
    ///
    /// 仅对**极热点的全局 key**使用。如果回源计算极快（<1ms DB 查询），
    /// 加锁的额外开销可能超过收益。
    pub async fn get_or_insert_with_lock<T, F, E>(
        &self,
        key: &str,
        ttl: Duration,
        lock_ttl_secs: u64,
        retry_delay: Duration,
        max_retries: u32,
        f: F,
    ) -> std::result::Result<T, CacheError>
    where
        T: Serialize + DeserializeOwned + Send,
        F: std::future::Future<Output = std::result::Result<T, E>> + Send,
        E: std::fmt::Display,
    {
        // 1. Fast path: try cache first
        if self.is_available()
            && let Some(val) = self.get::<T>(key).await?
        {
            return Ok(val);
        }

        // 1b. Check negative-cache marker (entity does not exist)
        //     Prevents repeated lock acquisition for non-existent keys.
        if self.is_available() && self.has_null_marker(key).await? {
            return Err(CacheError::FallbackFailed(
                "Entity does not exist (negative cache)".to_string(),
            ));
        }

        // 1c. If cache is not available, skip lock entirely and recompute directly.
        //     This avoids the costly spin-wait and unnecessary lock_key computation
        //     when Redis is intentionally disabled (no-op mode).
        if !self.is_available() {
            let val = f
                .await
                .map_err(|e| CacheError::FallbackFailed(format!("Fallback failed: {}", e)))?;
            return Ok(val);
        }

        // 2. Try to acquire distributed lock
        let lock_key = format!("{}lock:{}", self.key_prefix, key);
        let _guard: Option<lock::LockGuard> = match lock::LockGuard::acquire(
            self.pool.as_ref(),
            &lock_key,
            lock_ttl_secs,
            max_retries,
            retry_delay,
        )
        .await
        {
            Ok(Some(lock::AcquireResult::Acquired(guard))) => {
                // Lock acquired — only this request will recompute.

                // 3a. Double-check: the winning request might have populated the cache
                //     between our step 1 and acquiring the lock.
                match self.get::<T>(key).await {
                    Ok(Some(val)) => {
                        // guard dropped here via `return`, releasing lock
                        return Ok(val);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!(
                            "get_or_insert_with_lock: double-check error for key '{}': {}, proceeding to recompute",
                            key,
                            e
                        );
                    }
                }
                // Keep guard alive: the lock stays held through recompute+populate
                // (steps 3b-3c), preventing other contending requests from also
                // recomputing while the cache is still stale.
                Some(guard)
            }
            Ok(Some(lock::AcquireResult::Contended)) => {
                // 3b. Lock not acquired — poll cache with exponential backoff before
                //     giving up.  The winning request should populate the cache soon, so
                //     waiting here is better than immediately recomputing.
                //     Note: self.is_available() is guaranteed true here (step 1c).
                //     Backoff: retry_delay * 2^attempt, capped at 2x to avoid excessive
                //     total wait time across all retries.
                for attempt in 0..max_retries {
                    let backoff = retry_delay.saturating_mul(1u32 << attempt.min(1));
                    tokio::time::sleep(backoff).await;
                    match self.get::<T>(key).await {
                        Ok(Some(val)) => return Ok(val),
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(
                                "get_or_insert_with_lock: poll error for key '{}': {}, continuing spin-wait",
                                key,
                                e
                            );
                        }
                    }
                }
                None
            }
            Err(e) => {
                tracing::warn!(
                    "get_or_insert_with_lock: lock acquire error for key '{}': {}",
                    key,
                    e
                );
                None
            }
            // After step 1c, self.is_available() is guaranteed true, so
            // LockGuard::acquire receives Some(pool) and never returns Ok(None).
            Ok(None) => unreachable!(
                "get_or_insert_with_lock: Ok(None) is unreachable after step 1c is_available() guard"
            ),
        };

        // 4. Recompute (lock held if _guard is Some)
        let val = f
            .await
            .map_err(|e| CacheError::FallbackFailed(format!("Fallback failed: {}", e)))?;

        // 5. Populate cache (best-effort — failures are logged, not propagated)
        if let Err(e) = self.set(key, &val, ttl).await {
            tracing::warn!("Cache set failed for key '{}': {:?}", key, e);
        }

        Ok(val)
        // Lock released on _guard Drop (when Some)
    }

    // ── Negative Cache ──────────────────────────────────────────────────

    /// Store a short-lived negative-cache marker.
    ///
    /// Used when a database query confirms an entity does not exist. Prevents
    /// repeated cache-penetration queries for non-existent keys.
    pub async fn set_null(&self, key: &str, ttl: Duration) -> CacheResult<()> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(()),
        };

        let null_key = self.build_null_key(key);
        deadpool_redis::redis::cmd("SETEX")
            .arg(&null_key)
            .arg(ttl.as_secs())
            .arg("1")
            .query_async::<()>(&mut conn)
            .await?;

        Ok(())
    }

    /// Delete a key and its negative-cache marker.
    pub async fn invalidate(&self, key: &str) -> CacheResult<()> {
        let mut conn = match self.get_conn().await {
            Some(c) => c,
            None => return Ok(()),
        };

        // Delete both the cache key and the null marker
        let full_key = self.build_key(key);
        let null_key = self.build_null_key(key);

        deadpool_redis::redis::cmd("DEL")
            .arg(&full_key)
            .arg(&null_key)
            .query_async::<()>(&mut conn)
            .await?;

        Ok(())
    }

    // ── Internal Helpers ────────────────────────────────────────────────

    /// Get a Redis connection from the pool, or `None` in no-op mode.
    async fn get_conn(&self) -> Option<deadpool_redis::Connection> {
        match self.pool.as_ref() {
            Some(pool) => match pool.get().await {
                Ok(conn) => Some(conn),
                Err(e) => {
                    tracing::warn!("CacheService: failed to get connection from pool: {:?}", e);
                    None
                }
            },
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestValue {
        id: u32,
        name: String,
    }

    // ── helpers ────────────────────────────────────────────────────────

    async fn create_test_pool(db: u8) -> Option<deadpool_redis::Pool> {
        let url = format!("redis://127.0.0.1:6379/{}", db);
        let mut cfg = deadpool_redis::Config::from_url(&url);
        cfg.pool = Some(deadpool_redis::PoolConfig {
            max_size: 2,
            ..Default::default()
        });
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .ok()?;
        // Validate connection by sending PING
        let mut conn = pool.get().await.ok()?;
        let pong: Result<String, _> = deadpool_redis::redis::cmd("PING")
            .query_async(&mut conn)
            .await;
        if pong.is_err() {
            return None;
        }
        Some(pool)
    }

    // ── no-op mode: CacheService::disabled ─────────────────────────────

    #[test]
    fn test_cache_service_disabled() {
        let service = CacheService::disabled();
        assert!(!service.is_available());
    }

    #[test]
    fn test_cache_service_with_prefix() {
        let service = CacheService::disabled().with_prefix("custom:prefix:");
        assert!(!service.is_available());
    }

    #[tokio::test]
    async fn test_disabled_get_returns_none() {
        let service = CacheService::disabled();
        let result: CacheResult<Option<TestValue>> = service.get("test").await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_disabled_set_returns_ok() {
        let service = CacheService::disabled();
        let val = TestValue {
            id: 1,
            name: "test".to_string(),
        };
        let result = service.set("test", &val, Duration::from_secs(60)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disabled_delete_returns_ok() {
        let service = CacheService::disabled();
        let result = service.delete("test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disabled_exists_returns_false() {
        let service = CacheService::disabled();
        let result = service.exists("test").await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_disabled_set_null_returns_ok() {
        let service = CacheService::disabled();
        let result = service.set_null("test", Duration::from_secs(60)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disabled_invalidate_returns_ok() {
        let service = CacheService::disabled();
        let result = service.invalidate("test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disabled_get_or_insert_returns_fallback() {
        let service = CacheService::disabled();
        let result = service
            .get_or_insert("test", Duration::from_secs(60), async {
                Ok::<_, String>(42i32)
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_disabled_get_or_insert_fallback_failure() {
        let service = CacheService::disabled();
        let result: Result<i32, CacheError> = service
            .get_or_insert("test", Duration::from_secs(60), async {
                Err::<i32, _>("oops")
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CacheError::FallbackFailed(_)));
    }

    #[tokio::test]
    async fn test_disabled_get_or_insert_with_lock_returns_fallback() {
        let service = CacheService::disabled();
        let result = service
            .get_or_insert_with_lock(
                "test",
                Duration::from_secs(60),
                5,
                Duration::from_millis(10),
                3,
                async { Ok::<_, String>(99i32) },
            )
            .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 99);
    }

    #[tokio::test]
    async fn test_disabled_get_or_insert_with_lock_fallback_failure() {
        let service = CacheService::disabled();
        let result: Result<i32, CacheError> = service
            .get_or_insert_with_lock(
                "test",
                Duration::from_secs(60),
                5,
                Duration::from_millis(10),
                3,
                async { Err::<i32, _>("oops") },
            )
            .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CacheError::FallbackFailed(_)));
    }

    #[test]
    fn test_key_prefix_building() {
        let service = CacheService::disabled();
        assert_eq!(service.key_prefix, "keycompute:cache:");
    }

    #[test]
    fn test_debug_format() {
        let service = CacheService::disabled();
        let debug = format!("{:?}", service);
        assert!(debug.contains("CacheService"));
        assert!(debug.contains("key_prefix"));
    }

    #[test]
    fn test_custom_prefix_build_keys() {
        let service = CacheService::disabled().with_prefix("custom:");
        assert_eq!(service.key_prefix, "custom:");
    }

    // ── integration: CacheService with real Redis ───────────────────────

    /// Set a value, get it back, verify roundtrip.
    #[tokio::test]
    async fn test_cache_set_get_roundtrip() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };
        let cache = CacheService::with_pool(pool).with_prefix("test:cache:");

        let val = TestValue {
            id: 42,
            name: "hello".to_string(),
        };
        cache
            .set("roundtrip", &val, Duration::from_secs(60))
            .await
            .unwrap();

        let got: TestValue = cache.get("roundtrip").await.unwrap().unwrap();
        assert_eq!(got, val);

        cache.delete("roundtrip").await.unwrap();
        let missing: Option<TestValue> = cache.get("roundtrip").await.unwrap();
        assert!(missing.is_none());
    }

    /// Value expires after TTL.
    #[tokio::test]
    async fn test_cache_ttl_expiration() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };
        let cache = CacheService::with_pool(pool).with_prefix("test:cache:");

        cache
            .set("ttl_test", &99i32, Duration::from_secs(1))
            .await
            .unwrap();

        // Immediately readable
        let got: i32 = cache.get("ttl_test").await.unwrap().unwrap();
        assert_eq!(got, 99);

        // Wait for TTL to expire
        tokio::time::sleep(Duration::from_secs(2)).await;

        let missing: Option<i32> = cache.get("ttl_test").await.unwrap();
        assert!(missing.is_none());
    }

    /// Negative cache: set_null + get_or_insert returns FallbackFailed.
    #[tokio::test]
    async fn test_negative_cache_blocks_fallback() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };
        let cache = CacheService::with_pool(pool).with_prefix("test:cache:");

        cache
            .set_null("nonexistent", Duration::from_secs(60))
            .await
            .unwrap();

        let result: Result<i32, CacheError> = cache
            .get_or_insert("nonexistent", Duration::from_secs(60), async {
                Ok::<_, String>(42i32)
            })
            .await;
        assert!(matches!(result, Err(CacheError::FallbackFailed(_))));

        cache.invalidate("nonexistent").await.unwrap();
    }

    /// get_or_insert_with_lock returns cached value when key exists.
    #[tokio::test]
    async fn test_get_or_insert_with_lock_cache_hit() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };
        let cache = CacheService::with_pool(pool).with_prefix("test:cache:");

        // Pre-populate cache
        cache
            .set(
                "prepop",
                &"cached_value".to_string(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();

        // get_or_insert_with_lock should return cached value without running fallback
        let result: String = cache
            .get_or_insert_with_lock(
                "prepop",
                Duration::from_secs(60),
                5,
                Duration::from_millis(10),
                3,
                async { Ok::<_, String>("should_not_run".to_string()) },
            )
            .await
            .unwrap();
        assert_eq!(result, "cached_value");

        cache.delete("prepop").await.unwrap();
    }

    /// get_or_insert_with_lock: concurrent requests with same key, one acquires lock.
    #[tokio::test]
    async fn test_get_or_insert_with_lock_concurrent() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };
        let cache = Arc::new(CacheService::with_pool(pool).with_prefix("test:cache:"));
        let key = format!("concurrent_{}", uuid::Uuid::new_v4().simple());

        // Two concurrent requests for the same key
        let c1 = Arc::clone(&cache);
        let k1 = key.clone();
        let h1 = tokio::spawn(async move {
            c1.get_or_insert_with_lock(
                &k1,
                Duration::from_secs(30),
                10,
                Duration::from_millis(50),
                10,
                async { Ok::<_, String>("from_task_1".to_string()) },
            )
            .await
        });

        let c2 = Arc::clone(&cache);
        let k2 = key.clone();
        let h2 = tokio::spawn(async move {
            c2.get_or_insert_with_lock(
                &k2,
                Duration::from_secs(30),
                10,
                Duration::from_millis(50),
                10,
                async { Ok::<_, String>("from_task_2".to_string()) },
            )
            .await
        });

        let r1 = h1.await.unwrap().unwrap();
        let r2 = h2.await.unwrap().unwrap();

        // Both should succeed, values should match (one cached the other's result)
        assert_eq!(r1, r2, "Both requests should get the same cached value");

        cache.delete(&key).await.unwrap();
    }

    /// Key prefix isolation: two services with different prefixes don't interfere.
    #[tokio::test]
    async fn test_key_prefix_isolation() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };

        let svc_a = CacheService::with_pool(pool.clone()).with_prefix("tenant_a:");
        let svc_b = CacheService::with_pool(pool).with_prefix("tenant_b:");

        svc_a
            .set(
                "shared_key",
                &"value_a".to_string(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        svc_b
            .set(
                "shared_key",
                &"value_b".to_string(),
                Duration::from_secs(60),
            )
            .await
            .unwrap();

        let got_a: String = svc_a.get("shared_key").await.unwrap().unwrap();
        let got_b: String = svc_b.get("shared_key").await.unwrap().unwrap();

        assert_eq!(got_a, "value_a");
        assert_eq!(got_b, "value_b");
        assert_ne!(got_a, got_b, "Different prefixes must isolate keys");

        svc_a.delete("shared_key").await.unwrap();
        svc_b.delete("shared_key").await.unwrap();
    }
}
