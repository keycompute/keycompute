//! 租户管理模块集成测试

use client_api::api::tenant::{TenantApi, TenantQueryParams};
use client_api::error::ClientError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_list_tenants_success() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/tenants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "tenant_001",
                "name": "Acme Corporation",
                "status": "active",
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-15T00:00:00Z"
            },
            {
                "id": "tenant_002",
                "name": "TechStart Inc",
                "status": "active",
                "created_at": "2024-01-10T00:00:00Z",
                "updated_at": "2024-01-20T00:00:00Z"
            },
            {
                "id": "tenant_003",
                "name": "Global Solutions",
                "status": "suspended",
                "created_at": "2023-12-01T00:00:00Z",
                "updated_at": "2024-01-05T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = tenant_api.list_tenants(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let tenants = result.unwrap();
    assert_eq!(tenants.len(), 3);
    assert_eq!(tenants[0].name, "Acme Corporation");
    assert_eq!(tenants[0].status, "active");
    assert_eq!(tenants[2].status, "suspended");
}

#[tokio::test]
async fn test_list_tenants_empty() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/tenants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let result = tenant_api.list_tenants(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_list_tenants_with_pagination() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/tenants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "tenant_004",
                "name": "Test Tenant",
                "status": "active",
                "created_at": "2024-01-20T00:00:00Z",
                "updated_at": "2024-01-20T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let params = TenantQueryParams::new().with_limit(1).with_offset(3);
    let result = tenant_api.list_tenants(Some(&params), fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1);
}

#[tokio::test]
async fn test_list_tenants_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/tenants"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = tenant_api.list_tenants(None, "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_list_tenants_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/tenants"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result = tenant_api.list_tenants(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}
