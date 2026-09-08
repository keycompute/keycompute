//! 余额冻结/解冻测试

use bigdecimal::BigDecimal;
use chrono::Utc;
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_billing::balance::BalanceService;
use keycompute_db::{CreateUsageLogRequest, DbRouter, UsageLog};
use rust_decimal::Decimal;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
    use std::sync::Arc;
    use tokio::sync::Barrier;
    use tokio::task::JoinSet;

    async fn force_reservation_expired(pool: &DatabaseConnection, reservation_id: uuid::Uuid) {
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET expires_at = NOW() - INTERVAL '1 second' WHERE id = $1",
            [reservation_id.into()],
        ))
        .await
        .expect("reservation expiry should be forced for the test");
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
    }

    /// 测试冻结余额为 0 时冻结应成功（边界情况）
    #[tokio::test]
    async fn test_balance_freeze_zero_amount() {
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

        // 冻结 0 元：Decimal::ZERO，应被 DB 层拒绝（amount > 0 检查由调用方负责）
        // BalanceService::freeze 调用 UserBalance::freeze，后者检查 available_balance < amount
        // 0 < 0 为 false，所以不会报 insufficient，但 UPDATE 会执行 frozen_balance += 0
        // 这是允许的操作（幂等冻结 0 元）
        let (after, tx) = balance_service
            .freeze(user.id, Decimal::ZERO, Some("zero freeze"))
            .await
            .expect("freeze zero amount should succeed (no-op)");

        assert_eq!(after.available_balance, Decimal::ZERO);
        assert_eq!(after.frozen_balance, Decimal::ZERO);
        assert_eq!(tx.amount, Decimal::ZERO);
        assert_eq!(tx.transaction_type, "freeze");
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
    async fn concurrent_unbounded_requests_cannot_create_a_zero_reservation() {
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
                        None,
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
        let first = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                None,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("first reservation owner should succeed");
        let replacement = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                None,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("replacement reservation owner should succeed");

        assert_ne!(first.owner_token, replacement.owner_token);
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
                Some(Decimal::from(3)),
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
                Some(Decimal::from(5)),
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("competing reservation should succeed");
        let failed_growth = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Some(Decimal::from(6)),
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
                Some(Decimal::from(3)),
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
                Some(Decimal::from(6)),
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
                Some(Decimal::from(2)),
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

        let unbounded = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                None,
                std::time::Duration::from_secs(60),
            )
            .await
            .expect("unbounded replacement should own all reservable funds");
        assert_eq!(unbounded.amount, Decimal::from(10));
        let fully_reserved = keycompute_db::UserBalance::find_by_user(&pool, user.id)
            .await
            .expect("balance query should succeed")
            .expect("balance should exist");
        assert_eq!(fully_reserved.available_balance, Decimal::ZERO);
        assert_eq!(fully_reserved.frozen_balance, Decimal::from(10));

        assert!(
            service
                .release_request_reservation(billing_request_id, unbounded.owner_token)
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
    async fn expired_all_balance_reservation_is_reclaimed_before_admission() {
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
                None,
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("initial all-balance reservation should succeed");
        force_reservation_expired(&pool, stale.id).await;

        let replacement = service
            .reserve_request(
                user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                None,
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
                None,
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("single-read reservation should succeed");
        let batch_reservation = service
            .reserve_request(
                batch_user.id,
                tenant.id,
                uuid::Uuid::new_v4(),
                None,
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
                Some(Decimal::from(4)),
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
        let reservation = service
            .reserve_request(
                user.id,
                tenant.id,
                billing_request_id,
                Some(Decimal::from(8)),
                std::time::Duration::from_secs(26 * 60 * 60),
            )
            .await
            .expect("bounded reservation should succeed");
        assert_eq!(reservation.amount, Decimal::from(8));

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

        let (balance, transaction) = service
            .settle_request_reservation(
                billing_request_id,
                Decimal::from(3),
                usage_log.id,
                Some("reservation settlement test"),
            )
            .await
            .expect("reservation settlement should succeed")
            .expect("an active reservation should be settled");

        assert_eq!(transaction.amount, Decimal::from(-3));
        assert_eq!(transaction.balance_before, Decimal::from(10));
        assert_eq!(transaction.balance_after, Decimal::from(7));
        assert_eq!(
            transaction.balance_after - transaction.balance_before,
            transaction.amount
        );
        assert_eq!(balance.available_balance, Decimal::from(7));
        assert_eq!(balance.frozen_balance, Decimal::ZERO);
        assert_eq!(balance.total_consumed, Decimal::from(3));
    }
}
