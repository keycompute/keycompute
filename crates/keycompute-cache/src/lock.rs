//! # Distributed Locking
//!
//! Redis-backed distributed locking with atomic Lua-based acquire (SET NX EX),
//! safe release (Lua script with ownership verification), retry logic,
//! and automatic lock release on `Drop`.
//!
//! ## Strategy: fail-open vs fail-close
//!
//! - **fail-open** ([`LockGuard::acquire`]): Redis 不可用时返回 `Ok(None)`,
//!   调用方跳过关键区。适用于"有锁更好，没有也能凑合"的场景（如缓存防击穿）。
//! - **fail-close** ([`acquire_lock`]): Redis 不可用时返回 `Err`，
//!   调用方必须处理错误。适用于"没有锁就不能继续"的场景（如定时任务互斥）。
//!
//! ## Lua 安全释放
//!
//! 所有锁释放均通过 [`SAFE_RELEASE_SCRIPT`] Lua 脚本完成，原子地检查
//! 锁的值是否匹配，防止误释放其他持有者的锁。锁的默认 TTL 兜底机制
//! 确保即使进程崩溃，锁也不会永久占用。
//!
//! ## 适用场景判断（刚需场景）
//!
//! 分布式锁的适用场景比直觉中要窄得多。在选择分布式锁之前，优先考虑
//! 现有架构中更可靠的替代方案：
//!
//! | 场景 | 首选方案 | 说明 |
//! |---|---|---|
//! | DB 行级并发修改 | `SELECT ... FOR UPDATE` + 事务 | PostgreSQL MVCC 跨副本天然生效 |
//! | 唯一性约束 | DB `UNIQUE` 约束 | 数据库保证原子性，无需锁 |
//! | 原子计数/限流 | Redis `INCR` / ZSET | Redis 单线程模型保证原子性 |
//! | 条件更新防 TOCTOU | `UPDATE ... WHERE ... = ...` | 一条 SQL 完成读-改-写 |
//! | 刷新令牌轮转 | 事务内 DELETE + INSERT | 事务保证原子性 |
//! | 缓存击穿保护 | **分布式锁** | K8s 多副本 > 唯一可行方案 |
//! | 定时任务互斥 | **分布式锁** | 非幂等操作必须加锁 |

use std::time::Duration;
use uuid::Uuid;

