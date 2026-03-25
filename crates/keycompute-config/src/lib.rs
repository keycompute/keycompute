//! KeyCompute 配置管理模块
//!
//! 提供统一的配置加载机制：
//! 1. 环境变量优先（前缀 KC__，双下划线分隔层级）
//! 2. 配置文件回退（项目根目录 config.toml）
//! 3. 默认值兜底

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::path::Path;

pub mod auth;
pub mod crypto;
pub mod database;
pub mod email;
pub mod gateway;
pub mod redis;
pub mod server;

pub use auth::AuthConfig;
pub use crypto::CryptoConfig;
pub use database::DatabaseConfig;
pub use email::EmailConfig;
pub use gateway::{GatewayConfig, ProxyConfig};
pub use redis::RedisConfig;
pub use server::ServerConfig;

/// 全局应用配置
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    /// 服务器配置
    pub server: ServerConfig,
    /// 数据库配置
    pub database: DatabaseConfig,
    /// Redis 配置（可选）
    pub redis: Option<RedisConfig>,
    /// 认证配置
    pub auth: AuthConfig,
    /// Gateway 配置
    pub gateway: GatewayConfig,
    /// 加密配置（可选）
    pub crypto: Option<CryptoConfig>,
    /// 邮件服务配置
    pub email: EmailConfig,
}

/// 配置加载错误
#[derive(Debug, thiserror::Error)]
pub enum ConfigLoadError {
    #[error("配置解析失败: {0}")]
    ParseError(#[from] ConfigError),
    #[error("配置文件不存在: {0}")]
    FileNotFound(String),
    #[error("环境变量格式错误: {0}")]
    EnvFormatError(String),
}

impl AppConfig {
    /// 加载配置（环境变量优先，配置文件回退）
    ///
    /// 加载顺序：
    /// 1. 设置默认值
    /// 2. 从项目根目录 config.toml 加载（如果存在）
    /// 3. 从环境变量 KC__* 加载（覆盖配置文件）
    ///
    /// # 环境变量格式
    /// - 使用 `KC__` 前缀
    /// - 使用双下划线 `__` 分隔层级
    /// - 示例：`KC__SERVER__PORT=8080` 对应 `server.port`
    pub fn load() -> Result<Self, ConfigLoadError> {
        // 1. 设置默认值
        let mut builder = Self::create_default_builder()?;

        // 2. 从配置文件加载（如果存在）
        let config_paths = [
            "config.toml",
            "/opt/rust/project/keycompute/key_compute/config.toml",
        ];

        for path in &config_paths {
            if Path::new(path).exists() {
                tracing::info!("加载配置文件: {}", path);
                builder = builder.add_source(File::with_name(path).required(false));
                break;
            }
        }

        // 3. 从环境变量加载（覆盖配置文件）
        // 支持 KC__SECTION__KEY 格式
        builder = builder.add_source(
            Environment::with_prefix("KC")
                .separator("__")
                .try_parsing(true),
        );

        let config = builder.build()?;
        let app_config: AppConfig = config.try_deserialize()?;

        tracing::info!("配置加载成功");
        Ok(app_config)
    }

    /// 仅从环境变量加载配置
    pub fn from_env() -> Result<Self, ConfigLoadError> {
        // 设置默认值
        let mut builder = Self::create_default_builder()?;

        // 仅从环境变量加载
        builder = builder.add_source(
            Environment::with_prefix("KC")
                .separator("__")
                .try_parsing(true),
        );

        let config = builder.build()?;
        let app_config: AppConfig = config.try_deserialize()?;

        Ok(app_config)
    }

    /// 仅从配置文件加载配置
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigLoadError> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(ConfigLoadError::FileNotFound(
                path.to_string_lossy().to_string(),
            ));
        }

        // 设置默认值
        let mut builder = Self::create_default_builder()?;

        // 从指定文件加载
        builder = builder.add_source(File::from(path).required(true));

        let config = builder.build()?;
        let app_config: AppConfig = config.try_deserialize()?;

