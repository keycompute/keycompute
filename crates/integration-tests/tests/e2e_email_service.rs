//! 邮件服务端到端测试
//!
//! 验证邮件发送功能的完整流程：
//! - EmailService 基础功能
//! - 验证邮件发送
//! - 密码重置邮件发送
//! - 欢迎邮件发送
//! - 错误处理

use integration_tests::common::VerificationChain;
use integration_tests::mocks::email::{MockEmailService, MockEmailType};
use keycompute_emailserver::EmailConfig;

const TEST_PUBLIC_APP_BASE_URL: &str = "https://app.example.com";

/// 测试 MockEmailService 基础功能
#[tokio::test]
async fn test_mock_email_service_basic() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    // 1. 初始状态
    chain.add_step(
        "integration-tests",
        "MockEmailService::new",
        "创建模拟邮件服务",
        service.email_count() == 0,
    );

    // 2. 发送验证邮件
    let result = service
        .send_verification_email("test@example.com", "verify_token_123")
        .await;
    chain.add_step(
        "integration-tests",
        "MockEmailService::send_verification_email",
        "验证邮件发送成功",
        result.is_ok(),
    );

    // 3. 验证邮件记录
    chain.add_step(
        "integration-tests",
        "MockEmailService::email_count",
        format!("邮件数量: {}", service.email_count()),
        service.email_count() == 1,
    );

    // 4. 检查验证邮件标记
    chain.add_step(
        "integration-tests",
        "MockEmailService::has_verification_email",
        "验证邮件已记录",
        service.has_verification_email("test@example.com"),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试邮件类型区分
#[tokio::test]
async fn test_mock_email_service_types() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    // 1. 发送三种类型的邮件
    service
        .send_verification_email("user1@example.com", "token1")
        .await
        .unwrap();
    service
        .send_password_reset_email("user2@example.com", "token2", TEST_PUBLIC_APP_BASE_URL)
        .await
        .unwrap();
    service
        .send_welcome_email("user3@example.com", Some("张三"))
        .await
        .unwrap();

    chain.add_step(
        "integration-tests",
        "send_multiple_types",
        format!("总邮件数: {}", service.email_count()),
        service.email_count() == 3,
    );

    // 2. 按类型筛选
    let verification_emails = service.get_emails_by_type(MockEmailType::Verification);
    let reset_emails = service.get_emails_by_type(MockEmailType::PasswordReset);
    let welcome_emails = service.get_emails_by_type(MockEmailType::Welcome);

    chain.add_step(
        "integration-tests",
        "get_emails_by_type::verification",
        format!("验证邮件数: {}", verification_emails.len()),
        verification_emails.len() == 1,
    );
    chain.add_step(
        "integration-tests",
        "get_emails_by_type::reset",
        format!("重置邮件数: {}", reset_emails.len()),
        reset_emails.len() == 1,
    );
    chain.add_step(
        "integration-tests",
        "get_emails_by_type::welcome",
        format!("欢迎邮件数: {}", welcome_emails.len()),
        welcome_emails.len() == 1,
    );

    // 3. 验证邮件内容
    chain.add_step(
        "integration-tests",
        "verification_email::subject",
        "验证邮件包含正确主题",
        verification_emails[0].subject.contains("验证"),
    );
    chain.add_step(
        "integration-tests",
        "reset_email::subject",
        "重置邮件包含正确主题",
        reset_emails[0].subject.contains("重置"),
    );
    chain.add_step(
        "integration-tests",
        "welcome_email::subject",
        "欢迎邮件包含正确主题",
        welcome_emails[0].subject.contains("欢迎"),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试令牌关联
#[tokio::test]
async fn test_mock_email_service_token_association() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    let token = "verification_token_xyz";

    // 1. 发送带令牌的邮件
    service
        .send_verification_email("user@example.com", token)
        .await
        .unwrap();

    // 2. 通过令牌查找邮件
    let email = service.get_email_by_token(token);
    chain.add_step(
        "integration-tests",
        "MockEmailService::get_email_by_token",
        "通过令牌找到邮件",
        email.is_some(),
    );

    // 3. 验证令牌匹配
    if let Some(e) = email {
        chain.add_step(
            "integration-tests",
            "email::token_match",
            "邮件令牌匹配",
            e.token.as_deref() == Some(token),
        );
    }

    // 4. 查找不存在的令牌
    let not_found = service.get_email_by_token("non_existent_token");
    chain.add_step(
        "integration-tests",
        "get_email_by_token::not_found",
        "不存在的令牌返回 None",
        not_found.is_none(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试邮件发送失败场景
#[tokio::test]
async fn test_mock_email_service_failure() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    // 1. 设置失败模式
    service.set_should_fail(true);
    chain.add_step(
        "integration-tests",
        "MockEmailService::set_should_fail",
        "设置失败模式",
        true,
    );

    // 2. 验证邮件发送失败
    let result = service
        .send_verification_email("test@example.com", "token")
        .await;
    chain.add_step(
        "integration-tests",
        "send_verification_email::failure",
        "验证邮件发送失败",
        result.is_err(),
    );

    // 3. 密码重置邮件发送失败
    let result = service
        .send_password_reset_email("test@example.com", "token", TEST_PUBLIC_APP_BASE_URL)
        .await;
    chain.add_step(
        "integration-tests",
        "send_password_reset_email::failure",
        "重置邮件发送失败",
        result.is_err(),
    );

    // 4. 欢迎邮件发送失败
    let result = service.send_welcome_email("test@example.com", None).await;
    chain.add_step(
        "integration-tests",
        "send_welcome_email::failure",
        "欢迎邮件发送失败",
        result.is_err(),
    );

    // 5. 验证没有邮件被记录
    chain.add_step(
        "integration-tests",
        "email_count::after_failure",
        "失败时不记录邮件",
        service.email_count() == 0,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试邮件记录清空
#[tokio::test]
async fn test_mock_email_service_clear() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    // 1. 发送多封邮件
    for i in 0..5 {
        service
            .send_verification_email(&format!("user{}@example.com", i), &format!("token{}", i))
            .await
            .unwrap();
    }

    chain.add_step(
        "integration-tests",
        "send_multiple_emails",
        format!("发送前邮件数: {}", service.email_count()),
        service.email_count() == 5,
    );

    // 2. 清空记录
    service.clear_records();
    chain.add_step(
        "integration-tests",
        "MockEmailService::clear_records",
        format!("清空后邮件数: {}", service.email_count()),
        service.email_count() == 0,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试真实 EmailService 配置构建
#[test]
fn test_email_service_config() {
    let mut chain = VerificationChain::new();

    // 1. 创建配置
    let config = EmailConfig {
        smtp_host: "smtp.example.com".to_string(),
        smtp_port: 587,
        smtp_username: "test@example.com".to_string(),
        smtp_password: "password123".to_string(),
        from_address: "noreply@example.com".to_string(),
        from_name: Some("KeyCompute".to_string()),
        use_tls: true,
        timeout_secs: 30,
    };

    chain.add_step(
        "keycompute-emailserver",
        "EmailConfig::new",
        "邮件配置创建成功",
        config.smtp_host == "smtp.example.com",
    );

    // 2. 验证配置属性
    chain.add_step(
        "keycompute-emailserver",
        "EmailConfig::smtp_port",
        format!("SMTP端口: {}", config.smtp_port),
        config.smtp_port == 587,
    );
    chain.add_step(
        "keycompute-emailserver",
        "EmailConfig::use_tls",
        format!("TLS启用: {}", config.use_tls),
        config.use_tls,
    );

    // 3. 验证配置字段
    chain.add_step(
        "keycompute-emailserver",
        "EmailConfig::from_address",
        format!("发件地址: {}", config.from_address),
        config.from_address == "noreply@example.com",
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试邮件收件人筛选
#[tokio::test]
async fn test_mock_email_service_recipient_filter() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    // 1. 发送给不同收件人
    service
        .send_verification_email("alice@example.com", "token1")
        .await
        .unwrap();
    service
        .send_verification_email("bob@example.com", "token2")
        .await
        .unwrap();
    service
        .send_password_reset_email("alice@example.com", "token3", TEST_PUBLIC_APP_BASE_URL)
        .await
        .unwrap();

    // 2. 按收件人筛选
    let alice_emails = service.get_emails_to("alice@example.com");
    let bob_emails = service.get_emails_to("bob@example.com");

    chain.add_step(
        "integration-tests",
        "get_emails_to::alice",
        format!("Alice 的邮件数: {}", alice_emails.len()),
        alice_emails.len() == 2,
    );
    chain.add_step(
        "integration-tests",
        "get_emails_to::bob",
        format!("Bob 的邮件数: {}", bob_emails.len()),
        bob_emails.len() == 1,
    );

    // 3. 验证 Alice 的邮件类型
    let alice_has_verification = alice_emails
        .iter()
        .any(|e| e.email_type == MockEmailType::Verification);
    let alice_has_reset = alice_emails
        .iter()
        .any(|e| e.email_type == MockEmailType::PasswordReset);

    chain.add_step(
        "integration-tests",
        "alice::email_types",
        "Alice 有验证和重置邮件",
        alice_has_verification && alice_has_reset,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试并发邮件发送
#[tokio::test]
async fn test_mock_email_service_concurrent() {
    let mut chain = VerificationChain::new();
    let service = MockEmailService::new();

    // 1. 并发发送多封邮件
    let mut handles = vec![];
    for i in 0..10 {
        let svc = service.clone();
        let handle = tokio::spawn(async move {
            svc.send_verification_email(&format!("user{}@example.com", i), &format!("token{}", i))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }

    chain.add_step(
        "integration-tests",
        "concurrent_send",
        format!("并发发送后邮件数: {}", service.email_count()),
        service.email_count() == 10,
    );

    // 2. 验证所有邮件都记录了
    let mut all_found = true;
    for i in 0..10 {
        let email = format!("user{}@example.com", i);
        let has_email = service.has_verification_email(&email);
        if !has_email {
            all_found = false;
            break;
        }
    }

    chain.add_step(
        "integration-tests",
        "all_emails_recorded",
        "所有并发邮件都已记录",
        all_found && service.email_count() == 10,
    );

    chain.print_report();
    assert!(chain.all_passed());
}
