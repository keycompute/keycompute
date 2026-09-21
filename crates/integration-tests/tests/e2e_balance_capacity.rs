//! Hot-user reservations must queue before borrowing shared writer connections.
//! Monetary semantics use the real PostgreSQL schema; no production DB is used.
use integration_tests::db::{
    TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_billing::balance::BalanceService;
use keycompute_db::{DbRouter, UserBalance};
use rust_decimal::Decimal;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement, TransactionTrait};
use std::time::Duration;
use tokio::task::JoinSet;
use uuid::Uuid;

#[tokio::test]
async fn hot_user_queue_does_not_consume_another_users_writer_connection() {
    let admin = create_test_pool().await;
    let id = Uuid::new_v4().to_string();
    let mut cleanup = TestDataGuard::new(admin.clone(), &id);
    let tenant = create_test_tenant(&admin, "capacity", &id).await;
    let a = create_test_user(&admin, tenant.id, "hot", &id).await;
    let b = create_test_user(&admin, tenant.id, "other", &id).await;
    let tenant_id = tenant.id;
    let a_id = a.id;
    let b_id = b.id;
    let seed = BalanceService::new(DbRouter::single(admin.clone()));
    for user_id in [a_id, b_id] {
        seed.recharge(tenant_id, user_id, Decimal::from(100), None, None)
            .await
            .unwrap();
    }
    let application = format!("balance-capacity-{id}");
    let mut url = url::Url::parse(&integration_tests::common::resolve_database_url()).unwrap();
    url.query_pairs_mut()
        .append_pair("application_name", &application);
    let mut options = ConnectOptions::new(url.to_string());
    options
        .max_connections(2)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(2));
    let limited_db = Database::connect(options).await.unwrap();
    let service = BalanceService::new(DbRouter::single(limited_db.clone()));
    let lock = admin.begin().await.unwrap();
    lock.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT user_id FROM user_balances WHERE user_id=$1 FOR UPDATE",
        [a.id.into()],
    ))
    .await
    .unwrap();
    let mut tasks = JoinSet::new();
    let first = service.clone();
    tasks.spawn(async move {
        first
            .reserve_request(
                tenant_id,
                a_id,
                Uuid::new_v4(),
                Decimal::ONE,
                Duration::from_secs(60),
            )
            .await
    });
    // Synchronize with a real server-side lock wait, not an assumed sleep.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let row = admin.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT AS n FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock'",
                [application.clone().into()])).await.unwrap().unwrap();
            if row.try_get::<i64>("", "n").unwrap() == 1 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("hot reservation did not enter its PostgreSQL row wait");
    let second = service.clone();
    tasks.spawn(async move {
        second
            .reserve_request(
                tenant_id,
                a_id,
                Uuid::new_v4(),
                Decimal::ONE,
                Duration::from_secs(60),
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    let other = tokio::time::timeout(
        Duration::from_millis(750),
        service.reserve_request(
            tenant_id,
            b_id,
            Uuid::new_v4(),
            Decimal::ONE,
            Duration::from_secs(60),
        ),
    )
    .await
    .expect("hot-user queue consumed the remaining writer connection")
    .unwrap();
    assert_eq!(other.user_id, b_id);
    assert!(
        tasks.try_join_next().is_none(),
        "hot reservation escaped its still-held row lock"
    );
    lock.rollback().await.unwrap();
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    let balance = UserBalance::find_by_user(&admin, tenant_id, a_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(balance.available_balance, Decimal::from(98));
    assert_eq!(balance.frozen_balance, Decimal::from(2));
    limited_db.close().await.unwrap();
    cleanup.cleanup().await.unwrap();
}

#[tokio::test]
async fn concurrent_request_identity_conflicts_roll_back_the_balance_move() {
    let db = create_test_pool().await;
    let id = Uuid::new_v4().to_string();
    let mut cleanup = TestDataGuard::new(db.clone(), &id);
    let tenant = create_test_tenant(&db, "atomic-reserve", &id).await;
    let a = create_test_user(&db, tenant.id, "a", &id).await;
    let b = create_test_user(&db, tenant.id, "b", &id).await;
    let tenant_id = tenant.id;
    let a_id = a.id;
    let b_id = b.id;
    let service = BalanceService::new(DbRouter::single(db.clone()));
    for user_id in [a_id, b_id] {
        service
            .recharge(tenant_id, user_id, Decimal::from(100), None, None)
            .await
            .unwrap();
    }
    let request = Uuid::new_v4();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = JoinSet::new();
    for user in [a.id, b.id] {
        let service = service.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            service
                .reserve_request(
                    tenant_id,
                    user,
                    request,
                    Decimal::from(10),
                    Duration::from_secs(60),
                )
                .await
        });
    }
    let mut winner = None;
    let mut rejected = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(reservation) => {
                assert!(winner.is_none());
                winner = Some(reservation);
            }
            Err(_) => rejected += 1,
        }
    }
    assert_eq!(rejected, 1);
    let winner = winner.unwrap();
    let replay = service
        .reserve_request_with_owner_token(
            tenant_id,
            winner.user_id,
            request,
            winner.owner_token,
            Decimal::from(10),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(winner.id, replay.id);
    let first = UserBalance::find_by_user(&db, tenant_id, a_id)
        .await
        .unwrap()
        .unwrap();
    let second = UserBalance::find_by_user(&db, tenant_id, b_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        first.available_balance + second.available_balance,
        Decimal::from(190)
    );
    assert_eq!(
        first.frozen_balance + second.frozen_balance,
        Decimal::from(10)
    );
    assert!(
        service
            .release_request_reservation(tenant_id, winner.user_id, request, winner.owner_token,)
            .await
            .unwrap()
    );
    assert!(
        !service
            .release_request_reservation(tenant_id, winner.user_id, request, winner.owner_token,)
            .await
            .unwrap()
    );
    cleanup.cleanup().await.unwrap();
}
