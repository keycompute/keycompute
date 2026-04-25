//! KeyCompute 配置管理模块
//!
//! 提供统一的配置加载机制：
//! 1. 环境变量优先（前缀 KC__，双下划线分隔层级）
//! 2. 配置文件回退（项目根目录 config.toml）
//! 3. 默认值兜底
//! 4. 顶层 `APP_BASE_URL` 环境变量可覆盖公开链接基础地址

use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;
use std::net::IpAddr;
use std::path::Path;
use url::Url;

pub mod auth;
pub mod crypto;
pub mod database;
pub mod distribution;
pub mod email;
pub mod gateway;
pub mod redis;
pub mod server;

pub use auth::AuthConfig;
pub use auth::DEFAULT_JWT_SECRET;
pub use crypto::CryptoConfig;
pub use database::DatabaseConfig;
pub use distribution::DistributionConfig;
pub use email::EmailConfig;
pub use gateway::{GatewayConfig, ProxyConfig};
pub use redis::RedisConfig;
pub use server::ServerConfig;

/// 全局应用配置
#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    /// 对外公开的前端应用基础 URL（可选）
    pub app_base_url: Option<String>,
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
    /// 分销配置
    pub distribution: DistributionConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_base_url: Some(Self::default_app_base_url()),
            server: ServerConfig::default(),
            database: DatabaseConfig::default(),
            redis: None,
            auth: AuthConfig::default(),
            gateway: GatewayConfig::default(),
            crypto: None,
            email: EmailConfig::default(),
            distribution: DistributionConfig::default(),
        }
    }
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
    #[error("配置验证失败: {0}")]
    ValidationError(String),
}

impl AppConfig {
    fn default_app_base_url() -> String {
        "http://localhost:80".to_string()
    }

    pub fn resolved_app_base_url(&self) -> String {
        Self::normalize_app_base_url(self.app_base_url.clone())
            .unwrap_or_else(Self::default_app_base_url)
    }

    fn apply_global_env_overrides(mut app_config: AppConfig) -> AppConfig {
        if let Ok(url) = std::env::var("APP_BASE_URL") {
            app_config.app_base_url = Self::normalize_app_base_url(Some(url));
        } else {
            app_config.app_base_url = Self::normalize_app_base_url(app_config.app_base_url);
        }

        if app_config.app_base_url.is_none() {
            app_config.app_base_url = Some(Self::default_app_base_url());
        }

        app_config
    }

