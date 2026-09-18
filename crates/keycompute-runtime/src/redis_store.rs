//! Redis 运行时状态存储实现
//!
//! 提供基于 Redis 的运行时状态存储后端，支持：
//! - 分布式状态共享
//! - 自动过期清理
//! - 高可用性
//! - 连接池管理

use deadpool_redis::redis::AsyncCommands;
use deadpool_redis::{Config, Pool, Runtime, Timeouts};
use std::time::Duration;

/// Redis 存储错误
#[derive(Debug, thiserror::Error)]
pub enum RedisStoreError {
    /// 连接池错误
    #[error("Redis pool error: {0}")]
    PoolError(#[from] deadpool_redis::PoolError),
    /// Redis 错误
    #[error("Redis error: {0}")]
    RedisError(#[from] deadpool_redis::redis::RedisError),
    /// 连接错误
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    /// 创建连接池错误
    #[error("Failed to create pool: {0}")]
    CreatePoolError(String),
}

fn storage_role_hook(
    role: crate::redis_roles::RedisConnectionRole,
    budget: Duration,
    response_timeout: Duration,
) -> deadpool_redis::Hook {
    deadpool_redis::Hook::async_fn(move |connection, _| {
        let role = role.clone();
        Box::pin(async move {
            if matches!(role, crate::redis_roles::RedisConnectionRole::Unrestricted) {
                return Ok(());
            }
            connection.set_response_timeout(budget);
            let result = tokio::time::timeout(
                budget,
                crate::redis_roles::validate_connection(connection, &role),
            )
            .await;
            connection.set_response_timeout(response_timeout);
            result
                .map_err(|_| {
                    deadpool_redis::HookError::message("Redis storage-role verification timed out")
                })?
                .map_err(deadpool_redis::HookError::Backend)
        })
    })
}

/// Redis 运行时存储
#[derive(Debug, Clone)]
pub struct RedisRuntimeStore {
    pool: Pool,
    key_prefix: String,
    default_ttl: Duration,
}

impl RedisRuntimeStore {
    /// 创建新的 Redis 运行时存储
    ///
    /// # 参数
    /// - `redis_url`: Redis 连接 URL
    pub fn new(redis_url: &str) -> Result<Self, RedisStoreError> {
        Self::from_config(&RedisPoolConfig {
            url: redis_url.to_string(),
            ..RedisPoolConfig::default()
        })
    }

    /// 创建带自定义前缀的存储
    pub fn with_prefix(
        redis_url: &str,
        prefix: impl Into<String>,
    ) -> Result<Self, RedisStoreError> {
        Self::from_config(&RedisPoolConfig {
            url: redis_url.to_string(),
            key_prefix: prefix.into(),
            ..RedisPoolConfig::default()
        })
    }

    /// 从配置创建存储
    pub fn from_config(config: &RedisPoolConfig) -> Result<Self, RedisStoreError> {
        let pool =
            Self::create_pool_with_options(&config.url, config.pool_size, config.connect_timeout)?;

        Ok(Self {
            pool,
            key_prefix: config.key_prefix.clone(),
            default_ttl: config.default_ttl,
        })
    }

    /// 设置默认 TTL
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = ttl;
        self
    }

    /// 构建完整的 Redis Key
    fn build_key(&self, key: &str) -> String {
        format!("{}:{}", self.key_prefix, key)
    }

    /// 获取 Redis 连接
    async fn get_conn(&self) -> Result<deadpool_redis::Connection, RedisStoreError> {
        self.pool.get().await.map_err(Into::into)
    }

    /// 健康检查
    pub async fn health_check(&self) -> Result<(), RedisStoreError> {
        let mut conn = self.get_conn().await?;
        let _: () = deadpool_redis::redis::cmd("PING")
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    /// 从 URL 创建共享连接池（静态工厂）
    ///
    /// 供 `state.rs` 等调用方获取 Pool 后传递给多个消费者，
    /// 避免外部模块直接依赖 `deadpool_redis::Config`。
    pub fn create_pool(redis_url: &str) -> Result<Pool, RedisStoreError> {
        let defaults = RedisPoolConfig::default();
        Self::create_pool_with_options(redis_url, defaults.pool_size, defaults.connect_timeout)
    }

    /// 从 URL 和连接池参数创建共享连接池。
    pub fn create_pool_with_options(
        redis_url: &str,
        pool_size: usize,
        connect_timeout: Duration,
    ) -> Result<Pool, RedisStoreError> {
        Self::create_pool_with_timeouts(
            redis_url,
            pool_size,
            connect_timeout,
            Duration::from_secs(1),
            Duration::from_secs(5),
        )
    }

    /// Create a pool for short commands. All admission, connection and recycle
    /// waits are finite; response timeouts also cover a connected but stalled
    /// Redis server. Blocking consumers must use a separate pool and override
    /// the response timeout for the duration of their blocking operation.
    pub fn create_pool_with_timeouts(
        redis_url: &str,
        pool_size: usize,
        connect_timeout: Duration,
        wait_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<Pool, RedisStoreError> {
        Self::create_pool_with_role(
            redis_url,
            pool_size,
            connect_timeout,
            wait_timeout,
            response_timeout,
            crate::redis_roles::RedisConnectionRole::Unrestricted,
        )
    }

    /// Role checks run on creation AND every pooled checkout. Invalid idle
    /// sockets are discarded, not silently reused after policy/role drift.
    /// Already checked-out commands still require restrictive production ACLs.
    pub fn create_pool_with_role(
        redis_url: &str,
        pool_size: usize,
        connect_timeout: Duration,
        wait_timeout: Duration,
        response_timeout: Duration,
        role: crate::redis_roles::RedisConnectionRole,
    ) -> Result<Pool, RedisStoreError> {
        if pool_size == 0
            || pool_size > 65_536
            || connect_timeout.is_zero()
            || wait_timeout.is_zero()
            || response_timeout.is_zero()
            || wait_timeout > Duration::from_secs(60)
            || response_timeout > Duration::from_secs(60)
        {
            return Err(RedisStoreError::CreatePoolError(
                "invalid Redis pool capacity or timeout".to_string(),
            ));
        }
        let mut cfg = Config::from_url(redis_url);
        cfg.pool = Some(deadpool_redis::PoolConfig {
            max_size: pool_size,
            timeouts: Timeouts {
                create: Some(connect_timeout),
                wait: Some(wait_timeout),
                recycle: Some(wait_timeout),
            },
            ..Default::default()
        });
        cfg.builder()
            .map_err(|e| RedisStoreError::CreatePoolError(e.to_string()))?
            .runtime(Runtime::Tokio1)
            .post_create(deadpool_redis::Hook::sync_fn(move |connection, _| {
                connection.set_response_timeout(response_timeout);
                Ok(())
            }))
            .post_create(storage_role_hook(
                role.clone(),
                connect_timeout.min(Duration::from_secs(5)),
                response_timeout,
            ))
            .post_recycle(storage_role_hook(
                role,
                wait_timeout.min(Duration::from_secs(5)),
                response_timeout,
            ))
            .build()
            .map_err(|e| RedisStoreError::CreatePoolError(e.to_string()))
    }

    /// 使用已有连接池创建存储
    pub fn with_pool(pool: Pool) -> Self {
        Self {
            pool,
            key_prefix: "keycompute:runtime".to_string(),
            default_ttl: Duration::from_secs(300),
        }
    }

    /// 使用已有连接池 + 自定义前缀
    pub fn with_pool_and_prefix(pool: Pool, prefix: impl Into<String>) -> Self {
        Self {
            pool,
            key_prefix: prefix.into(),
            default_ttl: Duration::from_secs(300),
        }
    }

    /// 获取连接池状态
    pub fn pool_status(&self) -> deadpool_redis::Status {
        self.pool.status()
    }

    /// 获取 Redis 连接池引用
    pub fn pool(&self) -> &Pool {
        &self.pool
    }
}

impl RedisRuntimeStore {
    /// 批量获取值
    pub async fn mget(&self, keys: &[&str]) -> Vec<Option<String>> {
        let full_keys: Vec<String> = keys.iter().map(|k| self.build_key(k)).collect();

        match self.get_conn().await {
            Ok(mut conn) => conn.mget(&full_keys).await.unwrap_or_else(|_| vec![]),
            Err(e) => {
                tracing::warn!("Failed to get Redis connection: {}", e);
                vec![None; keys.len()]
            }
        }
    }

    /// 批量设置值
    pub async fn mset(&self, kvs: &[(&str, &str)], ttl: Option<Duration>) {
        let ttl = ttl.unwrap_or(self.default_ttl);

        match self.get_conn().await {
            Ok(mut conn) => {
                for (key, value) in kvs {
                    let full_key = self.build_key(key);
                    if let Err(e) = conn
                        .set_ex::<&str, &str, ()>(&full_key, *value, ttl.as_secs())
                        .await
                    {
                        tracing::warn!("Redis mset error: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Failed to get Redis connection: {}", e);
            }
        }
    }

    /// 检查键是否存在
    pub async fn exists(&self, key: &str) -> bool {
        match self.get_conn().await {
            Ok(mut conn) => conn.exists(self.build_key(key)).await.unwrap_or(false),
            Err(e) => {
                tracing::warn!("Failed to get Redis connection: {}", e);
                false
            }
        }
    }

    /// 获取剩余过期时间（秒）
    pub async fn ttl(&self, key: &str) -> i64 {
        match self.get_conn().await {
            Ok(mut conn) => conn.ttl(self.build_key(key)).await.unwrap_or(-2),
            Err(e) => {
                tracing::warn!("Failed to get Redis connection: {}", e);
                -2
            }
        }
    }

    /// 清理所有以当前前缀开头的键
    pub async fn flush_prefix(&self) -> Result<(), RedisStoreError> {
        let pattern = format!("{}:*", self.key_prefix);

        // 收集所有匹配的 key
        let mut keys = Vec::new();
        {
            let mut conn = self.get_conn().await?;
            let mut iter: deadpool_redis::redis::AsyncIter<String> = conn
                .scan_match(&pattern)
                .await
                .map_err(RedisStoreError::RedisError)?;
            while let Some(key) = iter.next_item().await {
                keys.push(key);
            }
        }

        // 批量删除 key
        if !keys.is_empty() {
            let mut conn = self.get_conn().await?;
            let _: () = conn.del(&keys).await.map_err(RedisStoreError::RedisError)?;
        }

        Ok(())
    }

    /// 获取 Key 前缀
    pub fn key_prefix(&self) -> &str {
        &self.key_prefix
    }
}

/// Redis 连接池配置
#[derive(Debug, Clone)]
pub struct RedisPoolConfig {
    /// Redis URL
    pub url: String,
    /// 连接池大小
    pub pool_size: usize,
    /// 连接超时
    pub connect_timeout: Duration,
    /// 默认 TTL
    pub default_ttl: Duration,
    /// Key 前缀
    pub key_prefix: String,
}

impl Default for RedisPoolConfig {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379".to_string(),
            pool_size: 10,
            connect_timeout: Duration::from_secs(5),
            default_ttl: Duration::from_secs(300),
            key_prefix: "keycompute:runtime".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_pool_uses_requested_size() {
        let pool = RedisRuntimeStore::create_pool_with_options(
            "redis://127.0.0.1:6379",
            17,
            Duration::from_secs(3),
        )
        .expect("pool construction should not require a live Redis server");

        assert_eq!(pool.status().max_size, 17);
    }

    #[test]
    fn every_default_factory_bounds_admission_and_recycling() {
        let url = "redis://127.0.0.1:6379";
        let plain = RedisRuntimeStore::new(url).unwrap();
        let prefixed = RedisRuntimeStore::with_prefix(url, "test:prefix").unwrap();
        assert_eq!(prefixed.key_prefix, "test:prefix");
        let direct = RedisRuntimeStore::create_pool(url).unwrap();
        for pool in [plain.pool(), prefixed.pool(), &direct] {
            assert_eq!(pool.status().max_size, 10);
            assert_eq!(pool.timeouts().create, Some(Duration::from_secs(5)));
            assert_eq!(pool.timeouts().wait, Some(Duration::from_secs(1)));
            assert_eq!(pool.timeouts().recycle, Some(Duration::from_secs(1)));
        }
    }

    #[test]
    fn configured_pool_preserves_each_timeout_budget() {
        let pool = RedisRuntimeStore::create_pool_with_timeouts(
            "redis://127.0.0.1:6379",
            7,
            Duration::from_secs(3),
            Duration::from_millis(123),
            Duration::from_millis(456),
        )
        .unwrap();
        assert_eq!(pool.status().max_size, 7);
        assert_eq!(pool.timeouts().create, Some(Duration::from_secs(3)));
        assert_eq!(pool.timeouts().wait, Some(Duration::from_millis(123)));
        assert_eq!(pool.timeouts().recycle, Some(Duration::from_millis(123)));
    }

    #[test]
    fn invalid_pool_settings_fail_before_connecting() {
        let second = Duration::from_secs(1);
        for (size, connect, wait, response) in [
            (0, second, second, second),
            (65_537, second, second, second),
            (1, Duration::ZERO, second, second),
            (1, second, Duration::ZERO, second),
            (1, second, second, Duration::ZERO),
            (1, second, Duration::from_secs(61), second),
            (1, second, second, Duration::from_secs(61)),
        ] {
            assert!(
                RedisRuntimeStore::create_pool_with_timeouts(
                    "redis://127.0.0.1:6379",
                    size,
                    connect,
                    wait,
                    response,
                )
                .is_err()
            );
        }
    }
}
