//! 健康检查模块集成测试

use client_api::api::health::HealthApi;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::create_test_client;

#[tokio::test]
async fn test_health_check_success() {
    let (client, mock_server) = create_test_client().await;
    let health_api = HealthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "healthy",
            "version": "0.1.0",
            "timestamp": "2024-01-01T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = health_api.health_check().await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert!(resp.is_healthy());
    assert_eq!(resp.version, Some("0.1.0".to_string()));
}

#[tokio::test]
async fn test_health_check_ok_status() {
    let (client, mock_server) = create_test_client().await;
    let health_api = HealthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok"
        })))
        .mount(&mock_server)
        .await;

    let result = health_api.health_check().await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_healthy());
}

#[tokio::test]
async fn test_health_check_unhealthy() {
    let (client, mock_server) = create_test_client().await;
    let health_api = HealthApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/health"))
        .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "status": "unhealthy",
            "error": "Database connection failed"
        })))
        .mount(&mock_server)
        .await;

    let result = health_api.health_check().await;

    assert!(result.is_err());
}
