//! 管理功能模块集成测试

use client_api::AssignableUserRole;
use client_api::api::admin::{
    AccountQueryParams, AdminApi, CalculateCostRequest, CreateAccountRequest, CreatePricingRequest,
    PendingTokenQueryParams, UpdateBalanceRequest, UpdateUserRequest,
};
use client_api::error::ClientError;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

fn account_json(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "tenant_id": "tenant_001",
        "name": format!("Account {id}"),
        "provider": "openai",
        "api_key_preview": "sk-xxx...",
        "api_base": null,
        "models": ["gpt-4"],
        "api_capabilities": ["chat_completions"],
        "rpm_limit": 60,
        "current_rpm": 0,
        "is_active": true,
        "is_healthy": true,
        "priority": 1,
        "visibility": "tenant",
        "created_at": "2024-01-01T00:00:00Z",
        "last_used_at": null
    })
}

fn pricing_json(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "tenant_id": null,
        "model_name": format!("model-{id}"),
        "billing_dimension": "openai",
        "input_price_per_1k": "0.03",
        "output_price_per_1k": "0.06",
        "currency": "USD",
        "is_default": false,
        "is_effective": true,
        "effective_from": "2024-01-01T00:00:00Z",
        "effective_until": null,
        "created_at": "2024-01-01T00:00:00Z"
    })
}

fn pending_token_json(id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "user_id": format!("user-{id}"),
        "token_preview": "ngt_xxx...",
        "status": "pending",
        "issued_at": "2024-01-01T00:00:00Z",
        "user_email": format!("{id}@example.com")
    })
}

// ==================== 用户管理测试 ====================

#[tokio::test]
async fn test_list_all_users_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "users": [
                {
                    "id": "user_001",
                    "email": "admin@example.com",
                    "name": "Admin User",
                    "role": "admin",
                    "tenant_id": "tenant_001",
                    "tenant_name": "Test Tenant",
                    "balance": 100.0,
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-15T00:00:00Z",
                    "last_login_at": null
                },
                {
                    "id": "user_002",
                    "email": "user@example.com",
                    "name": "Regular User",
                    "role": "user",
                    "tenant_id": "tenant_001",
                    "tenant_name": "Test Tenant",
                    "balance": 50.0,
                    "created_at": "2024-01-10T00:00:00Z",
                    "updated_at": "2024-01-20T00:00:00Z",
                    "last_login_at": null
                }
            ],
            "total": 2,
            "page": 1,
            "page_size": 10,
            "total_pages": 1
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api
        .list_all_users(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let users = result.unwrap();
    assert_eq!(users.users.len(), 2);
    assert_eq!(users.users[0].email, "admin@example.com");
    assert_eq!(users.users[0].role, "admin");
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
            "tenant_name": "Test Tenant",
            "balance": 75.5,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-15T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api
        .get_user_by_id("user_001", fixtures::TEST_ACCESS_TOKEN)
        .await;

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
            "success": true,
            "message": "User updated successfully",
            "user_id": "user_001",
            "email": "updated@example.com",
            "name": "Updated Name",
            "role": "admin"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateUserRequest::new()
        .with_name("Updated Name")
        .with_role(AssignableUserRole::Admin);
    let result = admin_api
        .update_user("user_001", &req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let user = result.unwrap();
    assert!(user.success);
    assert_eq!(user.user_id, "user_001");
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

    let result = admin_api
        .delete_user("user_001", fixtures::TEST_ACCESS_TOKEN)
        .await;

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
            "success": true,
            "message": "Balance updated",
            "user_id": "user_001",
            "amount": "50.00",
            "reason": "Admin recharge",
            "balance_before": "100.00",
            "new_balance": "150.00",
            "updated_by": "admin_001"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateBalanceRequest::add(50.0, "Admin recharge");
    let result = admin_api
        .update_user_balance("user_001", &req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    let resp = result.unwrap();
    assert!(resp.success);
    assert_eq!(resp.new_balance.as_deref(), Some("150.00"));
    assert_eq!(resp.available_balance_before.as_deref(), Some("100.00"));
}

// ==================== 账号管理测试 ====================

#[tokio::test]
async fn test_list_accounts_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accounts": [
            {
                "id": "account_001",
                "tenant_id": "tenant_001",
                "name": "OpenAI Account",
                "provider": "openai",
                "api_key_preview": "sk-xxx...",
                "api_base": null,
                "models": ["gpt-4", "gpt-3.5-turbo"],
                "api_capabilities": ["chat_completions", "responses"],
                "rpm_limit": 60,
                "current_rpm": 10,
                "is_active": true,
                "is_healthy": true,
                "priority": 1,
                "visibility": "tenant",
                "created_at": "2024-01-01T00:00:00Z",
                "last_used_at": null
            },
            {
                "id": "account_002",
                "tenant_id": "tenant_001",
                "name": "Anthropic Account",
                "provider": "anthropic",
                "api_key_preview": "sk-ant-xxx...",
                "api_base": null,
                "models": ["claude-3-opus"],
                "api_capabilities": ["messages"],
                "rpm_limit": 30,
                "current_rpm": 5,
                "is_active": true,
                "is_healthy": true,
                "priority": 2,
                "visibility": "tenant",
                "created_at": "2024-01-10T00:00:00Z",
                "last_used_at": null
            }
            ],
            "total": 2,
            "page": 1,
            "page_size": 20,
            "total_pages": 1
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api
        .list_accounts(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    let accounts = result.unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].provider, "openai");
}

