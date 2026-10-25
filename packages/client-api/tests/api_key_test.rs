//! API Key 管理模块集成测试

use client_api::api::api_key::{ApiKeyApi, CreateApiKeyRequest};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path, query_param};
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
                "is_active": true,
                "revoked": false,
                "expires_at": "2024-12-31T23:59:59Z",
                "last_used_at": "2024-01-15T10:30:00Z",
                "created_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "key_002",
                "name": "Production Key",
                "key_preview": "sk-...efgh",
                "is_active": true,
                "revoked": false,
                "expires_at": null,
                "last_used_at": null,
                "created_at": "2024-01-10T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = api_key_api
        .list_my_api_keys(false, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let keys = result.unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].name, "Development Key");
    assert_eq!(keys[0].key_preview, "sk-...abcd");
    assert!(!keys[0].revoked());
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

    let result = api_key_api
        .list_my_api_keys(false, fixtures::TEST_ACCESS_TOKEN)
        .await;

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

    let result = api_key_api.list_my_api_keys(false, "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_create_api_key_success() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "New API Key",
        "never_expires": true
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/keys"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "message": "API Key created successfully",
            "key_id": "11111111-1111-4111-8111-111111111111",
            "name": "New API Key",
            "key": "sk-live-abcdefghijklmnopqrstuvwxyz123456",
            "expires_at": null,
            "created_at": "2024-01-20T00:00:00Z",
            "never_expires": true
        })))
        .mount(&mock_server)
        .await;

    let req = CreateApiKeyRequest::new("New API Key").with_never_expires(true);
    let result = api_key_api
        .create_api_key(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.name, "New API Key");
    assert_eq!(resp.api_key, "sk-live-abcdefghijklmnopqrstuvwxyz123456");
}

#[tokio::test]
async fn test_reveal_api_key_is_owner_scoped_and_validates_the_response_id() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);
    let id = "11111111-1111-4111-8111-111111111111";

    Mock::given(method("POST"))
        .and(path(format!("/api/v1/keys/{id}/reveal")))
        .and(body_json(serde_json::json!({
            "current_password": "CurrentPassword123!"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "key_id": id,
            "key": "sk-revealed-key"
        })))
        .mount(&mock_server)
        .await;

    assert_eq!(
        api_key_api
            .reveal_api_key(id, "CurrentPassword123!", fixtures::TEST_ACCESS_TOKEN,)
            .await
            .unwrap(),
        "sk-revealed-key"
    );
    assert!(
        api_key_api
            .reveal_api_key(
                "not-a-uuid",
                "CurrentPassword123!",
                fixtures::TEST_ACCESS_TOKEN,
            )
            .await
            .is_err()
    );
    assert_eq!(mock_server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn test_create_api_key_with_default_server_expiration() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "Temporary Key",
        "never_expires": false
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/keys"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "message": "API Key created successfully",
            "key_id": "22222222-2222-4222-8222-222222222222",
            "name": "Temporary Key",
            "key": "sk-temp-xyz789",
            "expires_at": "2024-07-18T00:00:00Z",
            "created_at": "2024-01-20T00:00:00Z",
            "never_expires": false
        })))
        .mount(&mock_server)
        .await;

    let req = CreateApiKeyRequest::new("Temporary Key");
    let result = api_key_api
        .create_api_key(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.expires_at, Some("2024-07-18T00:00:00Z".to_string()));
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
    let result = api_key_api
        .create_api_key(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    // 409 会被映射为 Http 错误
    assert!(result.is_err());
}

#[tokio::test]
async fn test_delete_api_key_success() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let id = "11111111-1111-4111-8111-111111111111";
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/keys/{id}")))
        .and(query_param("permanent", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "message": "API Key deleted successfully",
            "key_id": id,
            "deleted": true
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api
        .delete_api_key(id, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let result = result.unwrap();
    assert_eq!(result.message, "API Key deleted successfully");
    assert!(result.deleted);
}

#[tokio::test]
async fn test_delete_api_key_rejects_a_non_permanent_or_mismatched_result() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);
    let id = "11111111-1111-4111-8111-111111111111";

    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/keys/{id}")))
        .and(query_param("permanent", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "message": "not actually deleted",
            "key_id": "22222222-2222-4222-8222-222222222222",
            "deleted": false
        })))
        .mount(&mock_server)
        .await;

    assert!(matches!(
        api_key_api
            .delete_api_key(id, fixtures::TEST_ACCESS_TOKEN)
            .await,
        Err(ClientError::InvalidResponse(_))
    ));
}

