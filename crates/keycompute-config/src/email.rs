//! 邮件服务配置
//!
//! 提供 SMTP 邮件发送配置：
//! - SMTP 服务器连接参数
//! - 发件人信息
//! - TLS 配置
//! - 验证链接基础 URL

use serde::Deserialize;

/// 邮件服务配置
#[derive(Debug, Deserialize, Clone)]
pub struct EmailConfig {
    /// SMTP 服务器地址
    pub smtp_host: String,
    /// SMTP 端口（默认 587）
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// SMTP 用户名
    pub smtp_username: String,
    /// SMTP 密码
    pub smtp_password: String,
    /// 发件人邮箱地址
    pub from_address: String,
    /// 发件人显示名称（可选）
    pub from_name: Option<String>,
    /// 是否使用 TLS（默认 true）
    #[serde(default = "default_use_tls")]
    pub use_tls: bool,
    /// 验证链接基础 URL（如 https://api.example.com）
    pub verification_base_url: String,
    /// 邮件发送超时（秒，默认 30）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_smtp_port() -> u16 {
    587
}

fn default_use_tls() -> bool {
    true
}

fn default_timeout_secs() -> u64 {
    30
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            smtp_host: "localhost".to_string(),
            smtp_port: 587,
            smtp_username: String::new(),
            smtp_password: String::new(),
            from_address: "noreply@localhost".to_string(),
            from_name: Some("KeyCompute".to_string()),
            use_tls: true,
            verification_base_url: "http://localhost:3000".to_string(),
            timeout_secs: 30,
        }
    }
}

impl EmailConfig {
    /// 检查配置是否有效（非默认值）
    pub fn is_configured(&self) -> bool {
        !self.smtp_host.is_empty()
            && self.smtp_host != "localhost"
            && !self.smtp_username.is_empty()
            && !self.smtp_password.is_empty()
    }

    /// 获取完整的发件人地址（带名称）
    pub fn from_header(&self) -> String {
        match &self.from_name {
            Some(name) => format!("{} <{}>", name, self.from_address),
            None => self.from_address.clone(),
        }
    }

    /// 生成邮箱验证链接
    pub fn verification_url(&self, token: &str) -> String {
        format!("{}/auth/verify-email/{}", self.verification_base_url, token)
    }

    /// 生成密码重置链接
    pub fn password_reset_url(&self, token: &str) -> String {
        format!(
            "{}/auth/reset-password?token={}",
            self.verification_base_url, token
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_email_config() {
        let config = EmailConfig::default();
        assert_eq!(config.smtp_port, 587);
        assert!(config.use_tls);
        assert_eq!(config.timeout_secs, 30);
    }

    #[test]
    fn test_from_header_with_name() {
        let config = EmailConfig {
            from_address: "noreply@example.com".to_string(),
            from_name: Some("KeyCompute".to_string()),
            ..Default::default()
        };
        assert_eq!(config.from_header(), "KeyCompute <noreply@example.com>");
    }

    #[test]
    fn test_from_header_without_name() {
        let config = EmailConfig {
            from_address: "noreply@example.com".to_string(),
            from_name: None,
            ..Default::default()
        };
        assert_eq!(config.from_header(), "noreply@example.com");
    }

    #[test]
    fn test_verification_url() {
        let config = EmailConfig {
            verification_base_url: "https://api.example.com".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config.verification_url("abc123"),
            "https://api.example.com/auth/verify-email/abc123"
        );
    }

    #[test]
    fn test_password_reset_url() {
        let config = EmailConfig {
            verification_base_url: "https://api.example.com".to_string(),
            ..Default::default()
        };
        assert_eq!(
            config.password_reset_url("reset456"),
            "https://api.example.com/auth/reset-password?token=reset456"
        );
    }

    #[test]
    fn test_is_configured() {
        let default_config = EmailConfig::default();
        assert!(!default_config.is_configured());

        let configured = EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_username: "user".to_string(),
            smtp_password: "pass".to_string(),
            ..Default::default()
        };
        assert!(configured.is_configured());
    }
}
