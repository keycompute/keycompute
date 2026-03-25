//! 邮件服务模块
//!
//! 提供 SMTP 邮件发送功能：
//! - 邮箱验证邮件
//! - 密码重置邮件
//! - 通用邮件发送
//!
//! # 配置
//!
//! 通过 `keycompute-config` 模块加载配置：
//! - 环境变量：`KC__EMAIL__SMTP_HOST`、`KC__EMAIL__SMTP_PORT` 等
//! - 配置文件：`config.toml` 中的 `[email]` 部分

use keycompute_config::EmailConfig;
use keycompute_types::KeyComputeError;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::authentication::Credentials,
};
use std::sync::Arc;

/// 邮件发送错误
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    /// 配置错误
    #[error("邮件服务未配置")]
    NotConfigured,

    /// 邮箱地址格式错误
    #[error("无效的邮箱地址: {0}")]
    InvalidAddress(String),

    /// 邮件构建错误
    #[error("邮件构建失败: {0}")]
    BuildError(String),

    /// SMTP 发送错误
    #[error("邮件发送失败: {0}")]
    SendError(String),
}

impl From<EmailError> for KeyComputeError {
    fn from(err: EmailError) -> Self {
        KeyComputeError::Internal(err.to_string())
    }
}

/// 邮件服务
#[derive(Clone)]
pub struct EmailService {
    config: EmailConfig,
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
}

impl EmailService {
    /// 创建邮件服务实例
    pub fn new(config: EmailConfig) -> Self {
        let transport = Self::build_transport(&config);
        Self { config, transport }
    }

    /// 从 Arc 创建（方便集成）
    pub fn from_arc(config: Arc<EmailConfig>) -> Self {
        Self::new((*config).clone())
    }

