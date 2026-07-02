//! Redis 限流器实现
//!
//! 基于 Redis 的分布式限流后端，支持多实例共享限流状态。
//! 使用 `deadpool-redis` 连接池管理 Redis 连接。
//!
//! # 连接池管理
//!
//! 通过 `RedisRateLimiter::new(pool)` 接受外部 `deadpool_redis::Pool`，
//! 可通过 `RateLimitService::with_redis_pool()` 在 `state.rs` 中与其他 Redis
//! 消费者（如 `RedisRuntimeStore`）共享同一连接池。
//!
//! # TPM 支持
//!
//! 通过 Lua 脚本（`RECORD_TOKENS_SCRIPT` 和 `GET_TOKEN_COUNT_SCRIPT`）实现
//! 基于 ZSET 的滑动窗口 Token 计数。
//! - ZSET member = `timestamp:uuid:tokens`（token 值编码在 member 中，无需辅助 key）
//! - Lua 脚本内原子执行 ZADD + EXPIRE

use crate::{DEFAULT_RPM_LIMIT, RateLimitKey, RateLimiter, WINDOW_SECS};
use async_trait::async_trait;
use deadpool_redis::redis::AsyncCommands;
use keycompute_types::{KeyComputeError, Result};
use std::time::Duration;
use uuid::Uuid;

/// Redis 限流器
///
/// 使用 Redis 实现分布式限流，支持：
/// - 滑动窗口限流
/// - 多实例共享限流状态
/// - 自动过期清理
/// - Token 计数（TPM）
#[derive(Debug, Clone)]
pub struct RedisRateLimiter {
    pool: deadpool_redis::Pool,
    window_size: Duration,
    key_prefix: String,
}

impl RedisRateLimiter {
    /// 使用已有连接池创建 Redis 限流器
    pub fn new(pool: deadpool_redis::Pool) -> Self {
        Self {
            pool,
            window_size: Duration::from_secs(WINDOW_SECS),
            key_prefix: "ratelimit".to_string(),
        }
    }

    /// 使用已有连接池 + 自定义前缀创建 Redis 限流器
    pub fn with_prefix(pool: deadpool_redis::Pool, prefix: impl Into<String>) -> Self {
        Self {
            pool,
            window_size: Duration::from_secs(WINDOW_SECS),
            key_prefix: prefix.into(),
        }
    }

