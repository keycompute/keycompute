//! 用量统计模块集成测试

use client_api::api::usage::{UsageApi, UsageQueryParams};
use client_api::error::ClientError;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_get_my_usage_success() {
    let (client, mock_server) = create_test_client().await;
    let usage_api = UsageApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "usage_001",
                "user_id": fixtures::TEST_USER_ID,
                "model": "gpt-4",
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150,
                "cost": 0.0045,
                "created_at": "2024-01-15T10:00:00Z"
            },
            {
                "id": "usage_002",
                "user_id": fixtures::TEST_USER_ID,
                "model": "gpt-3.5-turbo",
                "prompt_tokens": 200,
                "completion_tokens": 100,
                "total_tokens": 300,
                "cost": 0.0004,
                "created_at": "2024-01-15T09:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = usage_api.get_my_usage(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let records = result.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].model, "gpt-4");
    assert_eq!(records[0].total_tokens, 150);
}

#[tokio::test]
async fn test_get_my_usage_with_pagination() {
    let (client, mock_server) = create_test_client().await;
    let usage_api = UsageApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/usage"))
        .and(query_param("limit", "10"))
        .and(query_param("offset", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let params = UsageQueryParams::new()
        .with_limit(10)
        .with_offset(20);
    let result = usage_api.get_my_usage(Some(&params), fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_get_my_usage_with_date_range() {
    let (client, mock_server) = create_test_client().await;
    let usage_api = UsageApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/usage"))
        .and(query_param("start_date", "2024-01-01"))
        .and(query_param("end_date", "2024-01-31"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "usage_003",
                "user_id": fixtures::TEST_USER_ID,
                "model": "gpt-4",
                "prompt_tokens": 50,
                "completion_tokens": 25,
                "total_tokens": 75,
                "cost": 0.00225,
                "created_at": "2024-01-20T10:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let params = UsageQueryParams::new()
        .with_start_date("2024-01-01")
        .with_end_date("2024-01-31");
    let result = usage_api.get_my_usage(Some(&params), fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 1);
}

#[tokio::test]
async fn test_get_my_usage_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let usage_api = UsageApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/usage"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = usage_api.get_my_usage(None, "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_get_usage_stats_success() {
    let (client, mock_server) = create_test_client().await;
    let usage_api = UsageApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/usage/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_requests": 1500,
            "total_tokens": 450000,
            "total_prompt_tokens": 300000,
            "total_completion_tokens": 150000,
            "total_cost": 1.2345,
            "period": "monthly"
        })))
        .mount(&mock_server)
        .await;

    let result = usage_api.get_usage_stats(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.total_requests, 1500);
    assert_eq!(stats.total_tokens, 450000);
    assert_eq!(stats.total_cost, 1.2345);
    assert_eq!(stats.period, "monthly");
}

#[tokio::test]
async fn test_get_usage_stats_empty() {
    let (client, mock_server) = create_test_client().await;
    let usage_api = UsageApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/usage/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_requests": 0,
            "total_tokens": 0,
            "total_prompt_tokens": 0,
            "total_completion_tokens": 0,
            "total_cost": 0.0,
            "period": "daily"
        })))
        .mount(&mock_server)
        .await;

    let result = usage_api.get_usage_stats(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.total_requests, 0);
    assert_eq!(stats.total_cost, 0.0);
}