#[tokio::test]
async fn test_list_accounts_without_pagination_collects_all_filtered_pages() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .and(query_param("search", "Open AI"))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accounts": [account_json("account_001")],
            "total": 2,
            "page": 1,
            "page_size": 100,
            "total_pages": 2
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .and(query_param("search", "Open AI"))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accounts": [account_json("account_002")],
            "total": 2,
            "page": 2,
            "page_size": 100,
            "total_pages": 2
        })))
        .mount(&mock_server)
        .await;

    let params = AccountQueryParams::new().with_search("Open AI");
    let accounts = admin_api
        .list_accounts(Some(&params), fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();

    assert_eq!(
        accounts
            .iter()
            .map(|account| account.id.as_str())
            .collect::<Vec<_>>(),
        ["account_001", "account_002"]
    );
}

#[tokio::test]
async fn test_list_accounts_respects_explicit_page() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/accounts"))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accounts": [account_json("account_002")],
            "total": 3,
            "page": 2,
            "page_size": 1,
            "total_pages": 3
        })))
        .mount(&mock_server)
        .await;

    let params = AccountQueryParams::new().with_page(2).with_page_size(1);
    let accounts = admin_api
        .list_accounts(Some(&params), fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();

    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].id, "account_002");
}

#[tokio::test]
async fn test_create_account_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "account_new_001",
            "tenant_id": "tenant_001",
            "name": "New Gemini Account",
            "provider": "gemini",
            "api_key_preview": "AIza...",
            "api_base": null,
            "models": ["gemini-pro"],
            "api_capabilities": ["chat_completions"],
            "rpm_limit": 60,
            "current_rpm": 0,
            "is_active": true,
            "is_healthy": true,
            "priority": 1,
            "visibility": "tenant",
            "created_at": "2024-01-20T00:00:00Z",
            "last_used_at": null
        })))
        .mount(&mock_server)
        .await;

    let req = CreateAccountRequest::new("New Gemini Account", "gemini", "api_key_here");
    let result = admin_api
        .create_account(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

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

    let result = admin_api
        .delete_account("account_001", fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn test_refresh_account_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/accounts/account_001/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "message": "Account refreshed",
            "account_id": "account_001",
            "refreshed_by": "admin_001",
            "previous_models": ["gpt-4o"],
            "updated_models": ["gpt-4o", "gpt-4o-mini"]
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api
        .refresh_account("account_001", fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok(), "Expected Ok, got {result:?}");
    let response = result.unwrap();
    assert!(response.success);
    assert_eq!(response.account_id, "account_001");
    assert_eq!(response.previous_models, ["gpt-4o"]);
    assert_eq!(response.updated_models, ["gpt-4o", "gpt-4o-mini"]);
}

// ==================== 定价管理测试 ====================

#[tokio::test]
async fn test_list_pricing_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/pricing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "pricing": [
            {
                "id": "pricing_001",
                "tenant_id": null,
                "model_name": "gpt-4",
                "billing_dimension": "openai",
                "input_price_per_1k": "0.03",
                "output_price_per_1k": "0.06",
                "currency": "USD",
                "is_default": true,
                "is_effective": true,
                "effective_from": "2024-01-01T00:00:00Z",
                "effective_until": null,
                "created_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "pricing_002",
                "tenant_id": null,
                "model_name": "gpt-3.5-turbo",
                "billing_dimension": "openai",
                "input_price_per_1k": "0.0015",
                "output_price_per_1k": "0.002",
                "currency": "USD",
                "is_default": true,
                "is_effective": true,
                "effective_from": "2024-01-01T00:00:00Z",
                "effective_until": null,
                "created_at": "2024-01-01T00:00:00Z"
            }
            ],
            "total": 2,
            "page": 1,
            "page_size": 20,
            "total_pages": 1
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api.list_pricing(fixtures::TEST_ACCESS_TOKEN).await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    let pricing = result.unwrap();
    assert_eq!(pricing.len(), 2);
    assert_eq!(pricing[0].model_name, "gpt-4");
}