    /// 构建限流 Redis Key
    fn build_key(&self, key: &RateLimitKey, suffix: &str) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            self.key_prefix, key.tenant_id, key.user_id, key.api_key_id, suffix
        )
    }

    /// 构建 RPM 的 Redis Key
    fn build_rpm_key(&self, key: &RateLimitKey) -> String {
        self.build_key(key, "rpm")
    }

    /// 构建 TPM 的 Redis Key
    fn build_tpm_key(&self, key: &RateLimitKey) -> String {
        self.build_key(key, "tpm")
    }

    /// 获取 Redis 连接
    async fn get_conn(&self) -> Result<deadpool_redis::Connection> {
        self.pool
            .get()
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis connection error: {}", e)))
    }

    /// 获取当前 Unix 时间戳（秒）
    fn now_timestamp() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before epoch")
            .as_secs() as i64
    }

    /// 清理过期条目并返回当前窗口计数（RPM）
    async fn window_count(
        conn: &mut deadpool_redis::Connection,
        redis_key: &str,
        window_size: Duration,
    ) -> Result<u64> {
        let now = Self::now_timestamp();
        let window_start = now - window_size.as_secs() as i64;

        let _: () = conn
            .zrembyscore(redis_key, 0, window_start)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let count: u64 = conn
            .zcard(redis_key)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(count)
    }

    /// 获取过期时间（窗口大小的 2 倍，确保滑动窗口安全）
    fn expire_secs(&self) -> i64 {
        (self.window_size.as_secs() * 2) as i64
    }

    /// Lua 脚本：原子地检查并记录请求
    /// 返回 1 表示成功，0 表示限流
    const CHECK_AND_RECORD_SCRIPT: &str = r#"
        local key = KEYS[1]
        local now = tonumber(ARGV[1])
        local window_start = tonumber(ARGV[2])
        local limit = tonumber(ARGV[3])
        local member = ARGV[4]
        local expire_secs = tonumber(ARGV[5])

        -- 清理过期条目
        redis.call('ZREMRANGEBYSCORE', key, 0, window_start)

        -- 获取当前计数
        local count = redis.call('ZCARD', key)

        -- 检查是否超限
        if count >= limit then
            return 0
        end

        -- 添加新条目
        redis.call('ZADD', key, now, member)
        redis.call('EXPIRE', key, expire_secs)

        return 1
    "#;

    /// Lua 脚本：原子地记录 Token 使用量
    /// member 格式: "timestamp:uuid:tokens"（token 值编码在 member 中，消除辅助 key）
    const RECORD_TOKENS_SCRIPT: &str = r#"
        local key = KEYS[1]
        local now = tonumber(ARGV[1])
        local window_start = tonumber(ARGV[2])
        local expire_secs = tonumber(ARGV[3])
        local member = ARGV[4]

        -- 清理过期窗口
        redis.call('ZREMRANGEBYSCORE', key, 0, window_start)

        -- 添加 Token 记录（member 编码了 token 值，无需辅助 key）
        redis.call('ZADD', key, now, member)
        redis.call('EXPIRE', key, expire_secs)
    "#;

    /// Lua 脚本：获取当前窗口 Token 总和
    /// 从 member 中解析 token 计数值（格式: "timestamp:uuid:tokens"）
    const GET_TOKEN_COUNT_SCRIPT: &str = r#"
        local key = KEYS[1]
        local now = tonumber(ARGV[1])
        local window_start = tonumber(ARGV[2])
        local expire_secs = tonumber(ARGV[3])

        -- 清理过期条目
        redis.call('ZREMRANGEBYSCORE', key, 0, window_start)

        -- 计算当前窗口 Token 总和（从 member 中解析 token 值）
        local sum = 0
        local entries = redis.call('ZRANGEBYSCORE', key, window_start, '+inf')
        for i, entry in ipairs(entries) do
            -- member 格式: "timestamp:uuid:tokens"，取最后一个 : 之后的部分
            local colon_pos = string.find(entry, ':[^:]+$')
            if colon_pos then
                local token_str = string.sub(entry, colon_pos + 1)
                local token_count = tonumber(token_str)
                if token_count then
                    sum = sum + token_count
                end
            end
        end

        -- 更新过期时间
        redis.call('EXPIRE', key, expire_secs)

        return sum
    "#;
}

