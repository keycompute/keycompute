//! Redis 配置

use serde::Deserialize;

const fn default_pool_size() -> u32 {
    10
}

const fn default_connect_timeout_secs() -> u64 {
    5
}

const fn default_pool_wait_timeout_ms() -> u64 {
    1_000
}
const fn default_command_timeout_ms() -> u64 {
    5_000
}

/// Redis 配置
#[derive(Debug, Deserialize, Clone)]
pub struct RedisConfig {
    /// Redis 连接 URL
    pub url: String,
    /// 连接池大小
    #[serde(default = "default_pool_size")]
    pub pool_size: u32,
    /// Independent pool for blocking Node task claims (not shared with result waits).
    #[serde(default = "default_pool_size")]
    pub node_poll_pool_size: u32,
    /// Independent pool for blocking Node result notifications.
    #[serde(default = "default_pool_size")]
    pub node_result_pool_size: u32,
    /// Maximum wait for a pool slot or recycling probe, in milliseconds.
    #[serde(default = "default_pool_wait_timeout_ms")]
    pub pool_wait_timeout_ms: u64,
    /// Response timeout for non-blocking Redis commands, in milliseconds.
    /// BRPOP uses its own finite blocking deadline plus transport grace.
    #[serde(default = "default_command_timeout_ms")]
    pub command_timeout_ms: u64,
    /// 连接超时（秒）
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379".to_string(),
            pool_size: default_pool_size(),
            connect_timeout_secs: default_connect_timeout_secs(),
            node_poll_pool_size: default_pool_size(),
            node_result_pool_size: default_pool_size(),
            pool_wait_timeout_ms: default_pool_wait_timeout_ms(),
            command_timeout_ms: default_command_timeout_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_redis_config() {
        let config = RedisConfig::default();
        assert_eq!(config.url, "redis://127.0.0.1:6379");
        assert_eq!(config.pool_size, 10);
        assert_eq!(config.connect_timeout_secs, 5);
    }
    #[test]
    fn omitted_resource_settings_keep_finite_defaults() {
        let parsed: RedisConfig = config::Config::builder()
            .add_source(config::File::from_str(
                "url = 'redis://localhost:6379'",
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(parsed.pool_size, 10);
        assert_eq!(parsed.node_poll_pool_size, 10);
        assert_eq!(parsed.node_result_pool_size, 10);
        assert_eq!(parsed.pool_wait_timeout_ms, 1_000);
        assert_eq!(parsed.command_timeout_ms, 5_000);
    }
}
