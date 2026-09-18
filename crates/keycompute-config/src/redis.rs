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
const fn default_cache_timeout_ms() -> u64 {
    500
}

const fn default_command_timeout_ms() -> u64 {
    5_000
}

/// Redis 配置
#[derive(Debug, Deserialize, Clone)]
pub struct RedisConfig {
    /// Redis 连接 URL
    pub url: String,
    /// Optional disposable cache endpoint. Never falls back to the critical URL.
    #[serde(default)]
    pub cache_url: Option<String>,
    #[serde(default = "default_pool_size")]
    pub cache_pool_size: u32,
    /// Separate short timeout budget for optional cache operations.
    #[serde(default = "default_cache_timeout_ms")]
    pub cache_timeout_ms: u64,
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
            cache_url: None,
            cache_pool_size: default_pool_size(),
            cache_timeout_ms: default_cache_timeout_ms(),
            pool_size: default_pool_size(),
            connect_timeout_secs: default_connect_timeout_secs(),
            node_poll_pool_size: default_pool_size(),
            node_result_pool_size: default_pool_size(),
            pool_wait_timeout_ms: default_pool_wait_timeout_ms(),
            command_timeout_ms: default_command_timeout_ms(),
        }
    }
}

impl RedisConfig {
    /// Different database numbers or credentials are not physical isolation.
    /// A runtime run_id check additionally catches aliases of the same server.
    pub fn validate_cache_endpoint(&self) -> Result<(), &'static str> {
        let Some(cache) = self.cache_url.as_deref() else {
            return Ok(());
        };
        if cache.trim().is_empty() {
            return Err("Redis cache URL must not be empty");
        }
        let critical = url::Url::parse(&self.url).map_err(|_| "invalid critical Redis URL")?;
        let cache = url::Url::parse(cache).map_err(|_| "invalid cache Redis URL")?;
        let same_endpoint = match (critical.host_str(), cache.host_str()) {
            (Some(a), Some(b)) => {
                a.eq_ignore_ascii_case(b)
                    && critical.port().unwrap_or(6379) == cache.port().unwrap_or(6379)
            }
            (None, None) => critical.scheme() == cache.scheme() && critical.path() == cache.path(),
            _ => false,
        };
        if same_endpoint {
            Err(
                "Redis cache must use a separate server, not another database on the critical endpoint",
            )
        } else {
            Ok(())
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
    #[test]
    fn cache_configuration_does_not_accept_another_database_or_password_as_isolation() {
        for cache in [
            "redis://127.0.0.1:6379/1",
            "redis://:other@127.0.0.1:6379/2",
        ] {
            let config = RedisConfig {
                cache_url: Some(cache.into()),
                ..Default::default()
            };
            assert!(config.validate_cache_endpoint().is_err());
        }
        assert!(RedisConfig::default().validate_cache_endpoint().is_ok());
        let distinct = RedisConfig {
            cache_url: Some("redis://127.0.0.1:6380".into()),
            ..Default::default()
        };
        assert!(distinct.validate_cache_endpoint().is_ok());
        assert_eq!(distinct.cache_pool_size, 10);
        assert_eq!(distinct.cache_timeout_ms, 500);
    }
}
