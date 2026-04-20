//! 密码重置服务端到端测试
//!
//! 验证密码重置功能的完整流程：
//! - 请求密码重置
//! - 重置令牌验证
//! - 执行密码重置
//! - 邮件发送集成
//! - 安全考虑（防枚举）

use integration_tests::common::VerificationChain;
use integration_tests::mocks::database::{MockPasswordReset, MockUserTenantDatabase};
use integration_tests::mocks::email::{MockEmailService, MockEmailType};
use keycompute_auth::password::{EmailValidator, PasswordHasher, PasswordValidator};
use uuid::Uuid;

const TEST_PUBLIC_APP_BASE_URL: &str = "https://app.example.com";

/// 测试密码重置令牌生命周期
#[test]
fn test_password_reset_token_lifecycle() {
    let mut chain = VerificationChain::new();

    // 1. 创建重置记录
    let user_id = Uuid::new_v4();
    let reset = MockPasswordReset::new(user_id, "reset_token_123");

    chain.add_step(
        "integration-tests",
        "PasswordReset::new",
        "重置记录创建",
        reset.token == "reset_token_123" && !reset.used,
    );

    // 2. 验证初始状态
    chain.add_step(
        "integration-tests",
        "reset::is_valid_initial",
        "初始状态有效",
        reset.is_valid(),
    );

    // 3. 模拟使用令牌
    let mut used_reset = reset.clone();
    used_reset.used = true;

    chain.add_step(
        "integration-tests",
        "reset::mark_used",
        "使用后标记为已使用",
        used_reset.used,
    );

    chain.add_step(
        "integration-tests",
        "reset::is_valid_after_use",
        "使用后无效",
        !used_reset.is_valid(),
    );

    // 4. 模拟过期令牌
    let mut expired_reset = MockPasswordReset::new(user_id, "reset_token_456");
    expired_reset.expires_at = chrono::Utc::now() - chrono::Duration::hours(1);

    chain.add_step(
        "integration-tests",
        "reset::expired",
        "过期令牌无效",
        !expired_reset.is_valid(),
    );

    // 5. 验证令牌长度
    chain.add_step(
        "integration-tests",
        "reset::token_length",
        format!("令牌长度: {}", reset.token.len()),
        reset.token.len() >= 10,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试密码重置请求流程
#[tokio::test]
async fn test_password_reset_request_flow() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();
    let email_service = MockEmailService::new();

    // 1. 创建租户和用户
    let tenant = db.create_test_tenant();
    let user = db.create_test_user(tenant.id, "user");

    chain.add_step(
        "integration-tests",
        "create_user",
        format!("用户创建: {}", user.email),
        true,
    );

    // 2. 创建密码重置记录
    let reset_token = "password_reset_token_abc";
    let reset = MockPasswordReset::new(user.id, reset_token);

    chain.add_step(
        "integration-tests",
        "create_reset_record",
        "重置记录创建",
        reset.is_valid(),
    );

    // 3. 发送密码重置邮件
    let result = email_service
        .send_password_reset_email(&user.email, reset_token, TEST_PUBLIC_APP_BASE_URL)
        .await;
    chain.add_step(
        "integration-tests",
        "send_reset_email",
        "重置邮件发送成功",
        result.is_ok(),
    );

    // 4. 验证邮件已发送
    chain.add_step(
        "integration-tests",
        "verify_email_sent",
        "重置邮件已记录",
        email_service.has_password_reset_email(&user.email),
    );

    // 5. 验证邮件内容
    let emails = email_service.get_emails_by_type(MockEmailType::PasswordReset);
    chain.add_step(
        "integration-tests",
        "reset_email_content",
        format!("邮件主题: {}", emails[0].subject),
        emails[0].subject.contains("重置"),
    );

    // 6. 验证令牌关联
    let email_record = email_service.get_email_by_token(reset_token);
    chain.add_step(
        "integration-tests",
        "token_association",
        "邮件与令牌关联正确",
        email_record.is_some() && email_record.unwrap().to == user.email,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试密码重置完整链路
#[tokio::test]
async fn test_password_reset_full_flow() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();
    let email_service = MockEmailService::new();
    let password_hasher = PasswordHasher::new();

    // 1. 创建用户和凭证
    let tenant = db.create_test_tenant();
    let user = db.create_test_user(tenant.id, "user");
    let old_password = "OldP@ssw0rd123";
    let old_hash = password_hasher.hash(old_password).unwrap();

    chain.add_step("integration-tests", "step1_create_user", "创建用户", true);

    // 2. 请求密码重置
    let reset_token = "full_flow_reset_token";
    let reset = MockPasswordReset::new(user.id, reset_token);

    chain.add_step(
        "integration-tests",
        "step2_request_reset",
        "请求密码重置",
        reset.is_valid(),
    );

    // 3. 发送重置邮件
    let email_result = email_service
        .send_password_reset_email(&user.email, reset_token, TEST_PUBLIC_APP_BASE_URL)
        .await;
    chain.add_step(
        "integration-tests",
        "step3_send_email",
        "发送重置邮件",
        email_result.is_ok(),
    );

    // 4. 模拟用户点击邮件链接，验证令牌
    chain.add_step(
        "integration-tests",
        "step4_validate_token",
        "验证令牌有效",
        reset.is_valid(),
    );

    // 5. 执行密码重置
    let new_password = "NewSecureP@ss456!";
    let new_hash = password_hasher.hash(new_password).unwrap();

    chain.add_step(
        "keycompute-auth",
        "step5_hash_new_password",
        "新密码哈希成功",
        !new_hash.is_empty(),
    );

    // 6. 验证新旧密码不同
    chain.add_step(
        "keycompute-auth",
        "step6_different_hashes",
        "新旧密码哈希不同",
        old_hash != new_hash,
    );

    // 7. 验证新密码有效
    let verify_new = password_hasher.verify(new_password, &new_hash).unwrap();
    chain.add_step(
        "keycompute-auth",
        "step7_verify_new_password",
        "新密码验证通过",
        verify_new,
    );

    // 8. 验证旧密码不再有效（使用新哈希时）
    let verify_old_with_new = password_hasher.verify(old_password, &new_hash).unwrap();
    chain.add_step(
        "keycompute-auth",
        "step8_old_password_invalid",
        "旧密码在新哈希下无效",
        !verify_old_with_new,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试密码重置安全特性（防枚举）
#[tokio::test]
async fn test_password_reset_security() {
    let mut chain = VerificationChain::new();
    let email_validator = EmailValidator::new();

    // 1. 验证邮箱格式检查
    let valid_email = "user@example.com";
    let invalid_email = "not-an-email";

    chain.add_step(
        "keycompute-auth",
        "validate_email_format",
        "有效邮箱通过验证",
        email_validator.validate(valid_email).is_ok(),
    );

    chain.add_step(
        "keycompute-auth",
        "reject_invalid_email",
        "无效邮箱被拒绝",
        email_validator.validate(invalid_email).is_err(),
    );

    // 2. 测试不存在的邮箱（静默处理）
    let db = MockUserTenantDatabase::new();
    let non_existent_email = "nonexistent@example.com";

    let user = db
        .get_users_by_tenant(Uuid::new_v4())
        .into_iter()
        .find(|u| u.email == non_existent_email);

    chain.add_step(
        "integration-tests",
        "non_existent_user",
        "不存在的用户返回 None",
        user.is_none(),
    );

    // 3. 验证邮箱规范化
    let normalized = email_validator.normalize("  User@Example.COM  ");
    chain.add_step(
        "keycompute-auth",
        "email_normalization",
        format!("规范化: '{}' -> '{}'", "  User@Example.COM  ", normalized),
        normalized == "user@example.com",
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试密码强度验证
#[test]
fn test_password_strength_on_reset() {
    let mut chain = VerificationChain::new();
    let validator = PasswordValidator::new();

    // 1. 测试强密码
    let strong_passwords = vec!["MyNewP@ssw0rd!", "Str0ng!Pass123", "C0mpl3x#P@ssw0rd"];

    for pwd in strong_passwords {
        let result = validator.validate(pwd);
        chain.add_step(
            "keycompute-auth",
            "validate_strong",
            format!("强密码通过: {}", &pwd[..8]),
            result.is_ok(),
        );
    }

    // 2. 测试弱密码
    let weak_passwords = vec![
        ("short", "太短"),
        ("12345678", "纯数字"),
        ("password", "常见密码"),
        ("abcdefgh", "纯小写"),
        ("ABCDEFGH", "纯大写"),
    ];

    for (pwd, reason) in weak_passwords {
        let result = validator.validate(pwd);
        chain.add_step(
            "keycompute-auth",
            "reject_weak",
            format!("拒绝弱密码 ({}): {}", reason, pwd),
            result.is_err(),
        );
    }

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试重置邮件发送失败处理
#[tokio::test]
async fn test_reset_email_failure_handling() {
    let mut chain = VerificationChain::new();
    let email_service = MockEmailService::new();

    // 1. 设置邮件服务失败
    email_service.set_should_fail(true);

    // 2. 尝试发送重置邮件
    let result = email_service
        .send_password_reset_email("test@example.com", "token", TEST_PUBLIC_APP_BASE_URL)
        .await;
    chain.add_step(
        "integration-tests",
        "reset_email_failure",
        "重置邮件发送失败",
        result.is_err(),
    );

    // 3. 验证没有邮件记录
    chain.add_step(
        "integration-tests",
        "no_email_on_failure",
        "失败时不记录邮件",
        email_service.email_count() == 0,
    );

    // 4. 恢复服务
    email_service.set_should_fail(false);
    let result = email_service
        .send_password_reset_email("test@example.com", "token", TEST_PUBLIC_APP_BASE_URL)
        .await;
    chain.add_step(
        "integration-tests",
        "reset_after_recovery",
        "恢复后发送成功",
        result.is_ok(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试多次重置请求
#[tokio::test]
async fn test_multiple_reset_requests() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();
    let email_service = MockEmailService::new();

    // 1. 创建用户
    let tenant = db.create_test_tenant();
    let user = db.create_test_user(tenant.id, "user");

    // 2. 多次请求重置
    let tokens = vec!["token1", "token2", "token3"];
    for token in &tokens {
        let reset = MockPasswordReset::new(user.id, *token);
        let result = email_service
            .send_password_reset_email(&user.email, token, TEST_PUBLIC_APP_BASE_URL)
            .await;
        chain.add_step(
            "integration-tests",
            "reset_request",
            format!("重置请求 {} 发送成功", token),
            result.is_ok() && reset.is_valid(),
        );
    }

    // 3. 验证所有邮件都已发送
    let reset_emails = email_service.get_emails_by_type(MockEmailType::PasswordReset);
    chain.add_step(
        "integration-tests",
        "all_reset_emails_sent",
        format!("发送了 {} 封重置邮件", reset_emails.len()),
        reset_emails.len() == 3,
    );

    // 4. 验证每个令牌都有对应的邮件
    let mut all_tokens_have_email = true;
    for token in &tokens {
        let email = email_service.get_email_by_token(token);
        if email.is_none() {
            all_tokens_have_email = false;
            break;
        }
    }

    chain.add_step(
        "integration-tests",
        "tokens_have_email",
        "所有令牌都有对应邮件",
        all_tokens_have_email,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试密码哈希更新
#[test]
fn test_password_hash_update() {
    let mut chain = VerificationChain::new();
    let hasher = PasswordHasher::new();

    let password1 = "FirstP@ssw0rd!";
    let password2 = "SecondP@ssw0rd!";

    // 1. 初始密码哈希
    let hash1 = hasher.hash(password1).unwrap();
    chain.add_step(
        "keycompute-auth",
        "initial_hash",
        "初始密码哈希成功",
        !hash1.is_empty(),
    );

    // 2. 验证初始密码
    chain.add_step(
        "keycompute-auth",
        "verify_initial",
        "初始密码验证通过",
        hasher.verify(password1, &hash1).unwrap(),
    );

    // 3. 新密码哈希（模拟重置后）
    let hash2 = hasher.hash(password2).unwrap();
    chain.add_step(
        "keycompute-auth",
        "new_hash",
        "新密码哈希成功",
        !hash2.is_empty(),
    );

    // 4. 验证哈希不同
    chain.add_step(
        "keycompute-auth",
        "hashes_different",
        "新旧哈希不同",
        hash1 != hash2,
    );

    // 5. 验证新密码
    chain.add_step(
        "keycompute-auth",
        "verify_new",
        "新密码验证通过",
        hasher.verify(password2, &hash2).unwrap(),
    );

    // 6. 旧密码在新哈希下无效
    chain.add_step(
        "keycompute-auth",
        "old_password_invalid",
        "旧密码在新哈希下无效",
        !hasher.verify(password1, &hash2).unwrap(),
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试重置令牌清理
#[test]
fn test_reset_token_cleanup() {
    let mut chain = VerificationChain::new();

    // 1. 创建多个令牌
    let user_id = Uuid::new_v4();
    let mut tokens = vec![];

    for i in 0..5 {
        let mut reset = MockPasswordReset::new(user_id, format!("token_{}", i));
        if i % 2 == 0 {
            // 标记一些为已使用
            reset.used = true;
        }
        tokens.push(reset);
    }

    let used_count = tokens.iter().filter(|t| t.used).count();
    let valid_count = tokens.iter().filter(|t| t.is_valid()).count();

    chain.add_step(
        "integration-tests",
        "count_used_tokens",
        format!("已使用令牌数: {}", used_count),
        used_count == 3,
    );

    chain.add_step(
        "integration-tests",
        "count_valid_tokens",
        format!("有效令牌数: {}", valid_count),
        valid_count == 2,
    );

    // 2. 模拟清理已使用令牌
    tokens.retain(|t| !t.used);

    chain.add_step(
        "integration-tests",
        "after_cleanup",
        format!("清理后剩余: {}", tokens.len()),
        tokens.len() == 2,
    );

    chain.print_report();
    assert!(chain.all_passed());
}

/// 测试密码重置与邮件服务集成
#[tokio::test]
async fn test_password_reset_email_integration() {
    let mut chain = VerificationChain::new();
    let db = MockUserTenantDatabase::new();
    let email_service = MockEmailService::new();

    // 1. 创建用户
    let tenant = db.create_test_tenant();
    let user = db.create_test_user(tenant.id, "user");

    // 2. 创建重置令牌
    let reset_token = "integration_reset_token";
    let reset = MockPasswordReset::new(user.id, reset_token);

    chain.add_step(
        "integration-tests",
        "create_reset_token",
        "创建重置令牌",
        reset.is_valid(),
    );

    // 3. 发送邮件
    let result = email_service
        .send_password_reset_email(&user.email, reset_token, TEST_PUBLIC_APP_BASE_URL)
        .await;
    chain.add_step(
        "integration-tests",
        "send_reset_email",
        "发送重置邮件",
        result.is_ok(),
    );

    // 4. 验证邮件内容包含重置链接
    let emails = email_service.get_emails_by_type(MockEmailType::PasswordReset);
    let has_reset_link = emails.iter().any(|e| {
        e.text_body.contains("reset")
            || e.html_body
                .as_ref()
                .map(|h| h.contains("reset"))
                .unwrap_or(false)
    });

    chain.add_step(
        "integration-tests",
        "email_has_reset_link",
        "邮件包含重置链接",
        has_reset_link,
    );

    // 5. 验证邮件主题正确
    chain.add_step(
        "integration-tests",
        "email_subject_correct",
        "邮件主题正确",
        emails[0].subject.contains("重置"),
    );

    chain.print_report();
    assert!(chain.all_passed());
}
