//! 认证模块集成测试
//!
//! 使用 Wiremock 模拟后端服务，无需启动真实服务器

use client_api::api::auth::{
    AuthApi, CompleteRegistrationRequest, ForgotPasswordRequest, LoginRequest, RefreshTokenRequest,
    RequestRegistrationCodeRequest, ResetPasswordRequest,
};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_login_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    // Mock 登录成功响应
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user_id": fixtures::TEST_USER_ID,
            "tenant_id": "tenant_001",
            "email": fixtures::TEST_EMAIL,
            "role": "user",
            "access_token": fixtures::TEST_ACCESS_TOKEN,
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .mount(&mock_server)
        .await;

    // 执行登录
    let req = LoginRequest::new(fixtures::TEST_EMAIL, "password123");
    let result = auth_api.login(&req).await;

    // 验证结果
    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.access_token, fixtures::TEST_ACCESS_TOKEN);
    assert_eq!(resp.email, fixtures::TEST_EMAIL);
}

#[tokio::test]
async fn test_login_invalid_credentials() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    // Mock 401 响应
    Mock::given(method("POST"))
        .and(path("/api/v1/auth/login"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Invalid email or password"
        })))
        .mount(&mock_server)
        .await;

    let req = LoginRequest::new("wrong@example.com", "wrongpassword");
    let result = auth_api.login(&req).await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_request_registration_code_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    let expected_body = serde_json::json!({
        "email": "new@example.com",
        "referral_code": "6aac8ab5-aeec-48b8-a4cc-0a446d952862"
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/auth/register"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "email": "new@example.com",
            "message": "验证码已发送，请在 10 分钟内完成验证",
            "expires_in_seconds": 600
        })))
        .mount(&mock_server)
        .await;

    let req = RequestRegistrationCodeRequest::new("new@example.com")
        .with_referral_code("6aac8ab5-aeec-48b8-a4cc-0a446d952862");
    let result = auth_api.request_registration_code(&req).await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.email, "new@example.com");
    assert_eq!(resp.message, "验证码已发送，请在 10 分钟内完成验证");
    assert_eq!(resp.expires_in_seconds, 600);
}

#[tokio::test]
async fn test_complete_registration_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    let expected_body = serde_json::json!({
        "email": "new@example.com",
        "code": "123456",
        "password": "SecurePass123!",
        "name": "New User"
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/auth/register/complete"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "user_id": "user_new_001",
            "tenant_id": "tenant_001",
            "email": "new@example.com",
            "message": "注册成功，您现在可以登录了"
        })))
        .mount(&mock_server)
        .await;

    let req = CompleteRegistrationRequest::new("new@example.com", "123456", "SecurePass123!")
        .with_name("New User");
    let result = auth_api.complete_registration(&req).await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.email, "new@example.com");
    assert_eq!(resp.message, "注册成功，您现在可以登录了");
}

#[tokio::test]
async fn test_forgot_password_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "Test User",
        "email": fixtures::TEST_EMAIL
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/auth/forgot-password"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Password reset email sent"
        })))
        .mount(&mock_server)
        .await;

    let req = ForgotPasswordRequest::new("Test User", fixtures::TEST_EMAIL);
    let result = auth_api.forgot_password(&req).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_reset_password_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/auth/reset-password"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Password reset successfully"
        })))
        .mount(&mock_server)
        .await;

    let req = ResetPasswordRequest::new("reset_token_123", "NewPass123!");
    let result = auth_api.reset_password(&req).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_refresh_token_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/auth/refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user_id": fixtures::TEST_USER_ID,
            "tenant_id": "tenant_001",
            "email": fixtures::TEST_EMAIL,
            "role": "user",
            "access_token": "new_access_token_999",
            "token_type": "Bearer",
            "expires_in": 3600
        })))
        .mount(&mock_server)
        .await;

    let req = RefreshTokenRequest::new(fixtures::TEST_REFRESH_TOKEN);
    let result = auth_api.refresh_token(&req).await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.access_token, "new_access_token_999");
}

#[tokio::test]
async fn test_verify_reset_token_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/auth/verify-reset-token/valid_reset_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Token is valid"
        })))
        .mount(&mock_server)
        .await;

    let result = auth_api.verify_reset_token("valid_reset_token").await;

    assert!(result.is_ok());
}
