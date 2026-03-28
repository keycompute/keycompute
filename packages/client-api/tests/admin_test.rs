//! 管理功能模块集成测试

use client_api::api::admin::{
    AdminApi, CalculateCostRequest, CreateAccountRequest, CreatePricingRequest,
    UpdateBalanceRequest, UpdateUserRequest,
};
use client_api::error::ClientError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

// ==================== 用户管理测试 ====================

#[tokio::test]
async fn test_list_all_users_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "user_001",
                "email": "admin@example.com",
                "name": "Admin User",
                "role": "admin",
                "tenant_id": "tenant_001",
                "balance": 100.0,
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-15T00:00:00Z"
            },
            {
                "id": "user_002",
                "email": "user@example.com",
                "name": "Regular User",
                "role": "user",
                "tenant_id": "tenant_001",
                "balance": 50.0,
                "created_at": "2024-01-10T00:00:00Z",
                "updated_at": "2024-01-20T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_all_users(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let users = result.unwrap();
    assert_eq!(users.len(), 2);
    assert_eq!(users[0].email, "admin@example.com");
    assert_eq!(users[0].role, "admin");
}

#[tokio::test]
async fn test_get_user_by_id_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/users/user_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "user_001",
            "email": "user@example.com",
            "name": "Test User",
            "role": "user",
            "tenant_id": "tenant_001",
            "balance": 75.5,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-15T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.get_user_by_id("user_001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let user = result.unwrap();
    assert_eq!(user.id, "user_001");
    assert_eq!(user.email, "user@example.com");
}

#[tokio::test]
async fn test_update_user_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("PUT"))
        .and(path("/api/v1/users/user_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "user_001",
            "email": "updated@example.com",
            "name": "Updated Name",
            "role": "admin",
            "tenant_id": "tenant_001",
            "balance": 75.5,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateUserRequest::new().with_name("Updated Name").with_role("admin");
    let result = admin_api.update_user("user_001", &req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let user = result.unwrap();
    assert_eq!(user.name, Some("Updated Name".to_string()));
}

#[tokio::test]
async fn test_delete_user_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/users/user_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "User deleted successfully"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.delete_user("user_001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    assert_eq!(result.unwrap().message, "User deleted successfully");
}

#[tokio::test]
async fn test_update_user_balance_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/users/user_001/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "user_id": "user_001",
            "balance": 150.0,
            "currency": "USD"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateBalanceRequest::add(50.0);
    let result = admin_api.update_user_balance("user_001", &req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    assert_eq!(result.unwrap().balance, 150.0);
}

// ==================== 账号管理测试 ====================

#[tokio::test]
async fn test_list_accounts_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "account_001",
                "name": "OpenAI Account",
                "provider": "openai",
                "status": "active",
                "is_active": true,
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "account_002",
                "name": "Anthropic Account",
                "provider": "anthropic",
                "status": "active",
                "is_active": true,
                "created_at": "2024-01-10T00:00:00Z",
                "updated_at": "2024-01-10T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_accounts(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    let accounts = result.unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].provider, "openai");
}

#[tokio::test]
async fn test_create_account_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "account_new_001",
            "name": "New Gemini Account",
            "provider": "gemini",
            "status": "active",
            "is_active": true,
            "created_at": "2024-01-20T00:00:00Z",
            "updated_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreateAccountRequest::new("New Gemini Account", "gemini", "api_key_here");
    let result = admin_api.create_account(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    assert_eq!(result.unwrap().provider, "gemini");
}

#[tokio::test]
async fn test_delete_account_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/accounts/account_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Account deleted successfully"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.delete_account("account_001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
}

// ==================== 定价管理测试 ====================

#[tokio::test]
async fn test_list_pricing_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/pricing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "pricing_001",
                "model": "gpt-4",
                "input_price": 0.03,
                "output_price": 0.06,
                "currency": "USD",
                "is_default": true,
                "created_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "pricing_002",
                "model": "gpt-3.5-turbo",
                "input_price": 0.0015,
                "output_price": 0.002,
                "currency": "USD",
                "is_default": true,
                "created_at": "2024-01-01T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_pricing(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    let pricing = result.unwrap();
    assert_eq!(pricing.len(), 2);
    assert_eq!(pricing[0].model, "gpt-4");
}

#[tokio::test]
async fn test_create_pricing_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/pricing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "pricing_new_001",
            "model": "claude-3-opus",
            "input_price": 0.015,
            "output_price": 0.075,
            "currency": "USD",
            "is_default": false,
            "created_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreatePricingRequest::new("claude-3-opus", 0.015, 0.075, "USD");
    let result = admin_api.create_pricing(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    assert_eq!(result.unwrap().model, "claude-3-opus");
}

#[tokio::test]
async fn test_delete_pricing_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/pricing/pricing_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Pricing deleted successfully"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.delete_pricing("pricing_001", fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_calculate_cost_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/pricing/calculate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "gpt-4",
            "input_tokens": 1000,
            "output_tokens": 500,
            "input_cost": 0.03,
            "output_cost": 0.03,
            "total_cost": 0.06,
            "currency": "USD"
        })))
        .mount(&mock_server)
        .await;

    let req = CalculateCostRequest {
        model: "gpt-4".to_string(),
        input_tokens: 1000,
        output_tokens: 500,
    };
    let result = admin_api.calculate_cost(&req, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let cost = result.unwrap();
    assert_eq!(cost.total_cost, 0.06);
}

// ==================== 支付管理测试 ====================

#[tokio::test]
async fn test_list_all_payment_orders_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/admin/payments/orders"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "order_001",
                "out_trade_no": "PAY202401200001",
                "user_id": "user_001",
                "amount": 100.0,
                "currency": "USD",
                "status": "paid",
                "created_at": "2024-01-20T10:00:00Z"
            },
            {
                "id": "order_002",
                "out_trade_no": "PAY202401190001",
                "user_id": "user_002",
                "amount": 50.0,
                "currency": "USD",
                "status": "pending",
                "created_at": "2024-01-19T10:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_all_payment_orders(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok());
    let orders = result.unwrap();
    assert_eq!(orders.len(), 2);
    assert_eq!(orders[0].user_id, "user_001");
}

// ==================== 错误处理测试 ====================

#[tokio::test]
async fn test_admin_endpoints_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/users"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_all_users(None, "invalid_token").await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}

#[tokio::test]
async fn test_admin_endpoints_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_accounts(None, fixtures::TEST_ACCESS_TOKEN).await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}
