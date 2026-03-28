//! API Key 管理模块集成测试

use client_api::api::api_key::{ApiKeyApi, CreateApiKeyRequest};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_list_my_api_keys_success() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "key_001",
                "name": "Development Key",
                "key_preview": "sk-...abcd",
                "revoked": false,
                "revoked_at": null,
                "expires_at": "2024-12-31T23:59:59Z",
                "last_used_at": "2024-01-15T10:30:00Z",
                "created_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "key_002",
                "name": "Production Key",
                "key_preview": "sk-...efgh",
                "revoked": false,
                "revoked_at": null,
                "expires_at": null,
                "last_used_at": null,
                "created_at": "2024-01-10T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = api_key_api.list_my_api_keys(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let keys = result.unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].name, "Development Key");
    assert_eq!(keys[0].key_preview, "sk-...abcd");
    assert!(!keys[0].revoked);
    assert_eq!(keys[1].name, "Production Key");
}

#[tokio::test]
async fn test_list_my_api_keys_empty() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let result = api_key_api.list_my_api_keys(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_list_my_api_keys_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/keys"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Invalid token"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api.list_my_api_keys("invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_create_api_key_success() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "New API Key",
        "expires_at": null
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/keys"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "key_new_001",
            "name": "New API Key",
            "api_key": "sk-live-abcdefghijklmnopqrstuvwxyz123456",
            "key_preview": "sk-...6",
            "expires_at": null,
            "created_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreateApiKeyRequest::new("New API Key");
    let result = api_key_api.create_api_key(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.name, "New API Key");
    assert_eq!(resp.api_key, "sk-live-abcdefghijklmnopqrstuvwxyz123456");
    assert_eq!(resp.key_preview, "sk-...6");
}

#[tokio::test]
async fn test_create_api_key_with_expiration() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "Temporary Key",
        "expires_at": "2024-06-30T23:59:59Z"
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/keys"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "key_temp_001",
            "name": "Temporary Key",
            "api_key": "sk-temp-xyz789",
            "key_preview": "sk-...789",
            "expires_at": "2024-06-30T23:59:59Z",
            "created_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreateApiKeyRequest::new("Temporary Key")
        .with_expires_at("2024-06-30T23:59:59Z");
    let result = api_key_api.create_api_key(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.expires_at, Some("2024-06-30T23:59:59Z".to_string()));
}

#[tokio::test]
async fn test_create_api_key_duplicate_name() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/keys"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
            "error": "API Key with this name already exists"
        })))
        .mount(&mock_server)
        .await;

    let req = CreateApiKeyRequest::new("Existing Key Name");
    let result = api_key_api.create_api_key(&req, fixtures::TEST_ACCESS_TOKEN).await;

    // 409 会被映射为 Http 错误
    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_api_key_success() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/keys/key_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "API Key deleted successfully"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api.delete_api_key("key_001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().message, "API Key deleted successfully");
}

#[tokio::test]
async fn test_delete_api_key_not_found() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/keys/nonexistent_key"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "API Key not found"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api.delete_api_key("nonexistent_key", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::NotFound(_)));
}

#[tokio::test]
async fn test_delete_api_key_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/keys/key_001"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api.delete_api_key("key_001", "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_delete_api_key_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/keys/key_002"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Cannot delete this API Key"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api.delete_api_key("key_002", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}
