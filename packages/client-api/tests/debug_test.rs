//! 调试接口模块集成测试

use client_api::api::debug::DebugApi;
use client_api::error::ClientError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_debug_routing_success() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/debug/routing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "routes": [
                {
                    "path": "/api/v1/users",
                    "method": "GET",
                    "handler": "list_users"
                },
                {
                    "path": "/api/v1/users/:id",
                    "method": "GET",
                    "handler": "get_user"
                },
                {
                    "path": "/health",
                    "method": "GET",
                    "handler": "health_check"
                }
            ]
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.debug_routing(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let info = result.unwrap();
    assert_eq!(info.routes.len(), 3);
    assert_eq!(info.routes[0].path, "/api/v1/users");
    assert_eq!(info.routes[0].method, "GET");
}

#[tokio::test]
async fn test_get_provider_health_success() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/debug/providers"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "providers": {
                "openai": {
                    "status": "healthy",
                    "last_check": "2024-01-20T10:00:00Z",
                    "latency_ms": 150,
                    "error": null
                },
                "anthropic": {
                    "status": "healthy",
                    "last_check": "2024-01-20T10:00:00Z",
                    "latency_ms": 200,
                    "error": null
                },
                "gemini": {
                    "status": "unhealthy",
                    "last_check": "2024-01-20T09:55:00Z",
                    "latency_ms": null,
                    "error": "Connection timeout"
                }
            }
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.get_provider_health(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let health = result.unwrap();
    assert!(health.providers.contains_key("openai"));
    assert!(health.providers.contains_key("anthropic"));
    assert_eq!(health.providers["openai"].status, "healthy");
    assert_eq!(health.providers["gemini"].status, "unhealthy");
}

#[tokio::test]
async fn test_get_gateway_status_success() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/debug/gateway/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "healthy",
            "uptime_seconds": 86400,
            "version": "0.1.0"
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.get_gateway_status(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let status = result.unwrap();
    assert_eq!(status.status, "healthy");
    assert_eq!(status.uptime_seconds, 86400);
    assert_eq!(status.version, "0.1.0");
}

#[tokio::test]
async fn test_get_gateway_stats_success() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/debug/gateway/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_requests": 100000,
            "successful_requests": 95000,
            "failed_requests": 5000,
            "average_latency_ms": 125.5,
            "active_connections": 42
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.get_gateway_stats(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.total_requests, 100000);
    assert_eq!(stats.successful_requests, 95000);
    assert_eq!(stats.active_connections, 42);
}

#[tokio::test]
async fn test_check_provider_health_success() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/debug/gateway/health"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "checked_providers": ["openai", "anthropic", "gemini"],
            "healthy_providers": ["openai", "anthropic"],
            "unhealthy_providers": ["gemini"]
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.check_provider_health(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let check = result.unwrap();
    assert_eq!(check.checked_providers.len(), 3);
    assert_eq!(check.healthy_providers.len(), 2);
    assert_eq!(check.unhealthy_providers.len(), 1);
}

#[tokio::test]
async fn test_debug_endpoints_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/debug/routing"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.debug_routing("invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_debug_endpoints_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let debug_api = DebugApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/debug/providers"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result = debug_api.get_provider_health(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}
