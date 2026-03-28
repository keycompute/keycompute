//! 认证模块集成测试
//!
//! 使用 Wiremock 模拟后端服务，无需启动真实服务器

use client_api::api::auth::{
    AuthApi, ForgotPasswordRequest, LoginRequest, RegisterRequest, ResetPasswordRequest,
    RefreshTokenRequest, ResendVerificationRequest,
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
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": fixtures::TEST_ACCESS_TOKEN,
            "refresh_token": fixtures::TEST_REFRESH_TOKEN,
            "token_type": "Bearer",
            "expires_in": 3600,
            "user": {
                "id": fixtures::TEST_USER_ID,
                "email": fixtures::TEST_EMAIL,
                "name": "Test User",
                "role": "user"
            }
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
    assert_eq!(resp.user.email, fixtures::TEST_EMAIL);
}

#[tokio::test]
async fn test_login_invalid_credentials() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    // Mock 401 响应
    Mock::given(method("POST"))
        .and(path("/auth/login"))
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
async fn test_register_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    let expected_body = serde_json::json!({
        "email": "new@example.com",
        "password": "SecurePass123!",
        "name": "New User"
    });

    Mock::given(method("POST"))
        .and(path("/auth/register"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": fixtures::TEST_ACCESS_TOKEN,
            "refresh_token": fixtures::TEST_REFRESH_TOKEN,
            "token_type": "Bearer",
            "expires_in": 3600,
            "user": {
                "id": "user_new_001",
                "email": "new@example.com",
                "name": "New User",
                "role": "user"
            }
        })))
        .mount(&mock_server)
        .await;

    let req = RegisterRequest::new("new@example.com", "SecurePass123!")
        .with_name("New User");
    let result = auth_api.register(&req).await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.user.name, Some("New User".to_string()));
}

#[tokio::test]
async fn test_verify_email_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/auth/verify-email/valid_token_123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Email verified successfully"
        })))
        .mount(&mock_server)
        .await;

    let result = auth_api.verify_email("valid_token_123").await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().message, "Email verified successfully");
}

#[tokio::test]
async fn test_forgot_password_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/auth/forgot-password"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Password reset email sent"
        })))
        .mount(&mock_server)
        .await;

    let req = ForgotPasswordRequest::new(fixtures::TEST_EMAIL);
    let result = auth_api.forgot_password(&req).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_reset_password_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/auth/reset-password"))
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
        .and(path("/auth/refresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new_access_token_999",
            "refresh_token": "new_refresh_token_888",
            "token_type": "Bearer",
            "expires_in": 3600,
            "user": {
                "id": fixtures::TEST_USER_ID,
                "email": fixtures::TEST_EMAIL,
                "name": "Test User",
                "role": "user"
            }
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
async fn test_resend_verification_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/auth/resend-verification"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Verification email resent"
        })))
        .mount(&mock_server)
        .await;

    let req = ResendVerificationRequest::new(fixtures::TEST_EMAIL);
    let result = auth_api.resend_verification(&req).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_verify_reset_token_success() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/auth/verify-reset-token/valid_reset_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Token is valid"
        })))
        .mount(&mock_server)
        .await;

    let result = auth_api.verify_reset_token("valid_reset_token").await;

    assert!(result.is_ok());
}
