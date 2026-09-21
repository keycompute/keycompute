//! Payment order state-machine and database constraint integration tests.

use chrono::{Duration, Utc};
use integration_tests::common::generate_test_id;
use integration_tests::db::{
    cleanup_test_data, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_db::{
    CreatePaymentOrderRequest, CreateTenantMembershipRequest, CreditPaidOrderError, DbError,
    PaymentMethod, PaymentOrder, Tenant, TenantMembership, UserBalance,
    purge_expired_payment_security_events,
};
use keycompute_types::{CredentialKind, PlatformRole, TenantRole};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait};
use std::time::Duration as TokioDuration;

#[derive(FromQueryResult)]
struct CountRow {
    count: i64,
}

#[derive(FromQueryResult)]
struct NotificationStatusRow {
    processing_status: String,
}

/// Payment-order creation must acquire the tenant parent lock before locking
/// the user and inserting the child row. Otherwise a direct tenant delete can
/// hold the tenant lock while creation holds the user lock, deadlocking the
/// delete cascade.
#[tokio::test]
async fn payment_order_create_does_not_deadlock_with_tenant_delete() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment order cleanup should succeed");

    let tenant = create_test_tenant(&pool, "payment-delete-race", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "payment-delete-race", &test_id).await;

    // Hold the same parent lock used by Tenant::delete_in_tx before starting
    // a create request for a child payment order.
    let delete_tx = pool.begin().await.expect("delete transaction should begin");
    Tenant::find_by_id_for_update(&delete_tx, tenant.id)
        .await
        .expect("tenant lock should succeed")
        .expect("tenant should exist");

    let create_pool = pool.clone();
    let create_request = CreatePaymentOrderRequest {
        tenant_id: tenant.id,
        user_id: user.id,
        amount: Decimal::ONE,
        subject: "delete-race".to_string(),
        body: None,
        payment_method: PaymentMethod::WechatPay,
        payment_scene: "native".to_string(),
        expired_at: Utc::now() + Duration::minutes(30),
    };
    let out_trade_no = format!("TESTDELETE{}", test_id.replace('-', ""));
    let create_task = tokio::spawn(async move {
        PaymentOrder::create(&create_pool, &create_request, &out_trade_no, "").await
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
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND (query ILIKE '%payment_orders%' OR query ILIKE '%FROM tenants WHERE id = $1 FOR KEY SHARE%')) AS waiting".to_string(),
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
        tokio::time::sleep(TokioDuration::from_millis(10)).await;
    }
    assert!(
        create_is_waiting,
        "payment-order creation should wait on the tenant parent lock"
    );

    // Parent-first ordering lets the delete cascade complete while creation
    // is blocked; creation then observes the missing tenant and fails cleanly.
    tokio::time::timeout(TokioDuration::from_secs(3), tenant.delete_in_tx(&delete_tx))
        .await
        .expect("tenant delete should not deadlock")
        .expect("tenant delete should succeed");
    delete_tx
        .commit()
        .await
        .expect("tenant delete transaction should commit");

    let create_result = tokio::time::timeout(TokioDuration::from_secs(10), create_task)
        .await
        .expect("payment-order creation should finish after tenant deletion")
        .expect("create task should not panic");
    assert!(
        create_result.is_err(),
        "creating an order for a deleted tenant must fail"
    );

    cleanup_test_data(&pool, &test_id).await.ok();
}

