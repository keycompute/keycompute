//! 用户自服务模块集成测试

use client_api::api::user::{
    ChangePasswordRequest, UpdateProfileRequest, UserApi,
};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_get_current_user_success() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": fixtures::TEST_USER_ID,
            "email": fixtures::TEST_EMAIL,
            "name": "Test User",
            "role": "user",
            "tenant_id": "tenant_001",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-15T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = user_api.get_current_user(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let user = result.unwrap();
    assert_eq!(user.id, fixtures::TEST_USER_ID);
    assert_eq!(user.email, fixtures::TEST_EMAIL);
    assert_eq!(user.name, Some("Test User".to_string()));
    assert_eq!(user.role, "user");
    assert_eq!(user.tenant_id, "tenant_001");
}

#[tokio::test]
async fn test_get_current_user_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Invalid or expired token"
        })))
        .mount(&mock_server)
        .await;

    let result = user_api.get_current_user("invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_update_profile_success() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "Updated Name"
    });

    Mock::given(method("PUT"))
        .and(path("/api/v1/me/profile"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": fixtures::TEST_USER_ID,
            "email": fixtures::TEST_EMAIL,
            "name": "Updated Name",
            "role": "user",
            "tenant_id": "tenant_001",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateProfileRequest::new().with_name("Updated Name");
    let result = user_api.update_profile(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let user = result.unwrap();
    assert_eq!(user.name, Some("Updated Name".to_string()));
}

#[tokio::test]
async fn test_update_profile_clear_name() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    let expected_body = serde_json::json!({
        "name": null
    });

    Mock::given(method("PUT"))
        .and(path("/api/v1/me/profile"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": fixtures::TEST_USER_ID,
            "email": fixtures::TEST_EMAIL,
            "name": null,
            "role": "user",
            "tenant_id": "tenant_001",
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateProfileRequest::new(); // name is None
    let result = user_api.update_profile(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().name, None);
}

#[tokio::test]
async fn test_change_password_success() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    let expected_body = serde_json::json!({
        "current_password": "OldPass123!",
        "new_password": "NewPass456!"
    });

    Mock::given(method("PUT"))
        .and(path("/api/v1/me/password"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Password changed successfully"
        })))
        .mount(&mock_server)
        .await;

    let req = ChangePasswordRequest::new("OldPass123!", "NewPass456!");
    let result = user_api.change_password(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().message, "Password changed successfully");
}

#[tokio::test]
async fn test_change_password_wrong_current() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    Mock::given(method("PUT"))
        .and(path("/api/v1/me/password"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "Current password is incorrect"
        })))
        .mount(&mock_server)
        .await;

    let req = ChangePasswordRequest::new("WrongPass!", "NewPass456!");
    let result = user_api.change_password(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_change_password_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let user_api = UserApi::new(&client);

    Mock::given(method("PUT"))
        .and(path("/api/v1/me/password"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Authentication required"
        })))
        .mount(&mock_server)
        .await;

    let req = ChangePasswordRequest::new("OldPass123!", "NewPass456!");
    let result = user_api.change_password(&req, "expired_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}