/// Error type for lock operations.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Redis is not configured or unreachable.
    #[error("Redis is not available. Distributed locking is disabled.")]
    RedisNotAvailable,
    /// Redis command error.
    #[error("Redis error: {0}")]
    RedisError(#[from] deadpool_redis::redis::RedisError),
    /// Connection pool error.
    #[error("Connection pool error: {0}")]
    PoolError(#[from] deadpool_redis::PoolError),
    /// Lock operation failed after retries.
    #[error("Lock operation failed after retries: {0}")]
    LockFailed(String),
}

/// Result type alias for lock operations.
pub type LockResult<T> = std::result::Result<T, LockError>;

/// SAFE RELEASE Lua script: atomically checks if the lock value matches
/// before deleting. Prevents accidentally releasing another holder's lock.
///
/// KEYS[1] = lock key
/// ARGV[1] = expected lock value (UUID)
/// Returns: 1 if deleted, 0 if value mismatch (lock already expired or re-acquired)
const SAFE_RELEASE_SCRIPT: &str = r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("DEL", KEYS[1])
else
    return 0
end
"#;

// ── Low-level API (fail-close) ─────────────────────────────────────────

/// Acquire a distributed lock with retry mechanism.
///
/// # fail-close 语义
///
/// 当 Redis 不可用时返回 `Err(LockError::RedisNotAvailable)`，
/// 调用方必须处理错误。
///
/// # 参数
/// * `pool` - Redis 连接池引用
/// * `lock_key` - 锁的唯一 key
/// * `ttl_secs` - 锁的 TTL（秒）
/// * `max_retries` - 最大重试次数
/// * `retry_delay` - 重试间隔
///
/// # 返回
/// `Ok((true, lock_value))` 表示加锁成功，`Ok((false, _))` 表示未获取到锁
pub async fn acquire_lock(
    pool: &deadpool_redis::Pool,
    lock_key: &str,
    ttl_secs: u64,
    max_retries: u32,
    retry_delay: Duration,
) -> LockResult<(bool, String)> {
    let lock_value = Uuid::new_v4().to_string();
    let acquired = try_acquire_with_retry(
        pool,
        lock_key,
        &lock_value,
        ttl_secs,
        max_retries,
        retry_delay,
    )
    .await?;
    Ok((acquired, lock_value))
}

/// 实际执行 SET NX EX 加锁 + 重试逻辑
async fn try_acquire_with_retry(
    pool: &deadpool_redis::Pool,
    lock_key: &str,
    lock_value: &str,
    ttl_secs: u64,
    max_retries: u32,
    retry_delay: Duration,
) -> LockResult<bool> {
    for attempt in 0..max_retries {
        let mut conn = pool.get().await.map_err(LockError::PoolError)?;

        // SET key value NX EX seconds (原子加锁，非阻塞)
        let result: Option<String> = deadpool_redis::redis::cmd("SET")
            .arg(lock_key)
            .arg(lock_value)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await?;

        if result.is_some() {
            tracing::debug!(
                "Lock acquired for key: {} on attempt {}",
                lock_key,
                attempt + 1
            );
            return Ok(true);
        }

        // 非阻塞重试
        if attempt < max_retries - 1 {
            tokio::time::sleep(retry_delay).await;
        }
    }

    tracing::debug!(
        "Failed to acquire lock for key: {} after {} attempts",
        lock_key,
        max_retries
    );
    Ok(false)
}

/// Release a distributed lock with ownership verification.
///
/// 使用 Lua 脚本原子地检查锁值是否匹配，防止误释放其他持有者的锁。
///
/// # 参数
/// * `pool` - Redis 连接池引用
/// * `lock_key` - 锁的 key
/// * `lock_value` - 加锁时生成的 UUID 值
pub async fn release_lock(
    pool: &deadpool_redis::Pool,
    lock_key: &str,
    lock_value: &str,
) -> LockResult<()> {
    let mut conn = pool.get().await.map_err(LockError::PoolError)?;

    // Lua 脚本原子检查 + 删除
    let script = deadpool_redis::redis::Script::new(SAFE_RELEASE_SCRIPT);
    let deleted: i32 = script
        .key(lock_key)
        .arg(lock_value)
        .invoke_async(&mut conn)
        .await?;

    if deleted == 1 {
        tracing::debug!("Lock released for key: {}", lock_key);
    } else {
        tracing::warn!(
            "Lock release skipped for key: {} — value mismatch \
             (lock may have expired or been re-acquired)",
            lock_key
        );
    }

    Ok(())
}

// ── High-level API (fail-open) ─────────────────────────────────────────

/// Result of a lock acquisition attempt.
#[derive(Debug)]
pub enum AcquireResult {
    /// Lock was successfully acquired and guard is returned.
    Acquired(LockGuard),
    /// Lock was not acquired due to contention (another holder has it).
    Contended,
}

/// Lock guard that automatically releases the lock when dropped.
///
/// Stores a unique lock value (UUID) used for safe ownership-verified release.
pub struct LockGuard {
    pool: Option<deadpool_redis::Pool>,
    lock_key: String,
    lock_value: String,
}

impl std::fmt::Debug for LockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LockGuard")
            .field("lock_key", &self.lock_key)
            .field("has_pool", &self.pool.is_some())
            .finish()
    }
}

impl LockGuard {
    /// Create a new lock guard (internal use).
    fn new(pool: Option<deadpool_redis::Pool>, lock_key: String, lock_value: String) -> Self {
        Self {
            pool,
            lock_key,
            lock_value,
        }
    }

