//! 客户端配置
//!
//! 配置 HTTP 客户端的基本参数，如基础 URL、超时时间等

use crate::error::{ClientError, Result};

/// 客户端配置
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// API 基础 URL
    pub base_url: String,
    /// 请求超时时间（秒）
    pub timeout_secs: u64,
    /// 是否启用请求重试
    pub retry_enabled: bool,
    /// 最大重试次数
    pub max_retries: u32,
}

impl ClientConfig {
    /// 创建新的配置
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            timeout_secs: 30,
            retry_enabled: true,
            max_retries: 3,
        }
    }

    /// 设置超时时间
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// 设置是否启用重试
    pub fn with_retry(mut self, enabled: bool) -> Self {
        self.retry_enabled = enabled;
        self
    }

    /// 设置最大重试次数
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// 验证配置是否有效
    pub fn validate(&self) -> Result<()> {
        if self.base_url.is_empty() {
            return Err(ClientError::Config("Base URL cannot be empty".to_string()));
        }
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err(ClientError::Config(
                "Base URL must start with http:// or https://".to_string(),
            ));
        }
        if self.timeout_secs == 0 {
            return Err(ClientError::Config(
                "Timeout must be greater than 0".to_string(),
            ));
        }
        Ok(())
    }

    /// 构建完整 URL
    pub fn build_url(&self, path: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        format!("{}/{}", base, path)
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8080".to_string(),
            timeout_secs: 30,
            retry_enabled: true,
            max_retries: 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_validation() {
        // 有效配置
        let config = ClientConfig::new("http://localhost:8080");
        assert!(config.validate().is_ok());

        // 无效配置：空 URL
        let config = ClientConfig::new("");
        assert!(config.validate().is_err());

        // 无效配置：错误协议
        let config = ClientConfig::new("ftp://localhost");
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_build_url() {
        let config = ClientConfig::new("http://localhost:8080");
        assert_eq!(
            config.build_url("/api/v1/users"),
            "http://localhost:8080/api/v1/users"
        );
        assert_eq!(
            config.build_url("api/v1/users"),
            "http://localhost:8080/api/v1/users"
        );

        let config = ClientConfig::new("http://localhost:8080/");
        assert_eq!(
            config.build_url("/api/v1/users"),
            "http://localhost:8080/api/v1/users"
        );
    }
}
