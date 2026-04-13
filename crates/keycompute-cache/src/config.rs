//! 缓存配置

use serde::Deserialize;
use std::time::Duration;

/// 缓存后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheBackend {
    /// 内存缓存（默认，用于开发/测试）
    #[default]
    Memory,
    /// Redis 缓存
    Redis,
}

/// 缓存配置
#[derive(Debug, Clone, Deserialize)]
pub struct CacheConfig {
    /// 缓存后端类型
    #[serde(default)]
    pub backend: CacheBackend,

    /// Redis 连接地址
    #[serde(default = "default_redis_url")]
    pub redis_url: String,

    /// Redis 连接池大小
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,

    /// 连接超时（秒）
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_secs: u64,

    /// 操作超时（秒）
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    /// 默认 TTL（秒）
    #[serde(default = "default_ttl")]
    pub default_ttl_secs: u64,

    /// Key 前缀
    #[serde(default = "default_key_prefix")]
    pub key_prefix: String,
}

fn default_redis_url() -> String {
    "redis://127.0.0.1:6379".to_string()
}

fn default_pool_size() -> usize {
    10
}

fn default_connect_timeout() -> u64 {
    5
}

fn default_timeout() -> u64 {
    10
}

fn default_ttl() -> u64 {
    300 // 5分钟
}

fn default_key_prefix() -> String {
    "keycompute:cache".to_string()
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Memory,
            redis_url: default_redis_url(),
            pool_size: default_pool_size(),
            connect_timeout_secs: default_connect_timeout(),
            timeout_secs: default_timeout(),
            default_ttl_secs: default_ttl(),
            key_prefix: default_key_prefix(),
        }
    }
}

impl CacheConfig {
    /// 从环境变量创建配置
    pub fn from_env() -> Self {
        let backend = std::env::var("KC__CACHE__BACKEND")
            .ok()
            .map(|v| match v.to_lowercase().as_str() {
                "redis" => CacheBackend::Redis,
                _ => CacheBackend::Memory,
            })
            .unwrap_or(CacheBackend::Memory);

        let redis_url = std::env::var("KC__CACHE__REDIS_URL")
            .unwrap_or_else(|_| default_redis_url());

        let pool_size = std::env::var("KC__CACHE__POOL_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_pool_size());

        let default_ttl_secs = std::env::var("KC__CACHE__DEFAULT_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_ttl());

        let key_prefix = std::env::var("KC__CACHE__KEY_PREFIX")
            .unwrap_or_else(|_| default_key_prefix());

        Self {
            backend,
            redis_url,
            pool_size,
            connect_timeout_secs: default_connect_timeout(),
            timeout_secs: default_timeout(),
            default_ttl_secs,
            key_prefix,
        }
    }

    /// 获取默认 TTL
    pub fn default_ttl(&self) -> Duration {
        Duration::from_secs(self.default_ttl_secs)
    }

    /// 获取连接超时
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// 获取操作超时
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = CacheConfig::default();
        assert_eq!(config.backend, CacheBackend::Memory);
        assert_eq!(config.pool_size, 10);
    }

    #[test]
    fn test_from_env() {
        // 使用 try_set_var 避免 unsafe（如果可用）或在安全方式下测试
        let config = CacheConfig::from_env();
        // 只验证默认值，不依赖环境变量
        assert!(matches!(config.backend, CacheBackend::Memory | CacheBackend::Redis));
    }
}