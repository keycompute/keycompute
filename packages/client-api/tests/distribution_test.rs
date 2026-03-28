//! 分销模块集成测试
//!
//! 包含用户端和 Admin 端接口测试

use client_api::api::distribution::{
    CreateDistributionRuleRequest, DistributionApi, DistributionQueryParams,
    UpdateDistributionRuleRequest,
};
use client_api::error::ClientError;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

mod common;
use common::{create_test_client, fixtures};

// ==================== 用户端接口测试 ====================

#[tokio::test]
async fn test_get_my_distribution_earnings_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me/distribution/earnings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_earnings": 150.50,
            "available_earnings": 50.00,
            "withdrawn_earnings": 80.00,
            "pending_earnings": 20.50,
            "currency": "USD",
            "referral_count": 5
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_my_distribution_earnings(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let earnings = result.unwrap();
    assert_eq!(earnings.total_earnings, 150.50);
    assert_eq!(earnings.available_earnings, 50.00);
    assert_eq!(earnings.referral_count, 5);
}

#[tokio::test]
async fn test_get_my_distribution_earnings_new_user() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me/distribution/earnings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_earnings": 0.0,
            "available_earnings": 0.0,
            "withdrawn_earnings": 0.0,
            "pending_earnings": 0.0,
            "currency": "USD",
            "referral_count": 0
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_my_distribution_earnings(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let earnings = result.unwrap();
    assert_eq!(earnings.total_earnings, 0.0);
    assert_eq!(earnings.referral_count, 0);
}

#[tokio::test]
async fn test_get_my_referrals_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me/distribution/referrals"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "user_ref_001",
                "email": "referred1@example.com",
                "name": "Referred User 1",
                "joined_at": "2024-01-15T10:00:00Z",
                "total_spent": 100.0,
                "earnings_from_referral": 10.0
            },
            {
                "id": "user_ref_002",
                "email": "referred2@example.com",
                "name": null,
                "joined_at": "2024-01-10T08:00:00Z",
                "total_spent": 50.0,
                "earnings_from_referral": 5.0
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_my_referrals(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let referrals = result.unwrap();
    assert_eq!(referrals.len(), 2);
    assert_eq!(referrals[0].email, "referred1@example.com");
    assert_eq!(referrals[0].earnings_from_referral, 10.0);
}

#[tokio::test]
async fn test_get_my_referrals_empty() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me/distribution/referrals"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_my_referrals(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_get_my_referral_code_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me/referral/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "referral_code": "ABC123XYZ",
            "referral_link": "https://keycompute.com/register?ref=ABC123XYZ"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_my_referral_code(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.referral_code, "ABC123XYZ");
    assert_eq!(
        resp.referral_link,
        "https://keycompute.com/register?ref=ABC123XYZ"
    );
}

#[tokio::test]
async fn test_generate_invite_link_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("POST"))
        .and(path("/api/v1/me/referral/invite-link"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "invite_link": "https://keycompute.com/invite/xyz789",
            "expires_at": "2024-02-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .generate_invite_link(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let resp = result.unwrap();
    assert_eq!(resp.invite_link, "https://keycompute.com/invite/xyz789");
}

// ==================== Admin 端接口测试 ====================

#[tokio::test]
async fn test_list_distribution_records_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/distribution/records"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "dist_001",
                "referrer_id": "user_001",
                "referred_id": "user_002",
                "amount": 100.0,
                "commission": 10.0,
                "status": "confirmed",
                "created_at": "2024-01-15T10:00:00Z"
            },
            {
                "id": "dist_002",
                "referrer_id": "user_003",
                "referred_id": "user_004",
                "amount": 50.0,
                "commission": 5.0,
                "status": "pending",
                "created_at": "2024-01-14T08:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .list_distribution_records(None, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let records = result.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].commission, 10.0);
    assert_eq!(records[0].status, "confirmed");
}

