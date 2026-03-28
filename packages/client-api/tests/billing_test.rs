//! 账单模块集成测试

use client_api::api::billing::{BillingApi, BillingQueryParams};
use client_api::error::ClientError;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_list_billing_records_success() {
    let (client, mock_server) = create_test_client().await;
    let billing_api = BillingApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/billing/records"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "bill_001",
                "user_id": fixtures::TEST_USER_ID,
                "amount": 1.2345,
                "currency": "USD",
                "description": "API Usage - Jan 2024",
                "status": "paid",
                "created_at": "2024-01-31T23:59:59Z",
                "paid_at": "2024-01-31T23:59:59Z"
            },
            {
                "id": "bill_002",
                "user_id": fixtures::TEST_USER_ID,
                "amount": 10.0,
                "currency": "USD",
                "description": "Account recharge",
                "status": "completed",
                "created_at": "2024-01-15T10:00:00Z",
                "paid_at": "2024-01-15T10:05:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = billing_api.list_billing_records(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let records = result.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].id, "bill_001");
    assert_eq!(records[0].amount, 1.2345);
}

#[tokio::test]
async fn test_list_billing_records_with_filters() {
    let (client, mock_server) = create_test_client().await;
    let billing_api = BillingApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/billing/records"))
        .and(query_param("start_date", "2024-01-01"))
        .and(query_param("end_date", "2024-01-31"))
        .and(query_param("limit", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let params = BillingQueryParams::new()
        .with_start_date("2024-01-01")
        .with_end_date("2024-01-31")
        .with_limit(10);

    let result = billing_api.list_billing_records(Some(&params), fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_list_billing_records_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let billing_api = BillingApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/billing/records"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = billing_api.list_billing_records(None, "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_get_billing_stats_success() {
    let (client, mock_server) = create_test_client().await;
    let billing_api = BillingApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/billing/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_amount": 150.50,
            "total_paid": 100.0,
            "total_unpaid": 50.50,
            "currency": "USD",
            "period": "monthly"
        })))
        .mount(&mock_server)
        .await;

    let result = billing_api.get_billing_stats(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.total_amount, 150.50);
    assert_eq!(stats.total_paid, 100.0);
    assert_eq!(stats.currency, "USD");
}

#[tokio::test]
async fn test_get_billing_stats_new_user() {
    let (client, mock_server) = create_test_client().await;
    let billing_api = BillingApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/billing/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_amount": 0.0,
            "total_paid": 0.0,
            "total_unpaid": 0.0,
            "currency": "USD",
            "period": "monthly"
        })))
        .mount(&mock_server)
        .await;

    let result = billing_api.get_billing_stats(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.total_amount, 0.0);
    assert_eq!(stats.total_paid, 0.0);
}