        Ok(app_config)
    }

    /// 创建带默认值的配置构建器
    fn create_default_builder()
    -> Result<config::ConfigBuilder<config::builder::DefaultState>, ConfigError> {
        let builder = Config::builder()
            // 服务器默认值
            .set_default("server.bind_addr", "0.0.0.0")?
            .set_default("server.port", 3000)?
            // 数据库默认值
            .set_default("database.url", "postgres://localhost/keycompute")?
            .set_default("database.max_connections", 10)?
            .set_default("database.min_connections", 2)?
            .set_default("database.connect_timeout_secs", 30)?
            .set_default("database.idle_timeout_secs", 600)?
            .set_default("database.max_lifetime_secs", 1800)?
            // 认证默认值
            .set_default("auth.jwt_secret", "change-me-in-production")?
            .set_default("auth.jwt_issuer", "keycompute")?
            .set_default("auth.jwt_expiry_secs", 3600)?
            // Gateway 默认值
            .set_default("gateway.max_retries", 3)?
            .set_default("gateway.timeout_secs", 120)?
            .set_default("gateway.enable_fallback", true)?
            .set_default("gateway.request_timeout_secs", 120)?
            .set_default("gateway.stream_timeout_secs", 600)?
            // Gateway 重试策略默认值
            .set_default("gateway.retry.initial_backoff_ms", 100)?
            .set_default("gateway.retry.max_backoff_ms", 10000)?
            .set_default("gateway.retry.backoff_multiplier", 2.0)?;

        Ok(builder)
    }

    /// 验证配置有效性
    pub fn validate(&self) -> Result<(), ConfigLoadError> {
        // 验证服务器端口
        if self.server.port == 0 {
            return Err(ConfigLoadError::EnvFormatError(
                "服务器端口不能为 0".to_string(),
            ));
        }

        // 验证数据库 URL
        if self.database.url.is_empty() {
            return Err(ConfigLoadError::EnvFormatError(
                "数据库 URL 不能为空".to_string(),
            ));
        }

        Ok(())
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            redis: None,
            auth: AuthConfig::default(),
            gateway: GatewayConfig::default(),
            crypto: None,
            email: EmailConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_default_config() {
        let config = AppConfig::default();
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.server.bind_addr, "0.0.0.0");
    }

    #[test]
    #[serial]
    fn test_config_from_env() {
        // 注意：这个测试会读取实际的环境变量
        // 使用 unsafe 因为 set_var/remove_var 在 Rust 2024 中是 unsafe
        unsafe {
            std::env::set_var("KC__SERVER__PORT", "8080");
            std::env::set_var("KC__EMAIL__SMTP_HOST", "localhost");
            std::env::set_var("KC__EMAIL__SMTP_USERNAME", "test");
            std::env::set_var("KC__EMAIL__SMTP_PASSWORD", "test");
            std::env::set_var("KC__EMAIL__FROM_ADDRESS", "test@localhost");
            std::env::set_var("KC__EMAIL__VERIFICATION_BASE_URL", "http://localhost");
        }

        let config = AppConfig::from_env().expect("应该从环境变量加载配置");
        assert_eq!(config.server.port, 8080);

        // 清理
        unsafe {
            std::env::remove_var("KC__SERVER__PORT");
            std::env::remove_var("KC__EMAIL__SMTP_HOST");
            std::env::remove_var("KC__EMAIL__SMTP_USERNAME");
            std::env::remove_var("KC__EMAIL__SMTP_PASSWORD");
            std::env::remove_var("KC__EMAIL__FROM_ADDRESS");
            std::env::remove_var("KC__EMAIL__VERIFICATION_BASE_URL");
        }
    }

    #[test]
    #[serial]
    fn test_crypto_config_from_env() {
        // 设置 crypto 和 email 环境变量
        unsafe {
            std::env::set_var("KC__CRYPTO__SECRET_KEY", "dGVzdC1rZXktZnJvbS1lbnY=");
            std::env::set_var("KC__EMAIL__SMTP_HOST", "localhost");
            std::env::set_var("KC__EMAIL__SMTP_USERNAME", "test");
            std::env::set_var("KC__EMAIL__SMTP_PASSWORD", "test");
            std::env::set_var("KC__EMAIL__FROM_ADDRESS", "test@localhost");
            std::env::set_var("KC__EMAIL__VERIFICATION_BASE_URL", "http://localhost");
        }

        let config = AppConfig::from_env().expect("应该从环境变量加载配置");

        // 验证 crypto 配置被正确加载
        assert!(config.crypto.is_some(), "crypto 配置应该存在");
        let crypto = config.crypto.unwrap();
        assert!(crypto.has_key(), "crypto 应该有密钥");
        assert_eq!(crypto.secret_key(), Some("dGVzdC1rZXktZnJvbS1lbnY="));

        // 清理
        unsafe {
            std::env::remove_var("KC__CRYPTO__SECRET_KEY");
            std::env::remove_var("KC__EMAIL__SMTP_HOST");
            std::env::remove_var("KC__EMAIL__SMTP_USERNAME");
            std::env::remove_var("KC__EMAIL__SMTP_PASSWORD");
            std::env::remove_var("KC__EMAIL__FROM_ADDRESS");
            std::env::remove_var("KC__EMAIL__VERIFICATION_BASE_URL");
        }
    }
}