    fn normalize_app_base_url(value: Option<String>) -> Option<String> {
        value.and_then(|url| {
            let normalized = url.trim().trim_end_matches('/').to_string();
            if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            }
        })
    }

    fn is_local_development_host(url: &Url) -> bool {
        match url.host_str() {
            Some("localhost") => true,
            Some(host) => host
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false),
            None => false,
        }
    }

    fn validate_public_app_base_url(base_url: &str) -> Result<(), String> {
        let parsed = Url::parse(base_url)
            .map_err(|e| format!("APP_BASE_URL 必须是合法的绝对 URL: {}", e))?;

        if parsed.host_str().is_none() {
            return Err("APP_BASE_URL 必须包含主机名".to_string());
        }

        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("APP_BASE_URL 不能包含用户名或密码".to_string());
        }

        if parsed.query().is_some() || parsed.fragment().is_some() {
            return Err("APP_BASE_URL 不能包含查询参数或片段".to_string());
        }

        match parsed.scheme() {
            "https" => Ok(()),
            "http" if Self::is_local_development_host(&parsed) => Ok(()),
            "http" => Err("APP_BASE_URL 在非本地环境必须使用 https".to_string()),
            scheme => Err(format!(
                "APP_BASE_URL 仅支持 http/https 协议，当前为 {}",
                scheme
            )),
        }
    }

    /// 加载配置（环境变量优先，配置文件回退）
    ///
    /// 加载顺序：
    /// 1. 设置默认值
    /// 2. 从项目根目录 config.toml 加载（如果存在）
    /// 3. 从环境变量 KC__* 加载（覆盖配置文件）
    /// 4. 从顶层 APP_BASE_URL 加载公开链接基础地址（覆盖配置文件）
    ///
    /// # 环境变量格式
    /// - 使用 `KC__` 前缀
    /// - 使用双下划线 `__` 分隔层级
    /// - 示例：`KC__SERVER__PORT=8080` 对应 `server.port`
    /// - 顶层 `APP_BASE_URL` 用于公开前端地址
    pub fn load() -> Result<Self, ConfigLoadError> {
        // 1. 设置默认值
        let mut builder = Self::create_default_builder()?;

        // 2. 从配置文件加载（如果存在）
        let config_paths = ["config.toml"];

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
        let app_config: AppConfig = Self::apply_global_env_overrides(config.try_deserialize()?);

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
        let app_config: AppConfig = Self::apply_global_env_overrides(config.try_deserialize()?);

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
        let app_config: AppConfig = Self::apply_global_env_overrides(config.try_deserialize()?);

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
            .set_default("auth.jwt_secret", DEFAULT_JWT_SECRET)?
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
            .set_default("gateway.retry.backoff_multiplier", 2.0)?
            // 分销默认值
            .set_default("distribution.default_level1_ratio", 0.03)?
            .set_default("distribution.default_level2_ratio", 0.02)?
            .set_default("distribution.max_total_ratio", 0.30)?;

        Ok(builder)
    }

    /// 验证配置有效性
    ///
    /// 验证项包括：
    /// - 服务器绑定地址有效性
    /// - 服务器端口有效性
    /// - 数据库连接 URL 有效性
    /// - 数据库连接池配置合理性（max > 0, max >= min）
    /// - 数据库超时配置有效性
    /// - Email 配置有效性（SMTP 主机、端口、发件人地址）
    /// - JWT 密钥安全性（生产环境禁止使用默认值）
    /// - JWT 密钥长度警告
    /// - JWT 过期时间有效性
    /// - JWT 签发者有效性
    /// - 分销配置业务约束
    /// - 加密密钥配置提醒
    /// - Redis 配置验证（如果已配置）
    /// - Gateway 超时配置警告
    /// - Gateway 重试策略验证
    /// - Gateway 最大重试次数警告
    pub fn validate(&self) -> Result<(), ConfigLoadError> {
        // 验证服务器配置
        if self.server.bind_addr.is_empty() {
            return Err(ConfigLoadError::ValidationError(
                "服务器绑定地址不能为空".to_string(),
            ));
        }

        // 验证服务器端口（有效范围 1-65535）
        if self.server.port == 0 {
            return Err(ConfigLoadError::ValidationError(
                "服务器端口不能为 0".to_string(),
            ));
        }
        // 注意：u16 类型自动保证端口 <= 65535，无需额外检查

        // 验证数据库 URL
        if self.database.url.is_empty() {
            return Err(ConfigLoadError::ValidationError(
                "数据库 URL 不能为空".to_string(),
            ));
        }

        // 验证数据库连接池配置
        if self.database.max_connections == 0 {
            return Err(ConfigLoadError::ValidationError(
                "数据库最大连接数不能为 0".to_string(),
            ));
        }

        if self.database.max_connections < self.database.min_connections {
            return Err(ConfigLoadError::ValidationError(
                "数据库最大连接数不能小于最小连接数".to_string(),
            ));
        }

        // 数据库超时配置检查
        if self.database.connect_timeout_secs == 0 {
            return Err(ConfigLoadError::ValidationError(
                "数据库连接超时不能为 0".to_string(),
            ));
        }

        if self.database.idle_timeout_secs == 0 {
            tracing::warn!("⚠️  数据库空闲超时设置为 0，连接将永不过期");
        }

        if self.database.max_lifetime_secs == 0 {
            tracing::warn!("⚠️  数据库连接最大生命周期设置为 0，连接将永不过期");
        }

        // JWT 密钥安全检查
        if self.auth.jwt_secret == DEFAULT_JWT_SECRET {
            tracing::warn!(
                "⚠️  安全警告: JWT 密钥使用默认值，生产环境必须修改！请设置 KC__AUTH__JWT_SECRET 环境变量"
            );
            // 生产环境强制报错
            #[cfg(not(debug_assertions))]
            return Err(ConfigLoadError::ValidationError(
                "生产环境禁止使用默认 JWT 密钥，请设置 KC__AUTH__JWT_SECRET 环境变量".to_string(),
            ));
        }

        // JWT 密钥长度检查（排除默认密钥，避免重复警告）
        if self.auth.jwt_secret != DEFAULT_JWT_SECRET && self.auth.jwt_secret.len() < 32 {
            tracing::warn!("⚠️  安全警告: JWT 密钥长度不足 32 字符，建议使用更长的密钥");
        }

        // JWT 过期时间验证
        if self.auth.jwt_expiry_secs == 0 {
            return Err(ConfigLoadError::ValidationError(
                "JWT 过期时间不能为 0".to_string(),
            ));
        }

        if self.auth.jwt_expiry_secs > 86400 * 30 {
            // 超过 30 天
            tracing::warn!(
                "⚠️  JWT 过期时间设置为 {} 秒（超过 30 天），请确认是否符合安全策略",
                self.auth.jwt_expiry_secs
            );
        }

        // JWT 签发者验证
        if self.auth.jwt_issuer.is_empty() {
            return Err(ConfigLoadError::ValidationError(
                "JWT 签发者不能为空".to_string(),
            ));
        }

        // 数据库连接检查
        if self.database.url.contains("localhost") || self.database.url.contains("127.0.0.1") {
            tracing::debug!("数据库连接到本地地址，请确认生产环境配置正确");
        }

        // 分销配置验证
        if let Err(e) = self.distribution.validate() {
            return Err(ConfigLoadError::ValidationError(e));
        }

        let email_is_configured = self.email.is_configured();
        let email_is_partially_configured = self.email.is_partially_configured();

        // Email 配置检查
        if email_is_configured || email_is_partially_configured {
            if self.email.smtp_port == 0 {
                return Err(ConfigLoadError::ValidationError(
                    "SMTP 端口不能为 0".to_string(),
                ));
            }

            if self.email.smtp_host.trim().is_empty() {
                return Err(ConfigLoadError::ValidationError(
                    "SMTP 主机地址不能为空".to_string(),
                ));
            }

            if self.email.smtp_username.trim().is_empty() {
                return Err(ConfigLoadError::ValidationError(
                    "SMTP 用户名不能为空".to_string(),
                ));
            }

            if self.email.smtp_password.trim().is_empty() {
                return Err(ConfigLoadError::ValidationError(
                    "SMTP 密码不能为空".to_string(),
                ));
            }

            if self.email.from_address.trim().is_empty() {
                return Err(ConfigLoadError::ValidationError(
                    "Email 发件人地址不能为空".to_string(),
                ));
            }

            // 简单的邮箱格式验证
            if !self.email.from_address.contains('@') {
                tracing::warn!(
                    "⚠️  Email 发件人地址 '{}' 格式可能不正确，缺少 @ 符号",
                    self.email.from_address
                );
            }

            if self.email.timeout_secs == 0 {
                tracing::warn!("⚠️  Email 发送超时设置为 0，将禁用 SMTP 超时");
            }
        }

        let resolved_app_base_url = self.resolved_app_base_url();
        Self::validate_public_app_base_url(&resolved_app_base_url)
            .map_err(ConfigLoadError::ValidationError)?;

        if self.app_base_url.is_none() {
            tracing::info!(
                "💡 提示: 未显式配置 APP_BASE_URL，已回退为 {}",
                resolved_app_base_url
            );
        }

        if email_is_configured && self.app_base_url.is_none() {
            tracing::info!(
                "💡 Email 服务将使用默认公开地址 {} 生成链接",
                resolved_app_base_url
            );
        }

        // 加密配置提醒
        let has_crypto_key = self.crypto.as_ref().map(|c| c.has_key()).unwrap_or(false);
        if !has_crypto_key {
            tracing::info!(
                "💡 提示: 未配置加密密钥，Provider API Key 将明文存储。建议设置 KC__CRYPTO__SECRET_KEY"
            );
        }

        // Redis 配置检查
        if let Some(ref redis_config) = self.redis {
            // Redis 已配置，验证配置有效性
            if redis_config.url.is_empty() {
                return Err(ConfigLoadError::ValidationError(
                    "Redis URL 不能为空".to_string(),
                ));
            }
            if let Some(pool_size) = redis_config.pool_size
                && pool_size == 0
            {
                tracing::warn!("⚠️  Redis 连接池大小设置为 0，将使用默认值");
            }
        } else {
            tracing::info!("💡 提示: 未配置 Redis，分布式限流功能将不可用");
        }

        // Gateway 超时配置检查
        // 注意：timeout_secs=0 在 reqwest 中会立即超时（Duration::ZERO），
        // 导致所有请求失败，这几乎肯定是配置错误
        if self.gateway.timeout_secs == 0 {
            tracing::warn!("⚠️  Gateway 超时时间设置为 0，请求会立即超时失败！请检查配置");
        }

        // 检查 HTTP 请求超时
        if self.gateway.request_timeout_secs == 0 {
            tracing::warn!("⚠️  Gateway HTTP 请求超时设置为 0，非流式请求会立即失败！");
        }

        // 检查流式请求超时
        if self.gateway.stream_timeout_secs == 0 {
            tracing::warn!("⚠️  Gateway 流式请求超时设置为 0，流式请求会立即失败！");
        }

        if self.gateway.max_retries == 0 {
            tracing::warn!("⚠️  Gateway 最大重试次数设置为 0，请求失败时将不会重试");
        }

        if self.gateway.max_retries > 10 {
            tracing::warn!(
                "⚠️  Gateway 最大重试次数设置为 {}，可能导致请求延迟过高",
                self.gateway.max_retries
            );
        }

        // 重试策略验证
        // 先检查无效值（<= 0），再检查警告值（< 1.0）
        if self.gateway.retry.backoff_multiplier <= 0.0 {
            return Err(ConfigLoadError::ValidationError(format!(
                "Gateway 重试退避倍数必须大于 0，当前值为 {}",
                self.gateway.retry.backoff_multiplier
            )));
        }

        if self.gateway.retry.backoff_multiplier < 1.0 {
            tracing::warn!(
                "⚠️  Gateway 重试退避倍数 {} 小于 1.0，退避时间会递减！",
                self.gateway.retry.backoff_multiplier
            );
        }

        if self.gateway.retry.initial_backoff_ms > self.gateway.retry.max_backoff_ms {
            return Err(ConfigLoadError::ValidationError(
                "Gateway 重试初始退避时间不能大于最大退避时间".to_string(),
            ));
        }

        tracing::info!("配置验证通过");
        Ok(())
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
        assert_eq!(config.app_base_url.as_deref(), Some("http://localhost:80"));
    }

    #[test]
    #[serial]
    fn test_config_from_env() {
        // 注意：这个测试会读取实际的环境变量
        // 使用 unsafe 因为 set_var/remove_var 在 Rust 2024 中是 unsafe
        unsafe {
            std::env::set_var("KC__SERVER__PORT", "8080");
            std::env::set_var("APP_BASE_URL", "http://localhost");
            std::env::set_var("KC__EMAIL__SMTP_HOST", "localhost");
            std::env::set_var("KC__EMAIL__SMTP_USERNAME", "test");
            std::env::set_var("KC__EMAIL__SMTP_PASSWORD", "test");
            std::env::set_var("KC__EMAIL__FROM_ADDRESS", "test@localhost");
        }

        let config = AppConfig::from_env().expect("应该从环境变量加载配置");
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.app_base_url.as_deref(), Some("http://localhost"));

        // 清理
        unsafe {
            std::env::remove_var("KC__SERVER__PORT");
            std::env::remove_var("APP_BASE_URL");
            std::env::remove_var("KC__EMAIL__SMTP_HOST");
            std::env::remove_var("KC__EMAIL__SMTP_USERNAME");
            std::env::remove_var("KC__EMAIL__SMTP_PASSWORD");
            std::env::remove_var("KC__EMAIL__FROM_ADDRESS");
        }
    }

    #[test]
    #[serial]
    fn test_crypto_config_from_env() {
        // 设置 crypto 和 email 环境变量
        unsafe {
            std::env::set_var("KC__CRYPTO__SECRET_KEY", "dGVzdC1rZXktZnJvbS1lbnY=");
            std::env::set_var("APP_BASE_URL", "http://localhost");
            std::env::set_var("KC__EMAIL__SMTP_HOST", "localhost");
            std::env::set_var("KC__EMAIL__SMTP_USERNAME", "test");
            std::env::set_var("KC__EMAIL__SMTP_PASSWORD", "test");
            std::env::set_var("KC__EMAIL__FROM_ADDRESS", "test@localhost");
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
            std::env::remove_var("APP_BASE_URL");
            std::env::remove_var("KC__EMAIL__SMTP_HOST");
            std::env::remove_var("KC__EMAIL__SMTP_USERNAME");
            std::env::remove_var("KC__EMAIL__SMTP_PASSWORD");
            std::env::remove_var("KC__EMAIL__FROM_ADDRESS");
        }
    }

    #[test]
    fn test_validate_port_zero() {
        let mut config = AppConfig::default();
        config.server.port = 0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("端口"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_empty_database_url() {
        let mut config = AppConfig::default();
        config.database.url = "".to_string();
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("数据库 URL"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_database_pool_config() {
        let mut config = AppConfig::default();
        config.database.max_connections = 1;
        config.database.min_connections = 5;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("最大连接数"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_database_connect_timeout_zero() {
        // 数据库连接超时为 0 应该报错
        let mut config = AppConfig::default();
        config.database.connect_timeout_secs = 0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("连接超时"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_gateway_timeout_zero() {
        // 超时时间为 0 会立即超时，但现在只警告不报错
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.gateway.timeout_secs = 0;
        let result = config.validate();
        // 应该通过验证，但会有警告日志
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_jwt_short_key() {
        // 短 JWT 密钥应该触发警告（但不报错）
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "short-key".to_string(); // 9 字符，< 32
        let result = config.validate();
        // 应该通过验证，但会有警告日志
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_distribution_config() {
        let config = AppConfig {
            distribution: DistributionConfig {
                default_level1_ratio: 0.5,
                default_level2_ratio: 0.5,
                max_total_ratio: 0.3,
            },
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("分销比例"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_valid_config() {
        let mut config = AppConfig::default();
        // 设置非默认的 JWT 密钥避免警告
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_app_base_url_falls_back_when_email_enabled() {
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.app_base_url = None;
        config.email.smtp_host = "localhost".to_string();
        config.email.smtp_username = "mailer".to_string();
        config.email.smtp_password = "secret".to_string();
        config.email.from_address = "noreply@example.com".to_string();

        let result = config.validate();
        assert!(result.is_ok());
        assert_eq!(config.resolved_app_base_url(), "http://localhost:80");
    }

    #[test]
    fn test_resolved_app_base_url_falls_back_to_fixed_localhost_80() {
        let config = AppConfig {
            app_base_url: None,
            server: ServerConfig {
                port: 8088,
                ..Default::default()
            },
            ..Default::default()
        };

        assert_eq!(config.resolved_app_base_url(), "http://localhost:80");
    }

    #[test]
    fn test_validate_app_base_url_requires_supported_scheme() {
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.app_base_url = Some("ftp://example.com".to_string());

        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("http/https"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_app_base_url_requires_https_for_non_local_hosts() {
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.app_base_url = Some("http://example.com".to_string());

        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("https"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_app_base_url_accepts_local_http() {
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.app_base_url = Some("http://localhost:3000/base/".to_string());

        let result = config.validate();
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_gateway_retry_backoff_invalid() {
        // 重试初始退避时间大于最大退避时间应该报错
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.gateway.retry.initial_backoff_ms = 5000;
        config.gateway.retry.max_backoff_ms = 1000;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("初始退避时间"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_gateway_backoff_multiplier_zero() {
        // 重试退避倍数为 0 应该报错
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.gateway.retry.backoff_multiplier = 0.0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("退避倍数") && msg.contains("大于 0"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_gateway_backoff_multiplier_negative() {
        // 重试退避倍数为负数应该报错
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.gateway.retry.backoff_multiplier = -1.0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("退避倍数") && msg.contains("大于 0"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_database_max_connections_zero() {
        // 数据库最大连接数为 0 应该报错
        let mut config = AppConfig::default();
        config.database.max_connections = 0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("最大连接数"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_smtp_port_zero() {
        // SMTP 端口为 0 应该报错
        let mut config = AppConfig::default();
        config.email.smtp_port = 0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("SMTP 端口"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_redis_url_empty() {
        // Redis URL 为空应该报错
        let config = AppConfig {
            redis: Some(RedisConfig {
                url: "".to_string(),
                key_prefix: None,
                pool_size: Some(10),
                connect_timeout_secs: Some(5),
            }),
            ..Default::default()
        };
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("Redis URL"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_jwt_expiry_zero() {
        // JWT 过期时间为 0 应该报错
        let mut config = AppConfig::default();
        config.auth.jwt_expiry_secs = 0;
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("JWT 过期时间"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_gateway_max_retries_zero() {
        // 最大重试次数为 0 应该警告但不报错
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.gateway.max_retries = 0;
        let result = config.validate();
        // 应该通过验证，但会有警告日志
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_bind_addr_empty() {
        // 服务器绑定地址为空应该报错
        let mut config = AppConfig::default();
        config.server.bind_addr = "".to_string();
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("绑定地址"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_email_from_address_empty() {
        // Email 发件人地址为空应该报错
        let mut config = AppConfig::default();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_username = "mailer".to_string();
        config.email.smtp_password = "secret".to_string();
        config.email.from_address = "".to_string();
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("发件人地址"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_email_from_address_invalid() {
        // Email 发件人地址缺少 @ 符号应该警告但不报错
        let mut config = AppConfig::default();
        config.auth.jwt_secret = "a-very-secure-jwt-secret-key-for-testing".to_string();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_username = "mailer".to_string();
        config.email.smtp_password = "secret".to_string();
        config.email.from_address = "invalid-email".to_string();
        config.app_base_url = Some("https://app.example.com".to_string());
        let result = config.validate();
        // 应该通过验证，但会有警告日志
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_jwt_issuer_empty() {
        // JWT 签发者为空应该报错
        let mut config = AppConfig::default();
        config.auth.jwt_issuer = "".to_string();
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("签发者"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_smtp_host_empty() {
        // SMTP 主机为空应该报错
        let mut config = AppConfig::default();
        config.email.smtp_host = "".to_string();
        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("SMTP 主机"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }

    #[test]
    fn test_validate_email_whitespace_username_rejected() {
        let mut config = AppConfig::default();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_username = "   ".to_string();
        config.email.smtp_password = "secret".to_string();
        config.email.from_address = "noreply@example.com".to_string();

        let result = config.validate();
        assert!(result.is_err());
        match result {
            Err(ConfigLoadError::ValidationError(msg)) => {
                assert!(msg.contains("SMTP 用户名"));
            }
            _ => panic!("期望 ValidationError"),
        }
    }
}
