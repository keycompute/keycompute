//! 数据库连接测试

use integration_tests::common::VerificationChain;
use integration_tests::db::create_test_pool;
use keycompute_types::{
    AttemptStatus, AttemptTraceFinish, BillingStatus, ErrorOrigin, RequestLifecycleRecorder,
    RequestStatus, RequestTraceFinish, RequestTraceStart, RouteType, StreamEndReason,
    TraceErrorCategory, TraceErrorInfo,
};
use rust_decimal::Decimal;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use uuid::Uuid;

#[cfg(test)]
mod tests {
    use super::*;

    // Every isolated schema contains the complete production baseline. Creating or dropping too
    // many of them concurrently can exhaust PostgreSQL's shared lock table on otherwise valid
    // default installations (`max_locks_per_transaction`). Keep a small amount of test
    // parallelism while bounding each schema's whole lifetime, including `DROP SCHEMA CASCADE`.
    static ISOLATED_SCHEMA_PERMITS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

    fn test_database_url() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://keycompute:change-me-strong-password@localhost:5432/keycompute".to_string()
        })
    }

    async fn create_isolated_schema() -> (
        DatabaseConnection,
        String,
        tokio::sync::SemaphorePermit<'static>,
    ) {
        let permit = ISOLATED_SCHEMA_PERMITS
            .acquire()
            .await
            .expect("isolated schema concurrency semaphore should remain open");
        let admin = Database::connect(test_database_url())
            .await
            .expect("migration test admin connection should succeed");
        let schema = format!("migration_test_{}", Uuid::new_v4().simple());
        admin
            .execute_unprepared(&format!(r#"CREATE SCHEMA "{schema}""#))
            .await
            .expect("isolated migration test schema should be created");
        (admin, schema, permit)
    }

    async fn connect_to_schema(schema: &str) -> DatabaseConnection {
        let mut options = ConnectOptions::new(test_database_url());
        options
            .max_connections(1)
            .min_connections(1)
            .set_schema_search_path(schema);
        Database::connect(options)
            .await
            .expect("isolated migration test connection should succeed")
    }

    async fn drop_isolated_schema(admin: &DatabaseConnection, schema: &str) {
        admin
            .execute_unprepared(&format!(r#"DROP SCHEMA "{schema}" CASCADE"#))
            .await
            .expect("isolated migration test schema should be removed");
    }

    async fn create_responses_test_tenant(pool: &DatabaseConnection, label: &str) -> Uuid {
        let tenant_id = Uuid::new_v4();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)",
            [
                tenant_id.into(),
                label.into(),
                format!("responses-test-{}", Uuid::new_v4().simple()).into(),
            ],
        ))
        .await
        .expect("Responses test tenant should be created");
        tenant_id
    }

    async fn create_responses_test_account(
        pool: &DatabaseConnection,
        tenant_id: Uuid,
        name: &str,
        priority: i32,
    ) -> keycompute_db::Account {
        keycompute_db::Account::create(
            pool,
            &keycompute_db::CreateAccountRequest {
                tenant_id,
                provider: "openai".to_string(),
                name: name.to_string(),
                endpoint: format!("https://{}.example/v1", name.to_ascii_lowercase()),
                upstream_api_key_encrypted: format!("encrypted-{name}"),
                upstream_api_key_preview: "test****".to_string(),
                rpm_limit: Some(60),
                tpm_limit: Some(100_000),
                priority: Some(priority),
                models_supported: vec!["gpt-test".to_string()],
                api_capabilities: vec!["responses".to_string()],
                visibility: Some("tenant".to_string()),
            },
        )
        .await
        .expect("Responses test account should be created")
    }

    async fn insert_terminal_pending_trace(
        pool: &DatabaseConnection,
        request_id: Uuid,
        received_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
    ) {
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO gateway_requests (
                request_id,tenant_id,user_id,produce_ai_key_id,protocol,request_path,
                requested_model,is_stream,route_type,status,received_at,finished_at,
                billing_status,trace_quality
            ) VALUES ($1,$2,$3,$4,'openai','/v1/chat/completions','test-model',FALSE,
                      'provider_account','succeeded',$5,$6,'pending','actual')"#,
            [
                request_id.into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                received_at.into(),
                finished_at.into(),
            ],
        ))
        .await
        .expect("terminal pending trace should be inserted");
    }

    async fn insert_unfinished_node_trace(
        pool: &DatabaseConnection,
        request_id: Uuid,
        attempt_id: Uuid,
        task_id: Uuid,
        received_at: chrono::DateTime<chrono::Utc>,
    ) {
        let node_id = Uuid::new_v4();
        let session_id = Uuid::new_v4();
        let lease_id = Uuid::new_v4();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO gateway_requests (
                request_id,tenant_id,user_id,produce_ai_key_id,protocol,request_path,
                requested_model,is_stream,route_type,status,received_at,billing_status,trace_quality
            ) VALUES ($1,$2,$3,$4,'openai','/v1/chat/completions','test-model',FALSE,
                      'node','running',$5,'pending','actual')"#,
            [
                request_id.into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                received_at.into(),
            ],
        ))
        .await
        .expect("unfinished node trace should be inserted");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO gateway_request_attempts (
                id,request_id,attempt_no,attempt_kind,route_type,model,status,is_final,
                node_task_id,node_id,session_id,lease_id,started_at
            ) VALUES ($1,$2,1,'primary','node','test-model','running',FALSE,$3,$4,$5,$6,$7)"#,
            [
                attempt_id.into(),
                request_id.into(),
                task_id.into(),
                node_id.into(),
                session_id.into(),
                lease_id.into(),
                received_at.into(),
            ],
        ))
        .await
        .expect("running node attempt should be inserted");
    }

    async fn delete_gateway_trace(pool: &DatabaseConnection, request_id: Uuid) {
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM usage_logs WHERE request_id=$1",
            [request_id.into()],
        ))
        .await
        .expect("test usage should be removed");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM gateway_requests WHERE request_id=$1",
            [request_id.into()],
        ))
        .await
        .expect("test trace should be removed");
    }

    #[tokio::test]
    async fn responses_conversation_discovery_candidates_are_bounded_and_tenant_private() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses discovery candidate test").await;
        let other_tenant_id =
            create_responses_test_tenant(&pool, "Responses discovery isolation test").await;

        let mut eligible = Vec::new();
        for priority in 0..10 {
            eligible.push(
                create_responses_test_account(
                    &pool,
                    tenant_id,
                    &format!("eligible-{priority}"),
                    priority,
                )
                .await,
            );
        }
        let global = create_responses_test_account(&pool, tenant_id, "global", 100).await;
        let disabled = create_responses_test_account(&pool, tenant_id, "disabled", 99).await;
        let chat_only = create_responses_test_account(&pool, tenant_id, "chat-only", 98).await;
        let _other_tenant =
            create_responses_test_account(&pool, other_tenant_id, "other-tenant", 101).await;
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE accounts SET visibility = 'global' WHERE id = $1",
            [global.id.into()],
        ))
        .await
        .expect("global candidate should be configured");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE accounts SET enabled = FALSE WHERE id = $1",
            [disabled.id.into()],
        ))
        .await
        .expect("disabled candidate should be configured");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE accounts SET api_capabilities = ARRAY['chat_completions']::TEXT[] WHERE id = $1",
            [chat_only.id.into()],
        ))
        .await
        .expect("chat-only candidate should be configured");

        let candidates = keycompute_db::Account::find_tenant_discovery_candidates(
            &pool,
            tenant_id,
            "openai",
            "responses",
            9,
        )
        .await
        .expect("bounded discovery candidates should load");
        let expected_ids = eligible
            .iter()
            .rev()
            .take(9)
            .map(|account| account.id)
            .collect::<Vec<_>>();

        assert_eq!(
            candidates
                .iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert!(candidates.iter().all(|account| {
            account.tenant_id == tenant_id
                && account.visibility == "tenant"
                && account.enabled
                && account
                    .api_capabilities
                    .iter()
                    .any(|capability| capability == "responses")
        }));

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn stale_account_probe_snapshot_does_not_overwrite_new_configuration() {
        let pool = create_test_pool().await;
        let account = keycompute_db::Account::create(
            &pool,
            &keycompute_db::CreateAccountRequest {
                tenant_id: Uuid::new_v4(),
                provider: "openai".to_string(),
                name: format!("probe-race-{}", Uuid::new_v4()),
                endpoint: "https://old.example/v1".to_string(),
                upstream_api_key_encrypted: "test-encrypted-key".to_string(),
                upstream_api_key_preview: "test****".to_string(),
                rpm_limit: Some(60),
                tpm_limit: Some(100_000),
                priority: Some(0),
                models_supported: vec!["test-model".to_string()],
                api_capabilities: vec!["chat_completions".to_string()],
                visibility: Some("tenant".to_string()),
            },
        )
        .await
        .expect("probe race account should be created");
        let new_config_version = account.updated_at + chrono::Duration::seconds(1);
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE accounts SET endpoint=$1,updated_at=$2 WHERE id=$3",
            [
                "https://new.example/v1".into(),
                new_config_version.into(),
                account.id.into(),
            ],
        ))
        .await
        .expect("account configuration should change while probe is in flight");

        let persisted = keycompute_db::Account::record_probe_snapshot_if_config_current(
            &pool,
            account.id,
            account.updated_at,
            chrono::Utc::now(),
            42,
            "failed",
            Some("upstream_http_401"),
        )
        .await
        .expect("stale probe write should be evaluated");
        assert!(!persisted, "stale probe result must be discarded");
        let current = keycompute_db::Account::find_by_id(&pool, account.id)
            .await
            .expect("account reload should succeed")
            .expect("account should still exist");
        assert_eq!(current.endpoint, "https://new.example/v1");
        assert_eq!(current.updated_at, new_config_version);
        assert!(current.last_probe_at.is_none());

        assert!(
            keycompute_db::Account::record_probe_snapshot_if_config_current(
                &pool,
                account.id,
                new_config_version,
                chrono::Utc::now(),
                7,
                "succeeded",
                None,
            )
            .await
            .expect("current probe write should succeed")
        );
        let current = keycompute_db::Account::find_by_id(&pool, account.id)
            .await
            .expect("account reload should succeed")
            .expect("account should still exist");
        assert_eq!(current.last_probe_status.as_deref(), Some("succeeded"));
        assert_eq!(current.updated_at, new_config_version);

        current
            .delete(&pool)
            .await
            .expect("probe race account should be removed");
    }

    #[tokio::test]
    async fn expired_responses_cleanup_preserves_pending_settlement_and_account_fk() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id = create_responses_test_tenant(&pool, "Responses settlement test").await;
        let account = create_responses_test_account(&pool, tenant_id, "settlement", 0).await;
        let expired_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        for response_id in ["resp_pending", "resp_settled"] {
            if response_id == "resp_pending" {
                keycompute_db::ResponseAffinity::upsert_route_with_settlement(
                    &pool,
                    tenant_id,
                    response_id,
                    "openai",
                    Some("gpt-test"),
                    account.id,
                    expired_at,
                    serde_json::json!({"request_id": Uuid::new_v4()}),
                    chrono::Utc::now(),
                )
                .await
                .expect("route and settlement should be created atomically");
            } else {
                keycompute_db::ResponseAffinity::upsert_route(
                    &pool,
                    tenant_id,
                    response_id,
                    "openai",
                    Some("gpt-test"),
                    account.id,
                    expired_at,
                )
                .await
                .expect("test Responses affinity should be created");
            }
        }

        assert_eq!(
            keycompute_db::ResponseAffinity::delete_expired(&pool)
                .await
                .expect("expired cleanup should succeed"),
            1
        );
        let pending = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT settlement IS NOT NULL AS pending FROM response_affinities \
                 WHERE tenant_id=$1 AND response_id=$2",
                [tenant_id.into(), "resp_pending".into()],
            ))
            .await
            .expect("pending affinity query should succeed")
            .expect("pending settlement affinity must survive expiry");
        assert!(pending.try_get::<bool>("", "pending").unwrap());
        assert!(
            keycompute_db::ResponseAffinity::has_pending_settlement(
                &pool,
                tenant_id,
                "resp_pending",
            )
            .await
            .expect("pending settlement lookup should succeed")
        );
        assert!(
            account.delete(&pool).await.is_err(),
            "account FK must reject deletion while settlement remains"
        );

        let claimed = keycompute_db::ResponseAffinity::claim_due_settlements(
            &pool,
            1,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("settlement should be claimable");
        let lease_until = claimed[0]
            .settlement_lease_until
            .expect("claimed settlement should carry its lease");
        assert_eq!(
            keycompute_db::ResponseAffinity::clear_claimed_settlement(
                &pool,
                tenant_id,
                "resp_pending",
                lease_until,
            )
            .await
            .expect("settlement should clear"),
            1
        );
        assert!(
            !keycompute_db::ResponseAffinity::has_pending_settlement(
                &pool,
                tenant_id,
                "resp_pending",
            )
            .await
            .expect("cleared settlement lookup should succeed")
        );
        assert_eq!(
            keycompute_db::ResponseAffinity::delete_expired(&pool)
                .await
                .expect("settled expiry cleanup should succeed"),
            1
        );
        let reservation_id = format!("kc_reservation_{}", Uuid::new_v4().simple());
        keycompute_db::ResponseAffinity::reserve_account(
            &pool,
            tenant_id,
            &reservation_id,
            "openai",
            account.id,
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .expect("active request reservation should be created");
        assert!(
            keycompute_db::ResponseAffinity::lock_account_routes_and_has_deletion_blocker(
                &pool, account.id,
            )
            .await
            .expect("reservation inspection should succeed")
        );
        assert!(
            account.clone().delete(&pool).await.is_err(),
            "account FK must reject deletion while a request reservation exists"
        );
        keycompute_db::ResponseAffinity::delete_reservation(&pool, tenant_id, &reservation_id)
            .await
            .expect("request reservation should be released");
        account
            .delete(&pool)
            .await
            .expect("account should be deletable after settlement cleanup");

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn stateless_background_settlement_is_durable_but_not_resource_visible() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Stateless background settlement test").await;
        let account = create_responses_test_account(&pool, tenant_id, "stateless", 0).await;
        let response_id = "resp_stateless_background";
        let settlement = serde_json::json!({"request_id": Uuid::new_v4()});

        keycompute_db::ResponseAffinity::upsert_hidden_settlement(
            &pool,
            tenant_id,
            response_id,
            "openai",
            Some("gpt-test"),
            Some(account.id),
            chrono::Utc::now() + chrono::Duration::hours(24),
            settlement.clone(),
            chrono::Utc::now(),
        )
        .await
        .expect("stateless background settlement should be persisted");

        assert!(
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, response_id)
                .await
                .expect("resource visibility lookup should succeed")
                .is_none(),
            "store:false must not create a retrievable KeyCompute resource"
        );
        assert!(
            keycompute_db::ResponseAffinity::has_pending_settlement(&pool, tenant_id, response_id,)
                .await
                .expect("settlement lookup should succeed")
        );
        let claimed = keycompute_db::ResponseAffinity::claim_due_settlements(
            &pool,
            1,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("settlement worker should claim tombstoned work");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].response_id, response_id);
        assert_eq!(claimed[0].settlement, Some(settlement));

        keycompute_db::ResponseAffinity::upsert_route(
            &pool,
            tenant_id,
            "resp_existing_visible",
            "openai",
            Some("gpt-test"),
            account.id,
            chrono::Utc::now() + chrono::Duration::hours(24),
        )
        .await
        .expect("visible streaming affinity should exist before terminal accounting");
        keycompute_db::ResponseAffinity::upsert_hidden_settlement(
            &pool,
            tenant_id,
            "resp_existing_visible",
            "openai",
            Some("gpt-test"),
            Some(account.id),
            chrono::Utc::now() + chrono::Duration::hours(24),
            serde_json::json!({"request_id": Uuid::new_v4()}),
            chrono::Utc::now(),
        )
        .await
        .expect("terminal outbox should attach without changing existing visibility");
        let visible =
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, "resp_existing_visible")
                .await
                .unwrap()
                .expect("terminal accounting must not hide an existing stored response");
        assert!(visible.deleted_at.is_none());
        assert!(visible.settlement.is_some());

        let hidden_lease_until = claimed[0]
            .settlement_lease_until
            .expect("claimed hidden settlement should carry its lease");
        let visible_claim = keycompute_db::ResponseAffinity::claim_due_settlements(
            &pool,
            1,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("visible terminal outbox should be claimable");
        assert_eq!(visible_claim.len(), 1);
        assert_eq!(visible_claim[0].response_id, "resp_existing_visible");
        let visible_lease_until = visible_claim[0]
            .settlement_lease_until
            .expect("claimed visible settlement should carry its lease");
        assert_eq!(
            keycompute_db::ResponseAffinity::clear_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                hidden_lease_until,
            )
            .await
            .expect("settlement completion should remove the hidden row"),
            1
        );
        assert_eq!(
            keycompute_db::ResponseAffinity::clear_claimed_settlement(
                &pool,
                tenant_id,
                "resp_existing_visible",
                visible_lease_until,
            )
            .await
            .expect("visible terminal outbox should clear"),
            1
        );
        let router = keycompute_db::DbRouter::single(pool.clone());
        keycompute_db::ResponseAffinity::delete_route_preserving_settlement(
            router.as_ref(),
            tenant_id,
            "resp_existing_visible",
        )
        .await
        .expect("test visible route should be removed");
        assert!(
            !keycompute_db::ResponseAffinity::has_pending_settlement(
                &pool,
                tenant_id,
                response_id,
            )
            .await
            .expect("completed settlement lookup should succeed")
        );
        keycompute_db::ResponseAffinity::delete_settled_account_routes(&pool, account.id)
            .await
            .expect("account cleanup should drain the completed tombstone");
        account
            .delete(&pool)
            .await
            .expect("account should be deletable after hidden settlement completes");

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn accountless_terminal_settlement_is_durable_and_claimable() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Accountless terminal settlement test").await;
        let response_id = "resp_kc_settlement_accountless";
        let settlement = serde_json::json!({
            "request_id": Uuid::new_v4(),
            "account_id": Uuid::nil(),
            "terminal_status": "success",
        });

        keycompute_db::ResponseAffinity::upsert_hidden_settlement(
            &pool,
            tenant_id,
            response_id,
            "node",
            Some("gpt-test"),
            None,
            chrono::Utc::now() + chrono::Duration::hours(24),
            settlement.clone(),
            chrono::Utc::now(),
        )
        .await
        .expect("accountless terminal settlement should be persisted");
        keycompute_db::ResponseAffinity::upsert_hidden_settlement(
            &pool,
            tenant_id,
            response_id,
            "node",
            Some("gpt-test"),
            None,
            chrono::Utc::now() + chrono::Duration::hours(24),
            settlement.clone(),
            chrono::Utc::now(),
        )
        .await
        .expect("an accountless settlement retry should converge on the same outbox row");

        let claimed = keycompute_db::ResponseAffinity::claim_due_settlements(
            &pool,
            1,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("accountless settlement should be claimable");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].response_id, response_id);
        assert_eq!(claimed[0].account_id, None);
        assert_eq!(claimed[0].settlement, Some(settlement));
        let lease_until = claimed[0]
            .settlement_lease_until
            .expect("accountless claim should carry its lease");
        assert_eq!(
            keycompute_db::ResponseAffinity::clear_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                lease_until,
            )
            .await
            .expect("accountless settlement should clear"),
            1
        );

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn stale_settlement_worker_cannot_overwrite_or_clear_a_newer_lease() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses settlement lease fencing test").await;
        let account = create_responses_test_account(&pool, tenant_id, "lease-fencing", 0).await;
        let response_id = "resp_settlement_lease_fencing";
        let original_settlement = serde_json::json!({"worker": "original"});

        keycompute_db::ResponseAffinity::upsert_route_with_settlement(
            &pool,
            tenant_id,
            response_id,
            "openai",
            Some("gpt-test"),
            account.id,
            chrono::Utc::now() + chrono::Duration::hours(1),
            original_settlement.clone(),
            chrono::Utc::now() - chrono::Duration::seconds(1),
        )
        .await
        .expect("test settlement should be persisted");

        let stale_claim = keycompute_db::ResponseAffinity::claim_due_settlements(
            &pool,
            1,
            chrono::Utc::now() - chrono::Duration::seconds(1),
        )
        .await
        .expect("first worker should claim the settlement")
        .pop()
        .expect("first claim should return the settlement");
        let stale_lease_until = stale_claim
            .settlement_lease_until
            .expect("first claim should carry its lease");
        assert_eq!(
            keycompute_db::ResponseAffinity::renew_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                stale_lease_until,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("an expired lease renewal should be rejected cleanly"),
            None,
            "an unclaimed but expired generation must not be resurrected"
        );

        let current_claim = keycompute_db::ResponseAffinity::claim_due_settlements_for(
            &pool,
            1,
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("second worker should reclaim the expired lease")
        .pop()
        .expect("second claim should return the settlement");
        let current_lease_until = current_claim
            .settlement_lease_until
            .expect("second claim should carry its lease");
        assert_ne!(stale_lease_until, current_lease_until);
        let renewed_lease_until = keycompute_db::ResponseAffinity::renew_claimed_settlement(
            &pool,
            tenant_id,
            response_id,
            current_lease_until,
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("current worker lease renewal should succeed")
        .expect("current worker should still own its lease");
        assert!(renewed_lease_until > chrono::Utc::now());
        assert_eq!(
            keycompute_db::ResponseAffinity::renew_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                current_lease_until,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("superseded lease renewal should be rejected cleanly"),
            None
        );

        assert_eq!(
            keycompute_db::ResponseAffinity::reschedule_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                serde_json::json!({"worker": "stale"}),
                chrono::Utc::now(),
                stale_lease_until,
            )
            .await
            .expect("stale reschedule should be rejected cleanly"),
            0
        );
        assert_eq!(
            keycompute_db::ResponseAffinity::clear_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                stale_lease_until,
            )
            .await
            .expect("stale clear should be rejected cleanly"),
            0
        );
        let still_current =
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, response_id)
                .await
                .expect("settlement lookup should succeed")
                .expect("newer worker's row must remain");
        assert_eq!(still_current.settlement, Some(original_settlement));
        assert_eq!(
            still_current.settlement_lease_until,
            Some(renewed_lease_until)
        );

        assert_eq!(
            keycompute_db::ResponseAffinity::relinquish_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                renewed_lease_until,
            )
            .await
            .expect("current worker should relinquish its claim"),
            1
        );
        let reclaimed = keycompute_db::ResponseAffinity::claim_due_settlements_for(
            &pool,
            1,
            std::time::Duration::from_secs(60),
        )
        .await
        .expect("relinquished work should be immediately claimable")
        .pop()
        .expect("relinquished settlement should be returned");
        let reclaimed_lease_until = reclaimed
            .settlement_lease_until
            .expect("reclaimed settlement should carry its lease");

        assert_eq!(
            keycompute_db::ResponseAffinity::reschedule_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                serde_json::json!({"worker": "current"}),
                chrono::Utc::now() - chrono::Duration::seconds(1),
                reclaimed_lease_until,
            )
            .await
            .expect("current worker should reschedule"),
            1
        );
        let final_claim = keycompute_db::ResponseAffinity::claim_due_settlements(
            &pool,
            1,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("rescheduled work should be claimable")
        .pop()
        .expect("rescheduled work should be returned");
        assert_eq!(
            keycompute_db::ResponseAffinity::clear_claimed_settlement(
                &pool,
                tenant_id,
                response_id,
                final_claim
                    .settlement_lease_until
                    .expect("final claim should carry its lease"),
            )
            .await
            .expect("current lease owner should clear the settlement"),
            1
        );

        keycompute_db::ResponseAffinity::delete_settled_account_routes(&pool, account.id)
            .await
            .expect("test route cleanup should succeed");
        account
            .delete(&pool)
            .await
            .expect("test account should be removable");
        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn startup_tpm_scan_includes_delayed_and_leased_rows_in_a_finite_snapshot() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id = create_responses_test_tenant(&pool, "Startup TPM recovery scan").await;
        let account = create_responses_test_account(&pool, tenant_id, "startup-tpm", 0).await;
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(2);
        let delayed_until = chrono::Utc::now() + chrono::Duration::hours(1);

        for response_id in [
            "resp_startup_tpm_a",
            "resp_startup_tpm_b",
            "resp_startup_tpm_after_cutoff",
        ] {
            keycompute_db::ResponseAffinity::upsert_route_with_settlement(
                &pool,
                tenant_id,
                response_id,
                "openai",
                Some("gpt-test"),
                account.id,
                expires_at,
                serde_json::json!({"request_id": Uuid::new_v4()}),
                delayed_until,
            )
            .await
            .expect("startup recovery settlement should be persisted");
        }
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities SET settlement_lease_until = $1 \
             WHERE tenant_id = $2 AND response_id IN ($3, $4)",
            [
                delayed_until.into(),
                tenant_id.into(),
                "resp_startup_tpm_a".into(),
                "resp_startup_tpm_b".into(),
            ],
        ))
        .await
        .expect("test settlements should carry an existing worker lease");
        let router = keycompute_db::DbRouter::single(pool.clone());
        let cutoff = keycompute_db::ResponseAffinity::settlement_claim_cutoff(router.as_ref())
            .await
            .expect("startup recovery cutoff should use the writer clock");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities SET updated_at = $1 \
             WHERE tenant_id = $2 AND response_id = $3",
            [
                (cutoff + chrono::Duration::seconds(1)).into(),
                tenant_id.into(),
                "resp_startup_tpm_after_cutoff".into(),
            ],
        ))
        .await
        .expect("a concurrent post-cutoff update should be represented");

        let mut cursor = None;
        let mut response_ids = Vec::new();
        loop {
            let (rows, next_cursor) =
                keycompute_db::ResponseAffinity::scan_settlements_for_tpm_recovery(
                    router.as_ref(),
                    1,
                    cutoff,
                    cursor.as_ref(),
                )
                .await
                .expect("startup recovery scan should succeed");
            let count = rows.len();
            response_ids.extend(rows.into_iter().map(|row| row.response_id));
            if count < 1 {
                break;
            }
            cursor = next_cursor;
        }

        assert_eq!(
            response_ids,
            vec![
                "resp_startup_tpm_a".to_string(),
                "resp_startup_tpm_b".to_string(),
            ],
            "retry delay and an existing lease must not hide pre-cutoff TPM work, while a post-cutoff mutation keeps the scan finite"
        );
        for response_id in &response_ids {
            let row = keycompute_db::ResponseAffinity::find_active(
                router.as_ref(),
                tenant_id,
                response_id,
            )
            .await
            .expect("settlement lookup should succeed")
            .expect("startup recovery must not remove or hide the affinity");
            assert_eq!(
                row.settlement_lease_until
                    .expect("test worker lease should remain present")
                    .timestamp_micros(),
                delayed_until.timestamp_micros()
            );
            assert_eq!(
                row.settlement_next_poll_at
                    .expect("test retry delay should remain present")
                    .timestamp_micros(),
                delayed_until.timestamp_micros()
            );
        }

        drop(router);
        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn finite_settlement_claim_does_not_reclaim_a_quickly_relinquished_row() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        let worker_pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Finite Responses settlement claim test").await;
        let account = create_responses_test_account(&pool, tenant_id, "finite-claim", 0).await;
        let response_id = "resp_finite_claim_relinquish";

        keycompute_db::ResponseAffinity::upsert_route_with_settlement(
            &pool,
            tenant_id,
            response_id,
            "openai",
            Some("gpt-test"),
            account.id,
            chrono::Utc::now() + chrono::Duration::hours(1),
            serde_json::json!({"request_id": Uuid::new_v4()}),
            chrono::Utc::now(),
        )
        .await
        .expect("test settlement should be persisted");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities \
             SET settlement_next_poll_at = NOW() - INTERVAL '1 second', updated_at = NOW() \
             WHERE tenant_id = $1 AND response_id = $2",
            [tenant_id.into(), response_id.into()],
        ))
        .await
        .expect("test settlement should be made due in the database clock domain");

        // Start the worker transaction before reading the cutoff. PostgreSQL's
        // NOW() would otherwise regress updated_at to this old transaction
        // timestamp when the worker relinquishes its freshly claimed lease.
        let worker = worker_pool
            .begin()
            .await
            .expect("worker transaction should begin");
        worker
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT NOW()".to_string(),
            ))
            .await
            .expect("worker transaction timestamp should be established");
        let router = keycompute_db::DbRouter::single(pool.clone());
        let cutoff = keycompute_db::ResponseAffinity::settlement_claim_cutoff(router.as_ref())
            .await
            .expect("finite claim cutoff should come from the writer");
        let (mut claimed, cursor) =
            keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
                &worker,
                1,
                std::time::Duration::from_secs(60),
                cutoff,
                None,
            )
            .await
            .expect("due settlement should be claimed");
        assert!(cursor.is_some());
        let claimed = claimed.pop().expect("one settlement should be claimed");
        assert_eq!(claimed.response_id, response_id);
        assert_eq!(
            keycompute_db::ResponseAffinity::relinquish_claimed_settlement(
                &worker,
                tenant_id,
                response_id,
                claimed
                    .settlement_lease_until
                    .expect("claim should carry a lease"),
            )
            .await
            .expect("current worker should relinquish its lease"),
            1
        );
        worker
            .commit()
            .await
            .expect("claim and relinquish should commit");

        let (same_sweep, _) = keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
            &pool,
            1,
            std::time::Duration::from_secs(60),
            cutoff,
            None,
        )
        .await
        .expect("same-cutoff claim should execute");
        assert!(
            same_sweep.is_empty(),
            "relinquishing must not make a row eligible in the same finite sweep"
        );

        let next_cutoff = keycompute_db::ResponseAffinity::settlement_claim_cutoff(router.as_ref())
            .await
            .expect("next claim cutoff should come from the writer");
        let (mut next_sweep, _) =
            keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
                &pool,
                1,
                std::time::Duration::from_secs(60),
                next_cutoff,
                None,
            )
            .await
            .expect("next-cutoff claim should execute");
        assert_eq!(
            next_sweep
                .pop()
                .expect("next sweep should reclaim the relinquished row")
                .response_id,
            response_id
        );

        drop(router);
        drop(worker_pool);
        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn finite_settlement_keyset_and_concurrent_claims_are_complete_and_disjoint() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        let second_pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses settlement keyset test").await;
        let account = create_responses_test_account(&pool, tenant_id, "claim-keyset", 0).await;
        let response_ids = [
            "resp_claim_keyset_d",
            "resp_claim_keyset_a",
            "resp_claim_keyset_c",
            "resp_claim_keyset_b",
        ];
        for response_id in response_ids {
            keycompute_db::ResponseAffinity::upsert_route_with_settlement(
                &pool,
                tenant_id,
                response_id,
                "openai",
                Some("gpt-test"),
                account.id,
                chrono::Utc::now() + chrono::Duration::hours(1),
                serde_json::json!({"request_id": Uuid::new_v4()}),
                chrono::Utc::now(),
            )
            .await
            .expect("test settlement should be persisted");
        }
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE response_affinities \
             SET settlement_next_poll_at = NOW() - INTERVAL '1 second', updated_at = NOW() \
             WHERE tenant_id = $1 AND response_id = ANY($2)",
            [
                tenant_id.into(),
                response_ids
                    .iter()
                    .map(|value| (*value).to_string())
                    .collect::<Vec<_>>()
                    .into(),
            ],
        ))
        .await
        .expect("all test settlements should share one database poll timestamp");

        let router = keycompute_db::DbRouter::single(pool.clone());
        let cutoff = keycompute_db::ResponseAffinity::settlement_claim_cutoff(router.as_ref())
            .await
            .expect("finite claim cutoff should come from the writer");
        let (first_batch, first_cursor) =
            keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
                &pool,
                2,
                std::time::Duration::from_secs(60),
                cutoff,
                None,
            )
            .await
            .expect("first keyset batch should be claimed");
        let (second_batch, second_cursor) =
            keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
                &pool,
                2,
                std::time::Duration::from_secs(60),
                cutoff,
                first_cursor.as_ref(),
            )
            .await
            .expect("second keyset batch should be claimed");
        let (exhausted, _) = keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
            &pool,
            2,
            std::time::Duration::from_secs(60),
            cutoff,
            second_cursor.as_ref(),
        )
        .await
        .expect("exhausted keyset claim should execute");
        assert!(exhausted.is_empty());
        let expected = response_ids
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::HashSet<_>>();
        let sequential = first_batch
            .iter()
            .chain(&second_batch)
            .map(|row| row.response_id.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(sequential, expected);

        for row in first_batch.iter().chain(&second_batch) {
            assert_eq!(
                keycompute_db::ResponseAffinity::relinquish_claimed_settlement(
                    &pool,
                    row.tenant_id,
                    &row.response_id,
                    row.settlement_lease_until
                        .expect("claimed settlement should carry a lease"),
                )
                .await
                .expect("keyset claim should be relinquished"),
                1
            );
        }
        let concurrent_cutoff =
            keycompute_db::ResponseAffinity::settlement_claim_cutoff(router.as_ref())
                .await
                .expect("concurrent claim cutoff should come from the writer");
        let first_worker = pool
            .begin()
            .await
            .expect("first worker transaction should begin");
        let second_worker = second_pool
            .begin()
            .await
            .expect("second worker transaction should begin");
        let first_claim = keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
            &first_worker,
            2,
            std::time::Duration::from_secs(60),
            concurrent_cutoff,
            None,
        );
        let second_claim = keycompute_db::ResponseAffinity::claim_due_settlements_before_for(
            &second_worker,
            2,
            std::time::Duration::from_secs(60),
            concurrent_cutoff,
            None,
        );
        let (first_claim, second_claim) = tokio::join!(first_claim, second_claim);
        let (first_claim, _) = first_claim.expect("first concurrent claim should succeed");
        let (second_claim, _) = second_claim.expect("second concurrent claim should succeed");
        first_worker
            .commit()
            .await
            .expect("first worker claim should commit");
        second_worker
            .commit()
            .await
            .expect("second worker claim should commit");

        assert_eq!(first_claim.len(), 2);
        assert_eq!(second_claim.len(), 2);
        let first_ids = first_claim
            .iter()
            .map(|row| row.response_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let second_ids = second_claim
            .iter()
            .map(|row| row.response_id.clone())
            .collect::<std::collections::HashSet<_>>();
        assert!(first_ids.is_disjoint(&second_ids));
        assert_eq!(
            first_ids
                .union(&second_ids)
                .cloned()
                .collect::<std::collections::HashSet<_>>(),
            expected
        );

        drop(router);
        drop(second_pool);
        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn connection_material_update_invalidates_settled_responses_routes_atomically() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses connection update test").await;
        let account = create_responses_test_account(&pool, tenant_id, "connection-update", 0).await;
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);
        let root_warmup_id = "resp_ws_connection_update_root";
        keycompute_db::ResponseAffinity::upsert_local(
            &pool,
            tenant_id,
            root_warmup_id,
            "openai",
            None,
            serde_json::json!({"id": root_warmup_id, "model": "gpt-test"}),
            serde_json::json!({
                "items": [],
                "request_state": {},
                "upstream_previous_response_id": null
            }),
            128,
            expires_at,
        )
        .await
        .expect("root local warmup should be persisted without an account owner");
        for resource_id in ["resp_connection_update", "conv_connection_update"] {
            keycompute_db::ResponseAffinity::upsert_route(
                &pool,
                tenant_id,
                resource_id,
                "openai",
                Some("gpt-test"),
                account.id,
                expires_at,
            )
            .await
            .expect("settled resource route should be created");
        }
        let idempotency_id = "kc_idempotency_connection_update";
        keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            idempotency_id,
            "connection-update-fingerprint",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-test"),
            account.id,
        )
        .await
        .expect("idempotency binding should be created");

        let txn = pool.begin().await.expect("update transaction should begin");
        let locked = keycompute_db::Account::find_by_id_for_update(&txn, account.id)
            .await
            .expect("account lock should succeed")
            .expect("account should exist");
        assert!(
            !keycompute_db::ResponseAffinity::lock_account_routes_and_has_deletion_blocker(
                &txn, account.id,
            )
            .await
            .expect("route lock should succeed")
        );
        assert_eq!(
            keycompute_db::ResponseAffinity::delete_settled_account_routes(&txn, account.id)
                .await
                .expect("settled resource routes should be invalidated"),
            2
        );
        locked
            .update(
                &txn,
                &keycompute_db::UpdateAccountRequest {
                    tenant_id: None,
                    name: None,
                    endpoint: Some("https://replacement.example/v1".to_string()),
                    upstream_api_key_encrypted: None,
                    upstream_api_key_preview: None,
                    rpm_limit: None,
                    tpm_limit: None,
                    priority: None,
                    enabled: None,
                    models_supported: None,
                    api_capabilities: None,
                    visibility: None,
                },
            )
            .await
            .expect("account connection should update");
        txn.commit()
            .await
            .expect("update transaction should commit");

        for resource_id in ["resp_connection_update", "conv_connection_update"] {
            assert!(
                keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, resource_id)
                    .await
                    .expect("route lookup should succeed")
                    .is_none(),
                "old resource route must not survive a connection identity change"
            );
        }
        let root_warmup =
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, root_warmup_id)
                .await
                .expect("root warmup lookup should succeed")
                .expect("connection changes must preserve an ownerless root warmup");
        assert_eq!(root_warmup.account_id, None);
        assert!(root_warmup.local_response.is_some());
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
                &pool,
                tenant_id,
                idempotency_id,
            )
            .await
            .expect("idempotency lookup should succeed")
            .is_some(),
            "connection identity changes must preserve durable idempotency claims"
        );
        let updated = keycompute_db::Account::find_by_id(&pool, account.id)
            .await
            .expect("account reload should succeed")
            .expect("account should remain");
        assert_eq!(updated.endpoint, "https://replacement.example/v1");

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn account_deletion_preserves_account_independent_responses_state() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses account deletion claim test").await;
        let deleted_account =
            create_responses_test_account(&pool, tenant_id, "claim-deleted", 0).await;
        let replacement_account =
            create_responses_test_account(&pool, tenant_id, "claim-replacement", 1).await;
        let binding_id = "kc_idempotency_deleted_account";
        let billing_request_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let root_warmup_id = "resp_ws_deleted_account_root";
        keycompute_db::ResponseAffinity::upsert_local(
            &pool,
            tenant_id,
            root_warmup_id,
            "openai",
            None,
            serde_json::json!({"id": root_warmup_id, "model": "gpt-test"}),
            serde_json::json!({
                "items": [],
                "request_state": {},
                "upstream_previous_response_id": null
            }),
            128,
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .expect("root local warmup should not require an account owner");

        let (original_claim, inserted) = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            binding_id,
            "deleted-account-fingerprint",
            billing_request_id,
            user_id,
            key_id,
            "openai",
            Some("gpt-test"),
            deleted_account.id,
        )
        .await
        .expect("initial idempotency claim should persist");
        assert!(inserted);
        assert_eq!(
            keycompute_db::ResponseAffinity::delete_settled_account_routes(
                &pool,
                deleted_account.id,
            )
            .await
            .expect("account resource-route cleanup should succeed"),
            0,
            "the permanent claim must not be treated as a deletable resource route"
        );
        deleted_account
            .delete(&pool)
            .await
            .expect("a permanent claim must not prevent operational account deletion");

        let persisted = keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
            &pool, tenant_id, binding_id,
        )
        .await
        .expect("claim lookup after account deletion should succeed")
        .expect("account deletion must not erase a permanent idempotency claim");
        assert_eq!(persisted, original_claim);
        assert_eq!(
            keycompute_db::ResponseAffinity::find_active_local_response(
                &pool,
                tenant_id,
                root_warmup_id,
            )
            .await
            .expect("root warmup lookup should succeed after account deletion"),
            Some(serde_json::json!({
                "id": root_warmup_id,
                "model": "gpt-test"
            })),
            "account deletion must not erase an ownerless root warmup"
        );
        let router = keycompute_db::DbRouter::single(pool.clone());
        assert_eq!(
            keycompute_db::ResponseAffinity::delete_route_preserving_settlement(
                router.as_ref(),
                tenant_id,
                root_warmup_id,
            )
            .await
            .expect("ownerless root warmup deletion should succeed"),
            1
        );
        assert!(
            keycompute_db::ResponseAffinity::find_active_local_response(
                &pool,
                tenant_id,
                root_warmup_id,
            )
            .await
            .expect("deleted root warmup lookup should succeed")
            .is_none()
        );

        let (replayed_claim, replay_inserted) = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            binding_id,
            "different-fingerprint",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-other"),
            replacement_account.id,
        )
        .await
        .expect("replay should load the authoritative original claim");
        assert!(!replay_inserted);
        assert_eq!(replayed_claim, original_claim);
        assert_eq!(replayed_claim.account_id, original_claim.account_id);

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn responses_execution_reservation_closes_the_account_update_race() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let execution_pool = connect_to_schema(&schema).await;
        let update_pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&execution_pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&execution_pool, "Responses reservation race test").await;
        let account =
            create_responses_test_account(&execution_pool, tenant_id, "reservation-race", 0).await;
        let resource_id = "resp_reservation_race";
        let reservation_id = format!("kc_reservation_{}", Uuid::new_v4().simple());
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);
        keycompute_db::ResponseAffinity::upsert_route(
            &execution_pool,
            tenant_id,
            resource_id,
            "openai",
            Some("gpt-test"),
            account.id,
            expires_at,
        )
        .await
        .expect("resource route should be created");

        let execution_tx = execution_pool
            .begin()
            .await
            .expect("execution transaction should begin");
        keycompute_db::Account::find_by_id_for_key_share(&execution_tx, account.id)
            .await
            .expect("account lock should succeed")
            .expect("account should remain");
        let affinity = keycompute_db::ResponseAffinity::find_active_for_key_share(
            &execution_tx,
            tenant_id,
            resource_id,
        )
        .await
        .expect("affinity lock should succeed")
        .expect("resource route should remain");
        assert_eq!(affinity.account_id, Some(account.id));
        keycompute_db::ResponseAffinity::reserve_account(
            &execution_tx,
            tenant_id,
            &reservation_id,
            "openai",
            account.id,
            expires_at,
        )
        .await
        .expect("reservation should be installed in the same transaction");

        let account_id = account.id;
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let mut update = tokio::spawn(async move {
            let update_tx = update_pool
                .begin()
                .await
                .expect("update transaction should begin");
            let _ = started_tx.send(());
            keycompute_db::Account::find_by_id_for_update(&update_tx, account_id)
                .await
                .expect("account update lock should succeed")
                .expect("account should remain");
            let blocked =
                keycompute_db::ResponseAffinity::lock_account_routes_and_has_deletion_blocker(
                    &update_tx, account_id,
                )
                .await
                .expect("reservation inspection should succeed");
            update_tx
                .rollback()
                .await
                .expect("update transaction should roll back");
            blocked
        });
        started_rx.await.expect("update task should start");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut update)
                .await
                .is_err(),
            "the account update must wait for the atomic reservation transaction"
        );

        execution_tx
            .commit()
            .await
            .expect("execution reservation should commit");
        assert!(
            update.await.expect("update task should complete"),
            "the updater must observe the committed reservation and reject mutation"
        );

        keycompute_db::ResponseAffinity::delete_reservation(
            &execution_pool,
            tenant_id,
            &reservation_id,
        )
        .await
        .expect("reservation cleanup should succeed");
        account
            .delete(&execution_pool)
            .await
            .expect_err("the retained resource route should still guard the account FK");

        drop(execution_pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn responses_affinity_ids_cannot_be_reassigned_between_accounts() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses affinity collision test").await;
        let first_account = create_responses_test_account(&pool, tenant_id, "first", 1).await;
        let second_account = create_responses_test_account(&pool, tenant_id, "second", 0).await;
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);

        keycompute_db::ResponseAffinity::upsert_local(
            &pool,
            tenant_id,
            "resp_ws_ownerless_chain",
            "openai",
            None,
            serde_json::json!({"id": "resp_ws_ownerless_chain", "model": "gpt-test"}),
            serde_json::json!({
                "items": [],
                "request_state": {},
                "upstream_previous_response_id": "resp_upstream_parent"
            }),
            128,
            expires_at,
        )
        .await
        .expect_err("a chained local warmup must retain its upstream account owner");

        keycompute_db::ResponseAffinity::upsert_route(
            &pool,
            tenant_id,
            "resp_collision_route",
            "openai",
            Some("gpt-test"),
            first_account.id,
            expires_at,
        )
        .await
        .expect("first route should be persisted");
        let route_collision = keycompute_db::ResponseAffinity::upsert_route(
            &pool,
            tenant_id,
            "resp_collision_route",
            "openai",
            Some("gpt-test"),
            second_account.id,
            expires_at,
        )
        .await
        .expect_err("a second account must not take over an existing response ID");
        assert!(matches!(
            route_collision,
            keycompute_db::DbError::DuplicateKey { .. }
        ));
        assert_eq!(
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, "resp_collision_route",)
                .await
                .expect("route lookup should succeed")
                .expect("original route should remain")
                .account_id,
            Some(first_account.id)
        );

        let first_settlement = serde_json::json!({"request_id": Uuid::new_v4()});
        keycompute_db::ResponseAffinity::upsert_route_with_settlement(
            &pool,
            tenant_id,
            "resp_collision_settlement",
            "openai",
            Some("gpt-test"),
            first_account.id,
            expires_at,
            first_settlement.clone(),
            chrono::Utc::now(),
        )
        .await
        .expect("first settlement should be persisted");
        keycompute_db::ResponseAffinity::upsert_route_with_settlement(
            &pool,
            tenant_id,
            "resp_collision_settlement",
            "openai",
            Some("gpt-test"),
            second_account.id,
            expires_at,
            serde_json::json!({"request_id": Uuid::new_v4()}),
            chrono::Utc::now(),
        )
        .await
        .expect_err("a colliding settlement must not replace its owner");
        let settlement = keycompute_db::ResponseAffinity::find_active(
            &pool,
            tenant_id,
            "resp_collision_settlement",
        )
        .await
        .expect("settlement lookup should succeed")
        .expect("original settlement should remain");
        assert_eq!(settlement.account_id, Some(first_account.id));
        assert_eq!(settlement.settlement, Some(first_settlement));

        keycompute_db::ResponseAffinity::upsert_local(
            &pool,
            tenant_id,
            "resp_ws_collision",
            "openai",
            Some(first_account.id),
            serde_json::json!({"id": "resp_ws_collision", "model": "gpt-test"}),
            serde_json::json!({"items": []}),
            128,
            expires_at,
        )
        .await
        .expect("first local response should be persisted");
        keycompute_db::ResponseAffinity::upsert_local(
            &pool,
            tenant_id,
            "resp_ws_collision",
            "openai",
            Some(second_account.id),
            serde_json::json!({"id": "resp_ws_collision", "model": "gpt-other"}),
            serde_json::json!({"items": ["replacement"]}),
            256,
            expires_at,
        )
        .await
        .expect_err("a local response collision must not replace its owner");
        let local =
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, "resp_ws_collision")
                .await
                .expect("local response lookup should succeed")
                .expect("original local response should remain");
        assert_eq!(local.account_id, Some(first_account.id));
        assert_eq!(local.local_context, Some(serde_json::json!({"items": []})));
        assert_eq!(local.local_context_bytes, Some(128));
        assert_eq!(
            keycompute_db::ResponseAffinity::find_active_local_context_size(
                &pool,
                tenant_id,
                "resp_ws_collision",
            )
            .await
            .expect("local context size lookup should succeed"),
            Some(128)
        );
        assert_eq!(
            keycompute_db::ResponseAffinity::find_active_local_response(
                &pool,
                tenant_id,
                "resp_ws_collision",
            )
            .await
            .expect("local response projection should succeed"),
            Some(serde_json::json!({
                "id": "resp_ws_collision",
                "model": "gpt-test"
            }))
        );
        let local_state = keycompute_db::ResponseAffinity::find_active_local_state(
            &pool,
            tenant_id,
            "resp_ws_collision",
        )
        .await
        .expect("local state projection should succeed")
        .expect("local state should remain");
        assert_eq!(local_state.model.as_deref(), Some("gpt-test"));
        assert_eq!(local_state.local_context, serde_json::json!({"items": []}));

        keycompute_db::ResponseAffinity::upsert_local_with_quota(
            &pool,
            tenant_id,
            "resp_ws_quota",
            "openai",
            Some(first_account.id),
            serde_json::json!({"id": "resp_ws_quota", "model": "gpt-test"}),
            serde_json::json!({"items": []}),
            72,
            expires_at,
            2,
            200,
        )
        .await
        .expect("the exact warmup quota boundary should be accepted");
        keycompute_db::ResponseAffinity::upsert_local_with_quota(
            &pool,
            tenant_id,
            "resp_ws_quota",
            "openai",
            Some(first_account.id),
            serde_json::json!({"id": "resp_ws_quota", "model": "gpt-test"}),
            serde_json::json!({"items": ["replacement"]}),
            70,
            expires_at,
            2,
            200,
        )
        .await
        .expect("replacing the same warmup should exclude its previous usage");
        let quota_error = keycompute_db::ResponseAffinity::upsert_local_with_quota(
            &pool,
            tenant_id,
            "resp_ws_quota_overflow",
            "openai",
            Some(first_account.id),
            serde_json::json!({"id": "resp_ws_quota_overflow", "model": "gpt-test"}),
            serde_json::json!({"items": []}),
            1,
            expires_at,
            2,
            200,
        )
        .await
        .expect_err("a third active warmup must exceed the entry quota");
        assert!(matches!(
            quota_error,
            keycompute_db::DbError::ResourceLimitExceeded { .. }
        ));

        let concurrent_tenant_id =
            create_responses_test_tenant(&pool, "Responses concurrent warmup quota test").await;
        let concurrent_account =
            create_responses_test_account(&pool, concurrent_tenant_id, "quota", 0).await;
        let concurrent_pool = connect_to_schema(&schema).await;
        let first_insert = keycompute_db::ResponseAffinity::upsert_local_with_quota(
            &pool,
            concurrent_tenant_id,
            "resp_ws_concurrent_a",
            "openai",
            Some(concurrent_account.id),
            serde_json::json!({"id": "resp_ws_concurrent_a", "model": "gpt-test"}),
            serde_json::json!({"items": []}),
            1,
            expires_at,
            1,
            100,
        );
        let second_insert = keycompute_db::ResponseAffinity::upsert_local_with_quota(
            &concurrent_pool,
            concurrent_tenant_id,
            "resp_ws_concurrent_b",
            "openai",
            Some(concurrent_account.id),
            serde_json::json!({"id": "resp_ws_concurrent_b", "model": "gpt-test"}),
            serde_json::json!({"items": []}),
            1,
            expires_at,
            1,
            100,
        );
        let (first_insert, second_insert) = tokio::join!(first_insert, second_insert);
        assert_eq!(
            usize::from(first_insert.is_ok()) + usize::from(second_insert.is_ok()),
            1,
            "the tenant lock must make concurrent quota checks atomic"
        );
        let rejected = if let Err(error) = first_insert {
            error
        } else {
            second_insert.expect_err("one concurrent insert should be rejected")
        };
        assert!(matches!(
            rejected,
            keycompute_db::DbError::ResourceLimitExceeded { .. }
        ));
        drop(concurrent_pool);

        let billing_request_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let (first_binding, first_inserted) = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            "kc_idempotency_test",
            "fingerprint-a",
            billing_request_id,
            user_id,
            key_id,
            "openai",
            Some("gpt-test"),
            first_account.id,
        )
        .await
        .expect("first idempotency binding should persist");
        let (raced_binding, raced_inserted) = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            "kc_idempotency_test",
            "fingerprint-b",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-other"),
            second_account.id,
        )
        .await
        .expect("a retry should load the authoritative first binding");
        assert_eq!(first_binding.account_id, first_account.id);
        assert!(first_inserted);
        assert!(!raced_inserted);
        assert_eq!(raced_binding.account_id, first_account.id);
        assert_eq!(raced_binding, first_binding);

        let concurrent_billing_id = Uuid::new_v4();
        let first_claim = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            "kc_idempotency_concurrent",
            "fingerprint-concurrent",
            concurrent_billing_id,
            user_id,
            key_id,
            "openai",
            Some("gpt-test"),
            first_account.id,
        );
        let second_claim = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            "kc_idempotency_concurrent",
            "fingerprint-concurrent",
            concurrent_billing_id,
            user_id,
            key_id,
            "openai",
            Some("gpt-test"),
            first_account.id,
        );
        let (first_claim, second_claim) = tokio::join!(first_claim, second_claim);
        let (_, first_won) = first_claim.expect("first concurrent claim should resolve");
        let (_, second_won) = second_claim.expect("second concurrent claim should resolve");
        assert_ne!(
            first_won, second_won,
            "exactly one concurrent request may create the durable execution claim"
        );

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn idempotency_identity_quota_is_atomic_and_unstarted_claims_are_reclaimable() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        let second_pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses idempotency identity quota test").await;
        let account = create_responses_test_account(&pool, tenant_id, "identity-quota", 0).await;
        let user_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let first_token = Uuid::new_v4();
        let second_token = Uuid::new_v4();
        let lease_expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);

        let first =
            keycompute_db::ResponsesIdempotencyClaim::bind_for_execution_with_identity_quota(
                &pool,
                tenant_id,
                "kc_idempotency_identity_quota_a",
                "fingerprint-a",
                Uuid::new_v4(),
                user_id,
                key_id,
                "openai",
                Some("gpt-test"),
                account.id,
                first_token,
                lease_expires_at,
                1,
            );
        let second =
            keycompute_db::ResponsesIdempotencyClaim::bind_for_execution_with_identity_quota(
                &second_pool,
                tenant_id,
                "kc_idempotency_identity_quota_b",
                "fingerprint-b",
                Uuid::new_v4(),
                user_id,
                key_id,
                "openai",
                Some("gpt-test"),
                account.id,
                second_token,
                lease_expires_at,
                1,
            );
        let (first, second) = tokio::join!(first, second);
        assert_eq!(
            usize::from(first.is_ok()) + usize::from(second.is_ok()),
            1,
            "the tenant lock must make concurrent identity admission atomic"
        );
        let rejected = if let Err(error) = first.as_ref() {
            error
        } else {
            second
                .as_ref()
                .expect_err("one identity must exceed the quota")
        };
        assert!(matches!(
            rejected,
            keycompute_db::DbError::ResourceLimitExceeded { resource, .. }
                if resource == "Responses idempotency identities"
        ));
        let tenant = keycompute_db::Tenant::find_by_id(&pool, tenant_id)
            .await
            .expect("tenant counter query should succeed")
            .expect("identity counter tenant should exist");
        assert_eq!(tenant.responses_idempotency_claim_count, 1);
        assert!(
            serde_json::to_value(&tenant)
                .expect("tenant should serialize")
                .get("responses_idempotency_claim_count")
                .is_none(),
            "the internal storage counter must not become part of tenant API contracts"
        );

        let (claim, token) = match (first, second) {
            (Ok((claim, true)), Err(_)) => (claim, first_token),
            (Err(_), Ok((claim, true))) => (claim, second_token),
            outcomes => panic!("unexpected quota outcomes: {outcomes:?}"),
        };
        let (_, inserted) =
            keycompute_db::ResponsesIdempotencyClaim::bind_for_execution_with_identity_quota(
                &pool,
                tenant_id,
                &claim.binding_id,
                &claim.request_fingerprint,
                claim.billing_request_id,
                claim.user_id,
                claim.produce_ai_key_id,
                &claim.provider,
                claim.model.as_deref(),
                claim.account_id,
                Uuid::new_v4(),
                lease_expires_at,
                1,
            )
            .await
            .expect("an existing identity remains readable at the quota");
        assert!(!inserted);

        assert!(
            keycompute_db::ResponsesIdempotencyClaim::delete_unstarted_execution(
                &pool,
                tenant_id,
                &claim.binding_id,
                token,
            )
            .await
            .expect("a newly-created pre-dispatch claim should be removable")
        );
        let counter = keycompute_db::Tenant::find_by_id(&pool, tenant_id)
            .await
            .expect("tenant counter query should succeed")
            .expect("identity counter tenant should exist")
            .responses_idempotency_claim_count;
        assert_eq!(counter, 0);
        let (_, replacement_inserted) =
            keycompute_db::ResponsesIdempotencyClaim::bind_for_execution_with_identity_quota(
                &pool,
                tenant_id,
                "kc_idempotency_identity_quota_replacement",
                "fingerprint-replacement",
                Uuid::new_v4(),
                user_id,
                key_id,
                "openai",
                Some("gpt-test"),
                account.id,
                Uuid::new_v4(),
                lease_expires_at,
                1,
            )
            .await
            .expect("deleting an unstarted claim should restore quota capacity");
        assert!(replacement_inserted);

        drop(second_pool);
        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn rolled_back_idempotency_claim_can_be_retried() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses idempotency rollback test").await;
        let account = create_responses_test_account(&pool, tenant_id, "rollback", 0).await;
        let binding_id = "kc_idempotency_rolled_back";
        let billing_request_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let txn = pool.begin().await.expect("claim transaction should begin");
        let (_, inserted) = keycompute_db::ResponsesIdempotencyClaim::bind(
            &txn,
            tenant_id,
            binding_id,
            "fingerprint",
            billing_request_id,
            user_id,
            key_id,
            "openai",
            Some("gpt-test"),
            account.id,
        )
        .await
        .expect("provisional idempotency claim should be inserted");
        assert!(inserted);
        assert!(
            !keycompute_db::UsageLog::exists_by_billing_request_id(&txn, billing_request_id,)
                .await
                .expect("pre-dispatch ledger check should use the claim transaction")
        );
        // Simulate a definitive pre-dispatch validation failure. The handler
        // now performs its ledger/account checks inside this same transaction.
        txn.rollback()
            .await
            .expect("provisional claim should roll back");

        let (_, retry_inserted) = keycompute_db::ResponsesIdempotencyClaim::bind(
            &pool,
            tenant_id,
            binding_id,
            "fingerprint",
            billing_request_id,
            user_id,
            key_id,
            "openai",
            Some("gpt-test"),
            account.id,
        )
        .await
        .expect("retry should acquire a fresh claim");
        assert!(
            retry_inserted,
            "a local failure before dispatch must not permanently consume the key"
        );

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn idempotency_execution_lease_is_reclaimable_only_before_dispatch() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses idempotency lease test").await;
        let account = create_responses_test_account(&pool, tenant_id, "lease", 0).await;
        let binding_id = "kc_idempotency_lease";
        let first_token = Uuid::new_v4();
        let (claim, inserted) = keycompute_db::ResponsesIdempotencyClaim::bind_for_execution(
            &pool,
            tenant_id,
            binding_id,
            "fingerprint",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-test"),
            account.id,
            first_token,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("first execution lease should be created");
        assert!(inserted);
        assert_eq!(claim.execution_token, first_token);
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::reclaim_expired_execution(
                &pool,
                tenant_id,
                binding_id,
                Uuid::new_v4(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .await
            .expect("active lease check should succeed")
            .is_none(),
            "an active execution must continue excluding concurrent retries"
        );

        assert!(
            keycompute_db::ResponsesIdempotencyClaim::release_execution(
                &pool,
                tenant_id,
                binding_id,
                first_token,
            )
            .await
            .expect("failed execution lease should be released")
        );
        let replacement_token = Uuid::new_v4();
        let reclaimed = keycompute_db::ResponsesIdempotencyClaim::reclaim_expired_execution(
            &pool,
            tenant_id,
            binding_id,
            replacement_token,
            chrono::Utc::now() - chrono::Duration::seconds(1),
        )
        .await
        .expect("released lease should be reclaimable")
        .expect("retry should own the released lease");
        assert_eq!(reclaimed.execution_token, replacement_token);
        assert!(reclaimed.upstream_dispatched_at.is_none());
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::mark_execution_dispatched(
                &pool,
                tenant_id,
                binding_id,
                replacement_token,
            )
            .await
            .expect("dispatch fence should be persisted")
        );
        assert!(
            !keycompute_db::ResponsesIdempotencyClaim::release_execution(
                &pool,
                tenant_id,
                binding_id,
                replacement_token,
            )
            .await
            .expect("dispatched lease release should be rejected")
        );
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::reclaim_expired_execution(
                &pool,
                tenant_id,
                binding_id,
                Uuid::new_v4(),
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .await
            .expect("dispatched reclaim guard should succeed")
            .is_none(),
            "a dispatched execution must remain fenced after lease expiry"
        );

        assert!(
            !keycompute_db::ResponsesIdempotencyClaim::complete_execution_with_quota(
                &pool,
                tenant_id,
                binding_id,
                first_token,
                200,
                serde_json::json!([]),
                r#"{"id":"stale"}"#,
                chrono::Utc::now() + chrono::Duration::hours(24),
                16,
                1024,
            )
            .await
            .expect("stale completion should be fenced"),
            "the superseded holder must not overwrite the retry result"
        );
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::complete_execution_with_quota(
                &pool,
                tenant_id,
                binding_id,
                replacement_token,
                200,
                serde_json::json!([["content-type", "application/json"]]),
                r#"{"id":"resp_cached"}"#,
                chrono::Utc::now() - chrono::Duration::seconds(1),
                16,
                1024,
            )
            .await
            .expect("current holder should persist its terminal result")
        );
        assert!(
            !keycompute_db::ResponsesIdempotencyClaim::delete_unstarted_execution(
                &pool,
                tenant_id,
                binding_id,
                replacement_token,
            )
            .await
            .expect("terminal identity deletion guard should succeed"),
            "a completed execution must never be removed as an unstarted claim"
        );
        let replay_metadata =
            keycompute_db::ResponsesIdempotencyClaim::find_metadata_for_key_share(
                &pool, tenant_id, binding_id,
            )
            .await
            .expect("replay metadata lookup should succeed")
            .expect("completed claim metadata should exist");
        assert_eq!(
            replay_metadata.response_body_bytes,
            Some(i64::try_from(r#"{"id":"resp_cached"}"#.len()).unwrap())
        );
        assert_eq!(
            keycompute_db::ResponsesIdempotencyClaim::expire_completed_responses(&pool)
                .await
                .expect("expired result cleanup should succeed"),
            1
        );
        let expired = keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
            &pool, tenant_id, binding_id,
        )
        .await
        .expect("expired claim lookup should succeed")
        .expect("expiry must retain the permanent identity binding");
        assert_eq!(expired.execution_state, "expired");
        assert_eq!(expired.request_fingerprint, "fingerprint");
        assert!(expired.response_body.is_none());
        assert!(expired.response_body_bytes.is_none());

        let ambiguous_binding = "kc_idempotency_ambiguous";
        let ambiguous_token = Uuid::new_v4();
        let (_, inserted) = keycompute_db::ResponsesIdempotencyClaim::bind_for_execution(
            &pool,
            tenant_id,
            ambiguous_binding,
            "ambiguous-fingerprint",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-test"),
            account.id,
            ambiguous_token,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("ambiguous execution claim should be created");
        assert!(inserted);
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::mark_execution_dispatched(
                &pool,
                tenant_id,
                ambiguous_binding,
                ambiguous_token,
            )
            .await
            .expect("ambiguous dispatch should be fenced")
        );
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::expire_dispatched_execution(
                &pool,
                tenant_id,
                ambiguous_binding,
                ambiguous_token,
            )
            .await
            .expect("ambiguous execution should be finalized")
        );
        let ambiguous = keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
            &pool,
            tenant_id,
            ambiguous_binding,
        )
        .await
        .expect("ambiguous claim lookup should succeed")
        .expect("ambiguous identity must remain bound");
        assert_eq!(ambiguous.execution_state, "expired");
        assert!(ambiguous.upstream_dispatched_at.is_some());
        assert!(ambiguous.completed_at.is_some());

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn idempotency_replay_quota_is_atomic_and_oversized_entries_are_not_stored() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        let second_pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses idempotency quota test").await;
        let account = create_responses_test_account(&pool, tenant_id, "replay-quota", 0).await;
        let first_binding = "kc_idempotency_quota_first";
        let second_binding = "kc_idempotency_quota_second";
        let first_token = Uuid::new_v4();
        let second_token = Uuid::new_v4();

        for (binding_id, execution_token) in
            [(first_binding, first_token), (second_binding, second_token)]
        {
            let (_, inserted) = keycompute_db::ResponsesIdempotencyClaim::bind_for_execution(
                &pool,
                tenant_id,
                binding_id,
                &format!("fingerprint-{binding_id}"),
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                "openai",
                Some("gpt-test"),
                account.id,
                execution_token,
                chrono::Utc::now() + chrono::Duration::minutes(5),
            )
            .await
            .expect("quota test claim should be inserted");
            assert!(inserted);
            assert!(
                keycompute_db::ResponsesIdempotencyClaim::mark_execution_dispatched(
                    &pool,
                    tenant_id,
                    binding_id,
                    execution_token,
                )
                .await
                .expect("quota test dispatch should be fenced")
            );
        }

        let first_body = r#"{"id":"first"}"#;
        let second_body = r#"{"id":"second"}"#;
        let byte_quota = u64::try_from(first_body.len().max(second_body.len())).unwrap();
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);
        let first_completion =
            keycompute_db::ResponsesIdempotencyClaim::complete_execution_with_quota(
                &pool,
                tenant_id,
                first_binding,
                first_token,
                200,
                serde_json::json!([]),
                first_body,
                expires_at,
                1,
                byte_quota,
            );
        let second_completion =
            keycompute_db::ResponsesIdempotencyClaim::complete_execution_with_quota(
                &second_pool,
                tenant_id,
                second_binding,
                second_token,
                200,
                serde_json::json!([]),
                second_body,
                expires_at,
                1,
                byte_quota,
            );
        let (first_completed, second_completed) = tokio::join!(first_completion, second_completion);
        assert!(first_completed.expect("first concurrent completion should succeed"));
        assert!(second_completed.expect("second concurrent completion should succeed"));

        let first = keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
            &pool,
            tenant_id,
            first_binding,
        )
        .await
        .expect("first quota claim lookup should succeed")
        .expect("first quota claim should remain bound");
        let second = keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
            &pool,
            tenant_id,
            second_binding,
        )
        .await
        .expect("second quota claim lookup should succeed")
        .expect("second quota claim should remain bound");
        let retained = [&first, &second]
            .into_iter()
            .filter(|claim| claim.execution_state == "completed")
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1, "only one replay body may fit the quota");
        assert!(
            retained[0].response_body_bytes.unwrap_or_default()
                <= i64::try_from(byte_quota).unwrap()
        );
        for expired in [&first, &second]
            .into_iter()
            .filter(|claim| claim.execution_state == "expired")
        {
            assert!(expired.response_body.is_none());
            assert!(expired.response_body_bytes.is_none());
        }

        let oversized_binding = "kc_idempotency_quota_oversized";
        let oversized_token = Uuid::new_v4();
        let (_, inserted) = keycompute_db::ResponsesIdempotencyClaim::bind_for_execution(
            &pool,
            tenant_id,
            oversized_binding,
            "fingerprint-oversized",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "openai",
            Some("gpt-test"),
            account.id,
            oversized_token,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .expect("oversized quota test claim should be inserted");
        assert!(inserted);
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::mark_execution_dispatched(
                &pool,
                tenant_id,
                oversized_binding,
                oversized_token,
            )
            .await
            .expect("oversized response dispatch should be fenced")
        );
        let oversized_body = r#"{"id":"too-large-for-this-quota"}"#;
        assert!(
            keycompute_db::ResponsesIdempotencyClaim::complete_execution_with_quota(
                &pool,
                tenant_id,
                oversized_binding,
                oversized_token,
                200,
                serde_json::json!([]),
                oversized_body,
                expires_at,
                16,
                u64::try_from(oversized_body.len() - 1).unwrap(),
            )
            .await
            .expect("an oversized response should finalize without being cached")
        );
        let oversized = keycompute_db::ResponsesIdempotencyClaim::find_for_key_share(
            &pool,
            tenant_id,
            oversized_binding,
        )
        .await
        .expect("oversized claim lookup should succeed")
        .expect("oversized claim identity should remain bound");
        assert_eq!(oversized.execution_state, "expired");
        assert!(oversized.response_body.is_none());
        assert!(oversized.response_body_bytes.is_none());

        drop(second_pool);
        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn deleted_route_stays_tombstoned_when_terminal_settlement_arrives() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated Responses schema should migrate");
        let tenant_id =
            create_responses_test_tenant(&pool, "Responses delete settlement race test").await;
        let account = create_responses_test_account(&pool, tenant_id, "delete-race", 0).await;
        let response_id = "resp_deleted_before_terminal";
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);

        keycompute_db::ResponseAffinity::upsert_route(
            &pool,
            tenant_id,
            response_id,
            "openai",
            Some("gpt-test"),
            account.id,
            expires_at,
        )
        .await
        .expect("response.created should persist its route");
        let router = keycompute_db::DbRouter::single(pool.clone());
        keycompute_db::ResponseAffinity::delete_route_preserving_settlement(
            router.as_ref(),
            tenant_id,
            response_id,
        )
        .await
        .expect("DELETE should tombstone the route");
        assert!(
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, response_id)
                .await
                .expect("active route lookup should succeed")
                .is_none()
        );
        let tombstone = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT deleted_at IS NOT NULL AS deleted \
                 FROM response_affinities WHERE tenant_id = $1 AND response_id = $2",
                [tenant_id.into(), response_id.into()],
            ))
            .await
            .expect("tombstone lookup should succeed")
            .expect("DELETE must retain a tombstone until terminal persistence finishes");
        assert!(tombstone.try_get::<bool>("", "deleted").unwrap());

        let billing_request_id = Uuid::new_v4();
        keycompute_db::ResponseAffinity::upsert_route_with_settlement(
            &pool,
            tenant_id,
            response_id,
            "openai",
            Some("gpt-test"),
            account.id,
            expires_at,
            serde_json::json!({"billing_request_id": billing_request_id}),
            chrono::Utc::now(),
        )
        .await
        .expect("terminal settlement should attach to the tombstone");
        assert!(
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, response_id)
                .await
                .expect("active route lookup should succeed")
                .is_none(),
            "terminal persistence must not resurrect a deleted resource"
        );
        assert!(
            keycompute_db::ResponseAffinity::has_pending_settlement(&pool, tenant_id, response_id,)
                .await
                .expect("tombstoned settlement lookup should succeed")
        );

        keycompute_db::ResponseAffinity::clear_completed_settlements(
            &pool,
            tenant_id,
            billing_request_id,
        )
        .await
        .expect("settlement completion should remove the tombstone");
        let remaining = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS count FROM response_affinities \
                 WHERE tenant_id = $1 AND response_id = $2",
                [tenant_id.into(), response_id.into()],
            ))
            .await
            .expect("tombstone count should succeed")
            .expect("count query should return one row")
            .try_get::<i64>("", "count")
            .expect("count should decode");
        assert_eq!(remaining, 0);

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    #[tokio::test]
    async fn ledger_backed_consumption_records_overage_as_idempotent_debt() {
        use bigdecimal::BigDecimal;
        use keycompute_db::CreateUsageLogRequest;

        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("isolated billing schema should migrate");
        let tenant_id = create_responses_test_tenant(&pool, "Responses debt test").await;
        let account = create_responses_test_account(&pool, tenant_id, "debt", 0).await;
        let user_id = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let billing_request_id = Uuid::new_v4();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id, tenant_id, email) VALUES ($1, $2, $3)",
            [
                user_id.into(),
                tenant_id.into(),
                format!("debt-{}@example.com", Uuid::new_v4().simple()).into(),
            ],
        ))
        .await
        .unwrap();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO produce_ai_keys \
             (id, tenant_id, user_id, name, produce_ai_key_hash, produce_ai_key_preview) \
             VALUES ($1, $2, $3, 'debt-test', $4, 'sk-test')",
            [
                key_id.into(),
                tenant_id.into(),
                user_id.into(),
                format!("hash-{}", Uuid::new_v4()).into(),
            ],
        ))
        .await
        .unwrap();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO user_balances (tenant_id, user_id, available_balance) \
             VALUES ($1, $2, 0.10)",
            [tenant_id.into(), user_id.into()],
        ))
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let first_request = CreateUsageLogRequest {
            request_id: Uuid::new_v4(),
            tenant_id,
            user_id,
            produce_ai_key_id: key_id,
            model_name: "gpt-test".to_string(),
            provider_name: "openai".to_string(),
            account_id: account.id,
            input_tokens: 1,
            output_tokens: 1,
            input_unit_price_snapshot: BigDecimal::from(0),
            output_unit_price_snapshot: BigDecimal::from(0),
            user_amount: "0.25".parse::<BigDecimal>().unwrap(),
            currency: "CNY".to_string(),
            usage_source: "provider_reported".to_string(),
            status: "success".to_string(),
            started_at: now,
            finished_at: now,
        };
        let usage_log = keycompute_db::UsageLog::create_with_idempotency(
            &pool,
            &first_request,
            Some(billing_request_id),
        )
        .await
        .expect("first idempotent ledger write should succeed");
        let mut retry_request = first_request.clone();
        retry_request.request_id = Uuid::new_v4();
        let replayed_log = keycompute_db::UsageLog::create_with_idempotency(
            &pool,
            &retry_request,
            Some(billing_request_id),
        )
        .await
        .expect("concurrent-style idempotent ledger replay should load the first row");
        assert_eq!(replayed_log.id, usage_log.id);
        assert_eq!(replayed_log.request_id, first_request.request_id);
        assert_eq!(replayed_log.idempotency_id, Some(billing_request_id));
        let usage_log_id = usage_log.id;

        let (balance, transaction) = keycompute_db::UserBalance::consume(
            &pool,
            user_id,
            Decimal::new(25, 2),
            Some(usage_log_id),
            Some("API usage debt"),
        )
        .await
        .expect("accepted API usage should be recorded even when it exceeds the preflight balance");
        assert_eq!(balance.available_balance, Decimal::new(-15, 2));
        assert_eq!(transaction.balance_after, Decimal::new(-15, 2));

        let (replayed_balance, replayed_transaction) = keycompute_db::UserBalance::consume(
            &pool,
            user_id,
            Decimal::new(25, 2),
            Some(usage_log_id),
            Some("API usage debt replay"),
        )
        .await
        .expect("durable settlement replay should be idempotent");
        assert_eq!(replayed_balance.available_balance, Decimal::new(-15, 2));
        assert_eq!(replayed_transaction.id, transaction.id);
        assert!(
            keycompute_db::UserBalance::consume(
                &pool,
                user_id,
                Decimal::new(1, 2),
                None,
                Some("manual consumption"),
            )
            .await
            .is_err(),
            "manual consumption must retain the insufficient-balance guard"
        );

        let settlement = serde_json::json!({
            "request_id": first_request.request_id,
            "billing_request_id": billing_request_id,
        });
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(1);
        keycompute_db::ResponseAffinity::upsert_route_with_settlement(
            &pool,
            tenant_id,
            "resp_terminal_visible",
            "openai",
            Some("gpt-test"),
            account.id,
            expires_at,
            settlement.clone(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        keycompute_db::ResponseAffinity::upsert_hidden_settlement(
            &pool,
            tenant_id,
            "resp_terminal_hidden",
            "openai",
            Some("gpt-test"),
            Some(account.id),
            expires_at,
            settlement,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        keycompute_db::ResponseAffinity::clear_completed_settlements(
            &pool,
            tenant_id,
            billing_request_id,
        )
        .await
        .expect("successful terminal accounting should acknowledge every matching outbox row");
        let visible =
            keycompute_db::ResponseAffinity::find_active(&pool, tenant_id, "resp_terminal_visible")
                .await
                .unwrap()
                .expect("stored response routing must survive outbox acknowledgement");
        assert!(visible.settlement.is_none());
        let hidden_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS count FROM response_affinities \
                 WHERE tenant_id=$1 AND response_id='resp_terminal_hidden'",
                [tenant_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "count")
            .unwrap();
        assert_eq!(hidden_count, 0);

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    /// 测试数据库连接
    #[tokio::test]
    async fn test_database_connection() {
        let mut chain = VerificationChain::new();

        // 1. 连接数据库
        let pool = create_test_pool().await;
        chain.add_step(
            "keycompute-db",
            "create_test_pool",
            "Database connection established",
            true,
        );

        // 2. 测试简单查询
        let result = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT 1".to_string(),
            ))
            .await;
        let passed = result.is_ok();
        chain.add_step("keycompute-db", "SELECT 1", "Simple query executed", passed);

        // 3. 验证表存在（实际检查 COUNT(*) 值）
        let result = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM information_schema.tables WHERE table_name = 'tenants'",
                [],
            ))
            .await;
        let table_exists = result
            .ok()
            .flatten()
            .and_then(|r| r.try_get_by_index::<i64>(0).ok())
            .map(|count| count > 0)
            .unwrap_or(false);
        chain.add_step(
            "keycompute-db",
            "check_tenants_table",
            "Tenants table exists",
            table_exists,
        );

        chain.print_report();
        assert!(chain.all_passed(), "Database connection tests failed");
    }

    /// 测试数据库管理器
    #[tokio::test]
    async fn test_database_manager() {
        let mut chain = VerificationChain::new();

        // 直接使用 sea_orm ConnectOptions 创建连接池
        use sea_orm::ConnectOptions;
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://keycompute:change-me-strong-password@localhost:5432/keycompute".to_string()
        });
        let mut opt = ConnectOptions::new(&database_url);
        opt.max_connections(5);
        let pool = Database::connect(opt).await;

        chain.add_step(
            "keycompute-db",
            "ConnectOptions::connect",
            "Database pool created",
            pool.is_ok(),
        );

        let pool = pool.expect("Failed to create database pool");

        // 测试连接
        let test_result = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT 1".to_string(),
            ))
            .await;
        chain.add_step(
            "keycompute-db",
            "test_connection",
            "Connection test passed",
            test_result.is_ok(),
        );

        chain.print_report();
        assert!(chain.all_passed());
    }

    /// 全新数据库只会应用统一基线。
    #[tokio::test]
    async fn test_consolidated_baseline_migration_record() {
        let pool = create_test_pool().await;
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT version, name, COUNT(*) OVER () AS migration_count \
                 FROM schema_migrations LIMIT 1"
                    .to_string(),
            ))
            .await
            .expect("migration history query should succeed")
            .expect("migration history should exist");

        assert_eq!(row.try_get::<i64>("", "version").unwrap(), 1);
        assert_eq!(row.try_get::<String>("", "name").unwrap(), "baseline");
        assert_eq!(row.try_get::<i64>("", "migration_count").unwrap(), 1);
    }

    /// Two replicas starting against the same fresh database must serialize
    /// the baseline and converge on one migration-history row.
    #[tokio::test]
    async fn concurrent_migration_startup_applies_baseline_once() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let first = connect_to_schema(&schema).await;
        let second = connect_to_schema(&schema).await;

        let (first_result, second_result) = tokio::join!(
            keycompute_db::migrations::run_migrations(&first),
            keycompute_db::migrations::run_migrations(&second),
        );
        let history = first
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS migration_count, MIN(version) AS version FROM schema_migrations"
                    .to_string(),
            ))
            .await;

        drop(first);
        drop(second);
        drop_isolated_schema(&admin, &schema).await;

        first_result.expect("first migration runner should succeed");
        second_result.expect("concurrent migration runner should succeed");
        let history = history
            .expect("migration history query should succeed")
            .expect("migration history should exist");
        assert_eq!(history.try_get::<i64>("", "migration_count").unwrap(), 1);
        assert_eq!(history.try_get::<i64>("", "version").unwrap(), 1);
    }

    #[tokio::test]
    async fn saved_usage_side_effects_are_safe_to_replay_after_a_crash() {
        use bigdecimal::BigDecimal;
        use keycompute_db::CreateUsageLogRequest;
        use rust_decimal::Decimal;

        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("baseline migration should succeed");

        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO tenants (id,name,slug) VALUES ($1,'billing replay',$2)",
            [
                tenant_id.into(),
                format!("billing-replay-{tenant_id}").into(),
            ],
        ))
        .await
        .expect("tenant should be inserted");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO users (id,tenant_id,email) VALUES ($1,$2,$3)",
            [
                user_id.into(),
                tenant_id.into(),
                format!("billing-replay-{user_id}@example.test").into(),
            ],
        ))
        .await
        .expect("user should be inserted");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO user_balances (tenant_id,user_id,available_balance) VALUES ($1,$2,100)",
            [tenant_id.into(), user_id.into()],
        ))
        .await
        .expect("balance should be inserted");

        let request_id = Uuid::new_v4();
        let usage_log = keycompute_db::UsageLog::create(
            &pool,
            &CreateUsageLogRequest {
                request_id,
                tenant_id,
                user_id,
                produce_ai_key_id: Uuid::new_v4(),
                model_name: "gpt-test".to_string(),
                provider_name: "openai".to_string(),
                account_id: Uuid::new_v4(),
                input_tokens: 4,
                output_tokens: 6,
                input_unit_price_snapshot: BigDecimal::from(0),
                output_unit_price_snapshot: BigDecimal::from(0),
                user_amount: BigDecimal::from(5),
                currency: "CNY".to_string(),
                usage_source: "provider_reported".to_string(),
                status: "success".to_string(),
                started_at: chrono::Utc::now(),
                finished_at: chrono::Utc::now(),
            },
        )
        .await
        .expect("usage ledger should be inserted");
        let ctx = keycompute_types::RequestContext::new(
            request_id,
            user_id,
            tenant_id,
            Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        let billing = keycompute_billing::BillingService::with_pool(
            keycompute_db::DbRouter::single(pool.clone()),
        );

        billing
            .replay_saved_usage_effects(&ctx, &usage_log, user_id)
            .await
            .expect("first post-ledger settlement should succeed");
        billing
            .replay_saved_usage_effects(&ctx, &usage_log, user_id)
            .await
            .expect("crash replay should be idempotent");

        let balance = keycompute_db::UserBalance::find_by_user(&pool, user_id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        let consumption_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) AS count FROM balance_transactions WHERE usage_log_id=$1 AND transaction_type='consume'",
                [usage_log.id.into()],
            ))
            .await
            .expect("consumption query should succeed")
            .expect("consumption count should exist")
            .try_get::<i64>("", "count")
            .expect("consumption count should decode");

        assert_eq!(balance.available_balance, Decimal::from(95));
        assert_eq!(balance.total_consumed, Decimal::from(5));
        assert_eq!(consumption_count, 1);

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;
    }

    /// Migration history is an integrity boundary: editing an already-applied
    /// migration must fail closed instead of silently accepting schema drift.
    #[tokio::test]
    async fn migration_checksum_mismatch_is_rejected() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        keycompute_db::migrations::run_migrations(&pool)
            .await
            .expect("baseline migration should succeed before tampering");
        pool.execute_unprepared("UPDATE schema_migrations SET checksum='tampered'")
            .await
            .expect("migration checksum should be tampered for the test");

        let result = keycompute_db::migrations::run_migrations(&pool).await;

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;

        let error = result.expect_err("a checksum mismatch must reject startup");
        assert!(
            error.to_string().contains("checksum mismatch"),
            "unexpected migration error: {error}"
        );
    }

    /// The consolidated baseline only supports fresh deployments. A legacy
    /// application table without migration history must remain untouched and
    /// must not receive a misleading schema_migrations table.
    #[tokio::test]
    async fn nonempty_database_without_history_is_rejected_atomically() {
        let (admin, schema, _schema_permit) = create_isolated_schema().await;
        let pool = connect_to_schema(&schema).await;
        pool.execute_unprepared("CREATE TABLE legacy_application_data (id BIGINT PRIMARY KEY)")
            .await
            .expect("legacy application table should be created");

        let result = keycompute_db::migrations::run_migrations(&pool).await;
        let history_table = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT 1 AS present FROM information_schema.tables \
                 WHERE table_schema=current_schema() AND table_name='schema_migrations'"
                    .to_string(),
            ))
            .await;
        let legacy_table = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT 1 AS present FROM information_schema.tables \
                 WHERE table_schema=current_schema() AND table_name='legacy_application_data'"
                    .to_string(),
            ))
            .await;

        drop(pool);
        drop_isolated_schema(&admin, &schema).await;

        let error = result.expect_err("a non-empty database without history must be rejected");
        assert!(
            error
                .to_string()
                .contains("non-empty but has no migration history"),
            "unexpected migration error: {error}"
        );
        assert!(
            history_table
                .expect("migration-history existence query should succeed")
                .is_none(),
            "failed initialization must roll back schema_migrations creation"
        );
        assert!(
            legacy_table
                .expect("legacy-table existence query should succeed")
                .is_some(),
            "failed initialization must not modify existing application tables"
        );
    }

    /// 哨兵列校验：完整 schema 上应通过，结构缺失时应拒绝启动。
    #[tokio::test]
    async fn test_schema_sentinel_verification() {
        let pool = create_test_pool().await;

        // 当前测试库已应用完整 schema，哨兵校验必须通过
        keycompute_db::verify_schema_sentinels(&pool)
            .await
            .expect("sentinel verification must pass on an up-to-date schema");

        // 验证一个不存在的列必须失败并指名缺失项。
        let error = keycompute_db::verify_required_columns(
            &pool,
            &[
                ("payment_orders", "payment_scene"),
                ("payment_orders", "column_only_in_future_schema"),
            ],
        )
        .await
        .expect_err("a missing sentinel column must fail verification");
        let message = error.to_string();
        assert!(
            message.contains("payment_orders.column_only_in_future_schema"),
            "error should name the missing column, got: {message}"
        );
        assert!(
            !message.contains("payment_orders.payment_scene,"),
            "columns that exist must not be reported as missing, got: {message}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn stale_terminal_billing_reconciliation_resolves_pending_status() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let failed_request_id = Uuid::new_v4();
        let succeeded_request_id = Uuid::new_v4();
        let finished_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let received_at = finished_at - chrono::Duration::seconds(1);

        insert_terminal_pending_trace(&pool, failed_request_id, received_at, finished_at).await;
        insert_terminal_pending_trace(&pool, succeeded_request_id, received_at, finished_at).await;
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO usage_logs (
                request_id,tenant_id,user_id,produce_ai_key_id,model_name,provider_name,
                account_id,input_tokens,output_tokens,total_tokens,input_unit_price_snapshot,
                output_unit_price_snapshot,user_amount,currency,usage_source,status,
                started_at,finished_at
            ) VALUES ($1,$2,$3,$4,'test-model','openai',$5,1,1,2,0,0,0,'CNY',
                      'provider_reported','success',$6,$7)"#,
            [
                succeeded_request_id.into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                received_at.into(),
                finished_at.into(),
            ],
        ))
        .await
        .expect("committed usage should be inserted");

        keycompute_db::reconcile_stale_requests(router.as_ref(), 60, 200)
            .await
            .expect("stale billing reconciliation should succeed");

        for (request_id, expected) in [
            (failed_request_id, "failed"),
            (succeeded_request_id, "succeeded"),
        ] {
            let row = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT billing_status FROM gateway_requests WHERE request_id=$1",
                    [request_id.into()],
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.try_get::<String>("", "billing_status").unwrap(),
                expected
            );
        }

        delete_gateway_trace(&pool, failed_request_id).await;
        delete_gateway_trace(&pool, succeeded_request_id).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn stale_request_reconciliation_uses_last_lifecycle_activity() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let received_at = now - chrono::Duration::minutes(2);

        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO gateway_requests (
                request_id,tenant_id,user_id,produce_ai_key_id,protocol,request_path,
                requested_model,is_stream,route_type,status,received_at,updated_at,
                billing_status,trace_quality
            ) VALUES ($1,$2,$3,$4,'openai','/v1/chat/completions','test-model',FALSE,
                      'provider_account','running',$5,$6,'pending','actual')"#,
            [
                request_id.into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                received_at.into(),
                now.into(),
            ],
        ))
        .await
        .expect("active old request trace should be inserted");

        keycompute_db::reconcile_stale_requests(router.as_ref(), 60, 200)
            .await
            .expect("fresh lifecycle activity should be evaluated");
        let active = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.try_get::<String>("", "status").unwrap(), "running");
        assert!(
            active
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_none()
        );

        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE gateway_requests SET updated_at=$1 WHERE request_id=$2",
            [received_at.into(), request_id.into()],
        ))
        .await
        .expect("request trace should be made stale");
        keycompute_db::reconcile_stale_requests(router.as_ref(), 60, 200)
            .await
            .expect("inactive request should reconcile");
        let stale = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stale.try_get::<String>("", "status").unwrap(), "timed_out");
        assert!(
            stale
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn repeated_terminal_completion_is_idempotent() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);
        let request_id = Uuid::new_v4();
        let received_at = chrono::Utc::now();
        recorder
            .start_request(RequestTraceStart {
                request_id,
                client_request_id: None,
                tenant_id: Uuid::new_v4(),
                user_id: Uuid::new_v4(),
                produce_ai_key_id: Uuid::new_v4(),
                protocol: "openai".to_string(),
                request_path: "/v1/chat/completions".to_string(),
                requested_model: "test-model".to_string(),
                is_stream: false,
                received_at,
            })
            .await
            .expect("request trace should start");
        recorder
            .set_route(
                request_id,
                RouteType::ProviderAccount,
                RequestStatus::Routing,
            )
            .await
            .expect("request route should be recorded");
        let finish = RequestTraceFinish {
            request_id,
            status: RequestStatus::Succeeded,
            error: None,
            billing_status: BillingStatus::Pending,
            finished_at: chrono::Utc::now(),
        };

        recorder
            .finish_request_without_attempt(finish.clone())
            .await
            .expect("the first terminal completion should succeed");
        recorder
            .finish_request_without_attempt(finish)
            .await
            .expect("the repeated terminal completion should be idempotent");

        let request = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request.try_get::<String>("", "status").unwrap(),
            "succeeded"
        );
        assert!(
            request
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn non_final_attempt_completion_restores_the_request_status() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let received_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        insert_unfinished_node_trace(&pool, request_id, attempt_id, task_id, received_at).await;
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);

        recorder
            .finish_attempt_and_request(AttemptTraceFinish {
                attempt_id,
                request_id,
                attempt_status: AttemptStatus::Failed,
                request_status: RequestStatus::Queued,
                is_final: false,
                stream_end_reason: Some(StreamEndReason::UpstreamError),
                stream_error_count: Some(1),
                error: Some(TraceErrorInfo {
                    origin: ErrorOrigin::Node,
                    category: TraceErrorCategory::NodeFailed,
                    code: "node_requeued".to_string(),
                    summary: None,
                    retryable: Some(true),
                }),
                billing_status: BillingStatus::Pending,
                finished_at: chrono::Utc::now(),
            })
            .await
            .expect("the lifecycle fallback should complete the attempt");

        let request = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.try_get::<String>("", "status").unwrap(), "queued");
        assert!(
            request
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_none()
        );

        let attempt = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,is_final,finished_at FROM gateway_request_attempts WHERE id=$1",
                [attempt_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attempt.try_get::<String>("", "status").unwrap(), "failed");
        assert!(!attempt.try_get::<bool>("", "is_final").unwrap());
        assert!(
            attempt
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn final_attempt_can_complete_before_client_response_terminalizes_request() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let received_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        insert_unfinished_node_trace(&pool, request_id, attempt_id, task_id, received_at).await;
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);

        recorder
            .finish_attempt_and_request(AttemptTraceFinish {
                attempt_id,
                request_id,
                attempt_status: AttemptStatus::Succeeded,
                request_status: RequestStatus::Running,
                is_final: true,
                stream_end_reason: Some(StreamEndReason::Completed),
                stream_error_count: Some(0),
                error: None,
                billing_status: BillingStatus::Pending,
                finished_at: chrono::Utc::now(),
            })
            .await
            .expect("the final attempt should complete before the client response");

        let attempt = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,is_final,finished_at FROM gateway_request_attempts WHERE id=$1",
                [attempt_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.try_get::<String>("", "status").unwrap(),
            "succeeded"
        );
        assert!(attempt.try_get::<bool>("", "is_final").unwrap());
        assert!(
            attempt
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        let request = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.try_get::<String>("", "status").unwrap(), "running");
        assert!(
            request
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_none()
        );

        recorder
            .finish_request_without_attempt(RequestTraceFinish {
                request_id,
                status: RequestStatus::Succeeded,
                error: None,
                billing_status: BillingStatus::Pending,
                finished_at: chrono::Utc::now(),
            })
            .await
            .expect("the client response should terminalize the request independently");

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn failed_attempt_can_complete_before_client_disconnect_terminalizes_request() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let received_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        insert_unfinished_node_trace(&pool, request_id, attempt_id, task_id, received_at).await;
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);

        recorder
            .finish_attempt_and_request(AttemptTraceFinish {
                attempt_id,
                request_id,
                attempt_status: AttemptStatus::Failed,
                request_status: RequestStatus::Running,
                is_final: true,
                stream_end_reason: Some(StreamEndReason::UpstreamError),
                stream_error_count: Some(1),
                error: Some(TraceErrorInfo {
                    origin: ErrorOrigin::Node,
                    category: TraceErrorCategory::NodeFailed,
                    code: "node_failed".to_string(),
                    summary: None,
                    retryable: Some(true),
                }),
                billing_status: BillingStatus::Pending,
                finished_at: chrono::Utc::now(),
            })
            .await
            .expect("the failed Node attempt should close independently");

        let unfinished = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            unfinished.try_get::<String>("", "status").unwrap(),
            "running"
        );
        assert!(
            unfinished
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_none()
        );

        recorder
            .finish_request_without_attempt(keycompute_types::client_response_trace_finish(
                request_id,
                keycompute_types::ClientResponseOutcome::ClientDisconnected,
            ))
            .await
            .expect("the handler should persist the client disconnect");

        let request = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,error_category,error_code,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request.try_get::<String>("", "status").unwrap(),
            "cancelled"
        );
        assert_eq!(
            request.try_get::<String>("", "error_category").unwrap(),
            "client_disconnect"
        );
        assert_eq!(
            request.try_get::<String>("", "error_code").unwrap(),
            "client_disconnected"
        );
        assert!(
            request
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        let attempt = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status FROM gateway_request_attempts WHERE id=$1",
                [attempt_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attempt.try_get::<String>("", "status").unwrap(), "failed");

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn client_response_can_terminalize_before_final_attempt_is_persisted() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let received_at = chrono::Utc::now() - chrono::Duration::seconds(1);
        insert_unfinished_node_trace(&pool, request_id, attempt_id, task_id, received_at).await;
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);

        recorder
            .finish_request_without_attempt(RequestTraceFinish {
                request_id,
                status: RequestStatus::Succeeded,
                error: None,
                billing_status: BillingStatus::Pending,
                finished_at: chrono::Utc::now(),
            })
            .await
            .expect("the handler should terminalize the delivered response");
        recorder
            .finish_attempt_and_request(AttemptTraceFinish {
                attempt_id,
                request_id,
                attempt_status: AttemptStatus::Succeeded,
                request_status: RequestStatus::Running,
                is_final: true,
                stream_end_reason: Some(StreamEndReason::Completed),
                stream_error_count: Some(0),
                error: None,
                billing_status: BillingStatus::Pending,
                finished_at: chrono::Utc::now(),
            })
            .await
            .expect("a late attempt write must preserve the handler-owned request outcome");

        let request = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            request.try_get::<String>("", "status").unwrap(),
            "succeeded"
        );
        assert!(
            request
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        let attempt = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,is_final,finished_at FROM gateway_request_attempts WHERE id=$1",
                [attempt_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.try_get::<String>("", "status").unwrap(),
            "succeeded"
        );
        assert!(attempt.try_get::<bool>("", "is_final").unwrap());
        assert!(
            attempt
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn stale_image_success_without_client_response_is_reconciled_as_failed() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        insert_unfinished_node_trace(
            &pool,
            request_id,
            attempt_id,
            task_id,
            now - chrono::Duration::minutes(2),
        )
        .await;
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE gateway_requests SET updated_at=$1 WHERE request_id=$2",
            [
                (now - chrono::Duration::minutes(2)).into(),
                request_id.into(),
            ],
        ))
        .await
        .expect("unfinished image trace should be made stale");
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO node_tasks (
                id,request_id,user_id,model,payload_json,status,result_json,queued_at,
                finished_at,deadline_at,complete_grace_until
            ) VALUES ($1,$2,$3,'test-model','{}','image_succeeded','{}',$4,$5,$6,$7)"#,
            [
                task_id.into(),
                request_id.into(),
                Uuid::new_v4().into(),
                (now - chrono::Duration::minutes(2)).into(),
                (now - chrono::Duration::minutes(1)).into(),
                (now + chrono::Duration::minutes(1)).into(),
                (now + chrono::Duration::minutes(2)).into(),
            ],
        ))
        .await
        .expect("completed image task should be inserted");

        let reconciled = keycompute_db::reconcile_stale_requests(router.as_ref(), 60, 200)
            .await
            .expect("stale image completion should reconcile");
        assert!(reconciled >= 1);

        let request = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,trace_quality,billing_status,error_origin,error_category,error_code,finished_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.try_get::<String>("", "status").unwrap(), "failed");
        assert_eq!(
            request.try_get::<String>("", "trace_quality").unwrap(),
            "partial"
        );
        assert_eq!(
            request.try_get::<String>("", "billing_status").unwrap(),
            "not_applicable"
        );
        assert_eq!(
            request.try_get::<String>("", "error_origin").unwrap(),
            "gateway"
        );
        assert_eq!(
            request.try_get::<String>("", "error_category").unwrap(),
            "internal"
        );
        assert_eq!(
            request.try_get::<String>("", "error_code").unwrap(),
            "node_client_response_missing"
        );
        assert!(
            request
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "finished_at")
                .unwrap()
                .is_some()
        );

        let attempt = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status,is_final,stream_end_reason FROM gateway_request_attempts WHERE id=$1",
                [attempt_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            attempt.try_get::<String>("", "status").unwrap(),
            "succeeded"
        );
        assert!(attempt.try_get::<bool>("", "is_final").unwrap());
        assert_eq!(
            attempt.try_get::<String>("", "stream_end_reason").unwrap(),
            "completed"
        );

        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM node_tasks WHERE id=$1",
            [task_id.into()],
        ))
        .await
        .expect("image task should be removed");
        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn final_intermediate_flush_marks_partial_and_clears_failure_state() {
        let pool = create_test_pool().await;
        let router = keycompute_db::DbRouter::single(pool.clone());
        let request_id = Uuid::new_v4();
        let received_at = chrono::Utc::now();
        insert_terminal_pending_trace(&pool, request_id, received_at, received_at).await;
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);

        // An impossible client timestamp deterministically violates the schema
        // check and exercises the worker failure path after terminalization.
        recorder
            .record_client_first_content(request_id, received_at - chrono::Duration::seconds(1))
            .await
            .expect("the asynchronous update should enqueue");
        recorder
            .flush_intermediate_updates(request_id)
            .await
            .expect_err("the failed intermediate write must reach the barrier");

        let row = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT trace_quality,client_first_content_at FROM gateway_requests WHERE request_id=$1",
                [request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "trace_quality").unwrap(),
            "partial"
        );
        assert!(
            row.try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "client_first_content_at")
                .unwrap()
                .is_none()
        );

        // The first barrier consumes the per-request failure marker. A second
        // empty barrier must therefore complete successfully rather than
        // reporting the old failure forever.
        recorder
            .flush_intermediate_updates(request_id)
            .await
            .expect("failure state should be cleared after the first barrier");

        delete_gateway_trace(&pool, request_id).await;
    }

    #[tokio::test]
    async fn unrelated_intermediate_writes_do_not_block_request_flush() {
        let pool = create_test_pool().await;
        // This test measures request-local worker isolation, not connection
        // establishment latency. Keep enough ready connections for the lock
        // transaction, four blocked writers, and the healthy writer.
        let mut warm_connections = Vec::with_capacity(6);
        for _ in 0..6 {
            warm_connections.push(
                pool.get_postgres_connection_pool()
                    .acquire()
                    .await
                    .expect("test connection should prewarm"),
            );
        }
        drop(warm_connections);
        let router = keycompute_db::DbRouter::single(pool.clone());
        let recorder = keycompute_db::PostgresRequestLifecycleRecorder::new(router);
        let received_at = chrono::Utc::now();
        let blocked_request_ids = (0..4).map(|_| Uuid::new_v4()).collect::<Vec<_>>();
        let healthy_request_id = Uuid::new_v4();

        for request_id in blocked_request_ids
            .iter()
            .copied()
            .chain(std::iter::once(healthy_request_id))
        {
            recorder
                .start_request(RequestTraceStart {
                    request_id,
                    client_request_id: None,
                    tenant_id: Uuid::new_v4(),
                    user_id: Uuid::new_v4(),
                    produce_ai_key_id: Uuid::new_v4(),
                    protocol: "openai".to_string(),
                    request_path: "/v1/chat/completions".to_string(),
                    requested_model: "test-model".to_string(),
                    is_stream: true,
                    received_at,
                })
                .await
                .expect("request trace should start");
        }

        // Hold unrelated request rows long enough that a single global worker
        // would spend four 250 ms write timeouts ahead of the healthy barrier.
        let lock_tx = pool
            .begin()
            .await
            .expect("row-lock transaction should start");
        for request_id in &blocked_request_ids {
            lock_tx
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT request_id FROM gateway_requests WHERE request_id=$1 FOR UPDATE",
                    [(*request_id).into()],
                ))
                .await
                .expect("blocked request row should lock")
                .expect("blocked request trace should exist");
            recorder
                .record_client_first_content(*request_id, chrono::Utc::now())
                .await
                .expect("blocked intermediate update should enqueue");
        }

        let healthy_first_content_at = chrono::Utc::now();
        recorder
            .record_client_first_content(healthy_request_id, healthy_first_content_at)
            .await
            .expect("healthy intermediate update should enqueue");
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            recorder.flush_intermediate_updates(healthy_request_id),
        )
        .await
        .expect("unrelated row locks must not delay the healthy request barrier")
        .expect("healthy intermediate update should flush");

        let healthy = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT trace_quality,client_first_content_at FROM gateway_requests WHERE request_id=$1",
                [healthy_request_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            healthy.try_get::<String>("", "trace_quality").unwrap(),
            "actual"
        );
        assert!(
            healthy
                .try_get::<Option<chrono::DateTime<chrono::Utc>>>("", "client_first_content_at")
                .unwrap()
                .is_some()
        );

        lock_tx
            .rollback()
            .await
            .expect("row-lock transaction should roll back");
        for request_id in &blocked_request_ids {
            let _ = recorder.flush_intermediate_updates(*request_id).await;
        }
        for request_id in blocked_request_ids
            .into_iter()
            .chain(std::iter::once(healthy_request_id))
        {
            delete_gateway_trace(&pool, request_id).await;
        }
    }
}
