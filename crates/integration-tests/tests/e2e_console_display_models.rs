//! Real PostgreSQL regressions for the presentation-only aggregate queries.
//! Requires an explicitly supplied isolated DATABASE_URL; never discovers credentials.
use chrono::{DateTime, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::models::console_display;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;
fn time(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
async fn fixture() -> (DatabaseConnection, TestDataGuard, Uuid, Uuid) {
    assert!(
        std::env::var("DATABASE_URL").is_ok(),
        "explicit isolated test database required"
    );
    let db = create_test_pool().await;
    let seed = generate_test_id();
    let guard = TestDataGuard::new(db.clone(), seed.clone());
    let tenant = create_test_tenant(&db, "display-model", &seed).await;
    let user = create_test_user(&db, tenant.id, "display-model", &seed).await;
    (db, guard, tenant.id, user.id)
}
async fn usage(db: &DatabaseConnection, tenant: Uuid, user: Uuid, at: &str, count: i64) {
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,r#"
        INSERT INTO usage_logs(request_id,tenant_id,user_id,produce_ai_key_id,model_name,provider_name,
        account_id,input_tokens,output_tokens,total_tokens,input_unit_price_snapshot,output_unit_price_snapshot,
        user_amount,usage_source,status,started_at,finished_at,created_at)
        SELECT gen_random_uuid(),$1,$2,gen_random_uuid(),'display-fixture','fixture',gen_random_uuid(),
        1,2,3,0,0,0.0000000001,'upstream','success',$3,$3,$3 FROM generate_series(1,$4::bigint)
    "#,[tenant.into(),user.into(),time(at).into(),count.into()])).await.unwrap();
}
#[tokio::test]
async fn trend_counts_all_rows_zero_fills_and_uses_half_open_utc_bounds() {
    let (db, mut guard, tenant, user) = fixture().await;
    usage(&db, tenant, user, "2026-01-01T00:00:00Z", 150).await;
    usage(&db, tenant, user, "2026-01-02T12:00:00Z", 100).await;
    usage(&db, tenant, user, "2025-12-31T23:59:59Z", 1).await;
    usage(&db, tenant, user, "2026-01-04T00:00:00Z", 1).await;
    let peer = create_test_user(&db, tenant, "display-peer", &generate_test_id()).await;
    usage(&db, tenant, peer.id, "2026-01-01T00:00:00Z", 5).await;
    let value = console_display::usage_trend(
        &db,
        user,
        tenant,
        time("2026-01-01T00:00:00Z"),
        time("2026-01-04T00:00:00Z"),
        "day",
    )
    .await
    .unwrap();
    let buckets = value["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 3);
    assert_eq!(
        buckets
            .iter()
            .map(|v| v["requests"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        vec![150, 100, 0]
    );
    assert_eq!(buckets[0]["total_tokens"], 450);
    assert_eq!(
        buckets[0]["total_cost"]
            .as_str()
            .unwrap()
            .parse::<bigdecimal::BigDecimal>()
            .unwrap(),
        "0.000000015".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    let tx = db.begin().await.unwrap();
    tx.execute_unprepared("SET LOCAL TIME ZONE 'America/New_York'")
        .await
        .unwrap();
    let shifted = console_display::usage_trend(
        &tx,
        user,
        tenant,
        time("2026-01-01T00:00:00Z"),
        time("2026-01-04T00:00:00Z"),
        "day",
    )
    .await
    .unwrap();
    assert_eq!(
        value["buckets"], shifted["buckets"],
        "wire buckets must not change with database timezone"
    );
    tx.rollback().await.unwrap();
    guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn trend_empty_and_invalid_intervals_are_bounded() {
    let (db, mut guard, tenant, user) = fixture().await;
    let from = time("2026-01-01T00:00:00Z");
    let value = console_display::usage_trend(
        &db,
        user,
        tenant,
        from,
        from + chrono::Duration::hours(3),
        "hour",
    )
    .await
    .unwrap();
    assert_eq!(value["buckets"].as_array().unwrap().len(), 3);
    assert_eq!(value["buckets"][0]["requests"], 0);
    for (to, grain) in [
        (from, "day"),
        (from + chrono::Duration::days(367), "day"),
        (from + chrono::Duration::days(8), "hour"),
        (from + chrono::Duration::days(1), "minute"),
    ] {
        assert!(
            console_display::usage_trend(&db, user, tenant, from, to, grain)
                .await
                .is_err()
        );
    }
    guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn dashboard_counts_are_complete_but_previews_are_bounded_and_secret_free() {
    let (db, mut guard, tenant, user) = fixture().await;
    usage(&db, tenant, user, "2026-01-01T00:00:00Z", 250).await;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,r#"
      INSERT INTO produce_ai_keys(tenant_id,user_id,name,produce_ai_key_hash,produce_ai_key_preview,revoked,expires_at)
      SELECT $1,$2,'display-key-'||g::text,repeat(md5(gen_random_uuid()::text),2),'sk-fixture-****',g>10,
      CASE WHEN g=10 THEN NOW()-INTERVAL '1 day' ELSE NULL END FROM generate_series(1,12) g
    "#,[tenant.into(),user.into()])).await.unwrap();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,r#"
      INSERT INTO payment_orders(tenant_id,user_id,out_trade_no,amount,subject,expired_at,pay_url)
      SELECT $1,$2,gen_random_uuid()::text,10,'display fixture',NOW()+INTERVAL '1 day','https://secret.invalid/' FROM generate_series(1,10)
    "#,[tenant.into(),user.into()])).await.unwrap();
    let value = console_display::dashboard(&db, user, tenant).await.unwrap();
    assert_eq!(value["stats"]["total_requests"], 250);
    assert_eq!(value["active_key_count"], 9);
    assert_eq!(value["active_keys"].as_array().unwrap().len(), 4);
    assert_eq!(value["recent_usage"].as_array().unwrap().len(), 5);
    assert_eq!(value["recent_orders"].as_array().unwrap().len(), 3);
    let text = value.to_string();
    assert!(!text.contains("produce_ai_key_hash"));
    assert!(!text.contains("secret.invalid"));
    assert!(value["recent_usage"][0]["cost"].is_string());
    let foreign = console_display::dashboard(&db, Uuid::new_v4(), tenant)
        .await
        .unwrap();
    assert_eq!(foreign["stats"]["total_requests"], 0);
    assert_eq!(foreign["active_key_count"], 0);
    assert!(foreign["recent_orders"].as_array().unwrap().is_empty());
    guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn distribution_summary_aggregates_once_without_count_money_fanout() {
    let (db, mut guard, tenant, user) = fixture().await;
    usage(&db, tenant, user, "2026-01-01T00:00:00Z", 2).await;
    let referred = create_test_user(&db, tenant, "display-referred", &generate_test_id()).await;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO user_referrals(user_id,level1_referrer_id,level2_referrer_id) VALUES($1,$2,$2)",
        [referred.id.into(),user.into()])).await.unwrap();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,r#"
        INSERT INTO distribution_records(usage_log_id,tenant_id,beneficiary_id,share_amount,share_ratio,level,status)
        SELECT id,$1,$2,0.1000000001,0.1,'level1','settled' FROM usage_logs WHERE user_id=$2
    "#,[tenant.into(),user.into()])).await.unwrap();
    let value = console_display::distribution(&db, user).await.unwrap();
    let e = &value["earnings"];
    assert_eq!(e["level1_referrals"], 1);
    assert_eq!(e["level2_referrals"], 1);
    assert_eq!(
        e["total_earnings"]
            .as_str()
            .unwrap()
            .parse::<bigdecimal::BigDecimal>()
            .unwrap(),
        "0.2000000002".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    assert_eq!(e["settled_amount"], e["total_earnings"]);
    let empty = console_display::distribution(&db, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(empty["earnings"]["level1_referrals"], 0);
    guard.cleanup().await.unwrap();
}
