//! 系统设置模块集成测试

use client_api::api::settings::SettingsApi;
use client_api::error::ClientError;
use std::collections::HashMap;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

// ==================== Admin 接口测试 ====================

#[tokio::test]
async fn test_get_system_settings_success() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "site_name": "KeyCompute",
            "maintenance_mode": false,
            "max_api_keys_per_user": 10,
            "default_quota": 1000.0
        })))
        .mount(&mock_server)
        .await;

    let result = settings_api.get_system_settings(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let settings = result.unwrap();
    assert!(settings.contains_key("site_name"));
    assert!(settings.contains_key("maintenance_mode"));
}

#[tokio::test]
async fn test_update_system_settings_success() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("PUT"))
        .and(path("/api/v1/settings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "site_name": "Updated Name",
            "maintenance_mode": true
        })))
        .mount(&mock_server)
        .await;

    let mut updates = HashMap::new();
    updates.insert("site_name".to_string(), serde_json::json!("Updated Name"));
    updates.insert("maintenance_mode".to_string(), serde_json::json!(true));

    let result = settings_api
        .update_system_settings(&updates, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let settings = result.unwrap();
    assert!(settings.contains_key("site_name"));
}

#[tokio::test]
async fn test_get_system_setting_by_key_success() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/settings/site_name"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
            "KeyCompute"
        )))
        .mount(&mock_server)
        .await;

    let result = settings_api
        .get_system_setting_by_key("site_name", fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_update_system_setting_by_key_success() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("PUT"))
        .and(path("/api/v1/settings/site_name"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
            "New Site Name"
        )))
        .mount(&mock_server)
        .await;

    let result = settings_api
        .update_system_setting_by_key("site_name", &serde_json::json!("New Site Name"), fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_get_system_settings_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/settings"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = settings_api.get_system_settings("invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_get_system_settings_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/settings"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result = settings_api.get_system_settings(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}

// ==================== 公开接口测试 ====================

#[tokio::test]
async fn test_get_public_settings_success() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/settings/public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "site_name": "KeyCompute",
            "version": "1.0.0",
            "registration_enabled": true
        })))
        .mount(&mock_server)
        .await;

    let result = settings_api.get_public_settings().await;

    assert!(result.is_ok());
    let settings = result.unwrap();
    assert!(settings.contains_key("site_name"));
    assert!(settings.contains_key("version"));
}

#[tokio::test]
async fn test_get_public_settings_empty() {
    let (client, mock_server) = create_test_client().await;
    let settings_api = SettingsApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/settings/public"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&mock_server)
        .await;

    let result = settings_api.get_public_settings().await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}