#[tokio::test]
async fn test_list_distribution_records_with_params() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/distribution/records"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&mock_server)
        .await;

    let params = DistributionQueryParams::new()
        .with_start_date("2024-01-01")
        .with_end_date("2024-01-31")
        .with_limit(10);

    let result = distribution_api
        .list_distribution_records(Some(&params), fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_get_distribution_stats_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/distribution/stats"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_commission": 5000.0,
            "total_referrals": 150,
            "active_referrals": 120,
            "period": "monthly"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_distribution_stats(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let stats = result.unwrap();
    assert_eq!(stats.total_commission, 5000.0);
    assert_eq!(stats.total_referrals, 150);
    assert_eq!(stats.active_referrals, 120);
}

#[tokio::test]
async fn test_list_distribution_rules_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/distribution/rules"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "id": "rule_001",
                "name": "Standard Commission",
                "commission_rate": 0.10,
                "min_purchase_amount": 10.0,
                "max_commission_amount": 100.0,
                "is_active": true,
                "created_at": "2024-01-01T00:00:00Z"
            },
            {
                "id": "rule_002",
                "name": "VIP Commission",
                "commission_rate": 0.15,
                "min_purchase_amount": null,
                "max_commission_amount": null,
                "is_active": true,
                "created_at": "2024-01-10T00:00:00Z"
            }
        ])))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .list_distribution_rules(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let rules = result.unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].commission_rate, 0.10);
    assert_eq!(rules[1].commission_rate, 0.15);
}

#[tokio::test]
async fn test_create_distribution_rule_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    let expected_body = serde_json::json!({
        "name": "New Commission Rule",
        "commission_rate": 0.20,
        "min_purchase_amount": 20.0,
        "max_commission_amount": 200.0
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/distribution/rules"))
        .and(body_json(&expected_body))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "rule_new_001",
            "name": "New Commission Rule",
            "commission_rate": 0.20,
            "min_purchase_amount": 20.0,
            "max_commission_amount": 200.0,
            "is_active": true,
            "created_at": "2024-01-20T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = CreateDistributionRuleRequest::new("New Commission Rule", 0.20)
        .with_min_purchase_amount(20.0)
        .with_max_commission_amount(200.0);

    let result = distribution_api
        .create_distribution_rule(&req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    let rule = result.unwrap();
    assert_eq!(rule.name, "New Commission Rule");
    assert_eq!(rule.commission_rate, 0.20);
}

#[tokio::test]
async fn test_update_distribution_rule_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    // UpdateDistributionRuleRequest 使用 Option 字段，序列化时会包含 null
    // 所以我们不使用 body_json 匹配器，只匹配路径和方法
    Mock::given(method("PUT"))
        .and(path("/api/v1/distribution/rules/rule_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "rule_001",
            "name": "Updated Rule Name",
            "commission_rate": 0.12,
            "min_purchase_amount": 10.0,
            "max_commission_amount": 100.0,
            "is_active": false,
            "created_at": "2024-01-01T00:00:00Z"
        })))
        .mount(&mock_server)
        .await;

    let req = UpdateDistributionRuleRequest::new()
        .with_name("Updated Rule Name")
        .with_commission_rate(0.12)
        .with_is_active(false);

    let result = distribution_api
        .update_distribution_rule("rule_001", &req, fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok(), "Expected Ok, got {:?}", result);
    let rule = result.unwrap();
    assert_eq!(rule.name, "Updated Rule Name");
    assert!(!rule.is_active);
}

#[tokio::test]
async fn test_delete_distribution_rule_success() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/distribution/rules/rule_001"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "message": "Distribution rule deleted successfully"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .delete_distribution_rule("rule_001", fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(result.is_ok());
    assert_eq!(
        result.unwrap().message,
        "Distribution rule deleted successfully"
    );
}

#[tokio::test]
async fn test_delete_distribution_rule_not_found() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("DELETE"))
        .and(path("/api/v1/distribution/rules/nonexistent"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
            "error": "Distribution rule not found"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .delete_distribution_rule("nonexistent", fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::NotFound(_)));
}

// ==================== 错误处理测试 ====================

#[tokio::test]
async fn test_distribution_admin_endpoints_forbidden() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/distribution/stats"))
        .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
            "error": "Admin access required"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_distribution_stats(fixtures::TEST_ACCESS_TOKEN)
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::Forbidden(_)));
}

#[tokio::test]
async fn test_distribution_unauthorized() {
    let (client, mock_server) = create_test_client().await;
    let distribution_api = DistributionApi::new(&client);

    Mock::given(method("GET"))
        .and(path("/api/v1/me/distribution/earnings"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
            "error": "Unauthorized"
        })))
        .mount(&mock_server)
        .await;

    let result = distribution_api
        .get_my_distribution_earnings("invalid_token")
        .await;

    assert!(matches!(result.unwrap_err(), ClientError::Unauthorized(_)));
}