/// Payment callbacks must lock the tenant parent before the order and balance
/// rows.  Otherwise a callback that already owns the order can wait on the
/// tenant FK while a direct tenant delete waits on that same order during its
/// cascade.  Keep the delete transaction open while the callback is observed
/// waiting on its parent lock; the delete must then complete and the callback
/// must fail cleanly after the order is cascaded.
#[tokio::test]
async fn payment_order_credit_does_not_deadlock_with_tenant_delete() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment credit/delete race cleanup should succeed");

    let tenant = create_test_tenant(&pool, "payment-credit-delete-race", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "payment-credit-delete-race", &test_id).await;
    let order = PaymentOrder::create(
        &pool,
        &CreatePaymentOrderRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            amount: Decimal::ONE,
            subject: "credit/delete race".to_string(),
            body: None,
            payment_method: PaymentMethod::WechatPay,
            payment_scene: "native".to_string(),
            expired_at: Utc::now() + Duration::minutes(30),
        },
        &format!("TESTCREDITDELETE{}", test_id.replace('-', "")),
        "",
    )
    .await
    .expect("payment order should be created");

    // Hold the parent lock exactly as Tenant::delete_in_tx does before it
    // reaches the cascading DELETE statement.
    let delete_tx = pool.begin().await.expect("delete transaction should begin");
    Tenant::find_by_id_for_update(&delete_tx, tenant.id)
        .await
        .expect("tenant lock should succeed")
        .expect("tenant should exist");

    let callback_pool = pool.clone();
    let callback = tokio::spawn(async move {
        PaymentOrder::credit_paid(
            &callback_pool,
            order.id,
            "CREDIT-DELETE-TRADE",
            "CREDIT-DELETE-EVENT",
            serde_json::json!({"trade_no": "CREDIT-DELETE-TRADE"}),
            "credit/delete race",
        )
        .await
    });

    #[derive(Debug, FromQueryResult)]
    struct Waiting {
        waiting: bool,
    }
    let mut callback_is_waiting = false;
    for _ in 0..300 {
        let row = pool
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid <> pg_backend_pid() AND wait_event_type = 'Lock' AND query ILIKE '%FOR KEY SHARE OF tenants%') AS waiting".to_string(),
            ))
            .await
            .expect("lock wait probe should succeed")
            .expect("lock wait probe should return a row");
        if Waiting::from_query_result(&row, "")
            .expect("waiting flag should decode")
            .waiting
        {
            callback_is_waiting = true;
            break;
        }
        tokio::time::sleep(TokioDuration::from_millis(10)).await;
    }
    assert!(
        callback_is_waiting,
        "payment callback should wait on the tenant parent lock before touching the order"
    );

    // The HTTP/model lifecycle guard normally rejects a tenant that still has
    // payment history. Exercise the database cascade directly here so the
    // lock-order invariant remains covered even for a future low-level caller
    // that bypasses that guard (or a schema-triggered cascade).
    tokio::time::timeout(
        TokioDuration::from_secs(3),
        delete_tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM tenants WHERE id = $1",
            [tenant.id.into()],
        )),
    )
    .await
    .expect("tenant delete should not deadlock with payment credit")
    .expect_err("retained financial order must reject tenant deletion");
    delete_tx
        .rollback()
        .await
        .expect("failed tenant delete transaction should roll back");

    let callback_result = tokio::time::timeout(TokioDuration::from_secs(10), callback)
        .await
        .expect("payment callback should finish after tenant deletion")
        .expect("payment callback task should not panic");
    assert!(
        matches!(callback_result, Ok(true)),
        "retained payment must credit after deletion is rejected, got {callback_result:?}"
    );

    cleanup_test_data(&pool, &test_id).await.ok();
}

#[tokio::test]
async fn failed_transition_rejects_an_existing_paid_order() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment order cleanup should succeed");
    let tenant = create_test_tenant(&pool, "payment-state", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "payment-state", &test_id).await;
    let expired_at = Utc::now() + Duration::minutes(30);
    let out_trade_no = format!("TESTPAY{}", test_id.replace('-', ""));
    let order = PaymentOrder::create(
        &pool,
        &CreatePaymentOrderRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            amount: Decimal::ONE,
            subject: "state transition test".to_string(),
            body: None,
            payment_method: PaymentMethod::WechatPay,
            payment_scene: "native".to_string(),
            expired_at,
        },
        &out_trade_no,
        "",
    )
    .await
    .expect("payment order should be created");
    assert!(
        (order.expired_at - expired_at)
            .num_microseconds()
            .is_some_and(|difference| difference.abs() <= 1),
        "database expiration should preserve the provider deadline within PostgreSQL precision"
    );

    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE payment_orders SET status='paid', provider_trade_no=$1, paid_at=NOW() WHERE id=$2",
        ["provider-trade-paid".into(), order.id.into()],
    ))
    .await
    .expect("test should transition the order to paid");

    let error = PaymentOrder::mark_as_failed(&pool, order.id)
        .await
        .expect_err("a paid order must not be reported as newly failed");
    assert!(matches!(
        error,
        DbError::InvalidOrderStatus { expected, actual }
            if expected == "pending" && actual == "paid"
    ));

    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM payment_orders WHERE id=$1",
        [order.id.into()],
    ))
    .await
    .expect("test payment order should be removed");
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment state test cleanup should succeed");
}