#[tokio::test]
async fn test_list_pricing_collects_all_pages() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    for (page, id) in [(1, "pricing_001"), (2, "pricing_002")] {
        Mock::given(method("GET"))
            .and(path("/api/v1/pricing"))
            .and(query_param("page", page.to_string()))
            .and(query_param("page_size", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "pricing": [pricing_json(id)],
                "total": 2,
                "page": page,
                "page_size": 100,
                "total_pages": 2
            })))
            .mount(&mock_server)
            .await;
    }

    let pricing = admin_api
        .list_pricing(fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();

    assert_eq!(
        pricing
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["pricing_001", "pricing_002"]
    );
}

#[tokio::test]
async fn test_list_pending_tokens_collects_all_pages() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    for (page, id) in [(1, "token_001"), (2, "token_002")] {
        Mock::given(method("GET"))
            .and(path("/api/v1/admin/node-gateway/tokens/pending"))
            .and(query_param("page", page.to_string()))
            .and(query_param("page_size", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tokens": [pending_token_json(id)],
                "total": 2,
                "page": page,
                "page_size": 100,
                "total_pages": 2
            })))
            .mount(&mock_server)
            .await;
    }

    let tokens = admin_api
        .list_pending_tokens(fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();

    assert_eq!(
        tokens
            .iter()
            .map(|token| token.id.as_str())
            .collect::<Vec<_>>(),
        ["token_001", "token_002"]
    );
}

#[tokio::test]
async fn test_list_pending_tokens_page_sends_search_and_pagination() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/admin/node-gateway/tokens/pending"))
        .and(query_param("search", "alice+ops@example.com"))
        .and(query_param("page", "2"))
        .and(query_param("page_size", "25"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tokens": [pending_token_json("token_002")],
            "total": 26,
            "page": 2,
            "page_size": 25,
            "total_pages": 2
        })))
        .mount(&mock_server)
        .await;

    let params = PendingTokenQueryParams::default()
        .with_search("alice+ops@example.com")
        .with_page(2)
        .with_page_size(25);
    let page = admin_api
        .list_pending_tokens_page(&params, fixtures::TEST_ACCESS_TOKEN)
        .await
        .unwrap();

    assert_eq!(page.page, 2);
    assert_eq!(page.page_size, 25);
    assert_eq!(page.total, 26);
    assert_eq!(page.tokens[0].id, "token_002");
}

#[tokio::test]
async fn test_create_pricing_success() {
    let (client, mock_server) = create_test_client().await;
    let admin_api = AdminApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/pricing"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "message": "Pricing created",
            "pricing_id": "pricing_new_001",
            "model_name": "claude-3-opus",
            "billing_dimension": "anthropic",
            "input_price_per_1k": "0.015",
            "output_price_per_1k": "0.075",
            "is_default": false
        })))
        .mount(&mock_server)
        .await;

    let req = CreatePricingRequest::new("claude-3-opus", "anthropic", "0.015", "0.075", "USD");
    let result = admin_api
        .create_pricing(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    assert_eq!(result.unwrap().model_name, "claude-3-opus");
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

    let result = admin_api
        .delete_pricing("pricing_001", fixtures::TEST_ACCESS_TOKEN)
        .await;

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
    let result = admin_api
        .calculate_cost(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "orders": [
                {
                    "id": "order_001",
                    "tenant_id": null,
                    "user_id": "user_001",
                    "out_trade_no": "PAY202401200001",
                    "trade_no": null,
                    "amount": "100.00",
                    "status": "paid",
                    "subject": null,
                    "created_at": "2024-01-20T10:00:00Z"
                },
                {
                    "id": "order_002",
                    "tenant_id": null,
                    "user_id": "user_002",
                    "out_trade_no": "PAY202401190001",
                    "trade_no": null,
                    "amount": "50.00",
                    "status": "pending",
                    "subject": null,
                    "created_at": "2024-01-19T10:00:00Z"
                }
            ],
            "page": 1,
            "page_size": 20
        })))
        .mount(&mock_server)
        .await;

    let result = admin_api
        .list_all_payment_orders(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

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

    let result = admin_api
        .list_accounts(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}
