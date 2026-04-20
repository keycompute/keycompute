//! 模拟邮件服务
//!
//! 用于测试邮件发送功能，无需真实 SMTP 服务器

use keycompute_emailserver::EmailError;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const DEFAULT_PUBLIC_APP_BASE_URL: &str = "http://localhost:3000";

/// 模拟邮件记录
#[derive(Debug, Clone)]
pub struct MockEmailRecord {
    /// 邮件ID
    pub id: Uuid,
    /// 收件人
    pub to: String,
    /// 邮件主题
    pub subject: String,
    /// 邮件正文（文本）
    pub text_body: String,
    /// 邮件正文（HTML）
    pub html_body: Option<String>,
    /// 邮件类型
    pub email_type: MockEmailType,
    /// 令牌（用于验证/重置邮件）
    pub token: Option<String>,
    /// 发送时间
    pub sent_at: chrono::DateTime<chrono::Utc>,
}

/// 邮件类型
#[derive(Debug, Clone, PartialEq)]
pub enum MockEmailType {
    /// 邮箱验证邮件
    Verification,
    /// 密码重置邮件
    PasswordReset,
    /// 欢迎邮件
    Welcome,
    /// 普通邮件
    Generic,
}

/// 模拟邮件服务
#[derive(Clone)]
pub struct MockEmailService {
    /// 发送的邮件记录
    records: Arc<Mutex<Vec<MockEmailRecord>>>,
    /// 是否模拟发送失败
    should_fail: Arc<Mutex<bool>>,
}

impl MockEmailService {
    /// 创建新的模拟邮件服务
    pub fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
            should_fail: Arc::new(Mutex::new(false)),
        }
    }

    /// 设置是否模拟发送失败
    pub fn set_should_fail(&self, fail: bool) {
        *self.should_fail.lock().unwrap() = fail;
    }

    fn legacy_verification_url(&self, token: &str) -> String {
        format!(
            "{}/auth/verify-email/{}",
            DEFAULT_PUBLIC_APP_BASE_URL, token
        )
    }

    /// 发送验证邮件
    pub async fn send_verification_email(&self, to: &str, token: &str) -> Result<(), EmailError> {
        if *self.should_fail.lock().unwrap() {
            return Err(EmailError::SendError("Mock send failed".to_string()));
        }

        let record = MockEmailRecord {
            id: Uuid::new_v4(),
            to: to.to_string(),
            subject: "请验证您的邮箱地址".to_string(),
            text_body: format!("验证链接: {}", self.legacy_verification_url(token)),
            html_body: Some(format!(
                "<a href=\"{}\">验证邮箱</a>",
                self.legacy_verification_url(token)
            )),
            email_type: MockEmailType::Verification,
            token: Some(token.to_string()),
            sent_at: chrono::Utc::now(),
        };

        self.records.lock().unwrap().push(record);
        Ok(())
    }

    /// 发送密码重置邮件
    pub async fn send_password_reset_email(
        &self,
        to: &str,
        token: &str,
        app_base_url: &str,
    ) -> Result<(), EmailError> {
        if *self.should_fail.lock().unwrap() {
            return Err(EmailError::SendError("Mock send failed".to_string()));
        }

        let reset_url = format!(
            "{}/auth/reset-password/{}",
            app_base_url.trim().trim_end_matches('/'),
            token
        );

        let record = MockEmailRecord {
            id: Uuid::new_v4(),
            to: to.to_string(),
            subject: "重置您的密码".to_string(),
            text_body: format!("重置链接: {}", reset_url),
            html_body: Some(format!("<a href=\"{}\">重置密码</a>", reset_url)),
            email_type: MockEmailType::PasswordReset,
            token: Some(token.to_string()),
            sent_at: chrono::Utc::now(),
        };

        self.records.lock().unwrap().push(record);
        Ok(())
    }

    /// 发送欢迎邮件
    pub async fn send_welcome_email(&self, to: &str, name: Option<&str>) -> Result<(), EmailError> {
        if *self.should_fail.lock().unwrap() {
            return Err(EmailError::SendError("Mock send failed".to_string()));
        }

        let greeting = name.unwrap_or("新用户");
        let record = MockEmailRecord {
            id: Uuid::new_v4(),
            to: to.to_string(),
            subject: "欢迎加入 KeyCompute".to_string(),
            text_body: format!("您好 {}！欢迎加入 KeyCompute。", greeting),
            html_body: Some(format!("<h1>欢迎 {}!</h1>", greeting)),
            email_type: MockEmailType::Welcome,
            token: None,
            sent_at: chrono::Utc::now(),
        };

        self.records.lock().unwrap().push(record);
        Ok(())
    }

    /// 获取所有发送的邮件
    pub fn get_sent_emails(&self) -> Vec<MockEmailRecord> {
        self.records.lock().unwrap().clone()
    }

    /// 获取指定类型的邮件
    pub fn get_emails_by_type(&self, email_type: MockEmailType) -> Vec<MockEmailRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.email_type == email_type)
            .cloned()
            .collect()
    }

    /// 获取发送给指定邮箱的邮件
    pub fn get_emails_to(&self, email: &str) -> Vec<MockEmailRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.to == email)
            .cloned()
            .collect()
    }

    /// 获取包含指定令牌的邮件
    pub fn get_email_by_token(&self, token: &str) -> Option<MockEmailRecord> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.token.as_deref() == Some(token))
            .cloned()
    }

    /// 清空邮件记录
    pub fn clear_records(&self) {
        self.records.lock().unwrap().clear();
    }

    /// 获取邮件数量
    pub fn email_count(&self) -> usize {
        self.records.lock().unwrap().len()
    }

    /// 检查是否发送了验证邮件
    pub fn has_verification_email(&self, email: &str) -> bool {
        self.records
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.to == email && r.email_type == MockEmailType::Verification)
    }

    /// 检查是否发送了密码重置邮件
    pub fn has_password_reset_email(&self, email: &str) -> bool {
        self.records
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.to == email && r.email_type == MockEmailType::PasswordReset)
    }

    /// 检查是否发送了欢迎邮件
    pub fn has_welcome_email(&self, email: &str) -> bool {
        self.records
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.to == email && r.email_type == MockEmailType::Welcome)
    }
}

impl Default for MockEmailService {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MockEmailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockEmailService")
            .field("email_count", &self.email_count())
            .field("should_fail", &*self.should_fail.lock().unwrap())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_email_service_basic() {
        let service = MockEmailService::new();

        // 发送验证邮件
        service
            .send_verification_email("test@example.com", "token123")
            .await
            .unwrap();

        assert_eq!(service.email_count(), 1);
        assert!(service.has_verification_email("test@example.com"));

        // 发送密码重置邮件
        service
            .send_password_reset_email("test@example.com", "reset456", DEFAULT_PUBLIC_APP_BASE_URL)
            .await
            .unwrap();

        assert_eq!(service.email_count(), 2);
        assert!(service.has_password_reset_email("test@example.com"));
    }

    #[tokio::test]
    async fn test_mock_email_service_failure() {
        let service = MockEmailService::new();
        service.set_should_fail(true);

        let result = service
            .send_verification_email("test@example.com", "token123")
            .await;

        assert!(result.is_err());
        assert_eq!(service.email_count(), 0);
    }

    #[tokio::test]
    async fn test_mock_email_service_get_by_token() {
        let service = MockEmailService::new();

        service
            .send_verification_email("test@example.com", "token123")
            .await
            .unwrap();

        let email = service.get_email_by_token("token123");
        assert!(email.is_some());
        assert_eq!(email.unwrap().to, "test@example.com");
    }
}