#[tokio::test]
async fn provider_circuit_constraint_rejects_unknown_states() {
    let pool = create_test_pool().await;
    let transaction = pool.begin().await.expect("transaction should start");
    let error = transaction
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE payment_provider_states SET circuit_state=$1 WHERE payment_method='alipay'",
            ["unexpected".into()],
        ))
        .await
        .expect_err("unknown provider state should violate the database constraint");
    assert!(
        error
            .to_string()
            .contains("chk_payment_provider_states_circuit")
    );
    transaction
        .rollback()
        .await
        .expect("failed constraint transaction should roll back");
}

#[tokio::test]
async fn paid_order_credit_is_atomic_audited_and_idempotent() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment credit cleanup should succeed");
    let tenant = create_test_tenant(&pool, "payment-credit", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "payment-credit", &test_id).await;
    let out_trade_no = format!("TESTCREDIT{}", test_id.replace('-', ""));
    let provider_trade_no = format!("TRADE{}", test_id.replace('-', ""));
    let event_id = format!("EVENT{}", test_id.replace('-', ""));
    let order = PaymentOrder::create(
        &pool,
        &CreatePaymentOrderRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            amount: Decimal::new(1234, 2),
            subject: "credit transaction test".to_string(),
            body: None,
            payment_method: PaymentMethod::Alipay,
            payment_scene: "page".to_string(),
            expired_at: Utc::now() + Duration::minutes(30),
        },
        &out_trade_no,
        "",
    )
    .await
    .expect("payment order should be created");
    let payload = serde_json::json!({"trade_no": provider_trade_no, "status": "success"});

    let pool_a = pool.clone();
    let pool_b = pool.clone();
    let first = PaymentOrder::credit_paid(
        &pool_a,
        order.id,
        &provider_trade_no,
        &event_id,
        payload.clone(),
        "test recharge",
    );
    let second = PaymentOrder::credit_paid(
        &pool_b,
        order.id,
        &provider_trade_no,
        &event_id,
        payload,
        "test recharge",
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("first concurrent provider event should succeed");
    let second = second.expect("second concurrent provider event should be idempotent");
    assert_ne!(first, second, "exactly one concurrent call should credit");

    let balance = UserBalance::find_by_user(&pool, tenant.id, user.id)
        .await
        .expect("balance query should succeed")
        .expect("credited balance should exist");
    assert_eq!(balance.available_balance, Decimal::new(1234, 2));
    let transaction_count = CountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS count FROM balance_transactions WHERE order_id=$1",
        [order.id.into()],
    ))
    .one(&pool)
    .await
    .expect("transaction count query should succeed")
    .expect("transaction count should return a row");
    assert_eq!(transaction_count.count, 1);
    let notification = NotificationStatusRow::find_by_statement(
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT processing_status FROM payment_notifications WHERE payment_method='alipay' AND provider_event_id=$1",
            [event_id.as_str().into()],
        ),
    )
    .one(&pool)
    .await
    .expect("notification query should succeed")
    .expect("notification should be recorded");
    assert_eq!(notification.processing_status, "processed");

    let conflict = PaymentOrder::credit_paid(
        &pool,
        order.id,
        &provider_trade_no,
        &event_id,
        serde_json::json!({"trade_no": provider_trade_no, "status": "tampered"}),
        "test recharge",
    )
    .await
    .expect_err("an event id must not be reusable with a different payload");
    assert!(matches!(
        conflict,
        CreditPaidOrderError::NotificationConflict
    ));

    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM payment_notifications WHERE provider_event_id=$1",
        [event_id.into()],
    ))
    .await
    .expect("test notification should be removed");
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment credit test cleanup should succeed");
}