#[async_trait]
impl RateLimiter for RedisRateLimiter {
    async fn check(&self, key: &RateLimitKey) -> Result<bool> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);
        let count = Self::window_count(&mut conn, &redis_key, self.window_size).await?;
        Ok(count < DEFAULT_RPM_LIMIT as u64)
    }

    async fn check_with_config(
        &self,
        key: &RateLimitKey,
        config: &crate::RateLimitConfig,
    ) -> Result<bool> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);
        let count = Self::window_count(&mut conn, &redis_key, self.window_size).await?;
        Ok(count < config.rpm_limit as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);

        let now = Self::now_timestamp();

        // 使用 UUID 作为唯一成员，避免同一秒内的请求被去重
        let unique_member = format!("{}:{}", now, Uuid::new_v4().simple());

        let _: () = conn
            .zadd(&redis_key, &unique_member, now)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let _: () = conn
            .expire(&redis_key, self.expire_secs())
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(())
    }

    async fn check_and_record_with_config(
        &self,
        key: &RateLimitKey,
        config: &crate::RateLimitConfig,
    ) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);

        let now = Self::now_timestamp();

        let window_start = now - self.window_size.as_secs() as i64;
        let unique_member = format!("{}:{}", now, Uuid::new_v4().simple());

        // 使用 Lua 脚本原子执行检查和记录
        let result: i64 = deadpool_redis::redis::cmd("EVAL")
            .arg(Self::CHECK_AND_RECORD_SCRIPT)
            .arg(1)
            .arg(&redis_key)
            .arg(now)
            .arg(window_start)
            .arg(config.rpm_limit as i64)
            .arg(&unique_member)
            .arg(self.expire_secs())
            .query_async(&mut conn)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        if result == 1 {
            Ok(())
        } else {
            Err(KeyComputeError::RateLimitExceeded(format!(
                "Redis rate limit exceeded for tenant {}",
                key.tenant_id
            )))
        }
    }

    async fn record_tokens(&self, key: &RateLimitKey, tokens: u32) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_tpm_key(key);

        let now = Self::now_timestamp();

        let window_start = now - self.window_size.as_secs() as i64;
        // member 格式: "timestamp:uuid:tokens"
        // token 值编码在 member 中，消除辅助 key，避免 SETEX 与 ZSET 不一致
        let unique_member = format!("{}:{}:{}", now, Uuid::new_v4().simple(), tokens);

        deadpool_redis::redis::cmd("EVAL")
            .arg(Self::RECORD_TOKENS_SCRIPT)
            .arg(1)
            .arg(&redis_key)
            .arg(now)
            .arg(window_start)
            .arg(self.expire_secs())
            .arg(&unique_member)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(())
    }

    async fn get_count(&self, key: &RateLimitKey) -> Result<u64> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_rpm_key(key);
        Self::window_count(&mut conn, &redis_key, self.window_size).await
    }

    /// 获取当前窗口 Token 总和
    ///
    /// NOTE: 作为副作用，此方法会清理过期条目并刷新 key 的 TTL，
    /// 防止活跃 key 被提前驱逐。
    async fn get_token_count(&self, key: &RateLimitKey) -> Result<u64> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_tpm_key(key);

        let now = Self::now_timestamp();

        let window_start = now - self.window_size.as_secs() as i64;

        let count: i64 = deadpool_redis::redis::cmd("EVAL")
            .arg(Self::GET_TOKEN_COUNT_SCRIPT)
            .arg(1)
            .arg(&redis_key)
            .arg(now)
            .arg(window_start)
            .arg(self.expire_secs())
            .query_async(&mut conn)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(count as u64)
    }
}

impl RedisRateLimiter {
    /// 清理所有限流数据（用于测试或重置）
    pub async fn flush_all(&self) -> Result<()> {
        let pattern = format!("{}:*", self.key_prefix);

        let mut keys = Vec::new();
        {
            let mut conn = self.get_conn().await?;
            let mut iter: deadpool_redis::redis::AsyncIter<String> = conn
                .scan_match(&pattern)
                .await
                .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

            while let Some(key) = iter.next_item().await {
                keys.push(key);
            }
        }

        if !keys.is_empty() {
            let mut conn = self.get_conn().await?;
            let _: () = conn
                .del(&keys)
                .await
                .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    async fn create_test_pool() -> Option<deadpool_redis::Pool> {
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:6379");
        let pool = cfg
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|e| {
                eprintln!("Warning: Redis not available: {e}, skipping Redis tests");
            })
            .ok()?;
        // 验证实际连接可用
        let mut conn = pool.get().await.ok()?;
        let _: () = deadpool_redis::redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .ok()?;
        Some(pool)
    }

    async fn create_test_limiter() -> Option<RedisRateLimiter> {
        let pool = create_test_pool().await?;
        let limiter = RedisRateLimiter::new(pool);
        if limiter.flush_all().await.is_ok() {
            Some(limiter)
        } else {
            eprintln!("Warning: Redis not available (flush failed), skipping Redis tests");
            None
        }
    }

    #[tokio::test]
    async fn test_redis_rate_limiter_check_and_record() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        assert!(limiter.check(&key).await.unwrap());

        limiter.record(&key).await.unwrap();

        assert!(limiter.check(&key).await.unwrap());
    }

