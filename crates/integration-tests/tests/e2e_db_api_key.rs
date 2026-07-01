//! API Key 操作测试

use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{CreateProduceAiKeyRequest, ProduceAiKey};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_api_key_operations() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_usage_log_crud cleanup should succeed");

        // 1. 创建租户和用户
        let tenant = create_test_tenant(&pool, "apikey", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "apikey", &test_id).await;

        // 2. 创建 API Key
        let key_hash = format!("hash-{}", Uuid::new_v4().simple());
        let api_key = ProduceAiKey::create(
            &pool,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: user.id,
                name: "Test API Key".to_string(),
                produce_ai_key_hash: key_hash.clone(),
                produce_ai_key_preview: "sk-test-****".to_string(),
                expires_at: None,
            },
        )
        .await;

        chain.add_step(
            "keycompute-db",
            "ProduceAiKey::create",
            format!("API Key created: {:?}", api_key.as_ref().map(|k| k.id)),
            api_key.is_ok(),
        );

        let Ok(api_key) = api_key else {
            chain.print_report();
            return;
        };

        // 3. 查找 API Key (by hash)
        let found = ProduceAiKey::find_by_hash(&pool, &key_hash).await;
        chain.add_step(
            "keycompute-db",
            "ProduceAiKey::find_by_hash",
            "API Key found by hash",
            found.is_ok() && found.as_ref().unwrap().is_some(),
        );

        // 4. 验证 API Key 有效
        let found_key = ProduceAiKey::find_by_hash(&pool, &key_hash).await;
        let is_valid = found_key
            .as_ref()
            .map(|k| k.as_ref().map(|k| k.is_valid()).unwrap_or(false))
            .unwrap_or(false);
        chain.add_step(
            "keycompute-db",
            "ProduceAiKey::is_valid",
            format!("API Key is valid: {}", is_valid),
            is_valid,
        );

        // 5. 撤销 API Key
        let revoked = api_key.revoke(&pool).await;
        chain.add_step(
            "keycompute-db",
            "ProduceAiKey::revoke",
            "API Key revoked",
            revoked.is_ok(),
        );

        // 6. 验证撤销后无效
        let revoked_key = ProduceAiKey::find_by_hash(&pool, &key_hash).await;
        let is_valid_after = revoked_key
            .as_ref()
            .map(|k| k.as_ref().map(|k| k.is_valid()).unwrap_or(true))
            .unwrap_or(true);
        chain.add_step(
            "keycompute-db",
            "verify_revoked",
            "Revoked API Key is invalid",
            !is_valid_after,
        );

        chain.print_report();
        assert!(chain.all_passed(), "API Key tests failed");
    }

    // ============================================================================
    // UsageLog 测试
    // ============================================================================
}