#[tokio::test]
async fn pending_payment_keeps_original_tenant_after_membership_changes() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment credit cleanup should succeed");
    let source = create_test_tenant(&pool, "payment-move-source", &test_id).await;
    let target = create_test_tenant(&pool, "payment-move-target", &test_id).await;
    let user = create_test_user(&pool, source.id, "payment-move", &test_id).await;
    let order = PaymentOrder::create(
        &pool,
        &CreatePaymentOrderRequest {
            tenant_id: source.id,
            user_id: user.id,
            amount: Decimal::new(2500, 2),
            subject: "tenant move payment test".to_string(),
            body: None,
            payment_method: PaymentMethod::WechatPay,
            payment_scene: "native".to_string(),
            expired_at: Utc::now() + Duration::minutes(30),
        },
        &format!("TESTMOVE{}", test_id.replace('-', "")),
        "",
    )
    .await
    .expect("payment order should be created");

    let source_membership = TenantMembership::find_any(&pool, source.id, user.id)
        .await
        .expect("user lookup should succeed")
        .expect("source membership should exist");
    let membership_tx = pool
        .begin()
        .await
        .expect("membership transaction should begin");
    TenantMembership::create(
        &membership_tx,
        &CreateTenantMembershipRequest {
            tenant_id: target.id,
            user_id: user.id,
            role: TenantRole::Member,
        },
        &keycompute_db::AuditContext {
            actor_user_id: target.owner_user_id,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: PlatformRole::None,
            actor_tenant_role: Some(TenantRole::Admin),
            request_id: None,
        },
    )
    .await
    .expect("target membership should be added");
    TenantMembership::revoke(
        &membership_tx,
        source.id,
        user.id,
        source_membership.version,
        &keycompute_db::AuditContext {
            actor_user_id: source.owner_user_id,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: PlatformRole::None,
            actor_tenant_role: Some(TenantRole::Admin),
            request_id: None,
        },
    )
    .await
    .expect("source membership should be revoked");
    membership_tx
        .commit()
        .await
        .expect("membership transaction should commit");

    assert_eq!(
        PaymentOrder::find_by_id(&pool, order.id)
            .await
            .unwrap()
            .unwrap()
            .tenant_id,
        source.id
    );
    assert!(
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE payment_orders SET tenant_id=$2 WHERE id=$1",
            [order.id.into(), target.id.into()],
        ))
        .await
        .is_err(),
        "accepted payment identity remains immutable across memberships"
    );
    UserBalance::get_or_create(&pool, target.id, user.id)
        .await
        .unwrap();

    // A request authenticated before the move must re-check ownership at
    // insertion time instead of leaving a new pending order under the source
    // tenant. This is the stale-request half of the move/callback invariant.
    let stale_create = PaymentOrder::create(
        &pool,
        &CreatePaymentOrderRequest {
            tenant_id: source.id,
            user_id: user.id,
            amount: Decimal::new(100, 2),
            subject: "stale tenant order must be rejected".to_string(),
            body: None,
            payment_method: PaymentMethod::WechatPay,
            payment_scene: "native".to_string(),
            expired_at: Utc::now() + Duration::minutes(30),
        },
        &format!("TESTSTALE{}", test_id.replace('-', "")),
        "",
    )
    .await
    .expect_err("an order for the user's former tenant must be rejected");
    assert!(matches!(stale_create, DbError::UserTenantMismatch { .. }));

    assert!(
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM tenants WHERE id=$1",
            [source.id.into()],
        ))
        .await
        .is_err(),
        "retained orders protect their original tenant"
    );

    let provider_trade_no = format!("PAYMENT-MOVE-TRADE-{test_id}");
    let provider_event_id = format!("PAYMENT-MOVE-EVENT-{test_id}");
    PaymentOrder::credit_paid(
        &pool,
        order.id,
        &provider_trade_no,
        &provider_event_id,
        serde_json::json!({"trade_no": provider_trade_no}),
        "tenant move payment test",
    )
    .await
    .expect("payment callback should credit the order");

    let balance = UserBalance::find_by_user(&pool, source.id, user.id)
        .await
        .expect("balance query should succeed")
        .expect("balance should exist");
    assert_eq!(balance.tenant_id, source.id);
    assert_eq!(
        UserBalance::find_by_user(&pool, target.id, user.id)
            .await
            .unwrap()
            .unwrap()
            .available_balance,
        Decimal::ZERO
    );
    assert_eq!(balance.available_balance, Decimal::new(2500, 2));
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment move cleanup should succeed");
}

