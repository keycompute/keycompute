//! Real SQL tests for bounded referral pages and constant database round trips.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    CreateUserRequest, DbRouter, User, models::referral_display::find_referral_display_page,
};
use keycompute_server::{AppState, create_router};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, ExecResult, QueryResult, Statement,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;
use uuid::Uuid;

struct Counted<'a> {
    db: &'a DatabaseConnection,
    reads: AtomicUsize,
    writes: AtomicUsize,
    last: std::sync::Mutex<Option<Statement>>,
}
#[async_trait::async_trait]
impl ConnectionTrait for Counted<'_> {
    fn get_database_backend(&self) -> DbBackend {
        DbBackend::Postgres
    }
    async fn execute(&self, s: Statement) -> Result<ExecResult, DbErr> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.db.execute(s).await
    }
    async fn execute_unprepared(&self, s: &str) -> Result<ExecResult, DbErr> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.db.execute_unprepared(s).await
    }
    async fn query_one(&self, s: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.db.query_one(s).await
    }
    async fn query_all(&self, s: Statement) -> Result<Vec<QueryResult>, DbErr> {
        *self.last.lock().unwrap() = Some(s.clone());
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.db.query_all(s).await
    }
}
fn display_scope(tenant: Uuid, user: Uuid) -> keycompute_types::TenantScope {
    keycompute_types::TenantScope::checked(tenant, user, keycompute_types::TenantRole::Member)
        .unwrap()
}
struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    beneficiary: TenantActor,
    tenant: Uuid,
    users: Vec<Uuid>,
}
impl Fixture {
    async fn new(count: i64) -> Self {
        let db = create_test_pool().await;
        let seed = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), seed.clone());
        let owner = create_test_tenant(&db, "ref-owner", &seed).await;
        let tenant = create_test_tenant(&db, "referred", &seed).await.id;
        let beneficiary = create_test_user(&db, owner.id, "referrer", &seed).await;
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status) VALUES($1,$2,'member','active')",
            [tenant.into(),beneficiary.id.into()],
        )).await.unwrap();
        let mut referred_ids = Vec::new();
        for n in 1..=count {
            let user = User::create(
                &db,
                &CreateUserRequest {
                    email: format!("ref-{n}-{seed}@example.invalid"),
                    name: Some(format!("Referred {n}")),
                },
            )
            .await
            .unwrap();
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status) VALUES($1,$2,'member','active')",
                [tenant.into(), user.id.into()],
            )).await.unwrap();
            referred_ids.push(user.id);
        }
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            INSERT INTO user_referrals(user_id, level1_referrer_id, level2_referrer_id, created_at)
            SELECT u.id, $1, $1, '2026-01-01T00:00:00Z'::timestamptz FROM users u JOIN tenant_memberships m ON m.user_id=u.id WHERE m.tenant_id=$2 AND u.id=ANY($3)
        "#,
            [beneficiary.id.into(), tenant.into(), referred_ids.into()],
        ))
        .await
        .unwrap();
        let users = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT user_id FROM user_referrals WHERE level1_referrer_id=$1 ORDER BY created_at DESC, id DESC",
            [beneficiary.id.into()])).await.unwrap().iter().map(|r| r.try_get_by_index(0).unwrap()).collect();
        Self {
            db,
            guard,
            beneficiary,
            tenant,
            users,
        }
    }
    async fn usage(&self, user: Uuid, amount: &str) -> Uuid {
        let id = Uuid::new_v4();
        self.db.execute(Statement::from_sql_and_values(DbBackend::Postgres, r#"
            INSERT INTO usage_logs(id,request_id,tenant_id,user_id,produce_ai_key_id,model_name,
                provider_name,account_id,input_tokens,output_tokens,total_tokens,input_unit_price_snapshot,
                output_unit_price_snapshot,user_amount,usage_source,status,started_at,finished_at)
            VALUES($1,gen_random_uuid(),$2,$3,gen_random_uuid(),'fixture','fixture',gen_random_uuid(),
                1,1,2,1,1,$4::numeric,'upstream','success',NOW(),NOW())
        "#, [id.into(), self.tenant.into(), user.into(), amount.into()])).await.unwrap();
        id
    }
    async fn commission(&self, usage: Uuid, beneficiary: Uuid, level: &str, amount: &str) {
        self.db.execute(Statement::from_sql_and_values(DbBackend::Postgres, r#"
            INSERT INTO distribution_records(usage_log_id,tenant_id,beneficiary_id,share_amount,share_ratio,level)
            VALUES($1,$2,$3,$4::numeric,0.1,$5)
        "#, [usage.into(), self.tenant.into(), beneficiary.into(), amount.into(), level.into()])).await.unwrap();
    }
    fn counted(&self) -> Counted<'_> {
        Counted {
            db: &self.db,
            reads: AtomicUsize::new(0),
            writes: AtomicUsize::new(0),
            last: std::sync::Mutex::new(None),
        }
    }
}
#[tokio::test]
async fn same_query_count_for_twenty_and_thousand_referrals_with_stable_nonoverlapping_pages() {
    for count in [20, 1000] {
        let mut f = Fixture::new(count).await;
        let db = f.counted();
        let p1 = find_referral_display_page(&db, display_scope(f.tenant, f.beneficiary.id), 10, 0)
            .await
            .unwrap();
        let p2 = find_referral_display_page(&db, display_scope(f.tenant, f.beneficiary.id), 10, 10)
            .await
            .unwrap();
        assert_eq!(p1.total, count);
        assert_eq!(p2.total, count);
        assert_eq!(
            p1.referrals.iter().map(|r| r.user_id).collect::<Vec<_>>(),
            f.users[..10]
        );
        assert_eq!(
            p2.referrals.iter().map(|r| r.user_id).collect::<Vec<_>>(),
            f.users[10..20]
        );
        assert_eq!(
            db.reads.load(Ordering::SeqCst),
            2,
            "one actual DB call per page for either dataset size"
        );
        assert_eq!(db.writes.load(Ordering::SeqCst), 0);
        let mut explain = db.last.lock().unwrap().clone().unwrap();
        explain.sql = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {}", explain.sql);
        let plan: serde_json::Value =
            f.db.query_one(explain)
                .await
                .unwrap()
                .unwrap()
                .try_get_by_index(0)
                .unwrap();
        eprintln!(
            "REFERRAL_QUERY_PLAN users={count} business_sql_calls_per_page=1 planning_ms={} execution_ms={} output_rows={}",
            plan[0]["Planning Time"], plan[0]["Execution Time"], plan[0]["Plan"]["Actual Rows"]
        );

        let empty = find_referral_display_page(
            &db,
            display_scope(f.tenant, f.beneficiary.id),
            20,
            count + 10,
        )
        .await
        .unwrap();
        assert!(empty.referrals.is_empty());
        assert_eq!(empty.total, count);
        f.guard.cleanup().await.unwrap();
    }
}
#[tokio::test]
async fn independent_aggregates_preserve_precise_money_and_do_not_multiply_rows_or_beneficiaries() {
    let mut f = Fixture::new(3).await;
    let u = f.users[0];
    let first = f.usage(u, "1.0000000001").await;
    let second = f.usage(u, "2.0000000002").await;
    f.commission(first, f.beneficiary.id, "level1", "0.1000000001")
        .await;
    f.commission(first, f.beneficiary.id, "level2", "0.2000000002")
        .await;
    f.commission(second, f.beneficiary.id, "level1", "0.3000000003")
        .await;
    f.commission(first, f.users[1], "level1", "999").await;
    let page = find_referral_display_page(&f.db, display_scope(f.tenant, f.beneficiary.id), 20, 0)
        .await
        .unwrap();
    assert_eq!(
        page.total, 3,
        "same beneficiary at both levels is one referred user"
    );
    let row = page.referrals.iter().find(|r| r.user_id == u).unwrap();
    assert_eq!(
        row.total_consumption,
        "3.0000000003".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    assert_eq!(
        row.earnings,
        "0.6000000006".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    assert!(
        page.referrals
            .iter()
            .filter(|r| r.user_id != u)
            .all(|r| r.total_consumption == 0 && r.earnings == 0)
    );
    assert_ne!(
        f.beneficiary.tenant_id, f.tenant,
        "legitimate cross-tenant referral must remain visible"
    );
    let unrelated = find_referral_display_page(&f.db, display_scope(f.tenant, f.users[2]), 20, 0)
        .await
        .unwrap();
    assert_eq!(unrelated.total, 0);
    assert!(unrelated.referrals.is_empty());
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn empty_referral_count_and_invalid_pagination_are_not_ambiguous() {
    let mut f = Fixture::new(0).await;
    let db = f.counted();
    let page = find_referral_display_page(&db, display_scope(f.tenant, f.beneficiary.id), 20, 0)
        .await
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.referrals.is_empty());
    for (limit, offset) in [(0, 0), (101, 0), (20, -1), (20, i64::MAX)] {
        assert!(
            find_referral_display_page(
                &db,
                display_scope(f.tenant, f.beneficiary.id),
                limit,
                offset
            )
            .await
            .is_err()
        );
    }
    assert_eq!(
        db.reads.load(Ordering::SeqCst),
        1,
        "invalid input rejected before SQL"
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
#[serial_test::serial]
async fn http_referral_pages_preserve_auth_feature_guards_and_bounded_legacy_shape() {
    let mut f = Fixture::new(25).await;
    // Feature changes are isolated to this test database and restored below.
    let original: String =
        f.db.query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT value FROM system_settings WHERE key='distribution_enabled'".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    f.db.execute_unprepared(
        "UPDATE system_settings SET value='true' WHERE key='distribution_enabled'",
    )
    .await
    .unwrap();
    let mut state = AppState::with_pool(DbRouter::single(f.db.clone()));
    state.app_base_url = Some("https://console.example.invalid".into());
    let token = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            f.beneficiary.id,
            Some(f.beneficiary.tenant_id),
            f.beneficiary.token_version,
            Some(1),
            Some(1),
            3600,
        )
        .unwrap();
    let app = create_router(state.clone());
    let overview_path = "/api/v1/me/distribution/overview";
    for _ in 0..2 {
        let (status, overview) = get_page(&app, Some(&token), overview_path).await;
        assert_eq!(status, StatusCode::OK, "{overview}");
        assert_eq!(overview["earnings"]["level1_referrals"], 25);
        assert!(
            overview["referral"]["invite_link"]
                .as_str()
                .unwrap()
                .contains("/auth/register?ref=")
        );
    }
    assert_eq!(state.display_cache.metrics()["hit"], 1);
    let root = "/api/v1/me/distribution/referrals";
    let (status, legacy) = get_page(&app, Some(&token), root).await;
    assert_eq!(status, StatusCode::OK, "{legacy}");
    assert_eq!(legacy.as_array().unwrap().len(), 20);
    let (status, page) = get_page(&app, Some(&token), &format!("{root}?page=2&page_size=20")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 25);
    assert_eq!(page["total_pages"], 2);
    assert_eq!(page["referrals"].as_array().unwrap().len(), 5);
    let (status, page) = get_page(
        &app,
        Some(&token),
        &format!("{root}?page=1&page_size=9223372036854775807"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page_size"], 100);
    let (status, page) =
        get_page(&app, Some(&token), &format!("{root}?page=999&page_size=20")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 25);
    assert!(page["referrals"].as_array().unwrap().is_empty());
    assert_eq!(get_page(&app, None, root).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        get_page(&app, Some(&token), &format!("{root}?page=abc"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    f.db.execute_unprepared(
        "UPDATE system_settings SET value='false' WHERE key='distribution_enabled'",
    )
    .await
    .unwrap();
    let status = get_page(&app, Some(&token), root).await.0;
    let overview_disabled = get_page(&app, Some(&token), overview_path).await.0;
    let earnings_disabled = get_page(&app, Some(&token), "/api/v1/me/distribution/earnings")
        .await
        .0;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE system_settings SET value=$1 WHERE key='distribution_enabled'",
        [original.into()],
    ))
    .await
    .unwrap();
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(overview_disabled, StatusCode::FORBIDDEN);
    assert_eq!(earnings_disabled, StatusCode::FORBIDDEN);
    assert_eq!(
        state.display_cache.metrics()["hit"],
        1,
        "disabled feature must be checked before cache hit"
    );
    f.guard.cleanup().await.unwrap();
}
async fn get_page(
    app: &axum::Router,
    token: Option<&str>,
    uri: &str,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}