    /// Acquire a lock and return a guard that releases it on drop.
    ///
    /// # fail-open 语义
    ///
    /// - `Ok(Some(AcquireResult::Acquired(guard)))` — 加锁成功
    /// - `Ok(Some(AcquireResult::Contended))` — 锁被其他持有者占用
    /// - `Ok(None)` — Redis 不可用，跳过锁保护
    ///
    /// # 适用场景
    ///
    /// - **缓存击穿保护**：与 [`CacheService::get_or_insert_with_lock`] 配合
    /// - **定时任务互斥**：确保只有一个副本执行
    /// - **跨副本资源初始化**
    pub async fn acquire(
        pool: Option<&deadpool_redis::Pool>,
        lock_key: &str,
        ttl_secs: u64,
        max_retries: u32,
        retry_delay: Duration,
    ) -> LockResult<Option<AcquireResult>> {
        let pool = match pool {
            Some(p) => p,
            None => {
                tracing::warn!(
                    "Redis pool not available, skipping distributed lock for key: {}",
                    lock_key
                );
                return Ok(None);
            }
        };

        let (acquired, lock_value) =
            acquire_lock(pool, lock_key, ttl_secs, max_retries, retry_delay).await?;

        if acquired {
            Ok(Some(AcquireResult::Acquired(Self::new(
                Some(pool.clone()),
                lock_key.to_string(),
                lock_value,
            ))))
        } else {
            Ok(Some(AcquireResult::Contended))
        }
    }

    /// Release the lock explicitly (with ownership verification).
    ///
    /// Note: The lock is also released automatically when the guard is dropped.
    /// If this call fails, the fields are restored to `self` so that `Drop` can retry.
    pub async fn release(mut self) -> LockResult<()> {
        let pool = self.pool.take();
        let lock_key = std::mem::take(&mut self.lock_key);
        let lock_value = std::mem::take(&mut self.lock_value);

        match pool {
            Some(pool) => match release_lock(&pool, &lock_key, &lock_value).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    // Restore fields so Drop can attempt release again
                    self.pool = Some(pool);
                    self.lock_key = lock_key;
                    self.lock_value = lock_value;
                    Err(e)
                }
            },
            None => Ok(()),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            let lock_key = std::mem::take(&mut self.lock_key);
            let lock_value = std::mem::take(&mut self.lock_value);

            if !lock_key.is_empty() {
                // Use Handle::try_current() to avoid panicking when the tokio
                // runtime is unavailable (e.g. during shutdown or on non-tokio threads).
                // In that case the lock will expire naturally via its Redis TTL.
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        std::mem::drop(handle.spawn(async move {
                            if let Err(e) = release_lock(&pool, &lock_key, &lock_value).await {
                                tracing::warn!(
                                    "Failed to release lock on drop for key: {}: {}",
                                    lock_key,
                                    e
                                );
                            }
                        }));
                    }
                    Err(_) => {
                        tracing::warn!(
                            "No tokio runtime available, lock for key: {} will expire via TTL",
                            lock_key
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut conn = pool.get().await.ok()?;
        let pong: Result<String, _> = deadpool_redis::redis::cmd("PING")
            .query_async(&mut conn)
            .await;
        if pong.is_err() {
            return None;
        }
        Some(pool)
    }

    fn test_lock_key(name: &str) -> String {
        format!("keycompute:cache:test:lock:{}", name)
    }

    // ── no-op mode: LockGuard::acquire without Redis ───────────────────

    /// Without Redis pool, acquire returns Ok(None) (fail-open).
    #[tokio::test]
    async fn test_lock_acquire_no_pool() {
        let pool: Option<deadpool_redis::Pool> = None;
        let result = LockGuard::acquire(
            pool.as_ref(),
            "test:lock:noop",
            10,
            1,
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    /// LockGuard::release with no pool returns Ok.
    #[tokio::test]
    async fn test_lock_release_no_pool() {
        let guard = LockGuard::new(None, "test:key".to_string(), "test:val".to_string());
        let result = guard.release().await;
        assert!(result.is_ok());
    }

    // ── integration: LockGuard acquire + release ───────────────────────

    /// Acquire a lock, then explicitly release it.
    #[tokio::test]
    async fn test_lock_acquire_and_release() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };

        let key = test_lock_key("acquire_release");
        let _ = release_lock(&pool, &key, "cleanup").await; // clean from previous run

        // Acquire
        let result = LockGuard::acquire(Some(&pool), &key, 10, 1, Duration::ZERO)
            .await
            .unwrap();
        match result {
            Some(AcquireResult::Acquired(guard)) => {
                // Explicit release
                guard.release().await.unwrap();
            }
            Some(AcquireResult::Contended) => panic!("Should not be contended"),
            None => panic!("Should have pool"),
        }

        // Verify lock is released by re-acquiring successfully
        let result = LockGuard::acquire(Some(&pool), &key, 10, 1, Duration::ZERO)
            .await
            .unwrap();
        assert!(matches!(result, Some(AcquireResult::Acquired(_))));
    }

    /// Two concurrent acquires: one succeeds, the other gets Contended.
    #[tokio::test]
    async fn test_lock_contention() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };

        let key = test_lock_key("contention");
        let _ = release_lock(&pool, &key, "cleanup").await;

        // First acquire holds the lock
        let guard = match LockGuard::acquire(Some(&pool), &key, 5, 1, Duration::ZERO)
            .await
            .unwrap()
        {
            Some(AcquireResult::Acquired(g)) => g,
            _ => panic!("First acquire should succeed"),
        };

        // Second acquire should be Contended (lock still held)
        let result = LockGuard::acquire(Some(&pool), &key, 5, 1, Duration::ZERO)
            .await
            .unwrap();
        assert!(matches!(result, Some(AcquireResult::Contended)));

        // Drop first guard → lock released (async via fire-and-forget spawn)
        drop(guard);

        // Now third acquire should succeed.
        // Use multiple retries with a small delay to give the Drop handler
        // a chance to run and release the lock.
        let result = LockGuard::acquire(Some(&pool), &key, 5, 10, Duration::from_millis(50))
            .await
            .unwrap();
        assert!(matches!(result, Some(AcquireResult::Acquired(_))));
    }

    /// Lua script safety: releasing with wrong UUID does not delete.
    #[tokio::test]
    async fn test_lock_key_mismatch_safety() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };

        let key = test_lock_key("mismatch");
        let _ = release_lock(&pool, &key, "cleanup").await;

        // Acquire with one UUID
        let guard = match LockGuard::acquire(Some(&pool), &key, 10, 1, Duration::ZERO)
            .await
            .unwrap()
        {
            Some(AcquireResult::Acquired(g)) => g,
            _ => panic!("Acquire should succeed"),
        };

        // Try to release with different value (simulate wrong holder)
        let wrong_value = "00000000-0000-0000-0000-000000000000".to_string();
        let wrong_guard = LockGuard::new(Some(pool.clone()), key.clone(), wrong_value);
        wrong_guard.release().await.unwrap();

        // Original lock should still exist (release by wrong value did nothing)
        // So a new acquire should fail (Contended)
        let result = LockGuard::acquire(Some(&pool), &key, 5, 1, Duration::ZERO)
            .await
            .unwrap();
        assert!(
            matches!(result, Some(AcquireResult::Contended)),
            "Lock should still be held by original guard"
        );

        // Release properly
        drop(guard);
    }

    /// Lock auto-expires via TTL.
    #[tokio::test]
    async fn test_lock_ttl_expiration() {
        let Some(pool) = create_test_pool(15).await else {
            eprintln!("SKIP: Redis not available");
            return;
        };

        let key = test_lock_key("ttl");
        let _ = release_lock(&pool, &key, "cleanup").await;

        // Acquire with 1-second TTL — do NOT drop guard,
        // so the lock can only be released via TTL expiration
        let guard = match LockGuard::acquire(Some(&pool), &key, 1, 1, Duration::ZERO)
            .await
            .unwrap()
        {
            Some(AcquireResult::Acquired(g)) => g,
            _ => panic!("Acquire should succeed"),
        };

        // Use forget to skip Drop::drop's async lock release.
        // The lock must expire via Redis TTL, not via explicit release.
        std::mem::forget(guard);

        // Wait for TTL to expire
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Now should be able to acquire (lock expired via TTL)
        let guard2 = match LockGuard::acquire(Some(&pool), &key, 5, 1, Duration::ZERO)
            .await
            .unwrap()
        {
            Some(AcquireResult::Acquired(g)) => g,
            _ => panic!("Lock should have expired via TTL"),
        };

        // Clean up: explicitly release the lock we just acquired
        guard2.release().await.unwrap();
    }
}