    /// 构建 SMTP 传输
    fn build_transport(config: &EmailConfig) -> Option<AsyncSmtpTransport<Tokio1Executor>> {
        if !config.is_configured() {
            tracing::warn!("邮件服务未配置，邮件发送将被禁用");
            return None;
        }

        let creds = Credentials::new(config.smtp_username.clone(), config.smtp_password.clone());

        let transport = if config.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
                .ok()?
                .credentials(creds)
                .port(config.smtp_port)
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.smtp_host)
                .credentials(creds)
                .port(config.smtp_port)
                .build()
        };

        Some(transport)
    }

    /// 检查服务是否已配置
    pub fn is_configured(&self) -> bool {
        self.transport.is_some()
    }

    /// 发送邮箱验证邮件
    pub async fn send_verification_email(&self, to: &str, token: &str) -> Result<(), EmailError> {
        let verification_url = self.config.verification_url(token);

        let subject = "请验证您的邮箱地址";
        let text_body = format!(
            r#"您好！

感谢您注册 KeyCompute。

请点击以下链接验证您的邮箱地址：
{}

此链接将在 24 小时后过期。

如果您没有注册 KeyCompute 账户，请忽略此邮件。

祝好，
KeyCompute 团队
"#,
            verification_url
        );

        let html_body = format!(
            r#"<html>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
<div style="max-width: 600px; margin: 0 auto; padding: 20px;">
<h2 style="color: #2c5282;">请验证您的邮箱地址</h2>
<p>您好！</p>
<p>感谢您注册 KeyCompute。</p>
<p>请点击以下按钮验证您的邮箱地址：</p>
<p>
<a href="{}" style="display: inline-block; padding: 12px 24px; background-color: #4299e1; color: white; text-decoration: none; border-radius: 4px;">
验证邮箱
</a>
</p>
<p>或复制以下链接到浏览器：<br><code style="word-break: break-all;">{}</code></p>
<p style="color: #718096; font-size: 14px;">此链接将在 24 小时后过期。</p>
<p style="color: #718096; font-size: 14px;">如果您没有注册 KeyCompute 账户，请忽略此邮件。</p>
<hr style="border: none; border-top: 1px solid #e2e8f0; margin: 20px 0;">
<p style="color: #718096; font-size: 12px;">KeyCompute 团队</p>
</div>
</body>
</html>"#,
            verification_url, verification_url
        );

        self.send_html_email(to, subject, &text_body, &html_body)
            .await
    }

    /// 发送密码重置邮件
    pub async fn send_password_reset_email(&self, to: &str, token: &str) -> Result<(), EmailError> {
        let reset_url = self.config.password_reset_url(token);

        let subject = "重置您的密码";
        let text_body = format!(
            r#"您好！

我们收到了重置您密码的请求。

请点击以下链接重置密码：
{}

此链接将在 1 小时后过期。

如果您没有请求重置密码，请忽略此邮件，您的密码不会改变。

祝好，
KeyCompute 团队
"#,
            reset_url
        );

        let html_body = format!(
            r#"<html>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
<div style="max-width: 600px; margin: 0 auto; padding: 20px;">
<h2 style="color: #2c5282;">重置您的密码</h2>
<p>您好！</p>
<p>我们收到了重置您密码的请求。</p>
<p>请点击以下按钮重置密码：</p>
<p>
<a href="{}" style="display: inline-block; padding: 12px 24px; background-color: #e53e3e; color: white; text-decoration: none; border-radius: 4px;">
重置密码
</a>
</p>
<p>或复制以下链接到浏览器：<br><code style="word-break: break-all;">{}</code></p>
<p style="color: #718096; font-size: 14px;">此链接将在 1 小时后过期。</p>
<p style="color: #718096; font-size: 14px;">如果您没有请求重置密码，请忽略此邮件，您的密码不会改变。</p>
<hr style="border: none; border-top: 1px solid #e2e8f0; margin: 20px 0;">
<p style="color: #718096; font-size: 12px;">KeyCompute 团队</p>
</div>
</body>
</html>"#,
            reset_url, reset_url
        );

        self.send_html_email(to, subject, &text_body, &html_body)
            .await
    }

    /// 发送欢迎邮件（邮箱验证成功后）
    pub async fn send_welcome_email(&self, to: &str, name: Option<&str>) -> Result<(), EmailError> {
        let greeting = name
            .map(|n| format!("{}！", n))
            .unwrap_or_else(|| "！".to_string());

        let subject = "欢迎加入 KeyCompute";
        let text_body = format!(
            r#"您好{}！

恭喜您成功验证了邮箱地址。

现在您可以开始使用 KeyCompute 的全部功能：
• 创建和管理 API Key
• 配置 LLM Provider
• 监控使用量和费用

如果您有任何问题，请随时联系我们的支持团队。

祝好，
KeyCompute 团队
"#,
            greeting
        );

        let html_body = format!(
            r#"<html>
<body style="font-family: Arial, sans-serif; line-height: 1.6; color: #333;">
<div style="max-width: 600px; margin: 0 auto; padding: 20px;">
<h2 style="color: #2c5282;">欢迎加入 KeyCompute</h2>
<p>您好{}！</p>
<p>恭喜您成功验证了邮箱地址。</p>
<p>现在您可以开始使用 KeyCompute 的全部功能：</p>
<ul>
<li>创建和管理 API Key</li>
<li>配置 LLM Provider</li>
<li>监控使用量和费用</li>
</ul>
<p>如果您有任何问题，请随时联系我们的支持团队。</p>
<hr style="border: none; border-top: 1px solid #e2e8f0; margin: 20px 0;">
<p style="color: #718096; font-size: 12px;">KeyCompute 团队</p>
</div>
</body>
</html>"#,
            greeting
        );

        self.send_html_email(to, subject, &text_body, &html_body)
            .await
    }

    /// 发送纯文本邮件
    pub async fn send_text_email(
        &self,
        to: &str,
        subject: &str,
        body: &str,
    ) -> Result<(), EmailError> {
        let transport = self.transport.as_ref().ok_or(EmailError::NotConfigured)?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|_| EmailError::InvalidAddress(to.to_string()))?;

        let from_mailbox: Mailbox = self.config.from_address.parse().map_err(|_| {
            EmailError::BuildError(format!(
                "Invalid from address: {}",
                self.config.from_address
            ))
        })?;

        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject)
            .body(body.to_string())
            .map_err(|e| EmailError::BuildError(e.to_string()))?;

        transport
            .send(email)
            .await
            .map_err(|e| EmailError::SendError(e.to_string()))?;

        tracing::info!(
            to = %to,
            subject = %subject,
            "邮件发送成功"
        );

        Ok(())
    }

    /// 发送带 HTML 正文的多部分邮件
    pub async fn send_html_email(
        &self,
        to: &str,
        subject: &str,
        text_body: &str,
        html_body: &str,
    ) -> Result<(), EmailError> {
        let transport = self.transport.as_ref().ok_or(EmailError::NotConfigured)?;

        let to_mailbox: Mailbox = to
            .parse()
            .map_err(|_| EmailError::InvalidAddress(to.to_string()))?;

        let from_mailbox: Mailbox = self.config.from_address.parse().map_err(|_| {
            EmailError::BuildError(format!(
                "Invalid from address: {}",
                self.config.from_address
            ))
        })?;

        let email = Message::builder()
            .from(from_mailbox)
            .to(to_mailbox)
            .subject(subject)
            .multipart(
                lettre::message::MultiPart::alternative()
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(text_body.to_string()),
                    )
                    .singlepart(
                        lettre::message::SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html_body.to_string()),
                    ),
            )
            .map_err(|e| EmailError::BuildError(e.to_string()))?;

        transport
            .send(email)
            .await
            .map_err(|e| EmailError::SendError(e.to_string()))?;

        tracing::info!(
            to = %to,
            subject = %subject,
            "HTML 邮件发送成功"
        );

        Ok(())
    }
}

impl std::fmt::Debug for EmailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailService")
            .field("configured", &self.is_configured())
            .field("smtp_host", &self.config.smtp_host)
            .field("from_address", &self.config.from_address)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EmailConfig {
        EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_username: "test@example.com".to_string(),
            smtp_password: "testpass".to_string(),
            from_address: "noreply@example.com".to_string(),
            from_name: Some("KeyCompute".to_string()),
            use_tls: true,
            verification_base_url: "https://api.example.com".to_string(),
            timeout_secs: 30,
        }
    }

    #[test]
    fn test_email_service_creation() {
        let service = EmailService::new(test_config());
        assert!(service.is_configured());
    }

    #[test]
    fn test_email_service_not_configured() {
        let service = EmailService::new(EmailConfig::default());
        assert!(!service.is_configured());
    }

    #[test]
    fn test_invalid_email_address() {
        let service = EmailService::new(test_config());

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            service
                .send_text_email("invalid-email", "Test", "Body")
                .await
        });

        assert!(matches!(result, Err(EmailError::InvalidAddress(_))));
    }

    #[test]
    fn test_send_without_config() {
        let service = EmailService::new(EmailConfig::default());

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            service
                .send_text_email("test@example.com", "Test", "Body")
                .await
        });

        assert!(matches!(result, Err(EmailError::NotConfigured)));
    }
}
