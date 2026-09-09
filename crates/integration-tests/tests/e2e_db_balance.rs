//! 余额冻结/解冻测试

use bigdecimal::BigDecimal;
use chrono::Utc;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_billing::BillingService;
use keycompute_billing::balance::{
    BalanceService, ManualBalanceOperationDecision, ManualBalanceOperationKind,
    ManualBalanceOperationOutcome,
};
use keycompute_db::{BalanceReservationEvent, CreateUsageLogRequest, DbRouter, UsageLog};
use keycompute_routing::AccountStateStore;
use keycompute_types::{ExecutionPlan, ExecutionTarget, Message, PricingSnapshot, RequestContext};
use llm_gateway::{GatewayConfig, GatewayExecutor};
use llm_protocol_provider::{
    HttpTransport, ProviderAdapter, StreamBox, StreamEvent, UpstreamFailure, UpstreamFailureKind,
    UpstreamRequest, UpstreamResponse,
};
use rust_decimal::Decimal;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Barrier;
    use tokio::task::JoinSet;

    /// Provider double that preserves the real upstream HTTP 429 response through
    /// `GatewayExecutor`; no token event is emitted before the terminal failure.
    #[derive(Debug)]
    struct UpstreamRateLimitedProvider;

    #[async_trait::async_trait]
    impl ProviderAdapter for UpstreamRateLimitedProvider {
        fn name(&self) -> &'static str {
            "upstream-rate-limited"
        }

        fn supported_models(&self) -> Vec<&'static str> {
            vec!["gpt-rate-limited"]
        }

        async fn stream_chat(
            &self,
            _transport: &dyn HttpTransport,
            _request: UpstreamRequest,
        ) -> keycompute_types::Result<StreamBox> {
            unreachable!("GatewayExecutor uses the metadata-preserving entry point")
        }

        async fn stream_chat_with_meta(
            &self,
            _transport: &dyn HttpTransport,
            _request: UpstreamRequest,
        ) -> std::result::Result<UpstreamResponse<StreamBox>, UpstreamFailure> {
            Err(UpstreamFailure {
                kind: UpstreamFailureKind::HttpStatus,
                status: Some(429),
                headers_received_at: Some(Utc::now()),
                upstream_request_id: Some("upstream-rate-limit-test".to_string()),
                client_response: Some(Box::new(keycompute_types::ClientUpstreamResponse {
                    status: 429,
                    headers: vec![("retry-after".to_string(), "1".to_string())],
                    body: r#"{"error":{"type":"rate_limit_error","code":"rate_limit_exceeded"}}"#
                        .to_string(),
                })),
                retryable: true,
                stable_error_code: "upstream_http_429".to_string(),
                sanitized_summary: "upstream rate limited the request".to_string(),
            })
        }
    }

    async fn force_reservation_expired(pool: &DatabaseConnection, reservation_id: uuid::Uuid) {
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET expires_at = NOW() - INTERVAL '1 second' WHERE id = $1",
            [reservation_id.into()],
        ))
        .await
        .expect("reservation expiry should be forced for the test");
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_concurrent_admin_balance_operation(
        service: &BalanceService,
        kind: ManualBalanceOperationKind,
        tenant_id: uuid::Uuid,
        user_id: uuid::Uuid,
        actor_user_id: uuid::Uuid,
        amount: Decimal,
        reason: &str,
        idempotency_key: &str,
    ) -> Vec<ManualBalanceOperationOutcome> {
        const CONCURRENCY: usize = 8;
        let barrier = Arc::new(Barrier::new(CONCURRENCY));
        let mut tasks = JoinSet::new();
        for _ in 0..CONCURRENCY {
            let service = service.clone();
            let barrier = barrier.clone();
            let reason = reason.to_string();
            let idempotency_key = idempotency_key.to_string();
            tasks.spawn(async move {
                barrier.wait().await;
                service
                    .apply_admin_manual_operation(
                        kind,
                        tenant_id,
                        user_id,
                        actor_user_id,
                        amount,
                        &reason,
                        &idempotency_key,
                    )
                    .await
            });
        }

        let mut outcomes = Vec::with_capacity(CONCURRENCY);
        while let Some(result) = tasks.join_next().await {
            match result
                .expect("administrator operation task should not panic")
                .expect("administrator operation should succeed")
            {
                ManualBalanceOperationDecision::Completed(outcome) => outcomes.push(outcome),
                ManualBalanceOperationDecision::Conflict => {
                    panic!("identical concurrent administrator payload must not conflict")
                }
            }
        }
        assert_eq!(outcomes.len(), CONCURRENCY);
        assert!(
            outcomes.windows(2).all(|pair| pair[0] == pair[1]),
            "all matching retries must return the identical persisted result"
        );
        outcomes
    }

    async fn post_admin_balance_operation(
        app: &axum::Router,
        token: &str,
        uri: &str,
        idempotency_key: Option<&str>,
        amount: &str,
        reason: &str,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt;

        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"));
        if let Some(idempotency_key) = idempotency_key {
            request = request.header("idempotency-key", idempotency_key);
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(
                        serde_json::json!({"amount": amount, "reason": reason}).to_string(),
                    ))
                    .expect("administrator balance request should build"),
            )
            .await
            .expect("administrator balance request should complete");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("administrator balance response should be readable");
        let body = serde_json::from_slice(&body)
            .expect("administrator balance response should contain JSON");
        (status, body)
    }

    async fn post_generation_json(
        app: &axum::Router,
        token: &str,
        uri: &str,
        extra_headers: &[(&str, &str)],
        body: serde_json::Value,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        use axum::body::{Body, to_bytes};
        use axum::http::Request;
        use tower::ServiceExt;

        let mut request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"));
        for (name, value) in extra_headers {
            request = request.header(*name, *value);
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(Body::from(body.to_string()))
                    .expect("generation request should build"),
            )
            .await
            .expect("generation request should complete");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("generation response should be readable");
        let body = serde_json::from_slice(&body).expect("generation response should contain JSON");
        (status, body)
    }

    async fn assert_no_request_balance_reservation(
        pool: &DatabaseConnection,
        user_id: uuid::Uuid,
        expected_available: Decimal,
    ) {
        let balance = keycompute_db::UserBalance::find_by_user(pool, user_id)
            .await
            .expect("balance query after local TPM rejection should succeed")
            .expect("balance should exist");
        assert_eq!(balance.available_balance, expected_available);
        assert_eq!(balance.frozen_balance, Decimal::ZERO);

        let reservation_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM balance_reservations WHERE user_id = $1",
                [user_id.into()],
            ))
            .await
            .expect("balance reservation count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(reservation_count, 0);
    }

    /// 测试余额冻结：冻结后可用余额减少、冻结余额增加
    #[tokio::test]
    async fn test_balance_freeze_success() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("balance freeze cleanup should succeed");

        let tenant = create_test_tenant(&pool, "bf-suc", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "bf-suc", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));

        // 确保余额记录存在，然后充值
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        let (initial, _) = balance_service
            .recharge(
                user.id,
                tenant.id,
                Decimal::from(100),
                None,
                Some("initial recharge for freeze test"),
            )
            .await
            .expect("recharge should succeed");
        assert_eq!(initial.available_balance, Decimal::from(100));
        assert_eq!(initial.frozen_balance, Decimal::ZERO);

        // 冻结 30 元
        let (after_freeze, tx) = balance_service
            .freeze(user.id, Decimal::from(30), Some("test freeze half"))
            .await
            .expect("freeze should succeed");

        assert_eq!(
            after_freeze.available_balance,
            Decimal::from(70),
            "available should be 70 after freezing 30"
        );
        assert_eq!(
            after_freeze.frozen_balance,
            Decimal::from(30),
            "frozen should be 30"
        );
        // freeze 交易记录金额为负数（从用户视角可用余额减少）
        assert_eq!(tx.amount, Decimal::from(-30));
        assert_eq!(tx.transaction_type, "freeze");
        assert_eq!(tx.description.as_deref(), Some("test freeze half"));
    }

    /// 测试余额冻结合并冻结：第二次冻结累加到 frozen_balance
    #[tokio::test]
    async fn test_balance_freeze_cumulative() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("cumulative freeze cleanup should succeed");

        let tenant = create_test_tenant(&pool, "bf-cum", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "bf-cum", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        let _ = balance_service
            .recharge(
                user.id,
                tenant.id,
                Decimal::from(100),
                None,
                Some("recharge for cumulative test"),
            )
            .await
            .expect("recharge should succeed");

        // 第一次冻结 20
        let (b1, _) = balance_service
            .freeze(user.id, Decimal::from(20), None)
            .await
            .expect("first freeze should succeed");
        assert_eq!(b1.available_balance, Decimal::from(80));
        assert_eq!(b1.frozen_balance, Decimal::from(20));

        // 第二次冻结 40（累计冻结 60）
        let (b2, _) = balance_service
            .freeze(user.id, Decimal::from(40), None)
            .await
            .expect("second freeze should succeed");
        assert_eq!(b2.available_balance, Decimal::from(40));
        assert_eq!(b2.frozen_balance, Decimal::from(60));
    }

    /// 测试余额冻结不足时返回错误
    #[tokio::test]
    async fn test_balance_freeze_insufficient() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("insufficient freeze cleanup should succeed");

        let tenant = create_test_tenant(&pool, "bf-insuf", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "bf-insuf", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        // 只充 10 元，尝试冻结 100 元应失败
        let _ = balance_service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");

        let err = balance_service
            .freeze(user.id, Decimal::from(100), None)
            .await
            .expect_err("freeze with insufficient balance should fail");

        let err_msg = err.to_string().to_lowercase();
        assert!(
            err_msg.contains("insufficient") || err_msg.contains("not enough"),
            "error should indicate insufficient balance, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("available 10"),
            "error must preserve the actual available amount, got: {err_msg}"
        );
    }

    /// 零数和负数不能借由余额操作反向移动资金。
    #[tokio::test]
    async fn balance_mutations_reject_non_positive_amounts() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("zero amount freeze cleanup should succeed");

        let tenant = create_test_tenant(&pool, "bf-zero", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "bf-zero", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        for amount in [Decimal::ZERO, Decimal::from(-1)] {
            let recharge_error = balance_service
                .recharge(user.id, tenant.id, amount, None, Some("invalid recharge"))
                .await
                .expect_err("non-positive recharge must fail");
            assert!(
                recharge_error.to_string().contains("greater than zero"),
                "unexpected recharge error: {recharge_error}"
            );

            let consumption_error = balance_service
                .consume(user.id, amount, None, Some("invalid consumption"))
                .await
                .expect_err("non-positive administrative consumption must fail");
            assert!(
                consumption_error.to_string().contains("greater than zero"),
                "unexpected consumption error: {consumption_error}"
            );

            let freeze_error = balance_service
                .freeze(user.id, amount, Some("invalid freeze"))
                .await
                .expect_err("non-positive freeze must fail");
            assert!(
                freeze_error.to_string().contains("greater than zero"),
                "unexpected freeze error: {freeze_error}"
            );

            let unfreeze_error = balance_service
                .unfreeze(user.id, amount, Some("invalid unfreeze"))
                .await
                .expect_err("non-positive unfreeze must fail");
            assert!(
                unfreeze_error.to_string().contains("greater than zero"),
                "unexpected unfreeze error: {unfreeze_error}"
            );

            keycompute_db::UserBalance::recharge(&pool, user.id, tenant.id, amount, None, None)
                .await
                .expect_err("DB recharge guard must reject non-positive amounts");
            let tx = pool
                .begin()
                .await
                .expect("validation transaction should begin");
            keycompute_db::UserBalance::recharge_in_tx(&tx, user.id, tenant.id, amount, None, None)
                .await
                .expect_err("DB in-transaction recharge guard must reject non-positive amounts");
            keycompute_db::UserBalance::credit_tips(&tx, user.id, tenant.id, amount, None)
                .await
                .expect_err("DB tip-credit guard must reject non-positive amounts");
            tx.rollback()
                .await
                .expect("validation transaction should roll back");
            keycompute_db::UserBalance::consume(&pool, user.id, amount, None, None)
                .await
                .expect_err("DB consumption guard must reject non-positive manual amounts");
            keycompute_db::UserBalance::freeze(&pool, user.id, amount, None)
                .await
                .expect_err("DB freeze guard must reject non-positive amounts");
            keycompute_db::UserBalance::unfreeze(&pool, user.id, amount, None)
                .await
                .expect_err("DB unfreeze guard must reject non-positive amounts");
        }

        balance_service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("valid recharge should succeed");

        let after = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(after.available_balance, Decimal::from(10));
        assert_eq!(after.frozen_balance, Decimal::ZERO);
    }

    /// 测试余额解冻：冻结部分后解冻，恢复可用余额
    #[tokio::test]
    async fn test_balance_unfreeze_success() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("unfreeze cleanup should succeed");

        let tenant = create_test_tenant(&pool, "uf-suc", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "uf-suc", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        let _ = balance_service
            .recharge(user.id, tenant.id, Decimal::from(100), None, None)
            .await
            .expect("recharge should succeed");

        // 冻结 50
        let (frozen, _) = balance_service
            .freeze(user.id, Decimal::from(50), None)
            .await
            .expect("freeze should succeed");
        assert_eq!(frozen.available_balance, Decimal::from(50));
        assert_eq!(frozen.frozen_balance, Decimal::from(50));

        // 解冻 20
        let (unfrozen, tx) = balance_service
            .unfreeze(user.id, Decimal::from(20), Some("partial unfreeze"))
            .await
            .expect("unfreeze should succeed");

        assert_eq!(
            unfrozen.available_balance,
            Decimal::from(70),
            "available should be 70 after unfreezing 20"
        );
        assert_eq!(
            unfrozen.frozen_balance,
            Decimal::from(30),
            "frozen should be 30 after unfreezing 20"
        );
        // unfreeze 交易记录金额为正数（可用余额增加）
        assert_eq!(tx.amount, Decimal::from(20));
        assert_eq!(tx.transaction_type, "unfreeze");
        assert_eq!(tx.description.as_deref(), Some("partial unfreeze"));
    }

    /// 测试解冻金额超过冻结余额时返回错误
    #[tokio::test]
    async fn test_balance_unfreeze_insufficient() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("insufficient unfreeze cleanup should succeed");

        let tenant = create_test_tenant(&pool, "uf-insuf", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "uf-insuf", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        let _ = balance_service
            .recharge(user.id, tenant.id, Decimal::from(50), None, None)
            .await
            .expect("recharge should succeed");

        // 冻结 10
        let _ = balance_service
            .freeze(user.id, Decimal::from(10), None)
            .await
            .expect("freeze should succeed");

        // 尝试解冻 100（超过已冻结的 10）
        let err = balance_service
            .unfreeze(user.id, Decimal::from(100), None)
            .await
            .expect_err("unfreeze with insufficient frozen balance should fail");

        let err_msg = err.to_string().to_lowercase();
        assert!(
            err_msg.contains("insufficient") || err_msg.contains("not enough"),
            "error should indicate insufficient frozen balance, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("available 10"),
            "error must preserve the actual manually frozen amount, got: {err_msg}"
        );
    }

    /// 测试 freeze/unfreeze 完整往返：冻结后解冻回原状态
    #[tokio::test]
    async fn test_balance_freeze_unfreeze_roundtrip() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("roundtrip cleanup should succeed");

        let tenant = create_test_tenant(&pool, "bf-rt", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "bf-rt", &test_id).await;

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        let _ = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("get_or_create should succeed");

        let (initial, _) = balance_service
            .recharge(user.id, tenant.id, Decimal::from(100), None, None)
            .await
            .expect("recharge should succeed");

        // 冻结 100
        let (frozen, _) = balance_service
            .freeze(user.id, Decimal::from(100), None)
            .await
            .expect("freeze all should succeed");
        assert_eq!(frozen.available_balance, Decimal::ZERO);
        assert_eq!(frozen.frozen_balance, Decimal::from(100));

        // 解冻 100
        let (unfrozen, _) = balance_service
            .unfreeze(user.id, Decimal::from(100), None)
            .await
            .expect("unfreeze all should succeed");

        // 解冻后状态应和初始状态一致
        assert_eq!(
            unfrozen.available_balance, initial.available_balance,
            "available balance should be restored after full unfreeze"
        );
        assert_eq!(
            unfrozen.frozen_balance,
            Decimal::ZERO,
            "frozen balance should be zero after full unfreeze"
        );
    }

    /// 并发首充必须串行化余额流水的 before/after，不能只保证最终余额。
    #[tokio::test]
    async fn concurrent_first_recharges_preserve_a_contiguous_audit_chain() {
        const RECHARGE_COUNT: usize = 8;

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("concurrent recharge cleanup should succeed");
        let tenant = create_test_tenant(&pool, "recharge-race", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "recharge-race", &test_id).await;
        let barrier = Arc::new(Barrier::new(RECHARGE_COUNT));
        let mut tasks = JoinSet::new();

        for _ in 0..RECHARGE_COUNT {
            let service = BalanceService::new(DbRouter::single(pool.clone()));
            let barrier = Arc::clone(&barrier);
            tasks.spawn(async move {
                barrier.wait().await;
                service
                    .recharge(
                        user.id,
                        tenant.id,
                        Decimal::ONE,
                        None,
                        Some("concurrent first recharge"),
                    )
                    .await
                    .expect("concurrent first recharge should succeed")
            });
        }

        let mut transactions = Vec::with_capacity(RECHARGE_COUNT);
        while let Some(result) = tasks.join_next().await {
            let (_, transaction) = result.expect("recharge task should not panic");
            transactions.push(transaction);
        }
        transactions.sort_by_key(|transaction| transaction.balance_before);

        for (index, transaction) in transactions.iter().enumerate() {
            let expected_before = Decimal::from(index);
            assert_eq!(transaction.balance_before, expected_before);
            assert_eq!(transaction.balance_after, expected_before + Decimal::ONE);
        }
        let final_balance = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("final balance query should succeed")
            .expect("final balance should exist");
        assert_eq!(
            final_balance.available_balance,
            Decimal::from(RECHARGE_COUNT)
        );
    }

    #[tokio::test]
    async fn concurrent_explicit_full_balance_requests_cannot_oversubscribe() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("reservation race cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reserve-race", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reserve-race", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::ONE, None, None)
            .await
            .expect("reservation test recharge should succeed");

        let barrier = Arc::new(Barrier::new(2));
        let mut tasks = JoinSet::new();
        for _ in 0..2 {
            let service = service.clone();
            let barrier = Arc::clone(&barrier);
            tasks.spawn(async move {
                barrier.wait().await;
                service
                    .reserve_request(
                        user.id,
                        tenant.id,
                        uuid::Uuid::new_v4(),
                        Decimal::ONE,
                        std::time::Duration::from_secs(26 * 60 * 60),
                    )
                    .await
            });
        }

        let mut successes = Vec::new();
        let mut failures = 0;
        while let Some(result) = tasks.join_next().await {
            match result.expect("reservation task should not panic") {
                Ok(reservation) => successes.push(reservation),
                Err(error) => {
                    failures += 1;
                    assert!(error.to_string().contains("Insufficient balance"));
                }
            }
        }
        assert_eq!(successes.len(), 1);
        assert_eq!(failures, 1);
        assert_eq!(successes[0].amount, Decimal::ONE);

        let balance = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(balance.available_balance, Decimal::ZERO);
        assert_eq!(balance.frozen_balance, Decimal::ONE);
    }

    #[tokio::test]
    async fn stale_owner_cannot_release_a_reclaimed_request_reservation() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("reservation ownership cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reserve-owner", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reserve-owner", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::ONE, None, None)
            .await
            .expect("reservation ownership recharge should succeed");

        let billing_request_id = uuid::Uuid::new_v4();
        let first_owner_token = uuid::Uuid::new_v4();
        let first = service
            .reserve_request_with_owner_token(
                user.id,
                tenant.id,
                billing_request_id,
                first_owner_token,
                Decimal::ONE,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("first reservation owner should succeed");
        let replacement_owner_token = uuid::Uuid::new_v4();
        let replacement = service
            .reserve_request_with_owner_token(
                user.id,
                tenant.id,
                billing_request_id,
                replacement_owner_token,
                Decimal::ONE,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("replacement reservation owner should succeed");

        assert_eq!(first.owner_token, first_owner_token);
        assert_eq!(replacement.owner_token, replacement_owner_token);
        assert!(
            !service
                .release_request_reservation(billing_request_id, first.owner_token)
                .await
                .expect("stale release should be a successful no-op")
        );
        let still_reserved = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(still_reserved.available_balance, Decimal::ZERO);
        assert_eq!(still_reserved.frozen_balance, Decimal::ONE);

        assert!(
            service
                .release_request_reservation(billing_request_id, replacement.owner_token)
                .await
                .expect("current owner should release the reservation")
        );
        let released = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(released.available_balance, Decimal::ONE);
        assert_eq!(released.frozen_balance, Decimal::ZERO);
    }

    #[tokio::test]
    async fn active_reservation_reownership_resizes_the_frozen_amount() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("reservation resize cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reserve-resize", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reserve-resize", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("reservation resize recharge should succeed");

        let billing_request_id = uuid::Uuid::new_v4();
        let first = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(3),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("initial reservation should succeed");
        let competing_request_id = uuid::Uuid::new_v4();
        let competing = service
            .reserve_request(
                user.id,
                tenant.id,
                competing_request_id,
                Decimal::from(5),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("competing reservation should succeed");
        let failed_growth = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(6),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect_err("growth beyond this request's reservable capacity should fail");
        assert!(failed_growth.to_string().contains("Insufficient balance"));
        let after_failed_growth = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(after_failed_growth.available_balance, Decimal::from(2));
        assert_eq!(after_failed_growth.frozen_balance, Decimal::from(8));
        assert!(
            service
                .release_request_reservation(billing_request_id, first.owner_token)
                .await
                .expect("failed growth must preserve the current owner")
        );
        let first = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(3),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("released logical request should be reservable again");
        assert!(
            service
                .release_request_reservation(competing_request_id, competing.owner_token)
                .await
                .expect("competing reservation should release")
        );

        let grown = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(6),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("reservation growth should succeed");
        assert_eq!(grown.amount, Decimal::from(6));
        assert_ne!(first.owner_token, grown.owner_token);
        let after_growth = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(after_growth.available_balance, Decimal::from(4));
        assert_eq!(after_growth.frozen_balance, Decimal::from(6));
        assert!(
            !service
                .release_request_reservation(billing_request_id, first.owner_token)
                .await
                .expect("stale release should be a successful no-op")
        );

        let shrunk = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(2),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("reservation shrink should succeed");
        assert_eq!(shrunk.amount, Decimal::from(2));
        let after_shrink = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(after_shrink.available_balance, Decimal::from(8));
        assert_eq!(after_shrink.frozen_balance, Decimal::from(2));

        let full_balance = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(10),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("explicit full-balance replacement should own all reservable funds");
        assert_eq!(full_balance.amount, Decimal::from(10));
        let fully_reserved = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(fully_reserved.available_balance, Decimal::ZERO);
        assert_eq!(fully_reserved.frozen_balance, Decimal::from(10));

        assert!(
            service
                .release_request_reservation(billing_request_id, full_balance.owner_token)
                .await
                .expect("latest owner should release the resized reservation")
        );
        let released = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(released.available_balance, Decimal::from(10));
        assert_eq!(released.frozen_balance, Decimal::ZERO);
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn expired_explicit_full_balance_reservation_is_reclaimed_before_admission() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("expired reservation cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reserve-expiry", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reserve-expiry", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::ONE, None, None)
            .await
            .expect("reservation test recharge should succeed");

        let stale = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::ONE,
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("initial explicit full-balance reservation should succeed");
        force_reservation_expired(&pool, stale.id).await;

        let replacement = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::ONE,
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("expired funds should be reclaimed before admission");
        assert_eq!(replacement.amount, Decimal::ONE);

        let balance = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(balance.available_balance, Decimal::ZERO);
        assert_eq!(balance.frozen_balance, Decimal::ONE);
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn balance_reads_reclaim_expired_request_reservations() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("balance read expiry cleanup should succeed");
        let tenant = create_test_tenant(&pool, "read-expiry", &test_id).await;
        let single_user = create_test_user(&pool, tenant.id, "read-expiry-one", &test_id).await;
        let batch_user = create_test_user(&pool, tenant.id, "read-expiry-two", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));

        for user_id in [single_user.id, batch_user.id] {
            service
                .get_or_create(tenant.id, user_id)
                .await
                .expect("balance creation should succeed");
            service
                .recharge(user_id, tenant.id, Decimal::ONE, None, None)
                .await
                .expect("balance read expiry recharge should succeed");
        }
        let single_reservation = service
            .reserve_request(
                single_user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::ONE,
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("single-read reservation should succeed");
        let batch_reservation = service
            .reserve_request(
                batch_user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::ONE,
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("batch-read reservation should succeed");
        force_reservation_expired(&pool, single_reservation.id).await;
        force_reservation_expired(&pool, batch_reservation.id).await;

        let single_balance = service
            .find_by_user(single_user.id)
            .await
            .expect("single balance query should succeed")
            .expect("single balance should exist");
        assert_eq!(single_balance.available_balance, Decimal::ONE);
        assert_eq!(single_balance.frozen_balance, Decimal::ZERO);

        let batch_balances = service
            .find_by_users(&[batch_user.id])
            .await
            .expect("batch balance query should succeed");
        let batch_balance = batch_balances
            .get(&batch_user.id)
            .expect("batch balance should exist");
        assert_eq!(batch_balance.available_balance, Decimal::ONE);
        assert_eq!(batch_balance.frozen_balance, Decimal::ZERO);
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn unfreeze_reclaims_expired_reservations_before_releasing_manual_funds() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("unfreeze expiry cleanup should succeed");
        let tenant = create_test_tenant(&pool, "unfreeze-expiry", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "unfreeze-expiry", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("unfreeze expiry recharge should succeed");
        service
            .freeze(user.id, Decimal::from(3), Some("manual freeze"))
            .await
            .expect("manual freeze should succeed");
        let stale = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::from(4),
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("request reservation should succeed");
        force_reservation_expired(&pool, stale.id).await;

        let (balance, transaction) = service
            .unfreeze(user.id, Decimal::from(3), Some("release manual freeze"))
            .await
            .expect("manual funds should be unfrozen after expiry reclamation");
        assert_eq!(balance.available_balance, Decimal::from(10));
        assert_eq!(balance.frozen_balance, Decimal::ZERO);
        assert_eq!(transaction.amount, Decimal::from(3));
        assert_eq!(transaction.transaction_type, "unfreeze");
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn failed_manual_unfreeze_does_not_roll_back_expiry_reclamation() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("failed unfreeze reclamation cleanup should succeed");
        let tenant = create_test_tenant(&pool, "unfreeze-rollback", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "unfreeze-rollback", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(4), None, None)
            .await
            .expect("recharge should succeed");
        let stale = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::from(4),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("request reservation should succeed");
        force_reservation_expired(&pool, stale.id).await;

        let error = service
            .unfreeze(user.id, Decimal::ONE, Some("invalid manual release"))
            .await
            .expect_err("request-owned funds are not manually frozen funds");
        assert!(
            error.to_string().contains("available 0"),
            "error must report the true manual amount: {error}"
        );

        // Use the raw read so this assertion cannot itself trigger lazy
        // reclamation and hide a transaction rollback regression.
        let balance = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("raw balance query should succeed")
            .expect("balance should exist");
        assert_eq!(balance.available_balance, Decimal::from(4));
        assert_eq!(balance.frozen_balance, Decimal::ZERO);
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn independent_reclaimer_recovers_all_expired_rows_for_a_locked_user_batch() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("independent reclamation cleanup should succeed");
        let tenant = create_test_tenant(&pool, "sweep-expiry", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "sweep-expiry", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");
        let first = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::from(3),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("first reservation should succeed");
        let second = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::from(2),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("second reservation should succeed");
        force_reservation_expired(&pool, first.id).await;
        force_reservation_expired(&pool, second.id).await;

        assert_eq!(
            service
                .reclaim_expired_request_reservations(1)
                .await
                .expect("independent reclamation should succeed"),
            2,
            "the batch limit is per user so a user's aggregate is reclaimed atomically"
        );
        assert_eq!(
            service
                .reclaim_expired_request_reservations(1)
                .await
                .expect("repeated reclamation should succeed"),
            0
        );
        let balance = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("raw balance query should succeed")
            .expect("balance should exist");
        assert_eq!(balance.available_balance, Decimal::from(10));
        assert_eq!(balance.frozen_balance, Decimal::ZERO);
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn independent_reclaimer_skips_a_lower_sorted_inconsistent_user_with_batch_size_one() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("poison-user reclamation cleanup should succeed");
        let tenant = create_test_tenant(&pool, "sweep-poison", &test_id).await;
        let first_user = create_test_user(&pool, tenant.id, "sweep-poison-first", &test_id).await;
        let second_user = create_test_user(&pool, tenant.id, "sweep-poison-second", &test_id).await;
        let (inconsistent_user, healthy_user) = if first_user.id < second_user.id {
            (first_user, second_user)
        } else {
            (second_user, first_user)
        };
        assert!(
            inconsistent_user.id < healthy_user.id,
            "the poison user must sort before its healthy peer"
        );
        let service = BalanceService::new(DbRouter::single(pool.clone()));

        for user_id in [inconsistent_user.id, healthy_user.id] {
            service
                .get_or_create(tenant.id, user_id)
                .await
                .expect("balance creation should succeed");
            service
                .recharge(user_id, tenant.id, Decimal::from(5), None, None)
                .await
                .expect("recharge should succeed");
        }

        let inconsistent_reservation = service
            .reserve_request(
                inconsistent_user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::ONE,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("inconsistent user's reservation should initially succeed");
        let inconsistent_unexpired_reservation = service
            .reserve_request(
                inconsistent_user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::from(4),
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("inconsistent user's unexpired reservation should initially succeed");
        let healthy_reservation = service
            .reserve_request(
                healthy_user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                Decimal::from(3),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("healthy user's reservation should succeed");
        force_reservation_expired(&pool, inconsistent_reservation.id).await;
        force_reservation_expired(&pool, healthy_reservation.id).await;

        // Simulate an out-of-band repair or a future buggy writer while still
        // satisfying the row-local nonnegative schema constraints. The one
        // expired unit does not itself exceed frozen, but all five active units
        // do; PostgreSQL cannot express this aggregate invariant across tables.
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_balances SET available_balance = 3, frozen_balance = 2 WHERE user_id = $1",
            [inconsistent_user.id.into()],
        ))
        .await
        .expect("the test should be able to construct a cross-table inconsistency");

        // Other tests may share the database and contribute earlier healthy
        // candidates. A batch of one must keep making progress past those and
        // must never let our lower-sorted poison user hide its healthy peer.
        let mut healthy_reclaimed = false;
        for _ in 0..10_000 {
            let reclaimed = service
                .reclaim_expired_request_reservations(1)
                .await
                .expect("one inconsistent user must not block healthy candidates");
            let healthy_status = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT status FROM balance_reservations WHERE id = $1",
                    [healthy_reservation.id.into()],
                ))
                .await
                .expect("healthy reservation status query should succeed")
                .and_then(|row| row.try_get_by_index::<String>(0).ok())
                .expect("healthy reservation should still exist");
            if healthy_status == "expired" {
                healthy_reclaimed = true;
                break;
            }
            assert!(
                reclaimed > 0,
                "the sweeper stopped before reaching the healthy reservation"
            );
        }
        assert!(
            healthy_reclaimed,
            "the healthy reservation must be reclaimed"
        );

        let healthy_balance = keycompute_db::UserBalance::find_by_user(&pool, healthy_user.id)
            .await
            .expect("healthy balance query should succeed")
            .expect("healthy balance should exist");
        assert_eq!(healthy_balance.available_balance, Decimal::from(5));
        assert_eq!(healthy_balance.frozen_balance, Decimal::ZERO);

        let inconsistent_balance =
            keycompute_db::UserBalance::find_by_user(&pool, inconsistent_user.id)
                .await
                .expect("inconsistent balance query should succeed")
                .expect("inconsistent balance should exist");
        assert_eq!(inconsistent_balance.available_balance, Decimal::from(3));
        assert_eq!(inconsistent_balance.frozen_balance, Decimal::from(2));

        for (reservation_id, expected_status) in [
            (inconsistent_reservation.id, "active"),
            (inconsistent_unexpired_reservation.id, "active"),
            (healthy_reservation.id, "expired"),
        ] {
            let status = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT status FROM balance_reservations WHERE id = $1",
                    [reservation_id.into()],
                ))
                .await
                .expect("reservation status query should succeed")
                .and_then(|row| row.try_get_by_index::<String>(0).ok())
                .expect("reservation should still exist");
            assert_eq!(status, expected_status);
        }

        let strict_error = service
            .find_breakdown_by_user(inconsistent_user.id)
            .await
            .expect_err("foreground balance reads must remain fail-closed on the invariant breach");
        assert!(
            strict_error
                .to_string()
                .contains("active balance reservations"),
            "the foreground path should expose the aggregate invariant failure: {strict_error}"
        );

        cleanup_test_data(&pool, &test_id)
            .await
            .expect("poison-user test data should not affect later sweeper tests");
    }

    #[tokio::test]
    async fn administrative_reservation_release_replays_sequentially_and_concurrently() {
        const CONCURRENCY: usize = 8;

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("admin release replay cleanup should succeed");
        let tenant = create_test_tenant(&pool, "admin-release-replay", &test_id).await;
        let user =
            create_test_user(&pool, tenant.id, "admin-release-replay-target", &test_id).await;
        let administrator =
            create_test_user(&pool, tenant.id, "admin-release-replay-actor", &test_id).await;
        let other_administrator =
            create_test_user(&pool, tenant.id, "admin-release-replay-other", &test_id).await;
        let other_user =
            create_test_user(&pool, tenant.id, "admin-release-replay-user", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("target balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");
        let request_id = uuid::Uuid::new_v4();
        let reservation = service
            .reserve_request(
                user.id,
                tenant.id,
                request_id,
                Decimal::from(6),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("request reservation should succeed");

        let barrier = Arc::new(Barrier::new(CONCURRENCY));
        let mut tasks = JoinSet::new();
        for _ in 0..CONCURRENCY {
            let service = service.clone();
            let barrier = barrier.clone();
            let owner_token = reservation.owner_token;
            tasks.spawn(async move {
                barrier.wait().await;
                service
                    .admin_release_request_reservation(
                        user.id,
                        request_id,
                        owner_token,
                        administrator.id,
                        "  confirmed orphan  ",
                    )
                    .await
            });
        }

        let mut releases = Vec::with_capacity(CONCURRENCY);
        while let Some(result) = tasks.join_next().await {
            releases.push(
                result
                    .expect("admin release task should not panic")
                    .expect("admin release task should succeed")
                    .expect("an identical concurrent release must replay successfully"),
            );
        }
        assert_eq!(releases.len(), CONCURRENCY);
        assert!(releases.iter().all(|release| {
            release.released_reservation.id == reservation.id
                && release.released_reservation.status == "released"
                && release.released_reservation.release_kind.as_deref() == Some("administrative")
                && release.released_reservation.release_reason.as_deref()
                    == Some("confirmed orphan")
                && release.released_reservation.released_by == Some(administrator.id)
                && release.breakdown.balance.available_balance == Decimal::from(10)
                && release.breakdown.balance.frozen_balance == Decimal::ZERO
        }));

        let sequential_replay = service
            .admin_release_request_reservation(
                user.id,
                request_id,
                reservation.owner_token,
                administrator.id,
                "confirmed orphan",
            )
            .await
            .expect("sequential admin release replay should succeed")
            .expect("an identical sequential release must replay successfully");
        assert_eq!(sequential_replay.released_reservation.id, reservation.id);
        assert_eq!(
            sequential_replay.breakdown.balance.available_balance,
            Decimal::from(10)
        );
        assert_eq!(
            sequential_replay.breakdown.balance.frozen_balance,
            Decimal::ZERO
        );

        for (conflict_user, conflict_owner, conflict_actor, conflict_reason) in [
            (
                user.id,
                uuid::Uuid::new_v4(),
                administrator.id,
                "confirmed orphan",
            ),
            (
                user.id,
                reservation.owner_token,
                other_administrator.id,
                "confirmed orphan",
            ),
            (
                user.id,
                reservation.owner_token,
                administrator.id,
                "changed reason",
            ),
            (
                other_user.id,
                reservation.owner_token,
                administrator.id,
                "confirmed orphan",
            ),
        ] {
            assert!(
                service
                    .admin_release_request_reservation(
                        conflict_user,
                        request_id,
                        conflict_owner,
                        conflict_actor,
                        conflict_reason,
                    )
                    .await
                    .expect("conflicting release should be a safe decision")
                    .is_none(),
                "a changed release identity field must conflict"
            );
        }

        let events = BalanceReservationEvent::find_by_request(&pool, request_id)
            .await
            .expect("reservation audit events should be queryable");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "released")
                .count(),
            1,
            "concurrent and sequential replays must append one release event"
        );
    }

    #[tokio::test]
    async fn reservation_pages_are_bounded_without_truncating_the_exact_aggregate() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("reservation pagination cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reservation-page", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reservation-page", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");

        let reservation_amount = Decimal::new(1, 2);
        let total_reserved = Decimal::new(101, 2);
        let tx = pool
            .begin()
            .await
            .expect("fixture transaction should start");
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_balances SET available_balance = available_balance - $1, frozen_balance = frozen_balance + $1 WHERE user_id = $2",
            [total_reserved.into(), user.id.into()],
        ))
        .await
        .expect("fixture balance should reserve the generated amount");
        let inserted = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"INSERT INTO balance_reservations
                   (request_id, owner_token, tenant_id, user_id, amount, expires_at, created_at, updated_at)
                   SELECT gen_random_uuid(), gen_random_uuid(), $1, $2, $3,
                          NOW() + INTERVAL '1 hour', NOW(), NOW()
                   FROM generate_series(1, 101)"#,
                [tenant.id.into(), user.id.into(), reservation_amount.into()],
            ))
            .await
            .expect("101 same-timestamp reservations should be inserted")
            .rows_affected();
        assert_eq!(inserted, 101);
        tx.commit()
            .await
            .expect("fixture transaction should commit");

        let first = service
            .find_breakdown_page_by_user(user.id, None, 100)
            .await
            .expect("first reservation page should succeed")
            .expect("balance should exist");
        assert_eq!(first.breakdown.balance.frozen_balance, total_reserved);
        assert_eq!(first.breakdown.active_reserved, total_reserved);
        assert_eq!(first.breakdown.manually_frozen, Decimal::ZERO);
        assert_eq!(first.reservations.len(), 100);
        let next_cursor = first
            .next_cursor
            .expect("the 101st reservation must produce a next cursor");

        let second = service
            .find_breakdown_page_by_user(user.id, Some(next_cursor), 100)
            .await
            .expect("second reservation page should succeed")
            .expect("balance should exist");
        assert_eq!(second.breakdown.active_reserved, total_reserved);
        assert_eq!(second.reservations.len(), 1);
        assert!(second.next_cursor.is_none());

        let reservations = first
            .reservations
            .into_iter()
            .chain(second.reservations)
            .collect::<Vec<_>>();
        assert_eq!(reservations.len(), 101);
        assert!(
            reservations
                .iter()
                .all(|reservation| reservation.created_at == reservations[0].created_at),
            "the fixture must exercise the id tie-breaker at every page position"
        );
        assert_eq!(
            reservations
                .iter()
                .map(|reservation| reservation.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            101,
            "the page boundary must neither repeat nor skip a same-timestamp row"
        );
        assert!(
            reservations
                .windows(2)
                .all(|pair| pair[0].created_at > pair[1].created_at
                    || (pair[0].created_at == pair[1].created_at && pair[0].id > pair[1].id)),
            "the cursor traversal must preserve the declared created_at/id order"
        );
    }

    #[tokio::test]
    async fn balance_page_reclaims_a_large_expired_backlog_with_exact_audit_totals() {
        const EXPIRED_ROWS: i64 = 257;

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("expired backlog cleanup should succeed");
        let tenant = create_test_tenant(&pool, "expired-backlog", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "expired-backlog", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");

        let reservation_amount = Decimal::new(1, 2);
        let total_reserved = Decimal::new(EXPIRED_ROWS, 2);
        let tx = pool
            .begin()
            .await
            .expect("fixture transaction should start");
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_balances SET available_balance = available_balance - $1, frozen_balance = frozen_balance + $1 WHERE user_id = $2",
            [total_reserved.into(), user.id.into()],
        ))
        .await
        .expect("fixture balance should reserve the generated amount");
        let inserted = tx
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                r#"INSERT INTO balance_reservations
                   (request_id, owner_token, tenant_id, user_id, amount, expires_at)
                   SELECT gen_random_uuid(), gen_random_uuid(), $1, $2, $3,
                          NOW() - INTERVAL '1 second'
                   FROM generate_series(1, 257)"#,
                [tenant.id.into(), user.id.into(), reservation_amount.into()],
            ))
            .await
            .expect("expired reservation backlog should be inserted")
            .rows_affected();
        assert_eq!(inserted, EXPIRED_ROWS as u64);
        tx.commit()
            .await
            .expect("fixture transaction should commit");

        let page = service
            .find_breakdown_page_by_user(user.id, None, 100)
            .await
            .expect("balance page should reclaim the expired backlog")
            .expect("balance should exist");
        assert_eq!(page.breakdown.balance.available_balance, Decimal::from(10));
        assert_eq!(page.breakdown.balance.frozen_balance, Decimal::ZERO);
        assert_eq!(page.breakdown.active_reserved, Decimal::ZERO);
        assert_eq!(page.breakdown.manually_frozen, Decimal::ZERO);
        assert!(page.reservations.is_empty());
        assert!(page.next_cursor.is_none());

        let expired_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM balance_reservations WHERE user_id = $1 AND status = 'expired'",
                [user.id.into()],
            ))
            .await
            .expect("expired reservation count should be queryable")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .expect("expired reservation count should be returned");
        assert_eq!(expired_count, EXPIRED_ROWS);

        let expired_event_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM balance_reservation_events WHERE user_id = $1 AND event_type = 'expired'",
                [user.id.into()],
            ))
            .await
            .expect("expiration audit count should be queryable")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .expect("expiration audit count should be returned");
        assert_eq!(expired_event_count, EXPIRED_ROWS);
    }

    #[tokio::test]
    async fn breakdown_and_admin_release_keep_request_funds_separate_and_audited() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("admin reservation release cleanup should succeed");
        let tenant = create_test_tenant(&pool, "admin-release", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "admin-release-target", &test_id).await;
        let administrator =
            create_test_user(&pool, tenant.id, "admin-release-actor", &test_id).await;
        let other_user = create_test_user(&pool, tenant.id, "admin-release-other", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("target balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");
        service
            .freeze(user.id, Decimal::from(3), Some("manual hold"))
            .await
            .expect("manual freeze should succeed");
        let request_id = uuid::Uuid::new_v4();
        let first_owner = service
            .reserve_request(
                user.id,
                tenant.id,
                request_id,
                Decimal::from(4),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("request reservation should succeed");
        let reservation = service
            .reserve_request(
                user.id,
                tenant.id,
                request_id,
                Decimal::from(4),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("request reservation reownership should succeed");
        assert_ne!(first_owner.owner_token, reservation.owner_token);

        let page = service
            .find_breakdown_page_by_user(user.id, None, 1)
            .await
            .expect("balance reservation page should succeed")
            .expect("balance should exist");
        let breakdown = &page.breakdown;
        assert_eq!(breakdown.balance.available_balance, Decimal::from(3));
        assert_eq!(breakdown.balance.frozen_balance, Decimal::from(7));
        assert_eq!(breakdown.active_reserved, Decimal::from(4));
        assert_eq!(breakdown.manually_frozen, Decimal::from(3));
        assert_eq!(
            page.reservations
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![reservation.id]
        );
        assert!(page.next_cursor.is_none());

        assert!(
            service
                .admin_release_request_reservation(
                    other_user.id,
                    request_id,
                    reservation.owner_token,
                    administrator.id,
                    "wrong target",
                )
                .await
                .expect("cross-user release should be a safe no-op")
                .is_none()
        );
        let oversized_reason = "界"
            .repeat(keycompute_billing::balance::MAX_BALANCE_RESERVATION_RELEASE_REASON_CHARS + 1);
        let oversized_error = service
            .admin_release_request_reservation(
                user.id,
                request_id,
                reservation.owner_token,
                administrator.id,
                &oversized_reason,
            )
            .await
            .expect_err("oversized release reason must be rejected");
        assert!(oversized_error.to_string().contains("must not exceed"));
        assert!(
            service
                .admin_release_request_reservation(
                    user.id,
                    request_id,
                    first_owner.owner_token,
                    administrator.id,
                    "stale selection",
                )
                .await
                .expect("stale owner release should be a safe no-op")
                .is_none(),
            "an administrator must not release a newer owner's reservation"
        );
        let after_stale_release = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query after stale release should succeed")
            .expect("balance should exist");
        assert_eq!(after_stale_release.available_balance, Decimal::from(3));
        assert_eq!(after_stale_release.frozen_balance, Decimal::from(7));
        let released_result = service
            .admin_release_request_reservation(
                user.id,
                request_id,
                reservation.owner_token,
                administrator.id,
                "operator confirmed orphaned request",
            )
            .await
            .expect("administrative release should succeed")
            .expect("active reservation should be released");
        let released = released_result.released_reservation;
        assert_eq!(
            released_result.breakdown.balance.available_balance,
            Decimal::from(7)
        );
        assert_eq!(
            released_result.breakdown.balance.frozen_balance,
            Decimal::from(3)
        );
        assert_eq!(released_result.breakdown.active_reserved, Decimal::ZERO);
        assert_eq!(released_result.breakdown.manually_frozen, Decimal::from(3));
        assert_eq!(released.status, "released");
        assert!(released.released_at.is_some());
        assert_eq!(released.release_kind.as_deref(), Some("administrative"));
        assert_eq!(released.released_by, Some(administrator.id));
        assert_eq!(
            released.release_reason.as_deref(),
            Some("operator confirmed orphaned request")
        );
        let exact_replay = service
            .admin_release_request_reservation(
                user.id,
                request_id,
                reservation.owner_token,
                administrator.id,
                "  operator confirmed orphaned request  ",
            )
            .await
            .expect("exact administrative release replay should succeed")
            .expect("matching administrative release should replay");
        assert_eq!(exact_replay.released_reservation.id, released.id);
        assert_eq!(
            exact_replay.breakdown.balance.available_balance,
            Decimal::from(7)
        );
        assert_eq!(
            exact_replay.breakdown.balance.frozen_balance,
            Decimal::from(3)
        );
        let events = BalanceReservationEvent::find_by_request(&pool, request_id)
            .await
            .expect("reservation audit events should be queryable");
        assert!(
            events
                .windows(2)
                .all(|pair| pair[0].event_sequence < pair[1].event_sequence),
            "reservation audit sequence must be strictly increasing"
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["reserved", "reowned", "released"]
        );
        assert!(events.iter().any(|event| {
            event.event_type == "reserved"
                && event.owner_token == first_owner.owner_token
                && event.status == "active"
        }));
        assert!(events.iter().any(|event| {
            event.event_type == "reowned"
                && event.owner_token == reservation.owner_token
                && event.status == "active"
        }));
        assert!(events.iter().any(|event| {
            event.event_type == "released"
                && event.owner_token == reservation.owner_token
                && event.status == "released"
                && event.release_kind.as_deref() == Some("administrative")
        }));
        let update_event_error = pool
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE balance_reservation_events SET event_type = 'updated' WHERE id = $1",
                [events[0].id.into()],
            ))
            .await
            .expect_err("reservation audit events must reject updates");
        assert!(update_event_error.to_string().contains("append-only"));
        let delete_event_error = pool
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM balance_reservation_events WHERE id = $1",
                [events[0].id.into()],
            ))
            .await
            .expect_err("reservation audit events must reject deletes");
        assert!(delete_event_error.to_string().contains("append-only"));
        assert!(
            service
                .admin_release_request_reservation(
                    user.id,
                    request_id,
                    reservation.owner_token,
                    administrator.id,
                    "idempotent replay",
                )
                .await
                .expect("repeated release should be a safe no-op")
                .is_none()
        );
        let reactivation_error = service
            .reserve_request(
                user.id,
                tenant.id,
                request_id,
                Decimal::ONE,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect_err("an administrative release must fence request reactivation");
        assert!(
            reactivation_error
                .to_string()
                .contains("administratively released"),
            "unexpected reactivation error: {reactivation_error}"
        );
        let after_fenced_reactivation = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query after fenced reactivation should succeed")
            .expect("balance should exist");
        assert_eq!(
            after_fenced_reactivation.available_balance,
            Decimal::from(7)
        );
        assert_eq!(after_fenced_reactivation.frozen_balance, Decimal::from(3));

        let now = Utc::now();
        let usage_log = UsageLog::create(
            &pool,
            &CreateUsageLogRequest {
                request_id: uuid::Uuid::new_v4(),
                tenant_id: tenant.id,
                user_id: user.id,
                produce_ai_key_id: uuid::Uuid::new_v4(),
                model_name: "gpt-late-settlement".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                input_tokens: 1,
                output_tokens: 1,
                input_unit_price_snapshot: BigDecimal::from(1),
                output_unit_price_snapshot: BigDecimal::from(1),
                user_amount: BigDecimal::from(12),
                currency: "CNY".to_string(),
                usage_source: "provider_reported".to_string(),
                status: "success".to_string(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .expect("late usage log should be created");
        assert!(
            service
                .settle_request_reservation(
                    request_id,
                    Some(reservation.owner_token),
                    Decimal::from(12),
                    usage_log.id,
                    Some("late settlement after admin release"),
                )
                .await
                .expect("released reservation lookup should succeed")
                .is_none(),
            "released request must fall back to ordinary ledger consumption"
        );
        let (late_balance, _) = service
            .consume(
                user.id,
                Decimal::from(12),
                Some(usage_log.id),
                Some("late settlement fallback"),
            )
            .await
            .expect("ledger-backed late settlement may record debt");
        assert_eq!(late_balance.available_balance, Decimal::from(-5));
        assert_eq!(late_balance.frozen_balance, Decimal::from(3));

        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM balance_reservations WHERE request_id = $1",
            [request_id.into()],
        ))
        .await
        .expect("live reservation deletion should succeed without deleting its audit history");
        let retained_events = BalanceReservationEvent::find_by_request(&pool, request_id)
            .await
            .expect("reservation audit events should survive live-row deletion");
        assert_eq!(retained_events.len(), events.len());
    }

    #[tokio::test]
    async fn schema_rejects_negative_frozen_and_cumulative_balances() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("balance constraint cleanup should succeed");
        let tenant = create_test_tenant(&pool, "balance-check", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "balance-check", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");

        for column in ["frozen_balance", "total_recharged", "total_consumed"] {
            let statement = Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "UPDATE user_balances SET {column} = -1 WHERE user_id = '{0}'",
                    user.id
                ),
            );
            pool.execute(statement)
                .await
                .expect_err("schema must reject negative protected balance columns");
        }
    }

    #[tokio::test]
    async fn zero_cost_ledger_consumption_remains_idempotently_recordable() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("zero-cost ledger cleanup should succeed");
        let tenant = create_test_tenant(&pool, "zero-ledger", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "zero-ledger", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("recharge should succeed");

        let now = Utc::now();
        let usage_log = UsageLog::create(
            &pool,
            &CreateUsageLogRequest {
                request_id: uuid::Uuid::new_v4(),
                tenant_id: tenant.id,
                user_id: user.id,
                produce_ai_key_id: uuid::Uuid::new_v4(),
                model_name: "gpt-zero-cost".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                input_tokens: 0,
                output_tokens: 0,
                input_unit_price_snapshot: BigDecimal::from(1),
                output_unit_price_snapshot: BigDecimal::from(1),
                user_amount: BigDecimal::from(0),
                currency: "CNY".to_string(),
                usage_source: "estimated".to_string(),
                status: "error".to_string(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .expect("zero-cost usage log should be created");

        let (first_balance, first_transaction) = service
            .consume(
                user.id,
                Decimal::ZERO,
                Some(usage_log.id),
                Some("zero-cost upstream rejection"),
            )
            .await
            .expect("zero-cost ledger side effect should be recorded");
        let (replayed_balance, replayed_transaction) = service
            .consume(
                user.id,
                Decimal::ZERO,
                Some(usage_log.id),
                Some("zero-cost upstream rejection replay"),
            )
            .await
            .expect("zero-cost ledger side effect should replay idempotently");
        assert_eq!(first_balance.available_balance, Decimal::from(10));
        assert_eq!(first_balance.frozen_balance, Decimal::ZERO);
        assert_eq!(replayed_balance.available_balance, Decimal::from(10));
        assert_eq!(first_transaction.id, replayed_transaction.id);
        assert_eq!(first_transaction.amount, Decimal::ZERO);
    }

    #[tokio::test]
    async fn reservation_settlement_records_a_consistent_consumption_delta() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("settlement cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reserve-settle", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reserve-settle", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("settlement test recharge should succeed");

        let billing_request_id = uuid::Uuid::new_v4();
        let first_owner = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(8),
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("bounded reservation should succeed");
        let reservation = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(8),
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("reservation reownership should succeed");
        assert_eq!(reservation.amount, Decimal::from(8));
        assert_ne!(first_owner.owner_token, reservation.owner_token);

        let now = Utc::now();
        let usage_log = UsageLog::create(
            &pool,
            &CreateUsageLogRequest {
                request_id: uuid::Uuid::new_v4(),
                tenant_id: tenant.id,
                user_id: user.id,
                produce_ai_key_id: uuid::Uuid::new_v4(),
                model_name: "gpt-test".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                input_tokens: 1,
                output_tokens: 1,
                input_unit_price_snapshot: BigDecimal::from(1),
                output_unit_price_snapshot: BigDecimal::from(1),
                user_amount: BigDecimal::from(3),
                currency: "CNY".to_string(),
                usage_source: "provider_reported".to_string(),
                status: "success".to_string(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .expect("usage log should be created");

        assert!(
            service
                .settle_request_reservation(
                    billing_request_id,
                    Some(first_owner.owner_token),
                    Decimal::from(3),
                    usage_log.id,
                    Some("stale reservation settlement attempt"),
                )
                .await
                .expect("stale settlement should be a safe no-op")
                .is_none(),
            "O1 must not consume funds after O2 takes ownership"
        );
        let after_stale_settlement = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query after stale settlement should succeed")
            .expect("balance should exist");
        assert_eq!(after_stale_settlement.available_balance, Decimal::from(2));
        assert_eq!(after_stale_settlement.frozen_balance, Decimal::from(8));

        // O1 follows the documented None path and records the immutable usage
        // debit against available balance while O2 still owns the reservation.
        let (fallback_balance, fallback_transaction) = service
            .consume(
                user.id,
                Decimal::from(3),
                Some(usage_log.id),
                Some("stale owner ordinary settlement fallback"),
            )
            .await
            .expect("stale owner fallback consumption should succeed");
        assert_eq!(fallback_balance.available_balance, Decimal::from(-1));
        assert_eq!(fallback_balance.frozen_balance, Decimal::from(8));
        assert_eq!(fallback_balance.total_consumed, Decimal::from(3));

        let (balance, transaction) = service
            .settle_request_reservation(
                billing_request_id,
                Some(reservation.owner_token),
                Decimal::from(3),
                usage_log.id,
                Some("reservation settlement test"),
            )
            .await
            .expect("reservation settlement should succeed")
            .expect("an active reservation should be settled");

        assert_eq!(transaction.id, fallback_transaction.id);
        assert_eq!(transaction.amount, Decimal::from(-3));
        assert_eq!(transaction.balance_before, Decimal::from(2));
        assert_eq!(transaction.balance_after, Decimal::from(-1));
        assert_eq!(
            transaction.balance_after - transaction.balance_before,
            transaction.amount
        );
        assert_eq!(balance.available_balance, Decimal::from(7));
        assert_eq!(balance.frozen_balance, Decimal::ZERO);
        assert_eq!(balance.total_consumed, Decimal::from(3));
        let settlement_events = BalanceReservationEvent::find_by_request(&pool, billing_request_id)
            .await
            .expect("settlement reservation audit events should be queryable");
        assert!(
            settlement_events
                .windows(2)
                .all(|pair| pair[0].event_sequence < pair[1].event_sequence)
        );
        assert_eq!(
            settlement_events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            vec!["reserved", "reowned", "settled"]
        );
        assert_eq!(
            settlement_events
                .last()
                .and_then(|event| event.usage_log_id),
            Some(usage_log.id)
        );

        let (replayed_balance, replayed_transaction) = service
            .settle_request_reservation(
                billing_request_id,
                Some(first_owner.owner_token),
                Decimal::from(3),
                usage_log.id,
                Some("settled reservation idempotent replay"),
            )
            .await
            .expect("settled reservation replay should succeed")
            .expect("the same settled usage log remains idempotent");
        assert_eq!(replayed_transaction.id, transaction.id);
        assert_eq!(replayed_balance.available_balance, Decimal::from(7));
        assert_eq!(replayed_balance.frozen_balance, Decimal::ZERO);

        let mismatched_replay_error = service
            .settle_request_reservation(
                billing_request_id,
                Some(reservation.owner_token),
                Decimal::from(4),
                usage_log.id,
                Some("settled reservation mismatched amount replay"),
            )
            .await
            .expect_err("a settled reservation must reject a different amount replay");
        assert!(
            mismatched_replay_error
                .to_string()
                .contains("already bound to a different balance consumption")
        );
        let balance_after_mismatched_replay =
            keycompute_db::UserBalance::find_by_user(&pool, user.id)
                .await
                .expect("balance query after mismatched replay should succeed")
                .expect("balance should exist");
        assert_eq!(
            balance_after_mismatched_replay.available_balance,
            Decimal::from(7)
        );
        assert_eq!(
            balance_after_mismatched_replay.frozen_balance,
            Decimal::ZERO
        );
        assert_eq!(
            balance_after_mismatched_replay.total_consumed,
            Decimal::from(3)
        );
    }

    #[tokio::test]
    async fn local_tpm_rejections_never_freeze_balance_across_generation_protocols() {
        use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
        use keycompute_server::{create_router, state::AppState};

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("local TPM rejection cleanup should succeed");
        let tenant = create_test_tenant(&pool, "local-tpm-balance", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "local-tpm-balance", &test_id).await;
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET default_rpm_limit = 100, default_tpm_limit = 10 WHERE id = $1",
            [tenant.id.into()],
        ))
        .await
        .expect("test tenant limits should be updated");

        let openai_model = format!("gpt-local-tpm-{test_id}");
        let anthropic_model = format!("claude-local-tpm-{test_id}");
        for (provider, name, model, capabilities) in [
            (
                "openai",
                format!("local-tpm-openai-{test_id}"),
                openai_model.clone(),
                vec!["chat_completions".to_string(), "responses".to_string()],
            ),
            (
                "anthropic",
                format!("local-tpm-anthropic-{test_id}"),
                anthropic_model.clone(),
                vec!["messages".to_string()],
            ),
        ] {
            keycompute_db::Account::create(
                &pool,
                &keycompute_db::CreateAccountRequest {
                    tenant_id: tenant.id,
                    provider: provider.to_string(),
                    name,
                    endpoint: format!("https://{provider}.invalid/v1"),
                    upstream_api_key_encrypted: "plain-test-key".to_string(),
                    upstream_api_key_preview: "plain****".to_string(),
                    rpm_limit: Some(100),
                    tpm_limit: Some(10),
                    priority: Some(100),
                    models_supported: vec![model],
                    api_capabilities: capabilities,
                    visibility: Some("tenant".to_string()),
                },
            )
            .await
            .expect("protocol account should be created");
        }

        let balance_service = BalanceService::new(DbRouter::single(pool.clone()));
        balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        let starting_balance = Decimal::from(100);
        balance_service
            .recharge(user.id, tenant.id, starting_balance, None, None)
            .await
            .expect("test balance recharge should succeed");

        let state = AppState::with_pool(DbRouter::single(pool.clone()));
        let token = state
            .auth
            .get_jwt_validator()
            .expect("JWT validator should be configured")
            .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
            .expect("user token should be generated");
        let rate_key = RateLimitKey::new(tenant.id, user.id, uuid::Uuid::nil());
        state
            .rate_limiter
            .reserve_token_usage(
                &rate_key,
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                10,
                &RateLimitConfig::new(100, 10),
            )
            .await
            .expect("test should occupy the complete TPM window");
        let app = create_router(state);

        let requests = [
            (
                "/v1/chat/completions",
                Vec::new(),
                serde_json::json!({
                    "model": openai_model,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
            ),
            (
                "/v1/messages",
                vec![("anthropic-version", "2023-06-01")],
                serde_json::json!({
                    "model": anthropic_model,
                    "max_tokens": 1,
                    "messages": [{"role": "user", "content": "hello"}]
                }),
            ),
            (
                "/v1/responses",
                Vec::new(),
                serde_json::json!({"model": openai_model, "input": "hello"}),
            ),
        ];
        for (uri, headers, body) in requests {
            let (status, body) = post_generation_json(&app, &token, uri, &headers, body).await;
            assert_eq!(
                status,
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "{uri} should reject locally at TPM admission: {body}"
            );
            assert!(
                body.pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|message| message.to_ascii_lowercase().contains("rate limit")),
                "{uri} should expose a rate-limit error: {body}"
            );
            assert_no_request_balance_reservation(&pool, user.id, starting_balance).await;
        }
    }

    #[tokio::test]
    async fn upstream_rate_limit_zero_cost_settlement_releases_reservation_and_replays() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("zero-cost reservation cleanup should succeed");
        let tenant = create_test_tenant(&pool, "reserve-zero", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "reserve-zero", &test_id).await;
        let router = Arc::new(DbRouter::single(pool.clone()));
        let balance_service = BalanceService::new(Arc::clone(&router));
        let billing_service = BillingService::with_pool(Arc::clone(&router));
        balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        balance_service
            .recharge(user.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("upstream rate-limit settlement recharge should succeed");

        let billing_request_id = uuid::Uuid::new_v4();
        let reservation_owner_token = uuid::Uuid::new_v4();
        let reservation = balance_service
            .reserve_request_with_owner_token(
                user.id,
                tenant.id,
                billing_request_id,
                reservation_owner_token,
                Decimal::from(4),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("bounded reservation should succeed");
        assert_eq!(reservation.amount, Decimal::from(4));
        let balance_while_reserved = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("reserved balance should remain readable");
        assert_eq!(balance_while_reserved.available_balance, Decimal::from(6));
        assert_eq!(balance_while_reserved.frozen_balance, Decimal::from(4));

        let request_context = Arc::new(RequestContext::new(
            billing_request_id,
            user.id,
            tenant.id,
            uuid::Uuid::new_v4(),
            "gpt-rate-limited",
            vec![Message::user("hello")],
            true,
            PricingSnapshot::new("gpt-rate-limited", "CNY", Decimal::ONE, Decimal::ONE),
        ));
        request_context.set_balance_reservation_owner_token(reservation_owner_token);

        let provider_name = "upstream-rate-limited";
        let account_id = uuid::Uuid::new_v4();
        let mut providers = HashMap::new();
        providers.insert(
            provider_name.to_string(),
            Arc::new(UpstreamRateLimitedProvider) as Arc<dyn ProviderAdapter>,
        );
        let executor = GatewayExecutor::new(
            GatewayConfig {
                max_retries: 0,
                enable_fallback: false,
                ..GatewayConfig::default()
            },
            providers,
        );
        let mut events = executor
            .execute(
                Arc::clone(&request_context),
                ExecutionPlan::new(ExecutionTarget::new_provider(
                    provider_name,
                    account_id,
                    "http://upstream.invalid",
                    "mock-key",
                )),
                Arc::new(AccountStateStore::new()),
                None,
            )
            .await
            .expect("gateway executor should start the upstream attempt");
        let terminal_event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .expect("upstream 429 should promptly terminate execution")
            .expect("gateway should emit a terminal event");
        assert!(
            matches!(terminal_event, StreamEvent::Error { .. }),
            "the real executor must surface the provider HTTP 429 as an error"
        );
        assert_eq!(request_context.usage_snapshot(), (0, 0));
        assert_eq!(request_context.executed_provider_account(), None);
        let upstream_response = request_context
            .client_upstream_response()
            .expect("the executor should preserve the provider HTTP response");
        assert_eq!(upstream_response.status, 429);
        assert!(upstream_response.body.contains("rate_limit_exceeded"));

        // This is the same billing entry point used by the HTTP handlers after
        // their executor receiver reaches a terminal Error event.
        let usage_log = billing_service
            .finalize_and_trigger_distribution(
                &request_context,
                provider_name,
                account_id,
                "error",
                user.id,
            )
            .await
            .expect("upstream 429 should write a zero-cost ledger and settle the reservation");
        assert_eq!(usage_log.request_id, billing_request_id);
        assert_eq!(usage_log.status, "error");
        assert_eq!(usage_log.input_tokens, 0);
        assert_eq!(usage_log.output_tokens, 0);
        assert_eq!(usage_log.user_amount, BigDecimal::from(0));

        let settled_balance = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("settled balance should remain readable");
        assert_eq!(settled_balance.available_balance, Decimal::from(10));
        assert_eq!(settled_balance.frozen_balance, Decimal::ZERO);
        assert_eq!(settled_balance.total_consumed, Decimal::ZERO);

        let transaction_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM balance_transactions WHERE usage_log_id = $1 AND transaction_type = 'consume' AND amount = 0",
                [usage_log.id.into()],
            ))
            .await
            .expect("zero-cost transaction count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(transaction_count, 1);

        let replayed_usage_log = billing_service
            .finalize_and_trigger_distribution(
                &request_context,
                provider_name,
                account_id,
                "error",
                user.id,
            )
            .await
            .expect("upstream 429 billing replay should be idempotent");
        assert_eq!(replayed_usage_log.id, usage_log.id);
        let replayed_balance = balance_service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("replayed balance should remain readable");
        assert_eq!(replayed_balance.available_balance, Decimal::from(10));
        assert_eq!(replayed_balance.frozen_balance, Decimal::ZERO);
        assert_eq!(replayed_balance.total_consumed, Decimal::ZERO);

        let transaction_count_after_replay = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM balance_transactions WHERE usage_log_id = $1 AND transaction_type = 'consume' AND amount = 0",
                [usage_log.id.into()],
            ))
            .await
            .expect("replayed zero-cost transaction count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(transaction_count_after_replay, 1);

        let reservation_events =
            BalanceReservationEvent::find_by_request(&pool, billing_request_id)
                .await
                .expect("reservation audit history should remain readable");
        assert_eq!(
            reservation_events
                .iter()
                .filter(|event| event.event_type == "settled")
                .count(),
            1,
            "billing replay must not settle the same reservation twice"
        );
    }

    #[tokio::test]
    async fn all_admin_balance_operation_kinds_are_concurrently_exactly_once() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("administrator idempotency cleanup should succeed");
        let tenant = create_test_tenant(&pool, "admin-idempotency", &test_id).await;
        let actor = create_test_user(&pool, tenant.id, "admin-idempotency-actor", &test_id).await;
        let recharge_user =
            create_test_user(&pool, tenant.id, "admin-idempotency-recharge", &test_id).await;
        let consume_user =
            create_test_user(&pool, tenant.id, "admin-idempotency-consume", &test_id).await;
        let freeze_user =
            create_test_user(&pool, tenant.id, "admin-idempotency-freeze", &test_id).await;
        let unfreeze_user =
            create_test_user(&pool, tenant.id, "admin-idempotency-unfreeze", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        for user in [&recharge_user, &consume_user, &freeze_user, &unfreeze_user] {
            service
                .get_or_create(tenant.id, user.id)
                .await
                .expect("balance creation should succeed");
        }
        for user in [&consume_user, &freeze_user, &unfreeze_user] {
            service
                .recharge(user.id, tenant.id, Decimal::from(10), None, None)
                .await
                .expect("test setup recharge should succeed");
        }
        service
            .freeze(
                unfreeze_user.id,
                Decimal::from(5),
                Some("test setup manual hold"),
            )
            .await
            .expect("test setup freeze should succeed");

        let cases = [
            (
                ManualBalanceOperationKind::Recharge,
                recharge_user.id,
                Decimal::from(2),
                "concurrent administrator recharge",
                Decimal::from(2),
                Decimal::ZERO,
            ),
            (
                ManualBalanceOperationKind::Consume,
                consume_user.id,
                Decimal::from(2),
                "concurrent administrator consume",
                Decimal::from(8),
                Decimal::ZERO,
            ),
            (
                ManualBalanceOperationKind::Freeze,
                freeze_user.id,
                Decimal::from(2),
                "concurrent administrator freeze",
                Decimal::from(8),
                Decimal::from(2),
            ),
            (
                ManualBalanceOperationKind::Unfreeze,
                unfreeze_user.id,
                Decimal::from(2),
                "concurrent administrator unfreeze",
                Decimal::from(7),
                Decimal::from(3),
            ),
        ];

        for (kind, user_id, amount, reason, expected_available, expected_frozen) in cases {
            let key = format!("admin-balance-{kind:?}-{}", uuid::Uuid::new_v4());
            let outcomes = run_concurrent_admin_balance_operation(
                &service, kind, tenant.id, user_id, actor.id, amount, reason, &key,
            )
            .await;
            let outcome = &outcomes[0];
            assert_eq!(outcome.operation_type, kind.as_str());
            assert_eq!(outcome.tenant_id, tenant.id);
            assert_eq!(outcome.user_id, user_id);
            assert_eq!(outcome.actor_user_id, actor.id);
            assert_eq!(outcome.amount, amount);
            assert_eq!(outcome.reason, reason);
            assert_eq!(outcome.balance_after, expected_available);
            assert_eq!(outcome.frozen_balance_after, expected_frozen);

            let balance = keycompute_db::UserBalance::find_by_user(&pool, user_id)
                .await
                .expect("balance query should succeed")
                .expect("balance should exist");
            assert_eq!(balance.available_balance, expected_available);
            assert_eq!(balance.frozen_balance, expected_frozen);

            let claim_count = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT COUNT(*) FROM admin_balance_operations WHERE id = $1 AND completed_at IS NOT NULL",
                    [outcome.operation_id.into()],
                ))
                .await
                .expect("administrator operation count should succeed")
                .and_then(|row| row.try_get_by_index::<i64>(0).ok())
                .unwrap_or_default();
            assert_eq!(claim_count, 1);
            let transaction_count = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT COUNT(*) FROM balance_transactions WHERE id = $1",
                    [outcome.balance_transaction_id.into()],
                ))
                .await
                .expect("administrator balance transaction count should succeed")
                .and_then(|row| row.try_get_by_index::<i64>(0).ok())
                .unwrap_or_default();
            assert_eq!(transaction_count, 1);
        }
    }

    #[tokio::test]
    async fn admin_balance_idempotency_replays_normalized_payload_and_conflicts_on_every_identity_field()
     {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("administrator conflict cleanup should succeed");
        let tenant = create_test_tenant(&pool, "admin-conflict", &test_id).await;
        let other_tenant = create_test_tenant(&pool, "admin-conflict-other", &test_id).await;
        let actor = create_test_user(&pool, tenant.id, "admin-conflict-actor", &test_id).await;
        let target = create_test_user(&pool, tenant.id, "admin-conflict-target", &test_id).await;
        let other_target =
            create_test_user(&pool, tenant.id, "admin-conflict-other-target", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        for user in [&target, &other_target] {
            service
                .get_or_create(tenant.id, user.id)
                .await
                .expect("balance creation should succeed");
        }
        let key = format!("admin-balance-conflict-{}", uuid::Uuid::new_v4());
        let first = service
            .apply_admin_manual_operation(
                ManualBalanceOperationKind::Recharge,
                tenant.id,
                target.id,
                actor.id,
                Decimal::from(2),
                "initial grant",
                &key,
            )
            .await
            .expect("initial administrator operation should succeed");
        let ManualBalanceOperationDecision::Completed(first) = first else {
            panic!("initial administrator operation must complete")
        };

        let normalized_replay = service
            .apply_admin_manual_operation(
                ManualBalanceOperationKind::Recharge,
                tenant.id,
                target.id,
                actor.id,
                Decimal::new(200, 2),
                "  initial grant  ",
                &key,
            )
            .await
            .expect("normalized replay should succeed");
        assert_eq!(
            normalized_replay,
            ManualBalanceOperationDecision::Completed(first.clone())
        );

        let conflicts = [
            (
                ManualBalanceOperationKind::Recharge,
                tenant.id,
                target.id,
                actor.id,
                Decimal::from(3),
                "initial grant",
            ),
            (
                ManualBalanceOperationKind::Recharge,
                tenant.id,
                target.id,
                actor.id,
                Decimal::from(2),
                "changed reason",
            ),
            (
                ManualBalanceOperationKind::Freeze,
                tenant.id,
                target.id,
                actor.id,
                Decimal::from(2),
                "initial grant",
            ),
            (
                ManualBalanceOperationKind::Recharge,
                tenant.id,
                other_target.id,
                actor.id,
                Decimal::from(2),
                "initial grant",
            ),
            (
                ManualBalanceOperationKind::Recharge,
                other_tenant.id,
                target.id,
                actor.id,
                Decimal::from(2),
                "initial grant",
            ),
            (
                ManualBalanceOperationKind::Recharge,
                tenant.id,
                target.id,
                uuid::Uuid::new_v4(),
                Decimal::from(2),
                "initial grant",
            ),
        ];
        for (kind, tenant_id, user_id, actor_user_id, amount, reason) in conflicts {
            assert_eq!(
                service
                    .apply_admin_manual_operation(
                        kind,
                        tenant_id,
                        user_id,
                        actor_user_id,
                        amount,
                        reason,
                        &key,
                    )
                    .await
                    .expect("conflicting reuse should return a decision"),
                ManualBalanceOperationDecision::Conflict
            );
        }

        let target_balance = keycompute_db::UserBalance::find_by_user(&pool, target.id)
            .await
            .expect("target balance query should succeed")
            .expect("target balance should exist");
        assert_eq!(target_balance.available_balance, Decimal::from(2));
        assert_eq!(target_balance.frozen_balance, Decimal::ZERO);
        let other_balance = keycompute_db::UserBalance::find_by_user(&pool, other_target.id)
            .await
            .expect("other target balance query should succeed")
            .expect("other target balance should exist");
        assert_eq!(other_balance.available_balance, Decimal::ZERO);
        assert_eq!(other_balance.frozen_balance, Decimal::ZERO);
        let claim_row = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT idempotency_key_hash, request_fingerprint FROM admin_balance_operations WHERE id = $1",
                [first.operation_id.into()],
            ))
            .await
            .expect("administrator claim query should succeed")
            .expect("administrator claim should exist");
        let key_hash = claim_row
            .try_get_by_index::<String>(0)
            .expect("key hash should be readable");
        let fingerprint = claim_row
            .try_get_by_index::<String>(1)
            .expect("request fingerprint should be readable");
        assert_eq!(key_hash.len(), 64);
        assert_eq!(fingerprint.len(), 64);
        assert_ne!(key_hash, key, "the plaintext key must never be persisted");
    }

    #[tokio::test]
    async fn admin_balance_http_api_requires_keys_replays_results_and_returns_conflict() {
        use axum::http::StatusCode;
        use keycompute_server::{create_router, state::AppState};
        use keycompute_types::UserRole;

        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("administrator balance API cleanup should succeed");
        let tenant = create_test_tenant(&pool, "admin-balance-api", &test_id).await;
        let admin = keycompute_db::User::create(
            &pool,
            &keycompute_db::CreateUserRequest {
                tenant_id: tenant.id,
                email: format!("test-admin-balance-api-{test_id}@example.com"),
                name: Some("Balance API administrator".to_string()),
                role: Some(UserRole::Admin),
            },
        )
        .await
        .expect("administrator should be created");
        let target = create_test_user(&pool, tenant.id, "admin-balance-api-target", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, target.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(target.id, tenant.id, Decimal::from(10), None, None)
            .await
            .expect("test setup recharge should succeed");

        let state = AppState::with_pool(DbRouter::single(pool.clone()));
        let token = state
            .auth
            .get_jwt_validator()
            .expect("JWT validator should be configured")
            .generate_token_with_version(
                admin.id,
                admin.tenant_id,
                &admin.role,
                admin.token_version,
            )
            .expect("administrator token should be generated");
        let app = create_router(state);
        let freeze_uri = format!("/api/v1/users/{}/balance/freeze", target.id);
        let freeze_key = format!("http-freeze-{}", uuid::Uuid::new_v4());
        let (first_status, first_body) = post_admin_balance_operation(
            &app,
            &token,
            &freeze_uri,
            Some(&freeze_key),
            "2.00",
            "  incident hold  ",
        )
        .await;
        assert_eq!(first_status, StatusCode::OK, "{first_body}");
        assert_eq!(first_body["reason"], "incident hold");
        let (replay_status, replay_body) = post_admin_balance_operation(
            &app,
            &token,
            &freeze_uri,
            Some(&freeze_key),
            "2",
            "incident hold",
        )
        .await;
        assert_eq!(replay_status, StatusCode::OK, "{replay_body}");
        assert_eq!(replay_body, first_body);

        let (conflict_status, conflict_body) = post_admin_balance_operation(
            &app,
            &token,
            &freeze_uri,
            Some(&freeze_key),
            "3",
            "incident hold",
        )
        .await;
        assert_eq!(conflict_status, StatusCode::CONFLICT, "{conflict_body}");
        assert_eq!(conflict_body["error"]["code"], 409);

        let unfreeze_uri = format!("/api/v1/users/{}/balance/unfreeze", target.id);
        let (missing_status, missing_body) = post_admin_balance_operation(
            &app,
            &token,
            &unfreeze_uri,
            None,
            "2",
            "release incident hold",
        )
        .await;
        assert_eq!(missing_status, StatusCode::BAD_REQUEST, "{missing_body}");
        assert!(
            missing_body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Idempotency-Key"))
        );

        let unfreeze_key = format!("http-unfreeze-{}", uuid::Uuid::new_v4());
        let (unfreeze_status, unfreeze_body) = post_admin_balance_operation(
            &app,
            &token,
            &unfreeze_uri,
            Some(&unfreeze_key),
            "2",
            "release incident hold",
        )
        .await;
        assert_eq!(unfreeze_status, StatusCode::OK, "{unfreeze_body}");
        let (unfreeze_replay_status, unfreeze_replay_body) = post_admin_balance_operation(
            &app,
            &token,
            &unfreeze_uri,
            Some(&unfreeze_key),
            "2.0",
            " release incident hold ",
        )
        .await;
        assert_eq!(unfreeze_replay_status, StatusCode::OK);
        assert_eq!(unfreeze_replay_body, unfreeze_body);

        let update_uri = format!("/api/v1/users/{}/balance", target.id);
        let update_key = format!("http-update-{}", uuid::Uuid::new_v4());
        let (update_status, update_body) = post_admin_balance_operation(
            &app,
            &token,
            &update_uri,
            Some(&update_key),
            "1",
            "small grant",
        )
        .await;
        assert_eq!(update_status, StatusCode::OK, "{update_body}");
        let (update_replay_status, update_replay_body) = post_admin_balance_operation(
            &app,
            &token,
            &update_uri,
            Some(&update_key),
            "1.00",
            "small grant",
        )
        .await;
        assert_eq!(update_replay_status, StatusCode::OK);
        assert_eq!(update_replay_body, update_body);
        let (signed_conflict_status, signed_conflict_body) = post_admin_balance_operation(
            &app,
            &token,
            &update_uri,
            Some(&update_key),
            "-1",
            "small grant",
        )
        .await;
        assert_eq!(
            signed_conflict_status,
            StatusCode::CONFLICT,
            "{signed_conflict_body}"
        );

        let final_balance = keycompute_db::UserBalance::find_by_user(&pool, target.id)
            .await
            .expect("final balance query should succeed")
            .expect("final balance should exist");
        assert_eq!(final_balance.available_balance, Decimal::from(11));
        assert_eq!(final_balance.frozen_balance, Decimal::ZERO);
    }

    #[tokio::test]
    async fn failed_admin_balance_operations_leave_no_half_claim_and_same_key_can_later_succeed() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("failed administrator operation cleanup should succeed");
        let tenant = create_test_tenant(&pool, "admin-failed-claim", &test_id).await;
        let actor = create_test_user(&pool, tenant.id, "admin-failed-claim-actor", &test_id).await;
        let freeze_user = create_test_user(&pool, tenant.id, "admin-failed-freeze", &test_id).await;
        let consume_user =
            create_test_user(&pool, tenant.id, "admin-failed-consume", &test_id).await;
        let unfreeze_user =
            create_test_user(&pool, tenant.id, "admin-failed-unfreeze", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        for user in [&freeze_user, &consume_user, &unfreeze_user] {
            service
                .get_or_create(tenant.id, user.id)
                .await
                .expect("balance creation should succeed");
        }

        let freeze_key = format!("failed-freeze-{}", uuid::Uuid::new_v4());
        service
            .apply_admin_manual_operation(
                ManualBalanceOperationKind::Freeze,
                tenant.id,
                freeze_user.id,
                actor.id,
                Decimal::from(2),
                "freeze after funding",
                &freeze_key,
            )
            .await
            .expect_err("freeze without available funds should fail");

        let consume_key = format!("failed-consume-{}", uuid::Uuid::new_v4());
        service
            .apply_admin_manual_operation(
                ManualBalanceOperationKind::Consume,
                tenant.id,
                consume_user.id,
                actor.id,
                Decimal::from(2),
                "consume after funding",
                &consume_key,
            )
            .await
            .expect_err("consume without available funds should fail");

        let unfreeze_key = format!("failed-unfreeze-{}", uuid::Uuid::new_v4());
        service
            .apply_admin_manual_operation(
                ManualBalanceOperationKind::Unfreeze,
                tenant.id,
                unfreeze_user.id,
                actor.id,
                Decimal::from(2),
                "unfreeze after hold",
                &unfreeze_key,
            )
            .await
            .expect_err("unfreeze without manual funds should fail");

        let failed_claim_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM admin_balance_operations WHERE tenant_id = $1",
                [tenant.id.into()],
            ))
            .await
            .expect("failed claim count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(failed_claim_count, 0);

        for user in [&freeze_user, &consume_user, &unfreeze_user] {
            service
                .recharge(user.id, tenant.id, Decimal::from(2), None, None)
                .await
                .expect("funding should succeed");
        }
        service
            .freeze(
                unfreeze_user.id,
                Decimal::from(2),
                Some("manual hold before idempotent unfreeze"),
            )
            .await
            .expect("manual hold should succeed");

        for (kind, user_id, reason, key) in [
            (
                ManualBalanceOperationKind::Freeze,
                freeze_user.id,
                "freeze after funding",
                freeze_key,
            ),
            (
                ManualBalanceOperationKind::Consume,
                consume_user.id,
                "consume after funding",
                consume_key,
            ),
            (
                ManualBalanceOperationKind::Unfreeze,
                unfreeze_user.id,
                "unfreeze after hold",
                unfreeze_key,
            ),
        ] {
            assert!(matches!(
                service
                    .apply_admin_manual_operation(
                        kind,
                        tenant.id,
                        user_id,
                        actor.id,
                        Decimal::from(2),
                        reason,
                        &key,
                    )
                    .await
                    .expect("same key should succeed after the balance condition is repaired"),
                ManualBalanceOperationDecision::Completed(_)
            ));
        }
        let completed_claim_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM admin_balance_operations WHERE tenant_id = $1 AND completed_at IS NOT NULL",
                [tenant.id.into()],
            ))
            .await
            .expect("completed claim count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(completed_claim_count, 3);
        let pending_claim_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM admin_balance_operations WHERE tenant_id = $1 AND completed_at IS NULL",
                [tenant.id.into()],
            ))
            .await
            .expect("pending claim count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(pending_claim_count, 0);
    }

    #[tokio::test]
    #[serial_test::serial(balance_reservation_sweeper)]
    async fn manual_consume_and_freeze_reclaim_expired_reservations_before_balance_checks() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("manual mutation expiry cleanup should succeed");
        let tenant = create_test_tenant(&pool, "manual-expiry", &test_id).await;
        let consume_user =
            create_test_user(&pool, tenant.id, "manual-expiry-consume", &test_id).await;
        let freeze_user =
            create_test_user(&pool, tenant.id, "manual-expiry-freeze", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));

        let mut reservations = Vec::new();
        for user in [&consume_user, &freeze_user] {
            service
                .get_or_create(tenant.id, user.id)
                .await
                .expect("balance creation should succeed");
            service
                .recharge(user.id, tenant.id, Decimal::from(4), None, None)
                .await
                .expect("recharge should succeed");
            let reservation = service
                .reserve_request(
                    user.id,
                    tenant.id,
                    uuid::Uuid::new_v4(),
                    Decimal::from(4),
                    std::time::Duration::from_secs(60),
                )
                .await
                .expect("full explicit reservation should succeed");
            force_reservation_expired(&pool, reservation.id).await;
            reservations.push(reservation);
        }

        let (consumed_balance, consume_transaction) = service
            .consume(
                consume_user.id,
                Decimal::from(2),
                None,
                Some("manual consume after reservation expiry"),
            )
            .await
            .expect("expired reservation must not cause a false insufficient consume");
        assert_eq!(consumed_balance.available_balance, Decimal::from(2));
        assert_eq!(consumed_balance.frozen_balance, Decimal::ZERO);
        assert_eq!(consumed_balance.total_consumed, Decimal::from(2));
        assert_eq!(consume_transaction.balance_before, Decimal::from(4));
        assert_eq!(consume_transaction.balance_after, Decimal::from(2));

        let (frozen_balance, freeze_transaction) = service
            .freeze(
                freeze_user.id,
                Decimal::from(2),
                Some("manual freeze after reservation expiry"),
            )
            .await
            .expect("expired reservation must not cause a false insufficient freeze");
        assert_eq!(frozen_balance.available_balance, Decimal::from(2));
        assert_eq!(frozen_balance.frozen_balance, Decimal::from(2));
        assert_eq!(freeze_transaction.balance_before, Decimal::from(4));
        assert_eq!(freeze_transaction.balance_after, Decimal::from(2));

        for reservation in reservations {
            let status = pool
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT status FROM balance_reservations WHERE id = $1",
                    [reservation.id.into()],
                ))
                .await
                .expect("reservation status query should succeed")
                .and_then(|row| row.try_get_by_index::<String>(0).ok());
            assert_eq!(status.as_deref(), Some("expired"));
        }
    }

    #[tokio::test]
    async fn settlement_above_reservation_releases_all_frozen_and_debits_actual_once() {
        let pool = create_test_pool().await;
        let test_id = generate_test_id();
        cleanup_test_data(&pool, &test_id)
            .await
            .expect("above-reservation settlement cleanup should succeed");
        let tenant = create_test_tenant(&pool, "settle-above-reserved", &test_id).await;
        let user = create_test_user(&pool, tenant.id, "settle-above-reserved", &test_id).await;
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        service
            .get_or_create(tenant.id, user.id)
            .await
            .expect("balance creation should succeed");
        service
            .recharge(user.id, tenant.id, Decimal::from(5), None, None)
            .await
            .expect("recharge should succeed");

        let billing_request_id = uuid::Uuid::new_v4();
        let reservation = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Decimal::from(2),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("reservation should succeed");
        let now = Utc::now();
        let usage_log = UsageLog::create(
            &pool,
            &CreateUsageLogRequest {
                request_id: uuid::Uuid::new_v4(),
                tenant_id: tenant.id,
                user_id: user.id,
                produce_ai_key_id: uuid::Uuid::new_v4(),
                model_name: "gpt-above-reserved".to_string(),
                provider_name: "openai".to_string(),
                account_id: uuid::Uuid::new_v4(),
                input_tokens: 1,
                output_tokens: 1,
                input_unit_price_snapshot: BigDecimal::from(1),
                output_unit_price_snapshot: BigDecimal::from(1),
                user_amount: BigDecimal::from(8),
                currency: "CNY".to_string(),
                usage_source: "provider_reported".to_string(),
                status: "success".to_string(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .expect("usage log should be created");

        let (settled_balance, transaction) = service
            .settle_request_reservation(
                billing_request_id,
                Some(reservation.owner_token),
                Decimal::from(8),
                usage_log.id,
                Some("actual charge exceeded reservation"),
            )
            .await
            .expect("settlement should succeed")
            .expect("active reservation should settle");
        assert_eq!(settled_balance.available_balance, Decimal::from(-3));
        assert_eq!(settled_balance.frozen_balance, Decimal::ZERO);
        assert_eq!(settled_balance.total_consumed, Decimal::from(8));
        assert_eq!(transaction.amount, Decimal::from(-8));
        assert_eq!(transaction.balance_before, Decimal::from(5));
        assert_eq!(transaction.balance_after, Decimal::from(-3));

        let (replayed_balance, replayed_transaction) = service
            .settle_request_reservation(
                billing_request_id,
                Some(reservation.owner_token),
                Decimal::from(8),
                usage_log.id,
                Some("actual charge exceeded reservation replay"),
            )
            .await
            .expect("settlement replay should succeed")
            .expect("settled reservation should replay");
        assert_eq!(replayed_transaction.id, transaction.id);
        assert_eq!(replayed_balance.available_balance, Decimal::from(-3));
        assert_eq!(replayed_balance.frozen_balance, Decimal::ZERO);
        assert_eq!(replayed_balance.total_consumed, Decimal::from(8));

        let transaction_count = pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*) FROM balance_transactions WHERE usage_log_id = $1 AND transaction_type = 'consume'",
                [usage_log.id.into()],
            ))
            .await
            .expect("settlement transaction count should succeed")
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or_default();
        assert_eq!(transaction_count, 1);
    }
}
