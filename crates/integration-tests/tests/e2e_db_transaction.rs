//! 事务和完整业务链测试

use bigdecimal::BigDecimal;
use chrono::Utc;
use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{
    CreateProduceAiKeyRequest, CreateUsageLogRequest, ProduceAiKey, Tenant, UsageLog, User,
};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试数据库事务
    #[tokio::test]
    async fn test_database_transaction() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_cascade_delete cleanup should succeed");

        // 1. 测试事务提交
        let tx_slug = format!("test-tx-tenant-{}", test_id);
        let tenant_id = {
            let tx = pool.begin().await.expect("Failed to begin transaction");

            let tenant: Option<Tenant> = Tenant::find_by_statement(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO tenants (name, slug) VALUES ($1, $2) RETURNING *",
                ["Transaction Test Tenant".into(), tx_slug.as_str().into()],
            ))
            .one(&tx)
            .await
            .expect("Tenant insert should succeed");

            chain.add_step(
                "keycompute-db",
                "transaction_insert",
                "Insert in transaction",
                tenant.is_some(),
            );

            // 提交事务
            tx.commit().await.expect("Failed to commit transaction");

            tenant.expect("Tenant should exist").id
        };

        // 验证提交后数据存在
        let found = Tenant::find_by_id(&pool, tenant_id).await;
        chain.add_step(
            "keycompute-db",
            "verify_committed",
            "Data exists after commit",
            found.map(|t| t.is_some()).unwrap_or(false),
        );

        // 2. 测试事务回滚
        let rollback_slug = format!("test-rollback-tenant-{}", test_id);
        {
            let tx = pool.begin().await.expect("Failed to begin transaction");

            let _ = tx
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO tenants (name, slug) VALUES ($1, $2)",
                    ["Rollback Test Tenant".into(), rollback_slug.as_str().into()],
                ))
                .await; // intentionally ignoring error in test

            tx.rollback().await.expect("Failed to rollback transaction");
        }

        // 验证回滚后数据不存在
        let found = Tenant::find_by_slug(&pool, &rollback_slug).await;
        chain.add_step(
            "keycompute-db",
            "verify_rolled_back",
            "Data does not exist after rollback",
            found.map(|t| t.is_none()).unwrap_or(false),
        );

        chain.print_report();
        assert!(chain.all_passed(), "Transaction tests failed");
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
        let api_key = ProduceAiKey::create(
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

        let found_tenant = Tenant::find_by_id(&pool, found_user.tenant_id)
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
