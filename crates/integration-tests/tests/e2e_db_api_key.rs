//! API Key 操作测试

use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{CreateProduceAiKeyRequest, ProduceAiKey, Tenant};
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::time::Duration;
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
        let api_key = integration_tests::db::create_test_api_key(
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

        let api_key = api_key.expect("key fixture creation must succeed before assertions");

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
        let revoked = ProduceAiKey::revoke_owned(
            &pool,
            user.scope(),
            api_key.id,
            &keycompute_db::AuditContext {
                actor_user_id: user.id,
                credential_kind: keycompute_types::CredentialKind::Jwt,
                actor_platform_role: keycompute_types::PlatformRole::None,
                actor_tenant_role: Some(user.tenant_role),
                request_id: None,
            },
        )
        .await;
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

    /// API-key creation must acquire the tenant parent lock before locking the
    /// user and inserting the child row. Otherwise a direct tenant delete can
    /// hold the tenant lock while the create path holds the user lock, forming
    /// a parent/child deadlock during the cascade.
    #[tokio::test]
    async fn test_api_key_create_does_not_deadlock_with_tenant_delete() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");

        let tenant = create_test_tenant(&pool, "apikey-delete-race", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "apikey-delete-race", &test_id).await;

        // Hold the same parent lock used by Tenant::delete_in_tx before
        // starting a create request for a child row.
        let delete_tx = pool.begin().await.expect("delete transaction should begin");
        Tenant::find_by_id_for_update(&delete_tx, tenant.id)
            .await
            .expect("tenant lock should succeed")
            .expect("tenant should exist");

        let key_hash = format!("hash-delete-race-{}", Uuid::new_v4().simple());
        let create_pool = pool.clone();
        let create_req = CreateProduceAiKeyRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            name: "delete-race".to_string(),
            produce_ai_key_hash: key_hash,
            produce_ai_key_preview: "sk-race-****".to_string(),
            expires_at: None,
        };
        let create_task = tokio::spawn(async move {
            integration_tests::db::create_test_api_key(&create_pool, &create_req).await
        });

        #[derive(Debug, FromQueryResult)]
        struct Waiting {
            waiting: bool,
        }
        let mut create_is_waiting = false;
        for _ in 0..300 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND (query ILIKE '%produce_ai_keys%' OR query ILIKE '%FROM tenants WHERE id = $1 FOR SHARE%')) AS waiting".to_string(),
                ))
                .await
                .expect("lock wait probe should succeed")
                .expect("lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                create_is_waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            create_is_waiting,
            "API-key creation should wait on the tenant parent lock"
        );

        // Parent-first ordering lets the delete cascade complete while the
        // create is blocked; the create then observes the missing tenant and
        // fails cleanly instead of participating in a deadlock.
        tokio::time::timeout(Duration::from_secs(3), tenant.delete_in_tx(&delete_tx))
            .await
            .expect("tenant delete should not deadlock")
            .expect("tenant delete should succeed");
        delete_tx
            .commit()
            .await
            .expect("tenant delete transaction should commit");

        let create_result = tokio::time::timeout(Duration::from_secs(10), create_task)
            .await
            .expect("API-key creation should finish after tenant deletion")
            .expect("create task should not panic");
        assert!(
            create_result.is_err(),
            "creating a key for a deleted tenant must fail"
        );

        cleanup_test_data(&pool, &test_id).await.ok();
    }

    // ============================================================================
    // UsageLog 测试
    // ============================================================================
}
