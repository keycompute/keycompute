//! Distribution reports use one verified tenant and immutable ledger ownership.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use bigdecimal::BigDecimal;
use chrono::{Duration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::models::{
    distribution_scope::{self as dao, DistributionScope, RecordFilter, RuleFilter},
    referral_display::find_referral_display_page,
};
use keycompute_db::{CreateUsageLogRequest, DbRouter, Tenant, UsageLog, User};
use keycompute_server::{AppState, create_router};
use keycompute_types::{PlatformRole, PlatformScope, TenantRole, TenantScope};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    member: Uuid,
    run: String,
}
fn own(t: Uuid, u: Uuid, role: TenantRole) -> DistributionScope {
    DistributionScope::Owned(TenantScope::checked(t, u, role).unwrap())
}
async fn token(s: &AppState, db: &DatabaseConnection, user: Uuid, tenant: Option<Uuid>) -> String {
    let user = User::find_by_id(db, user).await.unwrap().unwrap();
    let raw = s
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
        .unwrap();
    let ctx = s.auth.verify_token(&raw).await.unwrap();
    s.auth
        .select_tenant(&ctx, tenant)
        .await
        .unwrap()
        .access_token
}
async fn get(s: &AppState, key: &str, path: &str) -> (StatusCode, Value) {
    let response = create_router(s.clone())
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "distribution-read-a", &run).await;
        let b = create_test_tenant(&db, "distribution-read-b", &run).await;
        let member = create_test_user(&db, a.id, "distribution-member", &run)
            .await
            .id;
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role) VALUES($1,$2,'member'),($1,$3,'member')",[b.id.into(),a.owner_user_id.into(),member.into()])).await.unwrap();
        let mut state = AppState::with_pool(DbRouter::single(db.clone()));
        state.app_base_url = Some("https://fixture.invalid".into());
        Self {
            db,
            guard,
            state,
            a,
            b,
            member,
            run,
        }
    }
    fn admin(&self) -> DistributionScope {
        DistributionScope::Tenant(
            TenantScope::checked(self.a.id, self.a.owner_user_id, TenantRole::Admin).unwrap(),
        )
    }
    fn path(&self, suffix: &str) -> String {
        format!("/api/v1/tenants/{}/distribution/{suffix}", self.a.id)
    }
    async fn row(
        &self,
        tenant: Uuid,
        beneficiary: Option<Uuid>,
        currency: &str,
        commission: i64,
    ) -> Uuid {
        let now = Utc::now();
        let log = UsageLog::create(
            &self.db,
            &CreateUsageLogRequest {
                request_id: Uuid::new_v4(),
                tenant_id: tenant,
                user_id: self.member,
                produce_ai_key_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                model_name: "dist-test".into(),
                provider_name: "openai".into(),
                input_tokens: 1,
                output_tokens: 2,
                input_unit_price_snapshot: BigDecimal::from(1),
                output_unit_price_snapshot: BigDecimal::from(1),
                user_amount: BigDecimal::from(10),
                currency: currency.into(),
                usage_source: "gateway_accumulated".into(),
                status: "success".into(),
                started_at: now,
                finished_at: now,
            },
        )
        .await
        .unwrap();
        let r=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO distribution_records(usage_log_id,tenant_id,beneficiary_scope,beneficiary_id,share_amount,share_ratio,level) VALUES($1,$2,$3,$4,$5,0.1,'level1') RETURNING id",[log.id.into(),tenant.into(),if beneficiary.is_some(){"tenant_member"}else{"everyone"}.into(),beneficiary.into(),BigDecimal::from(commission).into()])).await.unwrap().unwrap();
        r.try_get("", "id").unwrap()
    }
    async fn seed(&self) -> Vec<Uuid> {
        vec![
            self.row(self.a.id, Some(self.a.owner_user_id), "CNY", 1)
                .await,
            self.row(self.a.id, Some(self.a.owner_user_id), "USD", 2)
                .await,
            self.row(self.a.id, Some(self.member), "CNY", 3).await,
            self.row(self.a.id, None, "CNY", 4).await,
            self.row(self.b.id, Some(self.a.owner_user_id), "CNY", 900)
                .await,
        ]
    }
}
#[tokio::test]
async fn tenant_personal_and_platform_lists_counts_totals_share_exact_scope() {
    let mut f = Fixture::new().await;
    let ids = f.seed().await;
    let q = RecordFilter::default();
    let at = token(&f.state, &f.db, f.a.owner_user_id, Some(f.a.id)).await;
    let (status, first) = get(&f.state, &at, &f.path("records?page_size=2")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["total"], 4);
    assert_eq!(first["records"].as_array().unwrap().len(), 2);
    let (_, second) = get(&f.state, &at, &f.path("records?page_size=2&page=2")).await;
    assert!(first["records"].as_array().unwrap().iter().all(|a| {
        second["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|b| a["id"] != b["id"])
    }));
    let (_, all) = get(&f.state, &at, &f.path("records")).await;
    let records = all["records"].as_array().unwrap();
    assert!(records.iter().all(|v| v["tenant_id"] == json!(f.a.id)));
    assert!(
        records
            .iter()
            .any(|v| v["beneficiary_scope"] == "everyone" && v["beneficiary_id"].is_null())
    );
    let stats = dao::record_stats(&f.db, f.admin(), &q).await.unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(
        stats
            .iter()
            .find(|v| v.currency == "CNY")
            .unwrap()
            .total_earnings,
        BigDecimal::from(8)
    );
    assert_eq!(
        stats
            .iter()
            .find(|v| v.currency == "USD")
            .unwrap()
            .total_earnings,
        BigDecimal::from(2)
    );
    let owned = own(f.a.id, f.a.owner_user_id, TenantRole::Admin);
    assert_eq!(dao::record_count(&f.db, owned, &q).await.unwrap(), 2);
    assert!(dao::record(&f.db, owned, ids[2]).await.unwrap().is_none());
    assert!(
        dao::record(&f.db, f.admin(), ids[4])
            .await
            .unwrap()
            .is_none()
    );
    let other = own(f.b.id, f.a.owner_user_id, TenantRole::Member);
    assert_eq!(
        dao::record_stats(&f.db, other, &q).await.unwrap()[0].total_earnings,
        BigDecimal::from(900)
    );
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let rt = token(&f.state, &f.db, root.id, None).await;
    let platform = DistributionScope::Platform(
        PlatformScope::checked(root.id, PlatformRole::Root).unwrap(),
        f.a.id,
    );
    assert_eq!(dao::record_count(&f.db, platform, &q).await.unwrap(), 4);
    let one = dao::records(&f.db, platform, &q, 1, 0).await.unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].tenant_id, f.a.id);
    assert_eq!(
        get(
            &f.state,
            &rt,
            &format!("/api/v1/platform/distribution/tenants/{}/records", f.a.id)
        )
        .await
        .0,
        StatusCode::OK
    );
    for id in [ids[4], Uuid::new_v4()] {
        assert_eq!(
            get(&f.state, &at, &f.path(&format!("records/{id}")))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
    let f2 = RecordFilter {
        currency: Some("USD".into()),
        beneficiary_id: Some(f.a.owner_user_id),
        from: Some(Utc::now() - Duration::hours(1)),
        until: Some(Utc::now() + Duration::hours(1)),
        ..Default::default()
    };
    assert_eq!(
        dao::records(&f.db, platform, &f2, 20, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(dao::record_count(&f.db, platform, &f2).await.unwrap(), 1);
    assert_eq!(
        dao::record_stats(&f.db, platform, &f2).await.unwrap()[0].record_count,
        1
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn forged_roles_stale_memberships_and_inference_credentials_never_read_reports() {
    let mut f = Fixture::new().await;
    f.seed().await;
    let member_token = token(&f.state, &f.db, f.member, Some(f.a.id)).await;
    let admin_token = token(&f.state, &f.db, f.a.owner_user_id, Some(f.a.id)).await;
    for suffix in ["records", "rules", "stats"] {
        assert_eq!(
            get(&f.state, &member_token, &f.path(suffix)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let fake = DistributionScope::Tenant(
        TenantScope::checked(f.a.id, f.member, TenantRole::Admin).unwrap(),
    );
    assert_eq!(
        dao::record_count(&f.db, fake, &RecordFilter::default())
            .await
            .unwrap(),
        0
    );
    assert!(
        dao::rules(&f.db, fake, &RuleFilter::default(), 20, 0)
            .await
            .unwrap()
            .is_empty()
    );
    let fake_root = DistributionScope::Platform(
        PlatformScope::checked(f.member, PlatformRole::Root).unwrap(),
        f.a.id,
    );
    assert!(
        dao::records(&f.db, fake_root, &RecordFilter::default(), 20, 0)
            .await
            .unwrap()
            .is_empty()
    );
    for suffix in [
        "records?page_size=101",
        "records?level=level3",
        "records?status=anything",
        "records?tenant_id=00000000-0000-0000-0000-000000000000",
        "rules?platform_role=root",
    ] {
        assert_eq!(
            get(&f.state, &admin_token, &f.path(suffix)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let raw = keycompute_auth::ProduceAiKeyValidator::generate_key();
    integration_tests::db::create_test_api_key(
        &f.db,
        &keycompute_db::CreateProduceAiKeyRequest {
            tenant_id: f.a.id,
            user_id: f.a.owner_user_id,
            name: "distribution-inference-only".into(),
            produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&raw),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    for path in [
        f.path("records"),
        "/api/v1/me/distribution/records".into(),
        "/api/v1/me/distribution/earnings".into(),
        "/api/v1/me/distribution/overview".into(),
    ] {
        assert_eq!(get(&f.state, &raw, &path).await.0, StatusCode::FORBIDDEN);
    }
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='operator' WHERE id=$1",
        [f.member.into()],
    ))
    .await
    .unwrap();
    let op = token(&f.state, &f.db, f.member, None).await;
    assert_eq!(
        get(
            &f.state,
            &op,
            &format!("/api/v1/platform/distribution/tenants/{}/records", f.a.id)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let current = token(&f.state, &f.db, f.member, Some(f.a.id)).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), f.member.into()],
    ))
    .await
    .unwrap();
    assert_eq!(
        get(&f.state, &current, "/api/v1/me/distribution/records")
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert!(
        dao::records(
            &f.db,
            own(f.a.id, f.member, TenantRole::Member),
            &RecordFilter::default(),
            20,
            0
        )
        .await
        .unwrap()
        .is_empty()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn personal_earnings_cached_overviews_and_referral_amounts_do_not_cross_tenants() {
    let mut f = Fixture::new().await;
    f.seed().await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO user_referrals(user_id,level1_referrer_id) VALUES($1,$2)",
        [f.member.into(), f.a.owner_user_id.into()],
    ))
    .await
    .unwrap();
    let a = token(&f.state, &f.db, f.a.owner_user_id, Some(f.a.id)).await;
    let b = token(&f.state, &f.db, f.a.owner_user_id, Some(f.b.id)).await;
    let cache_hits_before = f.state.display_cache.metrics()["hit"].as_u64().unwrap();
    for _ in 0..2 {
        let (s, v) = get(&f.state, &a, "/api/v1/me/distribution/overview").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(
            v["earnings"]["total_earnings"]
                .as_str()
                .unwrap()
                .parse::<BigDecimal>()
                .unwrap(),
            BigDecimal::from(1)
        );
    }
    assert!(
        f.state.display_cache.metrics()["hit"].as_u64().unwrap() > cache_hits_before,
        "test must exercise a real cached overview"
    );
    let (s, v) = get(&f.state, &b, "/api/v1/me/distribution/overview").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        v["earnings"]["total_earnings"]
            .as_str()
            .unwrap()
            .parse::<BigDecimal>()
            .unwrap(),
        BigDecimal::from(900)
    );
    let (_, earnings) = get(&f.state, &a, "/api/v1/me/distribution/earnings").await;
    assert_eq!(earnings["currency"], "CNY");
    assert_eq!(
        earnings["total_earnings"]
            .as_str()
            .unwrap()
            .parse::<BigDecimal>()
            .unwrap(),
        BigDecimal::from(1)
    );
    let scope = TenantScope::checked(f.a.id, f.a.owner_user_id, TenantRole::Admin).unwrap();
    let referrals = find_referral_display_page(&f.db, scope, 20, 0)
        .await
        .unwrap();
    assert_eq!(referrals.total, 1);
    assert_eq!(
        referrals.referrals[0].total_consumption,
        BigDecimal::from(30)
    );
    assert_eq!(referrals.referrals[0].earnings, BigDecimal::from(1));
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$1 AND user_id=$2",
        [f.b.id.into(), f.a.owner_user_id.into()],
    ))
    .await
    .unwrap();
    assert_eq!(
        get(&f.state, &b, "/api/v1/me/distribution/overview")
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&f.state, &a, "/api/v1/me/distribution/overview")
            .await
            .0,
        StatusCode::OK
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn rule_reports_keep_literal_search_stable_pages_and_current_admin_checks() {
    use sea_orm::TransactionTrait;
    let mut f = Fixture::new().await;
    for (tenant, name) in [
        (f.a.id, format!("研发_%_{}", f.run)),
        (f.a.id, format!("研发AA{}", f.run)),
        (f.b.id, format!("研发_%_{}", f.run)),
    ] {
        integration_tests::db::seed_distribution_rule(
            &f.db,
            &keycompute_db::CreateDistributionRuleRequest {
                tenant_id: tenant,
                beneficiary_scope: keycompute_db::BeneficiaryScope::Everyone,
                beneficiary_id: None,
                name,
                description: None,
                commission_rate: "0.03".parse().unwrap(),
                priority: Some(10),
                effective_from: None,
                effective_until: None,
            },
        )
        .await
        .unwrap();
    }
    let filter = RuleFilter {
        search: Some("研发_%".into()),
        ..Default::default()
    };
    let rows = dao::rules(&f.db, f.admin(), &filter, 1, 0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(dao::rule_count(&f.db, f.admin(), &filter).await.unwrap(), 1);
    assert_eq!(rows[0].tenant_id, f.a.id);
    let at = token(&f.state, &f.db, f.a.owner_user_id, Some(f.a.id)).await;
    let (_, first) = get(&f.state, &at, &f.path("rules?page_size=1")).await;
    let (_, second) = get(&f.state, &at, &f.path("rules?page_size=1&page=2")).await;
    assert_eq!(first["total"], 2);
    assert_ne!(first["rules"][0]["id"], second["rules"][0]["id"]);
    let foreign = dao::rules(
        &f.db,
        DistributionScope::Tenant(
            TenantScope::checked(f.b.id, f.b.owner_user_id, TenantRole::Admin).unwrap(),
        ),
        &filter,
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        get(&f.state, &at, &f.path(&format!("rules/{}", foreign[0].id)))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("SET TRANSACTION READ ONLY")
        .await
        .unwrap();
    assert_eq!(
        dao::rules(&tx, f.admin(), &RuleFilter::default(), 20, 0)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        dao::record_count(&tx, f.admin(), &RecordFilter::default())
            .await
            .unwrap(),
        0
    );
    assert!(
        dao::record_stats(&tx, f.admin(), &RecordFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    tx.commit().await.unwrap();
    f.guard.cleanup().await.unwrap();
}
