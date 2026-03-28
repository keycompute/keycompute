//! 错误处理集成测试
//!
//! 测试各种 HTTP 错误状态码的处理

use client_api::api::auth::{AuthApi, LoginRequest};
use client_api::api::health::HealthApi;
use client_api::error::ClientError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::create_test_client;

#[tokio::test]
async fn test_401_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Invalid credentials"
        })))
        .mount(&mock_server)
        .await;

    let req = LoginRequest::new("test@example.com", "wrong");
    let result = auth_api.login(&req).await;

    match result.unwrap_err() {
        ClientError::Unauthorized(msg) => {
            assert!(msg.contains("401") || msg.contains("Invalid credentials"));
        }
        other => panic!("Expected Unauthorized error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_403_forbidden() {
    let (client, mock_server) = create_test_client().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/admin/users"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result: Result<serde_json::Value, _> = client
        .get_json("/api/v1/admin/users", Some("user_token"))
        .await;

    match result.unwrap_err() {
        ClientError::Forbidden(msg) => {
            assert!(msg.contains("403") || msg.contains("Admin access required"));
        }
        other => panic!("Expected Forbidden error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_404_not_found() {
    let (client, mock_server) = create_test_client().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/nonexistent"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "Resource not found"
        })))
        .mount(&mock_server)
        .await;

    let result: Result<serde_json::Value, _> = client
        .get_json("/api/v1/nonexistent", None)
        .await;

    match result.unwrap_err() {
        ClientError::NotFound(msg) => {
            assert!(msg.contains("404") || msg.contains("not found"));
        }
        other => panic!("Expected NotFound error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_429_rate_limited() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(429)
            .insert_header("Retry-After", "60")
            .set_body_json(serde_json::json!({
                "error": "Too many requests"
            })))
        .mount(&mock_server)
        .await;

    let req = LoginRequest::new("test@example.com", "password");
    let result = auth_api.login(&req).await;

    match result.unwrap_err() {
        ClientError::RateLimited(msg) => {
            assert!(msg.contains("429") || msg.contains("Too many requests"));
        }
        other => panic!("Expected RateLimited error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_500_server_error() {
    let (client, mock_server) = create_test_client().await;
    let health_api = HealthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(500).set_body_json(serde_json::json!({
            "error": "Internal server error"
        })))
        .mount(&mock_server)
        .await;

    let result = health_api.health_check().await;

    match result.unwrap_err() {
        ClientError::ServerError(msg) => {
            assert!(msg.contains("500") || msg.contains("Internal server error"));
        }
        other => panic!("Expected ServerError, got {:?}", other),
    }
}

#[tokio::test]
async fn test_503_service_unavailable() {
    let (client, mock_server) = create_test_client().await;
    let health_api = HealthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503)
            .insert_header("Retry-After", "120")
            .set_body_json(serde_json::json!({
                "error": "Service temporarily unavailable"
            })))
        .mount(&mock_server)
        .await;

    let result = health_api.health_check().await;

    match result.unwrap_err() {
        ClientError::ServiceUnavailable(msg) => {
            assert!(msg.contains("503") || msg.contains("unavailable"));
        }
        other => panic!("Expected ServiceUnavailable error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_network_timeout_simulation() {
    use std::time::Duration;

    let (client, mock_server) = create_test_client().await;
    let health_api = HealthApi::new(&client);

    // 模拟延迟响应（超过客户端超时时间）
    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(100)) // 很长的延迟
            .set_body_json(serde_json::json!({
                "status": "healthy"
            })))
        .mount(&mock_server)
        .await;

    // 注意：这个测试可能需要根据实际超时配置调整
    // 这里主要演示如何测试超时场景
    let result = health_api.health_check().await;

    // 应该会超时或返回网络错误
    assert!(result.is_err());
}

#[tokio::test]
async fn test_invalid_json_response() {
    let (client, mock_server) = create_test_client().await;

    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_string("not valid json {{{{"))
        .mount(&mock_server)
        .await;

    let result: Result<serde_json::Value, _> = client
        .get_json("/api/v1/me", Some("token"))
        .await;

    // reqwest 会将 JSON 解析错误包装为 Http 错误
    match result.unwrap_err() {
        ClientError::Http(_) | ClientError::Serialization(_) => {
            // 两种错误类型都接受，因为底层库可能以不同方式处理
        }
        other => panic!("Expected Http or Serialization error, got {:?}", other),
    }
}

#[tokio::test]
async fn test_error_helper_methods() {
    let (client, mock_server) = create_test_client().await;
    let auth_api = AuthApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&mock_server)
        .await;

    let req = LoginRequest::new("test@example.com", "wrong");
    let err = auth_api.login(&req).await.unwrap_err();

    assert!(err.is_auth_error());
    assert!(!err.is_rate_limited());
    assert!(!err.is_network_error());
}
