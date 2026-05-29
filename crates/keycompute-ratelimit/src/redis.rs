//! Redis 限流器实现
//!
//! 基于 Redis 的分布式限流后端，支持多实例共享限流状态。

use crate::{DEFAULT_RPM_LIMIT, RateLimitKey, RateLimiter, WINDOW_SECS};
use async_trait::async_trait;
use keycompute_types::{KeyComputeError, Result};
use redis::{AsyncCommands, Client};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Redis 限流器
///
/// 使用 Redis 实现分布式限流，支持：
/// - 滑动窗口限流
/// - 多实例共享限流状态
/// - 自动过期清理
#[derive(Debug, Clone)]
pub struct RedisRateLimiter {
    client: Arc<Client>,
    window_size: Duration,
    key_prefix: String,
}

impl RedisRateLimiter {
    /// 创建新的 Redis 限流器
    ///
    /// # 参数
    /// - `redis_url`: Redis 连接 URL，如 "redis://127.0.0.1:6379"
    pub fn new(redis_url: &str) -> Result<Self> {
        let client = Client::open(redis_url)
            .map_err(|e| KeyComputeError::Internal(format!("Failed to connect to Redis: {}", e)))?;

        Ok(Self {
            client: Arc::new(client),
            window_size: Duration::from_secs(WINDOW_SECS),
            key_prefix: "ratelimit".to_string(),
        })
    }

    /// 创建带自定义前缀的限流器
    pub fn with_prefix(redis_url: &str, prefix: impl Into<String>) -> Result<Self> {
        let client = Client::open(redis_url)
            .map_err(|e| KeyComputeError::Internal(format!("Failed to connect to Redis: {}", e)))?;

        Ok(Self {
            client: Arc::new(client),
            window_size: Duration::from_secs(WINDOW_SECS),
            key_prefix: prefix.into(),
        })
    }

    /// 构建 Redis Key
    fn build_key(&self, key: &RateLimitKey) -> String {
        format!(
            "{}:{}:{}:{}:rpm",
            self.key_prefix, key.tenant_id, key.user_id, key.api_key_id
        )
    }

    /// 获取 Redis 连接
    async fn get_conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        self.client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis connection error: {}", e)))
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
}

#[async_trait]
impl RateLimiter for RedisRateLimiter {
    async fn check(&self, key: &RateLimitKey) -> Result<bool> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_key(key);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let window_start = now - self.window_size.as_secs() as i64;

        let _: () = conn
            .zrembyscore(&redis_key, 0, window_start)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let count: u64 = conn
            .zcard(&redis_key)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(count < DEFAULT_RPM_LIMIT as u64)
    }

    async fn check_with_config(
        &self,
        key: &RateLimitKey,
        config: &crate::RateLimitConfig,
    ) -> Result<bool> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_key(key);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let window_start = now - self.window_size.as_secs() as i64;

        let _: () = conn
            .zrembyscore(&redis_key, 0, window_start)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let count: u64 = conn
            .zcard(&redis_key)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(count < config.rpm_limit as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_key(key);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // 使用 UUID 作为唯一成员，避免同一秒内的请求被去重
        let unique_member = format!("{}:{}", now, Uuid::new_v4().simple());

        let _: () = conn
            .zadd(&redis_key, &unique_member, now)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let expire_secs = self.window_size.as_secs() * 2;
        let _: () = conn
            .expire(&redis_key, expire_secs as i64)
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
        let redis_key = self.build_key(key);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let window_start = now - self.window_size.as_secs() as i64;
        let unique_member = format!("{}:{}", now, Uuid::new_v4().simple());
        let expire_secs = self.window_size.as_secs() * 2;

        // 使用 Lua 脚本原子执行检查和记录
        let result: i64 = redis::cmd("EVAL")
            .arg(Self::CHECK_AND_RECORD_SCRIPT)
            .arg(1)
            .arg(&redis_key)
            .arg(now)
            .arg(window_start)
            .arg(config.rpm_limit as i64)
            .arg(&unique_member)
            .arg(expire_secs as i64)
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

    async fn record_tokens(&self, _key: &RateLimitKey, _tokens: u32) -> Result<()> {
        // Redis 限流器暂不支持 Token 计数，留空实现
        Ok(())
    }

    async fn get_count(&self, key: &RateLimitKey) -> Result<u64> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_key(key);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let window_start = now - self.window_size.as_secs() as i64;

        let _: () = conn
            .zrembyscore(&redis_key, 0, window_start)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let count: u64 = conn
            .zcard(&redis_key)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(count)
    }

    async fn get_token_count(&self, _key: &RateLimitKey) -> Result<u64> {
        // Redis 限流器暂不支持 Token 计数
        Ok(0)
    }
}

impl RedisRateLimiter {
    /// 清理所有限流数据（用于测试或重置）
    pub async fn flush_all(&self) -> Result<()> {
        let pattern = format!("{}:*", self.key_prefix);

        let mut keys = Vec::new();
        {
            let mut conn = self.get_conn().await?;
            let mut iter: redis::AsyncIter<String> = conn
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

    async fn create_test_limiter() -> Option<RedisRateLimiter> {
        match RedisRateLimiter::new("redis://127.0.0.1:6379") {
            Ok(limiter) => {
                // 验证实际连接可用，避免本地 Redis 无认证时报错
                if limiter.flush_all().await.is_ok() {
                    Some(limiter)
                } else {
                    eprintln!(
                        "Warning: Redis not available (connection failed), skipping Redis tests"
                    );
                    None
                }
            }
            Err(_) => {
                eprintln!("Warning: Redis not available, skipping Redis tests");
                None
            }
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
}
