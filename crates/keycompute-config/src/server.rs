//! 服务器配置

use serde::Deserialize;

/// 服务器配置
#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    /// 绑定地址
    pub bind_addr: String,
    /// 监听端口
    pub port: u16,
    /// Maximum graceful drain interval; reserve supervisor cleanup headroom.
    #[serde(default = "default_shutdown_timeout_secs")]
    pub shutdown_timeout_secs: u64,
}

const fn default_shutdown_timeout_secs() -> u64 {
    120
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0".to_string(),
            port: 3000,
            shutdown_timeout_secs: default_shutdown_timeout_secs(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_server_config() {
        let config = ServerConfig::default();
        assert_eq!(config.bind_addr, "0.0.0.0");
        assert_eq!(config.port, 3000);
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    #[test]
    fn drain_deadline_defaults_parses_and_rejects_invalid_values() {
        assert_eq!(ServerConfig::default().shutdown_timeout_secs, 120);
        let parsed: ServerConfig = config::Config::builder()
            .add_source(config::File::from_str(
                "bind_addr='127.0.0.1'\nport=3000\nshutdown_timeout_secs=30",
                config::FileFormat::Toml,
            ))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(parsed.shutdown_timeout_secs, 30);
        for seconds in [0, 3601] {
            let mut config = crate::AppConfig::default();
            config.server.shutdown_timeout_secs = seconds;
            assert!(
                matches!(config.validate(),Err(crate::ConfigLoadError::ValidationError(message)) if message.contains("shutdown timeout"))
            );
        }
    }
}
