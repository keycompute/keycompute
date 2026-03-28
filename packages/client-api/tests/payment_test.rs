//! 支付模块集成测试

use client_api::api::payment::{CreatePaymentOrderRequest, PaymentApi, PaymentQueryParams};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

#[tokio::test]
async fn test_create_payment_order_success() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    let expected_body = serde_json::json!({
        "amount": 10.0,
        "currency": "USD",
        "payment_method": "alipay",
        "description": null
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/payments/orders"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "order_001",
            "out_trade_no": "PAY202401200001",
            "amount": 10.0,
            "currency": "USD",
            "status": "pending",
            "description": null,
            "payment_method": "alipay",
            "pay_url": "https://alipay.com/pay?order=xxx",
            "paid_at": null,
            "created_at": "2024-01-20T10:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreatePaymentOrderRequest::new(10.0, "USD", "alipay");
    let result = payment_api.create_payment_order(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let order = result.unwrap();
    assert_eq!(order.out_trade_no, "PAY202401200001");
    assert_eq!(order.amount, 10.0);
    assert_eq!(order.status, "pending");
}

#[tokio::test]
async fn test_create_payment_order_with_description() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    let expected_body = serde_json::json!({
        "amount": 50.0,
        "currency": "USD",
        "payment_method": "wechat",
        "description": "Account recharge"
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/payments/orders"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "order_002",
            "out_trade_no": "PAY202401200002",
            "amount": 50.0,
            "currency": "USD",
            "status": "pending",
            "description": "Account recharge",
            "payment_method": "wechat",
            "pay_url": "https://wechat.com/pay?order=yyy",
            "paid_at": null,
            "created_at": "2024-01-20T11:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreatePaymentOrderRequest::new(50.0, "USD", "wechat")
        .with_description("Account recharge");
    let result = payment_api.create_payment_order(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let order = result.unwrap();
    assert_eq!(order.description, Some("Account recharge".to_string()));
}

#[tokio::test]
async fn test_create_payment_order_invalid_amount() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/payments/orders"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "Invalid amount"
        })))
        .mount(&mock_server)
        .await;

    let req = CreatePaymentOrderRequest::new(-5.0, "USD", "alipay");
    let result = payment_api.create_payment_order(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_my_payment_orders_success() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/payments/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "order_001",
                "out_trade_no": "PAY202401200001",
                "amount": 10.0,
                "currency": "USD",
                "status": "paid",
                "created_at": "2024-01-20T10:00:00Z"
            },
            {
                "id": "order_002",
                "out_trade_no": "PAY202401190001",
                "amount": 50.0,
                "currency": "USD",
                "status": "pending",
                "created_at": "2024-01-19T10:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = payment_api.list_my_payment_orders(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let orders = result.unwrap();
    assert_eq!(orders.len(), 2);
    assert_eq!(orders[0].status, "paid");
    assert_eq!(orders[1].status, "pending");
}

#[tokio::test]
async fn test_list_my_payment_orders_with_status_filter() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/payments/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let params = PaymentQueryParams::new()
        .with_status("paid")
        .with_limit(5);

    let result = payment_api.list_my_payment_orders(Some(&params), fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_get_payment_order_success() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/payments/orders/order_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "order_001",
            "out_trade_no": "PAY202401200001",
            "amount": 10.0,
            "currency": "USD",
            "status": "paid",
            "description": null,
            "payment_method": "alipay",
            "pay_url": null,
            "paid_at": "2024-01-20T10:05:00Z",
            "created_at": "2024-01-20T10:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = payment_api.get_payment_order("order_001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let order = result.unwrap();
    assert_eq!(order.id, "order_001");
    assert_eq!(order.status, "paid");
}

#[tokio::test]
async fn test_get_payment_order_not_found() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/payments/orders/nonexistent"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "Order not found"
        })))
        .mount(&mock_server)
        .await;

    let result = payment_api.get_payment_order("nonexistent", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::NotFound(_)));
}

#[tokio::test]
async fn test_sync_payment_order_success() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/payments/sync/PAY202401200001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "order_001",
            "out_trade_no": "PAY202401200001",
            "amount": 10.0,
            "currency": "USD",
            "status": "paid",
            "description": null,
            "payment_method": "alipay",
            "pay_url": null,
            "paid_at": "2024-01-20T10:05:00Z",
            "created_at": "2024-01-20T10:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = payment_api.sync_payment_order("PAY202401200001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let order = result.unwrap();
    assert_eq!(order.status, "paid");
}

#[tokio::test]
async fn test_get_my_balance_success() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/payments/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "balance": 49.8766,
            "currency": "USD",
            "frozen_balance": 0.0
        })))
        .mount(&mock_server)
        .await;

    let result = payment_api.get_my_balance(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let balance = result.unwrap();
    assert_eq!(balance.balance, 49.8766);
    assert_eq!(balance.currency, "USD");
    assert_eq!(balance.frozen_balance, 0.0);
}

#[tokio::test]
async fn test_get_my_balance_zero() {
    let (client, mock_server) = create_test_client().await;
    let payment_api = PaymentApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/payments/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "balance": 0.0,
            "currency": "USD",
            "frozen_balance": 0.0
        })))
        .mount(&mock_server)
        .await;

    let result = payment_api.get_my_balance(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().balance, 0.0);
}
