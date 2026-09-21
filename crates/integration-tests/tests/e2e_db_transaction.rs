//! 事务和完整业务链测试

use bigdecimal::BigDecimal;
use chrono::Utc;
use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{CreateProduceAiKeyRequest, CreateUsageLogRequest, Tenant, UsageLog, User};
use sea_orm::TransactionTrait;
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试数据库事务
    #[tokio::test]
    async fn test_database_transaction() {
        let pool = create_test_pool().await;
        let run = generate_test_id();
        let owner = User::create(
            &pool,
            &keycompute_db::CreateUserRequest {
                email: format!("transaction-owner-{run}@example.com"),
                name: None,
            },
        )
        .await
        .unwrap();
        let audit = keycompute_db::AuditContext {
            actor_user_id: owner.id,
            credential_kind: keycompute_types::CredentialKind::Jwt,
            actor_platform_role: keycompute_types::PlatformRole::None,
            actor_tenant_role: None,
            request_id: None,
        };
        let request = |kind: &str| keycompute_db::CreateTenantRequest {
            name: kind.into(),
            slug: format!("test-{kind}-{run}"),
            description: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        };
        let committed = pool.begin().await.unwrap();
        let tenant = Tenant::create_owned(&committed, &request("commit"), owner.id, &audit)
            .await
            .unwrap();
        committed.commit().await.unwrap();
        assert!(
            Tenant::find_by_id(&pool, tenant.id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            keycompute_db::TenantMembership::find(&pool, tenant.id, owner.id)
                .await
                .unwrap()
                .is_some()
        );
        let rolled_back = pool.begin().await.unwrap();
        let aborted = Tenant::create_owned(&rolled_back, &request("rollback"), owner.id, &audit)
            .await
            .unwrap();
        assert!(
            Tenant::find_by_id(&rolled_back, aborted.id)
                .await
                .unwrap()
                .is_some()
        );
        rolled_back.rollback().await.unwrap();
        assert!(
            Tenant::find_by_id(&pool, aborted.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            keycompute_db::TenantMembership::find_any(&pool, aborted.id, owner.id)
                .await
                .unwrap()
                .is_none()
        );
        cleanup_test_data(&pool, &run).await.unwrap();
    }

    // ============================================================================
    // 完整业务链路测试
    // ============================================================================

    /// 测试完整的业务链路：租户 -> 用户 -> API Key -> UsageLog
    #[tokio::test]
    async fn test_full_business_chain() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_batch_operations cleanup should succeed");

        // 1. 创建租户
        let tenant = create_test_tenant(&pool, "full-chain", &test_id).await;
        chain.add_step(
            "keycompute-db",
            "step1_tenant",
            format!("Tenant: {} ({})", tenant.name, tenant.id),
            tenant.is_active(),
        );

        // 2. 创建用户
        let user = create_test_user(&pool, tenant.id, "full-chain", &test_id).await;
        chain.add_step(
            "keycompute-db",
            "step2_user",
            format!("User: {} ({})", user.email, user.id),
            user.tenant_id == tenant.id,
        );

        // 3. 创建 API Key
        let key_hash = format!("hash-full-chain-{}", Uuid::new_v4().simple());
        let api_key = integration_tests::db::create_test_api_key(
            &pool,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: user.id,
                name: "Full Chain Test Key".to_string(),
                produce_ai_key_hash: key_hash.clone(),
                produce_ai_key_preview: "sk-fc-****".to_string(),
                expires_at: None,
            },
        )
        .await
        .expect("Failed to create API key");

        chain.add_step(
            "keycompute-db",
            "step3_api_key",
            format!("API Key: {} ({})", api_key.name, api_key.id),
            !api_key.revoked,
        );

        // 4. 创建 UsageLog
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
                input_tokens: 2000,
                output_tokens: 1000,
                input_unit_price_snapshot: BigDecimal::from(5),
                output_unit_price_snapshot: BigDecimal::from(15),
                user_amount: BigDecimal::from(25), // (2000*5 + 1000*15) / 1000
                currency: "CNY".to_string(),
                usage_source: "provider_reported".to_string(),
                status: "success".to_string(),
                started_at: now - chrono::Duration::seconds(10),
                finished_at: now,
            },
        )
        .await
        .expect("Failed to create usage log");

        chain.add_step(
            "keycompute-db",
            "step4_usage_log",
            format!(
                "UsageLog: {} tokens, {} amount",
                usage_log.total_tokens, usage_log.user_amount
            ),
            usage_log.total_tokens == 3000,
        );

        // 5. 验证完整链路可追溯
        // 通过 request_id 找到 UsageLog -> 找到 User -> 找到 Tenant
        let found_log = UsageLog::find_by_request_id(&pool, request_id)
            .await
            .expect("Failed to find log")
            .expect("Log not found");

        let found_user = User::find_by_id(&pool, found_log.user_id)
            .await
            .expect("Failed to find user")
            .expect("User not found");

        let found_tenant = Tenant::find_by_id(&pool, tenant.id)
            .await
            .expect("Failed to find tenant")
            .expect("Tenant not found");

        chain.add_step(
            "keycompute-db",
            "step5_traceability",
            format!(
                "Traced: {} -> {} -> {}",
                found_tenant.name, found_user.email, found_log.model_name
            ),
            found_tenant.id == tenant.id
                && found_user.id == user.id
                && found_log.id == usage_log.id,
        );

        chain.print_report();
        assert!(chain.all_passed(), "Full business chain tests failed");
    }

    // ============================================================================
    // 余额冻结/解冻集成测试
    // ============================================================================
}
