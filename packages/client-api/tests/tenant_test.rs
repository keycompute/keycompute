//! 租户管理模块集成测试

use client_api::api::tenant::{
    CreateTenantRequest, TenantApi, TenantQueryParams, UpdateTenantRequest,
};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_list_tenants_success() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/platform/tenants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tenants": [
            {
                "id": "tenant_001",
                "name": "Acme Corporation",
                "description": "Test tenant 1",
                "user_count": 10,
                "is_active": true,
                "created_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "tenant_002",
                "name": "TechStart Inc",
                "description": "Test tenant 2",
                "user_count": 5,
                "is_active": true,
                "created_at": "2024-01-10T00:00:00Z"
            },
            {
                "id": "tenant_003",
                "name": "Global Solutions",
                "description": "Test tenant 3",
                "user_count": 2,
                "is_active": false,
                "created_at": "2023-12-01T00:00:00Z"
            }
            ],
            "total": 3,
            "page": 1,
            "page_size": 20,
            "total_pages": 1
        })))
        .mount(&mock_server)
        .await;

    let result = tenant_api
        .list_tenants(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let tenants = result.unwrap();
    assert_eq!(tenants.len(), 3);
    assert_eq!(tenants[0].name, "Acme Corporation");
    assert!(tenants[0].is_active);
    assert!(!tenants[2].is_active);
}

#[tokio::test]
async fn test_list_tenants_without_pagination_collects_all_filtered_pages() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    for (page, id) in [(1, "tenant_001"), (2, "tenant_002")] {
        Mock::given(method("GET"))
            .and(path("/api/v1/platform/tenants"))
            .and(query_param("search", "研发 租户"))
            .and(query_param("page", page.to_string()))
            .and(query_param("page_size", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tenants": [{
                    "id": id,
                    "name": format!("Tenant {id}"),
                    "description": null,
                    "user_count": 1,
                    "is_active": true,
                    "created_at": "2024-01-01T00:00:00Z"
                }],
                "total": 2,
                "page": page,
                "page_size": 100,
                "total_pages": 2
            })))
            .mount(&mock_server)
            .await;
    }

    let params = TenantQueryParams::new().with_search("研发 租户");
    let tenants = tenant_api
        .list_tenants(Some(&params), fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();

    assert_eq!(
        tenants
            .iter()
            .map(|tenant| tenant.id.as_str())
            .collect::<Vec<_>>(),
        ["tenant_001", "tenant_002"]
    );
}

#[tokio::test]
async fn test_list_tenants_empty() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/platform/tenants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tenants": [],
            "total": 0,
            "page": 1,
            "page_size": 20,
            "total_pages": 0
        })))
        .mount(&mock_server)
        .await;

    let result = tenant_api
        .list_tenants(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_list_tenants_with_pagination() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/platform/tenants"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tenants": [
            {
                "id": "tenant_004",
                "name": "Test Tenant",
                "description": "Test tenant 4",
                "user_count": 1,
                "is_active": true,
                "created_at": "2024-01-20T00:00:00Z"
            }
            ],
            "total": 4,
            "page": 4,
            "page_size": 1,
            "total_pages": 4
        })))
        .mount(&mock_server)
        .await;

    let params = TenantQueryParams::new().with_limit(1).with_offset(3);
    let result = tenant_api
        .list_tenants(Some(&params), fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1);
}

#[tokio::test]
async fn test_list_tenants_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/platform/tenants"))
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
        .and(path("/api/v1/platform/tenants"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result = tenant_api
        .list_tenants(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}

#[tokio::test]
async fn test_create_tenant_serializes_optional_slug_without_description() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);
    Mock::given(method("POST"))
        .and(path("/api/v1/platform/tenants"))
        .and(body_json(serde_json::json!({
            "name": "Research Center",
            "owner_user_id": "00000000-0000-0000-0000-000000000001",
            "slug": "research-center"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "tenant_001",
            "name": "Research Center",
            "slug": "research-center",
            "description": null,
            "user_count": 0,
            "account_count": 0,
            "status": "active",
            "is_active": true,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let tenant = tenant_api
        .create_tenant(
            &CreateTenantRequest::new("Research Center", uuid::Uuid::from_u128(1))
                .with_slug("research-center"),
            fixtures::TEST_ACCESS_TOKEN,
        )
        .await
        .unwrap();
    assert_eq!(tenant.slug, "research-center");
    assert_eq!(tenant.account_count, 0);
    assert_eq!(tenant.status, "active");
}

#[test]
fn test_create_tenant_without_slug_serializes_explicit_owner_and_name() {
    let body = serde_json::to_value(CreateTenantRequest::new(
        "Research Center",
        uuid::Uuid::from_u128(1),
    ))
    .expect("create tenant request must serialize");

    assert_eq!(
        body,
        serde_json::json!({"name": "Research Center", "owner_user_id":"00000000-0000-0000-0000-000000000001"})
    );
    assert!(body.get("description").is_none());
    assert!(body.get("status").is_none());
}

#[tokio::test]
async fn test_update_tenant_status_and_delete() {
    let (client, mock_server) = create_test_client().await;
    let tenant_api = TenantApi::new(&client);
    Mock::given(method("PUT"))
        .and(path("/api/v1/platform/tenants/tenant_001"))
        .and(body_json(serde_json::json!({"status": "inactive"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "tenant_001",
            "name": "Research Center",
            "slug": "research-center",
            "description": null,
            "user_count": 0,
            "account_count": 0,
            "status": "inactive",
            "is_active": false,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/platform/tenants/tenant_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Tenant deleted successfully"
        })))
        .mount(&mock_server)
        .await;

    let tenant = tenant_api
        .update_tenant(
            "tenant_001",
            &UpdateTenantRequest::new().with_status("inactive"),
            fixtures::TEST_ACCESS_TOKEN,
        )
        .await
        .unwrap();
    assert!(!tenant.is_active);
    let response = tenant_api
        .delete_tenant("tenant_001", fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();
    assert_eq!(response.message, "Tenant deleted successfully");
}

#[tokio::test]
async fn tenant_creation_rejects_a_nil_owner_before_http() {
    let (client, server) = create_test_client().await;
    let error = TenantApi::new(&client)
        .create_tenant(
            &CreateTenantRequest::new("invalid owner", uuid::Uuid::nil()),
            fixtures::TEST_ACCESS_TOKEN,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ClientError::Config(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}
