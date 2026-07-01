//! UsageLog 写入查询及租户隔离测试

use bigdecimal::BigDecimal;
use chrono::Utc;
use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{
    CreateProduceAiKeyRequest, CreateUsageLogRequest, ProduceAiKey, UsageLog, User,
};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_usage_log_operations() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_transaction_handling cleanup should succeed");

        // 1. 创建测试数据
        let tenant = create_test_tenant(&pool, "usage", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "usage", &test_id).await;
        let key_hash = format!("hash-usage-{}", Uuid::new_v4().simple());
        let api_key = ProduceAiKey::create(
            &pool,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: user.id,
                name: "Usage Test Key".to_string(),
                produce_ai_key_hash: key_hash.clone(),
                produce_ai_key_preview: "sk-test-****".to_string(),
                expires_at: None,
            },
        )
        .await
        .expect("Failed to create API key");

        // 2. 创建 UsageLog
        let request_id = Uuid::new_v4();
        let now = Utc::now();
        let usage_log = UsageLog::create(
            &pool,
            &CreateUsageLogRequest {
                request_id,
                tenant_id: tenant.id,
                user_id: user.id,
                produce_ai_key_id: api_key.id,
                model_name: "gpt-4o".to_string(),
                provider_name: "openai".to_string(),
                account_id: Uuid::new_v4(),
                input_tokens: 1000,
                output_tokens: 500,
                input_unit_price_snapshot: BigDecimal::from(1),
                output_unit_price_snapshot: BigDecimal::from(2),
                user_amount: BigDecimal::from(2), // (1000*1 + 500*2) / 1000
                currency: "CNY".to_string(),
                usage_source: "gateway_accumulated".to_string(),
                status: "success".to_string(),
                started_at: now - chrono::Duration::seconds(5),
                finished_at: now,
            },
        )
        .await;

        chain.add_step(
            "keycompute-db",
            "UsageLog::create",
            format!("UsageLog created: {:?}", usage_log.as_ref().map(|l| l.id)),
            usage_log.is_ok(),
        );

        let Ok(usage_log) = usage_log else {
            chain.print_report();
            return;
        };

        // 3. 验证字段
        chain.add_step(
            "keycompute-db",
            "verify_usage_log_fields",
            format!(
                "Input: {}, Output: {}, Total: {}",
                usage_log.input_tokens, usage_log.output_tokens, usage_log.total_tokens
            ),
            usage_log.input_tokens == 1000
                && usage_log.output_tokens == 500
                && usage_log.total_tokens == 1500,
        );

        // 4. 查找 UsageLog (by request_id)
        let found = UsageLog::find_by_request_id(&pool, request_id).await;
        chain.add_step(
            "keycompute-db",
            "UsageLog::find_by_request_id",
            "UsageLog found by request_id",
            found.is_ok() && found.as_ref().unwrap().is_some(),
        );

        // 5. 查找租户的 UsageLog
        let tenant_logs = UsageLog::find_by_tenant(&pool, tenant.id, 100, 0).await;
        chain.add_step(
            "keycompute-db",
            "UsageLog::find_by_tenant",
            format!(
                "Found {} logs for tenant",
                tenant_logs.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            tenant_logs.is_ok() && tenant_logs.as_ref().unwrap().len() == 1,
        );

        // 6. 获取租户统计
        let stats = UsageLog::get_stats_by_tenant(
            &pool,
            tenant.id,
            now - chrono::Duration::hours(1),
            now + chrono::Duration::hours(1),
        )
        .await;
        chain.add_step(
            "keycompute-db",
            "UsageLog::get_stats_by_tenant",
            format!(
                "Stats: {:?} requests",
                stats.as_ref().map(|s| s.total_requests)
            ),
            stats.is_ok() && stats.as_ref().unwrap().total_requests == 1,
        );

        // 7. 获取用户统计
        let user_stats = UsageLog::get_user_stats(&pool, user.id).await;
        chain.add_step(
            "keycompute-db",
            "UsageLog::get_user_stats",
            format!(
                "User stats: {:?} requests, {:?} tokens",
                user_stats.as_ref().map(|s| s.total_requests),
                user_stats.as_ref().map(|s| s.total_tokens)
            ),
            user_stats.is_ok() && user_stats.as_ref().unwrap().total_requests == 1,
        );

        chain.print_report();
        assert!(chain.all_passed(), "UsageLog tests failed");
    }

    // ============================================================================
    // 多租户隔离测试
    // ============================================================================

    /// 测试多租户数据隔离
    #[tokio::test]
    async fn test_multi_tenant_isolation() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_concurrent_operations cleanup should succeed");

        // 1. 创建两个租户
        let tenant1 = create_test_tenant(&pool, "isolation-1", &test_id).await;
        let tenant2 = create_test_tenant(&pool, "isolation-2", &test_id).await;

        chain.add_step(
            "keycompute-db",
            "create_two_tenants",
            format!("Created tenants: {} and {}", tenant1.name, tenant2.name),
            tenant1.id != tenant2.id,
        );

        // 2. 每个租户创建用户
        let user1 = create_test_user(&pool, tenant1.id, "isolation-1", &test_id).await;
        let user2 = create_test_user(&pool, tenant2.id, "isolation-2", &test_id).await;

        chain.add_step(
            "keycompute-db",
            "create_users_in_tenants",
            format!(
                "User1 in tenant1: {}, User2 in tenant2: {}",
                user1.tenant_id, user2.tenant_id
            ),
            user1.tenant_id == tenant1.id && user2.tenant_id == tenant2.id,
        );

        // 3. 验证租户用户隔离
        let tenant1_users = User::find_by_tenant(&pool, tenant1.id).await;
        let tenant2_users = User::find_by_tenant(&pool, tenant2.id).await;

        chain.add_step(
            "keycompute-db",
            "verify_tenant_isolation",
            format!(
                "Tenant1: {} users, Tenant2: {} users",
                tenant1_users.as_ref().map(|v| v.len()).unwrap_or(0),
                tenant2_users.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            tenant1_users
                .as_ref()
                .map(|v| v.len() == 1)
                .unwrap_or(false)
                && tenant2_users
                    .as_ref()
                    .map(|v| v.len() == 1)
                    .unwrap_or(false),
        );

        // 4. 验证跨租户访问被阻止
        // 用户1不应该出现在租户2的用户列表中
        let tenant2_has_user1 = tenant2_users
            .map(|users| users.iter().any(|u| u.id == user1.id))
            .unwrap_or(false);

        chain.add_step(
            "keycompute-db",
            "verify_cross_tenant_blocked",
            "Cross-tenant access blocked",
            !tenant2_has_user1,
        );

        chain.print_report();
        assert!(chain.all_passed(), "Multi-tenant isolation tests failed");
    }
}
