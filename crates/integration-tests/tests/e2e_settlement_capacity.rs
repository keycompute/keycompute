//! Authoritative, idempotent post-ledger work and bounded hot-user waiters.
use integration_tests::db::{
    TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_billing::{BillingService, balance::BalanceService};
use keycompute_db::{
    CreateProduceAiKeyRequest, CreateUsageLogRequest, DbRouter, UsageLog, models::node_tip::NodeTip,
};
use keycompute_types::{PricingSnapshot, RequestContext};
use rust_decimal::Decimal;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement, TransactionTrait};
use std::{sync::Arc, time::Duration};
use tokio::task::JoinSet;
use uuid::Uuid;

async fn ledger(db: &sea_orm::DatabaseConnection, tenant: Uuid, user: Uuid) -> UsageLog {
    let key = integration_tests::db::create_test_api_key(
        db,
        &CreateProduceAiKeyRequest {
            tenant_id: tenant,
            user_id: user,
            name: "settlement-capacity".into(),
            produce_ai_key_hash: Uuid::new_v4().simple().to_string(),
            produce_ai_key_preview: "test***".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    UsageLog::create(
        db,
        &CreateUsageLogRequest {
            request_id: Uuid::new_v4(),
            tenant_id: tenant,
            user_id: user,
            produce_ai_key_id: key.id,
            model_name: "test".into(),
            provider_name: "openai".into(),
            account_id: Uuid::new_v4(),
            input_tokens: 1,
            output_tokens: 1,
            input_unit_price_snapshot: 1.into(),
            output_unit_price_snapshot: 1.into(),
            user_amount: 1.into(),
            currency: "CNY".into(),
            usage_source: "upstream".into(),
            status: "success".into(),
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
        },
    )
    .await
    .unwrap()
}
fn context(log: &UsageLog) -> RequestContext {
    RequestContext::new(
        log.request_id,
        log.user_id,
        log.tenant_id,
        log.produce_ai_key_id,
        "test",
        vec![],
        false,
        PricingSnapshot::default(),
    )
}

#[tokio::test]
async fn tip_probe_preserves_successful_node_replay_and_skips_other_completions() {
    let db = create_test_pool().await;
    let run = Uuid::new_v4().to_string();
    let mut cleanup = TestDataGuard::new(db.clone(), &run);
    let tenant = create_test_tenant(&db, "tips", &run).await;
    let user = create_test_user(&db, tenant.id, "consumer", &run).await;
    let owner = create_test_user(&db, tenant.id, "owner", &run).await;
    let log = ledger(&db, tenant.id, user.id).await;
    assert!(
        NodeTip::create_from_usage_log(&db, log.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        NodeTip::create_from_usage_log(&db, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    let node = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO nodes(id,tenant_id,owner_user_id,client_instance_id,display_name,status,capabilities_json) VALUES($1,$4,$2,$3,'test','online','{}')",
        [node.into(),owner.id.into(),run.clone().into(),tenant.id.into()])).await.unwrap();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO node_tasks(request_id,tenant_id,user_id,model,payload_json,status,assigned_node_id,deadline_at,complete_grace_until) VALUES($1,$4,$2,'test','{}','queued',$3,NOW()+INTERVAL '1 minute',NOW()+INTERVAL '2 minutes')",
        [log.request_id.into(),user.id.into(),node.into(),tenant.id.into()])).await.unwrap();
    assert!(
        NodeTip::create_from_usage_log(&db, log.id)
            .await
            .unwrap()
            .is_none()
    );
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_tasks SET status='succeeded' WHERE request_id=$1",
        [log.request_id.into()],
    ))
    .await
    .unwrap();
    let tip = NodeTip::create_from_usage_log(&db, log.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tip.owner_user_id, owner.id);
    assert_eq!(tip.consumer_user_id, user.id);
    assert!(tip.tip_amount > Decimal::ZERO);
    assert!(
        NodeTip::create_from_usage_log(&db, log.id)
            .await
            .unwrap()
            .is_none()
    );
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM node_tips WHERE usage_log_id=$1",
        [log.id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM node_tasks WHERE request_id=$1",
        [log.request_id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM nodes WHERE id=$1",
        [node.into()],
    ))
    .await
    .unwrap();
    cleanup.cleanup().await.unwrap();
}

#[tokio::test]
async fn hot_user_settlements_do_not_starve_another_users_writer_and_replay_once() {
    let admin = create_test_pool().await;
    let run = Uuid::new_v4().to_string();
    let mut cleanup = TestDataGuard::new(admin.clone(), &run);
    let tenant = create_test_tenant(&admin, "settle", &run).await;
    let hot = create_test_user(&admin, tenant.id, "hot", &run).await;
    let other = create_test_user(&admin, tenant.id, "other", &run).await;
    let seed = BalanceService::new(DbRouter::single(admin.clone()));
    for id in [hot.id, other.id] {
        seed.recharge(tenant.id, id, 100.into(), None, None)
            .await
            .unwrap();
    }
    let a = ledger(&admin, tenant.id, hot.id).await;
    let b = ledger(&admin, tenant.id, hot.id).await;
    let c = ledger(&admin, tenant.id, other.id).await;
    let app = format!("settlement-test-{run}");
    let mut url = url::Url::parse(&integration_tests::common::resolve_database_url()).unwrap();
    url.query_pairs_mut().append_pair("application_name", &app);
    let mut options = ConnectOptions::new(url.to_string());
    options
        .max_connections(2)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(2));
    let limited = Database::connect(options).await.unwrap();
    let billing = Arc::new(BillingService::with_pool(DbRouter::single(limited.clone())));
    let lock = admin.begin().await.unwrap();
    lock.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT user_id FROM user_balances WHERE user_id=$1 FOR UPDATE",
        [hot.id.into()],
    ))
    .await
    .unwrap();
    let mut work = JoinSet::new();
    let copy = billing.clone();
    let first = a.clone();
    work.spawn(async move {
        copy.replay_saved_usage_effects(&context(&first), &first, first.user_id)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2),async {loop {
        let row=admin.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::BIGINT AS n FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock'",[app.clone().into()])).await.unwrap().unwrap();
        if row.try_get::<i64>("","n").unwrap()==1 {break;} tokio::task::yield_now().await;
    }}).await.unwrap();
    let copy = billing.clone();
    let second = b.clone();
    work.spawn(async move {
        copy.replay_saved_usage_effects(&context(&second), &second, second.user_id)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while BalanceService::settlement_status().queued == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_millis(750),
        billing.replay_saved_usage_effects(&context(&c), &c, c.user_id),
    )
    .await
    .expect("hot settlements consumed both writer slots")
    .unwrap();
    assert!(work.try_join_next().is_none());
    lock.rollback().await.unwrap();
    while let Some(result) = work.join_next().await {
        result.unwrap().unwrap();
    }
    billing
        .replay_saved_usage_effects(&context(&a), &a, a.user_id)
        .await
        .unwrap();
    let balance = keycompute_db::UserBalance::find_by_user(&admin, tenant.id, hot.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(balance.available_balance, Decimal::from(98));
    assert_eq!(balance.total_consumed, Decimal::from(2));
    drop(billing);
    limited.close().await.unwrap();
    cleanup.cleanup().await.unwrap();
}