#[tokio::test]
async fn paid_order_replay_paths_distinguish_provider_identity() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment replay cleanup should succeed");
    let tenant = create_test_tenant(&pool, "payment-replay", &test_id).await;
    let user = create_test_user(&pool, tenant.id, "payment-replay", &test_id).await;
    let out_trade_no = format!("TESTREPLAY{}", test_id.replace('-', ""));
    let provider_trade_no = format!("TRADEA{}", test_id.replace('-', ""));
    let first_event = format!("EVENTA{}", test_id.replace('-', ""));
    let second_event = format!("EVENTB{}", test_id.replace('-', ""));
    let third_event = format!("EVENTC{}", test_id.replace('-', ""));
    let order = PaymentOrder::create(
        &pool,
        &CreatePaymentOrderRequest {
            tenant_id: tenant.id,
            user_id: user.id,
            amount: Decimal::new(500, 2),
            subject: "replay identity test".to_string(),
            body: None,
            payment_method: PaymentMethod::WechatPay,
            payment_scene: "native".to_string(),
            expired_at: Utc::now() + Duration::minutes(30),
        },
        &out_trade_no,
        "",
    )
    .await
    .expect("payment order should be created");

    let credited = PaymentOrder::credit_paid(
        &pool,
        order.id,
        &provider_trade_no,
        &first_event,
        serde_json::json!({"trade_no": provider_trade_no, "status": "success"}),
        "replay test recharge",
    )
    .await
    .expect("first provider event should credit the order");
    assert!(credited, "first event must perform the actual credit");

    // 同一渠道交易号、不同事件 ID 的重复成功回调：必须走幂等路径，不得二次入账
    let replay = PaymentOrder::credit_paid(
        &pool,
        order.id,
        &provider_trade_no,
        &second_event,
        serde_json::json!({"trade_no": provider_trade_no, "status": "success", "retry": true}),
        "replay test recharge",
    )
    .await
    .expect("replay with the same provider trade no should be idempotent");
    assert!(!replay, "replay must not credit the balance again");

    // 同一订单携带不同渠道交易号：必须拒为 ProviderIdentityMismatch
    let mismatch = PaymentOrder::credit_paid(
        &pool,
        order.id,
        "TRADEB-different",
        &third_event,
        serde_json::json!({"trade_no": "TRADEB-different", "status": "success"}),
        "replay test recharge",
    )
    .await
    .expect_err("a different provider trade no must not confirm a paid order");
    assert!(matches!(
        mismatch,
        CreditPaidOrderError::ProviderIdentityMismatch
    ));

    let balance = UserBalance::find_by_user(&pool, tenant.id, user.id)
        .await
        .expect("balance query should succeed")
        .expect("credited balance should exist");
    assert_eq!(
        balance.available_balance,
        Decimal::new(500, 2),
        "balance must reflect exactly one credit across all replay paths"
    );
    let transaction_count = CountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS count FROM balance_transactions WHERE order_id=$1",
        [order.id.into()],
    ))
    .one(&pool)
    .await
    .expect("transaction count query should succeed")
    .expect("transaction count should return a row");
    assert_eq!(transaction_count.count, 1);

    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM payment_notifications WHERE provider_event_id IN ($1, $2, $3)",
        [first_event.into(), second_event.into(), third_event.into()],
    ))
    .await
    .expect("test notifications should be removed");
    cleanup_test_data(&pool, &test_id)
        .await
        .expect("payment replay test cleanup should succeed");
}

#[tokio::test]
async fn security_event_retention_only_removes_events_past_the_window() {
    let pool = create_test_pool().await;
    let test_id = generate_test_id();
    let stale_digest = format!("stale{}", test_id.replace('-', ""));
    let fresh_digest = format!("fresh{}", test_id.replace('-', ""));

    // 插入 91 天前（应被清理）与 89 天前（应保留）的事件各一条
    for (digest, days) in [(&stale_digest, 91), (&fresh_digest, 89)] {
        pool.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO payment_security_events(payment_method, event_type, payload_digest, detail, source_ip, created_at)\
             VALUES ('alipay', 'retention_test', $1, 'retention window test', '203.0.113.1', NOW() - make_interval(days => $2::int))",
            [digest.as_str().into(), days.into()],
        ))
        .await
        .expect("test security event should be inserted");
    }

    let removed = purge_expired_payment_security_events(&pool, 90)
        .await
        .expect("retention purge should succeed");
    assert!(removed >= 1, "at least the stale event must be removed");

    let remaining = CountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS count FROM payment_security_events WHERE payload_digest IN ($1, $2)",
        [stale_digest.as_str().into(), fresh_digest.as_str().into()],
    ))
    .one(&pool)
    .await
    .expect("remaining events query should succeed")
    .expect("remaining events query should return a row");
    assert_eq!(
        remaining.count, 1,
        "exactly the 89-day event must survive the 90-day retention window"
    );
    let survivor = CountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS count FROM payment_security_events WHERE payload_digest = $1",
        [fresh_digest.as_str().into()],
    ))
    .one(&pool)
    .await
    .expect("survivor query should succeed")
    .expect("survivor query should return a row");
    assert_eq!(survivor.count, 1, "the fresh event must be the survivor");

    pool.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM payment_security_events WHERE payload_digest IN ($1, $2)",
        [stale_digest.into(), fresh_digest.into()],
    ))
    .await
    .expect("test security events should be removed");
}