#[tokio::test]
async fn test_delete_api_key_not_found() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let id = "22222222-2222-4222-8222-222222222222";
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/keys/{id}")))
        .and(query_param("permanent", "true"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "API Key not found"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api
        .delete_api_key(id, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::NotFound(_)));
}

#[tokio::test]
async fn test_delete_api_key_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let id = "33333333-3333-4333-8333-333333333333";
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/keys/{id}")))
        .and(query_param("permanent", "true"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api.delete_api_key(id, "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_delete_api_key_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let api_key_api = ApiKeyApi::new(&client);

    let id = "44444444-4444-4444-8444-444444444444";
    Mock::given(method("DELETE"))
        .and(path(format!("/api/v1/keys/{id}")))
        .and(query_param("permanent", "true"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Cannot delete this API Key"
        })))
        .mount(&mock_server)
        .await;

    let result = api_key_api
        .delete_api_key(id, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}

#[tokio::test]
async fn personal_key_creation_errors_and_debug_never_reflect_a_one_time_secret() {
    let (client, server) = create_test_client().await;
    let api = ApiKeyApi::new(&client);
    let marker = "sk-private-response-marker";
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(503)
                .set_body_json(serde_json::json!({"error":{"message":marker}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let result = api
        .create_api_key(&CreateApiKeyRequest::new("safe"), "fixture")
        .await
        .unwrap_err();
    assert!(matches!(result, ClientError::ServiceUnavailable(_)));
    assert!(!format!("{result:?}").contains(marker));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let data:client_api::api::api_key::CreateApiKeyResponse=serde_json::from_value(serde_json::json!({
        "success":true,"key_id":"11111111-1111-4111-8111-111111111111","name":"safe","key":marker,"message":marker,"expires_at":null,"created_at":"now","never_expires":true
    })).unwrap();
    assert!(!format!("{data:?}").contains(marker));
}
#[tokio::test]
async fn personal_key_creation_rejects_inconsistent_results_without_replaying() {
    let (client, server) = create_test_client().await;
    let api = ApiKeyApi::new(&client);
    let value = serde_json::json!({"success":true,"key_id":"11111111-1111-4111-8111-111111111111","name":"safe","key":"sk-one-time-fixture","expires_at":null,"created_at":"now","never_expires":true});
    for (field, replacement) in [
        ("name", serde_json::json!("another")),
        ("key_id", serde_json::json!("not-an-id")),
        ("never_expires", serde_json::json!(false)),
        ("key", serde_json::json!("sk-danger\nheader")),
        ("success", serde_json::json!(false)),
    ] {
        server.reset().await;
        let mut body = value.clone();
        body[field] = replacement;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            api.create_api_key(
                &CreateApiKeyRequest::new("safe").with_never_expires(true),
                "fixture"
            )
            .await
            .is_err()
        );
        server.verify().await;
    }
    server.reset().await;
    for name in ["", "\n", "  "] {
        assert!(
            api.create_api_key(&CreateApiKeyRequest::new(name), "fixture")
                .await
                .is_err()
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn personal_key_control_reads_never_reuse_display_cache() {
    use client_api::{ApiClient, ClientConfig, api::api_key::ApiKeyQueryParams};
    let server = wiremock::MockServer::start().await;
    let client = ApiClient::new(
        ClientConfig::new(server.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap();
    let api = ApiKeyApi::new(&client);
    Mock::given(method("GET"))
        .and(path("/api/v1/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"keys":[],"total":0,"page":1,"page_size":20,"total_pages":0}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    for _ in 0..2 {
        api.list_my_api_keys_page(
            &ApiKeyQueryParams::new().with_page(1).with_page_size(20),
            "fixture",
        )
        .await
        .unwrap();
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}
