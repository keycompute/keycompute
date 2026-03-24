//! Redis 限流器实现
//!
//! 基于 Redis 的分布式限流后端，支持多实例共享限流状态。

use crate::{RPM_LIMIT, RateLimitKey, RateLimiter, WINDOW_SECS};
use async_trait::async_trait;
use keycompute_types::{KeyComputeError, Result};
use redis::{AsyncCommands, Client};
use std::sync::Arc;
use std::time::Duration;

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

        Ok(count < RPM_LIMIT as u64)
    }

    async fn record(&self, key: &RateLimitKey) -> Result<()> {
        let mut conn = self.get_conn().await?;
        let redis_key = self.build_key(key);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let _: () = conn
            .zadd(&redis_key, now, now)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        let expire_secs = self.window_size.as_secs() * 2;
        let _: () = conn
            .expire(&redis_key, expire_secs as i64)
            .await
            .map_err(|e| KeyComputeError::Internal(format!("Redis error: {}", e)))?;

        Ok(())
    }

    fn rpm_limit(&self) -> u32 {
        RPM_LIMIT
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

    fn create_test_limiter() -> Option<RedisRateLimiter> {
        match RedisRateLimiter::new("redis://127.0.0.1:6379") {
            Ok(limiter) => Some(limiter),
            Err(_) => {
                eprintln!("Warning: Redis not available, skipping Redis tests");
                None
            }
        }
    }

    #[tokio::test]
    async fn test_redis_rate_limiter_check_and_record() {
        let Some(limiter) = create_test_limiter() else {
            return;
        };

        let _ = limiter.flush_all().await;

        let key = RateLimitKey::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        assert!(limiter.check(&key).await.unwrap());

        limiter.record(&key).await.unwrap();

        assert!(limiter.check(&key).await.unwrap());
    }
}