    #[tokio::test]
    async fn test_redis_tpm_token_recording() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 初始 token 计数应为 0
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(count, 0, "Initial token count should be 0");

        // 记录 100 tokens
        limiter.record_tokens(&key, 100).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 100,
            "After recording 100 tokens, count should be 100"
        );

        // 再记录 50 tokens
        limiter.record_tokens(&key, 50).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 150,
            "After recording 50 more tokens, count should be 150"
        );
    }

    #[tokio::test]
    async fn test_redis_tpm_edge_cases() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // 零 token 记录
        limiter.record_tokens(&key, 0).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 0,
            "After recording 0 tokens, count should still be 0"
        );

        // 单 token 记录
        limiter.record_tokens(&key, 1).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(count, 1, "After recording 1 token, count should be 1");

        // 大 token 值（接近 u32::MAX）
        limiter.record_tokens(&key, 999_999_999).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 1_000_000_000,
            "After recording 999999999 tokens, count should be 1000000000"
        );
    }

    #[tokio::test]
    async fn test_redis_tpm_boundary() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let limit: u64 = 100;

        // 记录恰好 limit-1 tokens
        limiter.record_tokens(&key, 99).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert!(
            count < limit,
            "99 tokens < 100 limit, should be below limit"
        );

        // 再记录 1 token → 达到 limit
        limiter.record_tokens(&key, 1).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(count, limit, "100 tokens = 100 limit, should exactly match");

        // 再记录 1 token → 超出 limit
        limiter.record_tokens(&key, 1).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert!(count > limit, "101 tokens > 100 limit, should exceed limit");
    }

    #[tokio::test]
    async fn test_redis_tpm_key_isolation() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        // 两个不同的 tenant 共享同一 Redis 限流器（共享同一连接池）
        let tenant_a_key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let tenant_b_key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        // Tenant A 记录 200 tokens
        limiter.record_tokens(&tenant_a_key, 200).await.unwrap();
        let count_a = limiter.get_token_count(&tenant_a_key).await.unwrap();
        assert_eq!(count_a, 200, "Tenant A should have 200 tokens");

        // Tenant B 的 token 计数应为 0（完全隔离）
        let count_b = limiter.get_token_count(&tenant_b_key).await.unwrap();
        assert_eq!(
            count_b, 0,
            "Tenant B should have 0 tokens (isolated from A)"
        );

        // Tenant B 记录 50 tokens，不影响 Tenant A
        limiter.record_tokens(&tenant_b_key, 50).await.unwrap();
        let count_b = limiter.get_token_count(&tenant_b_key).await.unwrap();
        assert_eq!(count_b, 50, "Tenant B should have 50 tokens");
        let count_a = limiter.get_token_count(&tenant_a_key).await.unwrap();
        assert_eq!(count_a, 200, "Tenant A should still have 200 tokens");
    }

    #[tokio::test]
    async fn test_redis_tpm_concurrent_recording() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let limiter = std::sync::Arc::new(limiter);

        // 并发记录不同 token 值，验证 Lua 脚本原子性
        let mut handles = Vec::new();
        for i in 0..10 {
            let limiter = std::sync::Arc::clone(&limiter);
            let key = key.clone();
            handles.push(tokio::spawn(async move {
                limiter.record_tokens(&key, (i + 1) * 10).await.unwrap();
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // 总和应为 10+20+...+100 = 550
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 550,
            "After concurrent recording of 10+20+...+100, count should be 550, got {}",
            count
        );
    }

    /// 测试 TPM 滑动窗口时间边界
    ///
    /// 验证窗口外的过期 ZSET 条目被 GET_TOKEN_COUNT_SCRIPT 正确排除，
    /// 窗口内的 token 被正确计入。
    #[tokio::test]
    async fn test_redis_tpm_window_boundary() {
        let Some(limiter) = create_test_limiter().await else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let redis_key = limiter.build_tpm_key(&key);

        // 通过直接 Redis 连接注入一条窗口外的过期数据（timestamp 远早于窗口起点）
        {
            let mut conn = limiter.pool.get().await.unwrap();
            let old_timestamp: f64 = 1000000.0; // Unix 纪元后约 11 天，远早于当前时间
            let old_member = format!(
                "{}:{}:{}",
                old_timestamp as i64,
                Uuid::new_v4().simple(),
                999
            );
            let _: () = deadpool_redis::redis::cmd("ZADD")
                .arg(&redis_key)
                .arg(old_timestamp)
                .arg(&old_member)
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        // get_token_count 应排除窗口外的 999 tokens，返回 0
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 0,
            "Window-expired tokens should be excluded from count"
        );

        // 记录 100 tokens 在当前窗口内
        limiter.record_tokens(&key, 100).await.unwrap();
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 100,
            "Current window tokens should be counted correctly"
        );

        // 确认窗口外旧条目仍被排除，仅窗口内 token 被计入
        let count = limiter.get_token_count(&key).await.unwrap();
        assert_eq!(
            count, 100,
            "Only window-in tokens should be counted after re-check"
        );
    }

    /// 测试 Redis 不可用时 fail-closed 错误传播
    ///
    /// 验证当 Redis 连接池不可用时：
    /// - `check_and_record_with_config` 返回 `Err(KeyComputeError::Internal)` 而不是 `Err(RateLimitExceeded)`
    /// - `record_tokens` 返回 `Err`
    /// - `get_token_count` 返回 `Err`
    ///
    /// 这保证了 middleware 层（rate_limit_middleware / public_auth_rate_limit_middleware）
    /// 能通过 match 捕获到非 RateLimitExceeded 错误并返回 503（fail-closed）。
    #[tokio::test]
    async fn test_redis_unavailable_fail_closed() {
        // 使用一个不会连接成功的 Redis URL 创建池
        let cfg = deadpool_redis::Config::from_url("redis://127.0.0.1:16379/");
        let pool = match cfg.create_pool(Some(deadpool_redis::Runtime::Tokio1)) {
            Ok(p) => p,
            Err(_) => return, // 创建池本身不应失败，只是连接会超时
        };

        let limiter = RedisRateLimiter::new(pool);
        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let config = crate::RateLimitConfig::new(10, 1000);

        // check_and_record_with_config 应返回 Internal 错误（不是 RateLimitExceeded）
        // 这是 fail-closed 的核心验证：Redis 不可用时禁止放行请求
        let result = limiter.check_and_record_with_config(&key, &config).await;
        match result {
            Err(KeyComputeError::Internal(_)) => {
                // 期望行为：Redis 连接失败，返回 Internal 错误
            }
            Err(KeyComputeError::RateLimitExceeded(_)) => {
                panic!(
                    "FAIL-CLOSED VIOLATION: Redis unavailable but got RateLimitExceeded, \
                     not Internal error. Request would have been incorrectly allowed through."
                );
            }
            Ok(_) => {
                panic!(
                    "FAIL-CLOSED VIOLATION: Redis unavailable but check_and_record succeeded. \
                     Request was allowed through without rate limiting."
                );
            }
            Err(other) => {
                // 其他错误类型也可以（只要不是 RateLimitExceeded）
                eprintln!("Got unexpected error type: {:?}", other);
            }
        }

        // record_tokens 也应返回错误（Redis 不可用时无法写入）
        let record_result = limiter.record_tokens(&key, 100).await;
        assert!(
            record_result.is_err(),
            "FAIL-CLOSED VIOLATION: Redis unavailable but record_tokens succeeded"
        );

        // get_token_count 也应返回错误（Redis 不可用时无法读取）
        let count_result = limiter.get_token_count(&key).await;
        assert!(
            count_result.is_err(),
            "FAIL-CLOSED VIOLATION: Redis unavailable but get_token_count succeeded"
        );
    }
}
