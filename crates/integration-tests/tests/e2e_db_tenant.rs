//! 租户 CRUD 测试

use integration_tests::common::VerificationChain;
use integration_tests::common::generate_test_id;
use integration_tests::db::{cleanup_test_data, create_test_pool, create_test_tenant};
use keycompute_db::Tenant;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::time::Duration;
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json,
        extract::{Path, State},
    };
    use bigdecimal::BigDecimal;
    use keycompute_auth::Permission;
    use keycompute_db::models::pricing_model::BillingDimension;
    use keycompute_db::{
        Account, CreateAccountRequest, CreateDistributionRuleRequest, CreatePaymentOrderRequest,
        CreatePricingRequest, PaymentMethod, PaymentOrder, PricingModel, ResponseAffinity,
        ResponsesIdempotencyClaim, TenantDistributionRule, UpdateUserRequest, User, UserBalance,
    };
    use keycompute_server::{AppState, AuthExtractor};
    use rust_decimal::Decimal;
    use std::{str::FromStr, sync::Arc};

    #[tokio::test]
    async fn test_tenant_crud() {
        let mut chain = VerificationChain::new();

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("test_tenant_crud cleanup should succeed");

        // 1. 创建租户
        let tenant = create_test_tenant(&pool, "crud", &test_id).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::create",
            format!("Tenant created: {} ({})", tenant.name, tenant.id),
            !tenant.id.is_nil() && tenant.status == "active",
        );

        // 2. 查找租户 (by ID)
        let found = Tenant::find_by_id(&pool, tenant.id).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::find_by_id",
            "Tenant found by ID",
            found.is_ok() && found.as_ref().unwrap().is_some(),
        );

        // 3. 查找租户 (by slug)
        let found_by_slug = Tenant::find_by_slug(&pool, &tenant.slug).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::find_by_slug",
            format!(
                "Tenant found by slug: {:?}",
                found_by_slug
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .map(|t| t.name.clone())
            ),
            found_by_slug.is_ok() && found_by_slug.as_ref().unwrap().is_some(),
        );

        // 4. 更新租户
        let update_req = keycompute_db::UpdateTenantRequest {
            name: Some("Updated Test Tenant".to_string()),
            description: Some("Updated description".to_string()),
            status: None,
            default_rpm_limit: Some(200),
            default_tpm_limit: Some(100000),
        };
        let updated = tenant.update(&pool, &update_req).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::update",
            format!(
                "Tenant updated: {:?}",
                updated.as_ref().map(|t| t.name.clone())
            ),
            updated.is_ok() && updated.as_ref().unwrap().name == "Updated Test Tenant",
        );

        // 5. 验证更新
        if let Ok(Some(t)) = Tenant::find_by_id(&pool, tenant.id).await {
            chain.add_step(
                "keycompute-db",
                "verify_update",
                format!("RPM: {}, TPM: {}", t.default_rpm_limit, t.default_tpm_limit),
                t.default_rpm_limit == 200 && t.default_tpm_limit == 100000,
            );
        }

        // 6. 查找所有租户
        let all = Tenant::find_all(&pool).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::find_all",
            format!(
                "Found {} tenants",
                all.as_ref().map(|v| v.len()).unwrap_or(0)
            ),
            all.is_ok(),
        );

        // 7. 删除租户
        let delete_result = tenant.delete(&pool).await;
        chain.add_step(
            "keycompute-db",
            "Tenant::delete",
            "Tenant deleted",
            delete_result.is_ok(),
        );

        // 8. 验证删除
        let after_delete = Tenant::find_by_id(&pool, tenant.id).await;
        chain.add_step(
            "keycompute-db",
            "verify_delete",
            "Tenant no longer exists",
            after_delete.map(|t| t.is_none()).unwrap_or(false),
        );

        chain.print_report();
        assert!(chain.all_passed(), "Tenant CRUD tests failed");
    }

    #[tokio::test]
    async fn test_tenant_delete_rejects_tenant_pricing_models() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("pricing delete guard cleanup should succeed");

        let tenant = create_test_tenant(&pool, "pricing-delete-guard", &test_id).await;
        let pricing = PricingModel::create(
            &pool,
            &CreatePricingRequest {
                tenant_id: Some(tenant.id),
                model_name: format!("pricing-delete-guard-{test_id}"),
                billing_dimension: BillingDimension::ProviderAccount,
                currency: Some("CNY".to_string()),
                input_price_per_1k: BigDecimal::from_str("0.1").unwrap(),
                output_price_per_1k: BigDecimal::from_str("0.3").unwrap(),
                is_default: Some(false),
                effective_from: None,
                effective_until: None,
            },
        )
        .await
        .expect("tenant pricing model should be created");

        let delete_result = tenant.delete(&pool).await;
        assert!(delete_result.is_err());
        assert!(
            Tenant::find_by_id(&pool, tenant.id)
                .await
                .unwrap()
                .is_some()
        );

        pricing.delete(&pool).await.unwrap();
        tenant.delete(&pool).await.unwrap();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("pricing delete guard cleanup should succeed");
    }

    #[tokio::test]
    async fn test_tenant_delete_cascades_distribution_rules() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("distribution rule cleanup should succeed");

        let tenant = create_test_tenant(&pool, "distribution-delete-cascade", &test_id).await;
        let rule = TenantDistributionRule::create(
            &pool,
            &CreateDistributionRuleRequest {
                tenant_id: tenant.id,
                beneficiary_id: uuid::Uuid::nil(),
                name: format!("distribution-delete-cascade-{test_id}"),
                description: None,
                commission_rate: BigDecimal::from_str("0.03").unwrap(),
                priority: Some(TenantDistributionRule::GLOBAL_OVERRIDE_PRIORITY),
                effective_from: None,
                effective_until: None,
            },
        )
        .await
        .expect("tenant distribution rule should be created");

        tenant
            .delete(&pool)
            .await
            .expect("tenant deletion should cascade tenant distribution rules");
        assert!(
            TenantDistributionRule::find_by_id(&pool, rule.id)
                .await
                .unwrap()
                .is_none()
        );

        cleanup_test_data(&pool, &test_id)
            .await
            .expect("distribution rule cleanup should succeed");
    }

    /// Moving the only user out of a tenant must not make its historical
    /// payment/ledger rows silently deletable through the tenant cascade.
    #[tokio::test]
    async fn test_tenant_delete_rejects_retained_financial_history_after_user_move() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("financial history cleanup should succeed");

        let source = create_test_tenant(&pool, "financial-history-source", &test_id).await;
        let target = create_test_tenant(&pool, "financial-history-target", &test_id).await;
        let user = integration_tests::db::create_test_user(
            &pool,
            source.id,
            "financial-history-user",
            &test_id,
        )
        .await;
        let order = PaymentOrder::create(
            &pool,
            &CreatePaymentOrderRequest {
                tenant_id: source.id,
                user_id: user.id,
                amount: Decimal::new(500, 2),
                subject: "retained payment history".to_string(),
                body: None,
                payment_method: PaymentMethod::WechatPay,
                payment_scene: "native".to_string(),
                expired_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            },
            &format!("TESTHISTORY{}", test_id.replace('-', "")),
            "",
        )
        .await
        .expect("historical order should be created");
        PaymentOrder::close(&pool, order.id)
            .await
            .expect("order should become a retained closed record");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO balance_transactions (tenant_id, user_id, order_id, transaction_type, amount, balance_before, balance_after, description) VALUES ($1, $2, $3, 'recharge', $4, 0, $4, 'retained payment history')",
            [
                source.id.into(),
                user.id.into(),
                order.id.into(),
                Decimal::new(500, 2).into(),
            ],
        ))
        .await
        .expect("historical balance transaction should be created");

        let current = User::find_by_id(&pool, user.id)
            .await
            .expect("user lookup should succeed")
            .expect("user should exist");
        let move_tx = pool.begin().await.expect("transaction should begin");
        current
            .update_in_tx(
                &move_tx,
                &UpdateUserRequest {
                    name: None,
                    role: None,
                    tenant_id: Some(target.id),
                },
            )
            .await
            .expect("user should move to the target tenant");
        move_tx
            .commit()
            .await
            .expect("user move transaction should commit");

        let delete_result = source.delete(&pool).await;
        assert!(matches!(
            delete_result,
            Err(keycompute_db::DbError::TenantHasFinancialHistory {
                payment_orders: 1,
                balance_transactions: 1,
                balance_reservations: 0,
            })
        ));
        assert!(
            Tenant::find_by_id(&pool, source.id)
                .await
                .unwrap()
                .is_some()
        );

        // Remove the retained records explicitly for test cleanup; production
        // deletion requires the same deliberate archival/removal decision.
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM balance_transactions WHERE order_id = $1",
            [order.id.into()],
        ))
        .await
        .expect("test ledger row should be removed");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM payment_orders WHERE id = $1",
            [order.id.into()],
        ))
        .await
        .expect("test order should be removed");
        source
            .delete(&pool)
            .await
            .expect("source tenant should delete after financial history removal");
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("financial history cleanup should succeed");
    }

    #[tokio::test]
    async fn test_tenant_delete_rejects_pending_responses_work() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("Responses deletion guard cleanup should succeed");

        let tenant = create_test_tenant(&pool, "responses-delete-guard", &test_id).await;
        let response_id = format!("resp_kc_settlement_{}", Uuid::new_v4().simple());
        ResponseAffinity::upsert_hidden_settlement(
            &pool,
            tenant.id,
            &response_id,
            "openai",
            Some("gpt-test"),
            None,
            chrono::Utc::now() + chrono::Duration::hours(1),
            serde_json::json!({
                "terminal_status": "success",
                "account_id": Uuid::nil().to_string(),
            }),
            chrono::Utc::now(),
        )
        .await
        .expect("accountless terminal settlement should be created");

        let delete_result = tenant.delete(&pool).await;
        assert!(matches!(
            delete_result,
            Err(keycompute_db::DbError::TenantHasPendingResponsesWork {
                pending_settlements: 1,
                pending_reservations: 0,
                in_progress_claims: 0,
            })
        ));
        assert!(
            Tenant::find_by_id(&pool, tenant.id)
                .await
                .unwrap()
                .is_some()
        );

        pool.execute(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "DELETE FROM response_affinities WHERE tenant_id = $1 AND response_id = $2",
            [tenant.id.into(), response_id.into()],
        ))
        .await
        .expect("test settlement should be cleared");

        let binding_id = format!("responses-delete-guard-{test_id}");
        let claim = ResponsesIdempotencyClaim::bind_for_execution(
            &pool,
            tenant.id,
            &binding_id,
            "delete-guard-fingerprint",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-test"),
            Uuid::nil(),
            Uuid::new_v4(),
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("in-progress Responses claim should be created")
        .0;

        let delete_result = tenant.delete(&pool).await;
        assert!(matches!(
            delete_result,
            Err(keycompute_db::DbError::TenantHasPendingResponsesWork {
                pending_settlements: 0,
                pending_reservations: 0,
                in_progress_claims: 1,
            })
        ));
        assert!(
            Tenant::find_by_id(&pool, tenant.id)
                .await
                .unwrap()
                .is_some()
        );

        ResponsesIdempotencyClaim::delete_unstarted_execution(
            &pool,
            tenant.id,
            &binding_id,
            claim.execution_token,
        )
        .await
        .expect("test claim should be removable before dispatch");
        tenant
            .delete(&pool)
            .await
            .expect("tenant should delete after Responses work is cleared");
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("Responses deletion guard cleanup should succeed");
    }

    /// Execution reservations are tenant-scoped even when they use a global
    /// account owned by another tenant.  Deleting the source tenant must not
    /// cascade the reservation while the request can still release it or
    /// replace it with its durable affinity.
    #[tokio::test]
    async fn test_tenant_delete_rejects_global_account_reservation() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("global reservation cleanup should succeed");

        let source = create_test_tenant(&pool, "global-reservation-source", &test_id).await;
        let owner = create_test_tenant(&pool, "global-reservation-owner", &test_id).await;
        let account = Account::create(
            &pool,
            &CreateAccountRequest {
                tenant_id: owner.id,
                provider: "openai".to_string(),
                name: format!("global-reservation-account-{test_id}"),
                endpoint: "https://example.invalid/v1".to_string(),
                upstream_api_key_encrypted: "test-key".to_string(),
                upstream_api_key_preview: "test-key".to_string(),
                rpm_limit: None,
                tpm_limit: None,
                priority: None,
                models_supported: vec!["gpt-test".to_string()],
                api_capabilities: vec!["responses".to_string()],
                pool_enabled: None,
                visibility: Some("global".to_string()),
            },
        )
        .await
        .expect("global account should be created");
        let reservation_id = format!("kc_reservation_global_{test_id}");
        ResponseAffinity::reserve_account(
            &pool,
            source.id,
            &reservation_id,
            "openai",
            account.id,
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .expect("cross-tenant reservation should be created");

        let blockers = Tenant::find_deletion_blockers(&pool, source.id)
            .await
            .expect("reservation blocker query should succeed");
        assert_eq!(blockers.pending_response_settlements, 0);
        assert_eq!(blockers.pending_response_reservations, 1);
        assert_eq!(blockers.in_progress_response_claims, 0);

        let delete_result = source.delete(&pool).await;
        assert!(matches!(
            delete_result,
            Err(keycompute_db::DbError::TenantHasPendingResponsesWork {
                pending_settlements: 0,
                pending_reservations: 1,
                in_progress_claims: 0,
            })
        ));
        assert!(
            Tenant::find_by_id(&pool, source.id)
                .await
                .unwrap()
                .is_some()
        );

        ResponseAffinity::delete_reservation(&pool, source.id, &reservation_id)
            .await
            .expect("reservation should be releasable");
        source
            .delete(&pool)
            .await
            .expect("source tenant should delete after reservation release");
        account
            .delete(&pool)
            .await
            .expect("global account owner should remain independently deletable");
        owner
            .delete(&pool)
            .await
            .expect("global account owner tenant should delete");
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("global reservation cleanup should succeed");
    }

    /// Existing affinity rows can be changed to a durable settlement without
    /// taking a lock on their parent tenant. The deletion guard must therefore
    /// lock every child row while it counts; otherwise a concurrent upsert can
    /// commit a settlement after a zero count and have it silently cascaded.
    #[tokio::test]
    async fn test_tenant_deletion_blocker_lock_serializes_settlement_upsert() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");

        let tenant = create_test_tenant(&pool, "responses-lock-race", &test_id).await;
        let account = Account::create(
            &pool,
            &CreateAccountRequest {
                tenant_id: tenant.id,
                provider: "openai".to_string(),
                name: format!("responses-lock-race-{test_id}"),
                endpoint: "https://example.invalid/v1".to_string(),
                upstream_api_key_encrypted: "test-key".to_string(),
                upstream_api_key_preview: "test-key".to_string(),
                rpm_limit: None,
                tpm_limit: None,
                priority: None,
                models_supported: vec!["gpt-test".to_string()],
                api_capabilities: vec!["responses".to_string()],
                pool_enabled: None,
                visibility: None,
            },
        )
        .await
        .expect("test account should be created");
        let response_id = format!("resp_lock_race_{test_id}");
        ResponseAffinity::upsert_route(
            &pool,
            tenant.id,
            &response_id,
            "openai",
            Some("gpt-test"),
            account.id,
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .expect("non-settled affinity should be created");

        // This is the same lock order used by the production delete handler:
        // tenant first, then the durable Responses blocker snapshot.
        let delete_tx = pool.begin().await.expect("delete transaction should begin");
        Tenant::find_by_id_for_update(&delete_tx, tenant.id)
            .await
            .expect("tenant lock should succeed")
            .expect("tenant should exist");
        let blockers = Tenant::find_deletion_blockers(&delete_tx, tenant.id)
            .await
            .expect("blocker snapshot should succeed");
        assert_eq!(blockers.pending_response_settlements, 0);

        // The upsert targets the already-existing row. The one-shot model API
        // acquires the tenant parent lock before the child upsert, so it must
        // wait either on the parent delete lock or (for an already-started
        // transaction) on the child-row lock held by find_deletion_blockers.
        let updater_pool = pool.clone();
        let response_id_for_task = response_id.clone();
        let mut updater = tokio::spawn(async move {
            ResponseAffinity::upsert_route_with_settlement(
                &updater_pool,
                tenant.id,
                &response_id_for_task,
                "openai",
                Some("gpt-test"),
                account.id,
                chrono::Utc::now() + chrono::Duration::hours(1),
                serde_json::json!({
                    "terminal_status": "success",
                    "account_id": account.id.to_string(),
                }),
                chrono::Utc::now(),
            )
            .await
        });

        #[derive(Debug, FromQueryResult)]
        struct Waiting {
            waiting: bool,
        }
        let mut updater_is_waiting = false;
        for _ in 0..200 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND (query ILIKE '%response_affinities%' OR query ILIKE '%FROM tenants%')) AS waiting".to_string(),
                ))
                .await
                .expect("lock wait probe should succeed")
                .expect("lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                updater_is_waiting = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            updater_is_waiting,
            "settlement upsert should wait on the deletion blocker snapshot"
        );

        delete_tx
            .rollback()
            .await
            .expect("delete transaction should roll back");
        tokio::time::timeout(Duration::from_secs(3), &mut updater)
            .await
            .expect("settlement upsert should finish after blocker rollback")
            .expect("settlement upsert task should not panic")
            .expect("settlement upsert should succeed");

        let blockers_after = Tenant::find_deletion_blockers(&pool, tenant.id)
            .await
            .expect("post-upsert blocker snapshot should succeed");
        assert_eq!(blockers_after.pending_response_settlements, 1);

        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM response_affinities WHERE tenant_id = $1 AND response_id = $2",
            [tenant.id.into(), response_id.into()],
        ))
        .await
        .expect("test affinity should be removed");
        account
            .delete(&pool)
            .await
            .expect("test account should be removed");
        tenant
            .delete(&pool)
            .await
            .expect("test tenant should be removed");
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
    }

    /// Responses reservations lock the requesting tenant before the selected
    /// account. Account edits that carry a tenant ID must follow the same
    /// order; otherwise an edit can hold the account while waiting for the
    /// tenant and deadlock a reservation holding the inverse pair.
    #[tokio::test]
    async fn test_account_update_and_responses_reservation_share_tenant_first_lock_order() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");

        let tenant = create_test_tenant(&pool, "account-lock-order", &test_id).await;
        let account = Account::create(
            &pool,
            &CreateAccountRequest {
                tenant_id: tenant.id,
                provider: "openai".to_string(),
                name: format!("account-lock-order-{test_id}"),
                endpoint: "https://example.invalid/v1".to_string(),
                upstream_api_key_encrypted: "test-key".to_string(),
                upstream_api_key_preview: "test-key".to_string(),
                rpm_limit: None,
                tpm_limit: None,
                priority: None,
                models_supported: vec!["gpt-test".to_string()],
                api_capabilities: vec!["responses".to_string()],
                pool_enabled: None,
                visibility: None,
            },
        )
        .await
        .expect("test account should be created");

        // Hold the tenant lock exactly as reserve_responses_execution_target
        // does before it snapshots the account.
        let reservation_tx = pool
            .begin()
            .await
            .expect("reservation transaction should begin");
        Tenant::find_by_id_for_key_share(&reservation_tx, tenant.id)
            .await
            .expect("reservation should lock the tenant")
            .expect("tenant should exist");

        let router = keycompute_db::DbRouter::single(pool.clone());
        let state = AppState::with_pool(Arc::clone(&router));
        let auth = AuthExtractor::new(Uuid::new_v4(), tenant.id, Uuid::new_v4(), "system")
            .with_permissions(vec![Permission::SystemAdmin]);
        let request = keycompute_server::handlers::admin_account::UpdateAccountRequest {
            tenant_id: Some(tenant.id),
            name: Some("updated-lock-order".to_string()),
            api_key: None,
            api_base: None,
            models: Some(vec!["gpt-test".to_string()]),
            api_capabilities: Some(vec!["responses".to_string()]),
            rpm_limit: Some(61),
            tpm_limit: Some(100001),
            is_active: Some(true),
            priority: Some(0),
            pool_enabled: None,
            visibility: Some("tenant".to_string()),
        };
        let update_task = tokio::spawn(async move {
            keycompute_server::handlers::admin_account::update_account(
                auth,
                Path(account.id),
                State(state),
                Json(request),
            )
            .await
        });

        // The update must reach its target-tenant lock while the reservation
        // owns a compatible KEY SHARE lock. This also makes the test robust to
        // connection-pool scheduling instead of relying on a fixed sleep.
        #[derive(Debug, FromQueryResult)]
        struct Waiting {
            waiting: bool,
        }
        let mut update_reached_tenant_lock = false;
        for _ in 0..300 {
            let row = pool
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query ILIKE '%FROM tenants WHERE id = $1 FOR UPDATE%') AS waiting".to_string(),
                ))
                .await
                .expect("lock wait probe should succeed")
                .expect("lock wait probe should return a row");
            if Waiting::from_query_result(&row, "")
                .expect("waiting flag should decode")
                .waiting
            {
                update_reached_tenant_lock = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            update_reached_tenant_lock,
            "account update should wait on the reservation's tenant lock"
        );

        // If the update had locked the account first, this account key-share
        // request would complete the inverse wait cycle and PostgreSQL would
        // abort one transaction as a deadlock. With tenant-first ordering it
        // succeeds immediately, then releasing the reservation lets the edit
        // continue.
        let reservation_account = tokio::time::timeout(
            Duration::from_secs(3),
            Account::find_by_id_for_key_share(&reservation_tx, account.id),
        )
        .await
        .expect("reservation account lock should not deadlock")
        .expect("reservation account snapshot should succeed")
        .expect("account should exist");
        assert_eq!(reservation_account.id, account.id);
        reservation_tx
            .rollback()
            .await
            .expect("reservation transaction should roll back");

        let update_response = tokio::time::timeout(Duration::from_secs(5), update_task)
            .await
            .expect("account update should finish after reservation release")
            .expect("account update task should not panic")
            .expect("account update should succeed");
        assert_eq!(
            update_response.0["name"],
            serde_json::json!("updated-lock-order")
        );

        let updated = Account::find_by_id(&pool, account.id)
            .await
            .expect("updated account lookup should succeed")
            .expect("updated account should exist");
        assert_eq!(updated.name, "updated-lock-order");
        assert_eq!(updated.rpm_limit, 61);
        assert_eq!(updated.tpm_limit, 100001);

        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
    }

    /// A tenant delete takes the parent lock before it reaches cascading child
    /// rows. A concurrent user move must acquire the source tenant's
    /// compatible key-share lock before it owns pending orders/balance rows;
    /// otherwise the FK update can wait on the parent while the delete waits
    /// on the child. This test keeps the delete's child lock wait open while
    /// the complete reassignment transaction runs.
    #[tokio::test]
    async fn test_tenant_delete_does_not_deadlock_with_user_reassignment() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");

        let source = create_test_tenant(&pool, "delete-move-source", &test_id).await;
        let target = create_test_tenant(&pool, "delete-move-target", &test_id).await;
        let user =
            integration_tests::db::create_test_user(&pool, source.id, "delete-move-user", &test_id)
                .await;
        UserBalance::get_or_create(&pool, source.id, user.id)
            .await
            .expect("balance should be created");
        let order = PaymentOrder::create(
            &pool,
            &CreatePaymentOrderRequest {
                tenant_id: source.id,
                user_id: user.id,
                amount: Decimal::ONE,
                subject: "tenant delete/move lock test".to_string(),
                body: None,
                payment_method: PaymentMethod::WechatPay,
                payment_scene: "native".to_string(),
                expired_at: chrono::Utc::now() + chrono::Duration::minutes(30),
            },
            &format!("TESTDELETEMOVE{}", test_id.replace('-', "")),
            "",
        )
        .await
        .expect("pending order should be created");

        // The move owns the user, target tenant, and pending order first.
        // Keeping the order lock while the delete takes the source parent
        // reproduces the only lock ordering that could form a cycle.
        let move_tx = pool.begin().await.expect("move transaction should begin");
        let locked_user = User::find_by_id_for_no_key_update(&move_tx, user.id)
            .await
            .expect("move should lock the user")
            .expect("user should exist");
        Tenant::find_by_id_for_update(&move_tx, target.id)
            .await
            .expect("move should lock the target tenant")
            .expect("target tenant should exist");
        move_tx
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM payment_orders WHERE id = $1 FOR UPDATE",
                [order.id.into()],
            ))
            .await
            .expect("move should lock the pending order");

        let delete_tx = pool.begin().await.expect("delete transaction should begin");
        Tenant::find_by_id_for_update(&delete_tx, source.id)
            .await
            .expect("delete should lock the source tenant")
            .expect("source tenant should exist");
        let (delete_started_tx, delete_started_rx) = tokio::sync::oneshot::channel();
        let delete_task = tokio::spawn(async move {
            let _ = delete_started_tx.send(());
            // A cascading tenant delete eventually takes this child lock. Use
            // the same lock explicitly so the test remains independent of
            // PostgreSQL's internal cascade plan and exposes a deterministic
            // parent -> child wait.
            let result = delete_tx
                .query_all(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT id FROM payment_orders WHERE tenant_id = $1 FOR UPDATE",
                    [source.id.into()],
                ))
                .await;
            let _ = delete_tx.rollback().await;
            result
        });
        delete_started_rx
            .await
            .expect("delete child-lock task should start");

        // Let the child-lock query reach PostgreSQL before the move attempts
        // its tenant-FK updates. The assertion below is bounded by timeout, so
        // a scheduling delay cannot leave a leaked transaction.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let move_result = tokio::time::timeout(Duration::from_secs(3), async {
            PaymentOrder::reassign_pending_for_user(&move_tx, user.id, source.id, target.id)
                .await
                .expect("pending order should move");
            UserBalance::reassign_tenant(&move_tx, user.id, target.id)
                .await
                .expect("balance should move");
            locked_user
                .update_in_tx(
                    &move_tx,
                    &UpdateUserRequest {
                        name: None,
                        role: None,
                        tenant_id: Some(target.id),
                    },
                )
                .await
                .expect("user should move");
            move_tx.commit().await.expect("move should commit");
        })
        .await;
        assert!(
            move_result.is_ok(),
            "user reassignment must not wait on the source tenant while delete waits on its order"
        );

        delete_task
            .await
            .expect("delete child-lock task should not panic")
            .expect("delete child-lock query should finish after reassignment");

        let moved = User::find_by_id(&pool, user.id)
            .await
            .expect("moved user lookup should succeed")
            .expect("moved user should remain");
        assert_eq!(moved.tenant_id, target.id);
        assert_eq!(
            PaymentOrder::find_by_id(&pool, order.id)
                .await
                .expect("moved order lookup should succeed")
                .expect("moved order should remain")
                .tenant_id,
            target.id
        );

        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cleanup should succeed");
    }

    // ============================================================================
    // 用户 CRUD 测试
    // ============================================================================
}
