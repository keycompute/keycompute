//! Real SQL and HTTP checks for personal, tenant and aggregate usage scopes.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Duration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::models::usage_log::{PlatformUsageScope, TenantUsageScope, UserUsageScope};
use keycompute_db::{
    AuditContext, CreateTenantMembershipRequest, CreateUsageLogRequest, DbRouter, Tenant,
    TenantMembership, UsageLog, User,
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{
    CredentialKind, MembershipStatus, PlatformRole, PlatformScope, TenantRole, TenantScope,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn admin(t: &Tenant) -> TenantScope {
    TenantScope::checked(t.id, t.owner_user_id, TenantRole::Admin).unwrap()
}
fn audit(t: &Tenant) -> AuditContext {
    AuditContext {
        actor_user_id: t.owner_user_id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(TenantRole::Admin),
        request_id: Some(Uuid::new_v4()),
    }
}
async fn usage(
    db: &DatabaseConnection,
    t: Uuid,
    user: Uuid,
    amount: i64,
    currency: &str,
) -> UsageLog {
    let now = Utc::now();
    UsageLog::create(
        db,
        &CreateUsageLogRequest {
            request_id: Uuid::new_v4(),
            tenant_id: t,
            user_id: user,
            produce_ai_key_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            model_name: "test-model".into(),
            provider_name: "openai".into(),
            input_tokens: 10,
            output_tokens: 20,
            input_unit_price_snapshot: BigDecimal::from(1),
            output_unit_price_snapshot: BigDecimal::from(1),
            user_amount: BigDecimal::from(amount),
            currency: currency.into(),
            usage_source: "gateway_accumulated".into(),
            status: "success".into(),
            started_at: now,
            finished_at: now,
        },
    )
    .await
    .unwrap()
}
struct Fixture {
    db: DatabaseConnection,
    a: Tenant,
    b: Tenant,
    user: TenantActor,
    own: UsageLog,
    other: UsageLog,
    foreign: UsageLog,
    guard: TestDataGuard,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "usage-scope-a", &run).await;
        let b = create_test_tenant(&db, "usage-scope-b", &run).await;
        let user = create_test_user(&db, a.id, "usage-shared", &run).await;
        let other_user = create_test_user(&db, a.id, "usage-other", &run).await;
        let tx = db.begin().await.unwrap();
        TenantMembership::create(
            &tx,
            &CreateTenantMembershipRequest {
                tenant_id: b.id,
                user_id: user.id,
                tenant_role: TenantRole::Member,
            },
            &audit(&b),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let own = usage(&db, a.id, user.id, 2, "CNY").await;
        let other = usage(&db, a.id, other_user.id, 5, "CNY").await;
        let foreign = usage(&db, b.id, user.id, 7, "USD").await;
        Self {
            db,
            a,
            b,
            user,
            own,
            other,
            foreign,
            guard,
        }
    }
    async fn finish(&mut self) {
        self.guard.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn detail_list_count_and_stats_keep_personal_and_tenant_scopes_separate() {
    let mut f = Fixture::new().await;
    let personal = UserUsageScope::new(f.user.scope());
    let tenant = TenantUsageScope::new(admin(&f.a)).unwrap();
    assert_eq!(
        personal
            .list(&f.db, None, None, 100, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(personal.count(&f.db, None, None).await.unwrap(), 1);
    assert_eq!(
        personal.all_time_stats(&f.db).await.unwrap().total_cost,
        BigDecimal::from(2)
    );
    assert_eq!(
        personal.find(&f.db, f.own.id).await.unwrap().unwrap().id,
        f.own.id
    );
    for foreign in [f.other.id, f.foreign.id, Uuid::new_v4()] {
        assert!(personal.find(&f.db, foreign).await.unwrap().is_none());
    }
    assert_eq!(tenant.count(&f.db, None, None).await.unwrap(), 2);
    assert!(tenant.find(&f.db, f.other.id).await.unwrap().is_some());
    assert!(tenant.find(&f.db, f.foreign.id).await.unwrap().is_none());
    assert_eq!(
        tenant
            .stats(
                &f.db,
                Utc::now() - Duration::hours(1),
                Utc::now() + Duration::hours(1)
            )
            .await
            .unwrap()
            .total_amount,
        BigDecimal::from(7)
    );
    assert_eq!(
        UserUsageScope::new(admin(&f.a))
            .count(&f.db, None, None)
            .await
            .unwrap(),
        0,
        "admin does not widen personal scope"
    );
    assert!(TenantUsageScope::new(f.user.scope()).is_err());
    let forged = TenantUsageScope::new(
        TenantScope::checked(f.b.id, f.a.owner_user_id, TenantRole::Admin).unwrap(),
    )
    .unwrap();
    assert_eq!(forged.count(&f.db, None, None).await.unwrap(), 0);
    assert!(forged.find(&f.db, f.foreign.id).await.unwrap().is_none());
    f.finish().await;
}

#[tokio::test]
async fn stale_scopes_cannot_survive_demotion_suspension_or_tenant_deactivation() {
    let mut f = Fixture::new().await;
    let tx = f.db.begin().await.unwrap();
    let elevated =
        TenantMembership::set_role(&tx, f.a.id, f.user.id, TenantRole::Admin, 1, &audit(&f.a))
            .await
            .unwrap();
    tx.commit().await.unwrap();
    let stale =
        TenantUsageScope::new(TenantScope::checked(f.a.id, f.user.id, TenantRole::Admin).unwrap())
            .unwrap();
    assert_eq!(stale.count(&f.db, None, None).await.unwrap(), 2);
    let tx = f.db.begin().await.unwrap();
    let lowered = TenantMembership::set_role(
        &tx,
        f.a.id,
        f.user.id,
        TenantRole::Member,
        elevated.authz_version,
        &audit(&f.a),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(stale.count(&f.db, None, None).await.unwrap(), 0);
    assert!(stale.find(&f.db, f.other.id).await.unwrap().is_none());
    let personal = UserUsageScope::new(f.user.scope());
    assert_eq!(personal.count(&f.db, None, None).await.unwrap(), 1);
    let tx = f.db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        f.a.id,
        f.user.id,
        MembershipStatus::Suspended,
        lowered.authz_version,
        &audit(&f.a),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(personal.count(&f.db, None, None).await.unwrap(), 0);
    assert!(personal.find(&f.db, f.own.id).await.unwrap().is_none());
    let in_b =
        UserUsageScope::new(TenantScope::checked(f.b.id, f.user.id, TenantRole::Member).unwrap());
    assert_eq!(in_b.count(&f.db, None, None).await.unwrap(), 1);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET status='suspended' WHERE id=$1",
        [f.user.id.into()],
    ))
    .await
    .unwrap();
    assert_eq!(in_b.count(&f.db, None, None).await.unwrap(), 0);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET status='inactive' WHERE id=$1",
        [f.a.id.into()],
    ))
    .await
    .unwrap();
    assert_eq!(
        TenantUsageScope::new(admin(&f.a))
            .unwrap()
            .count(&f.db, None, None)
            .await
            .unwrap(),
        0
    );
    f.finish().await;
}

#[tokio::test]
async fn range_pagination_model_groups_and_currency_aggregates_are_consistent() {
    let mut f = Fixture::new().await;
    let start: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
    let end = start + Duration::hours(1);
    for (id, at) in [(f.own.id, start), (f.other.id, start), (f.foreign.id, end)] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE usage_logs SET created_at=$2 WHERE id=$1",
            [id.into(), at.into()],
        ))
        .await
        .unwrap();
    }
    let tenant = TenantUsageScope::new(admin(&f.a)).unwrap();
    let first = tenant
        .list(&f.db, Some(start), Some(end), 1, 0)
        .await
        .unwrap();
    let second = tenant
        .list(&f.db, Some(start), Some(end), 1, 1)
        .await
        .unwrap();
    assert_ne!(first[0].id, second[0].id);
    assert_eq!(
        tenant.count(&f.db, Some(start), Some(end)).await.unwrap(),
        2
    );
    let groups = tenant.by_model(&f.db, start, end).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].request_count, 2);
    assert_eq!(groups[0].amount, BigDecimal::from(7));
    assert!(
        tenant
            .list(&f.db, Some(end), Some(start), 20, 0)
            .await
            .is_err()
    );
    assert!(tenant.count(&f.db, Some(end), Some(start)).await.is_err());
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let platform =
        PlatformUsageScope::new(PlatformScope::checked(root.id, PlatformRole::Root).unwrap());
    let bounded = platform.stats(&f.db, start, end).await.unwrap();
    assert_eq!(bounded.len(), 1);
    assert_eq!(bounded[0].currency, "CNY");
    let all = platform
        .stats(&f.db, start, end + Duration::seconds(1))
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|row| row.total_amount == 7));
    let forged =
        PlatformUsageScope::new(PlatformScope::checked(f.user.id, PlatformRole::Root).unwrap());
    assert!(forged.stats(&f.db, start, end).await.unwrap().is_empty());
    let tx = f.db.begin().await.unwrap();
    let actor = AuditContext {
        actor_user_id: root.id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        request_id: Some(Uuid::new_v4()),
    };
    User::set_security(
        &tx,
        f.user.id,
        PlatformRole::Operator,
        keycompute_types::UserStatus::Active,
        &actor,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let operator =
        PlatformUsageScope::new(PlatformScope::checked(f.user.id, PlatformRole::Operator).unwrap());
    assert_eq!(
        operator.stats(&f.db, start, end).await.unwrap()[0].total_requests,
        2
    );
    f.finish().await;
}

async fn get(state: &AppState, token: &str, path: String) -> Value {
    let response = create_router(state.clone())
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()
}
#[tokio::test]
async fn personal_http_usage_and_cache_follow_selected_membership() {
    let mut f = Fixture::new().await;
    let state = AppState::with_pool(DbRouter::single(f.db.clone()));
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(f.user.id, None, f.user.token_version, None, None, 3600)
        .unwrap();
    let context = state.auth.verify_token(&global).await.unwrap();
    for (tenant, expected_id, cost) in [
        (f.a.id, f.own.id, 2.0),
        (f.b.id, f.foreign.id, 7.0),
        (f.a.id, f.own.id, 2.0),
    ] {
        let token = state
            .auth
            .select_tenant(&context, Some(tenant))
            .await
            .unwrap()
            .access_token;
        let path = format!(
            "/api/v1/usage?page=1&page_size=1&tenant_id={}&user_id={}",
            f.b.id, f.a.owner_user_id
        );
        let page = get(&state, &token, path).await;
        assert_eq!(page["total"], json!(1));
        assert_eq!(page["records"][0]["id"], json!(expected_id));
        let stats = get(&state, &token, "/api/v1/usage/stats".into()).await;
        assert_eq!(stats["total_requests"], json!(1));
        assert_eq!(stats["total_cost"].as_f64().unwrap(), cost);
    }
    f.finish().await;
}

#[tokio::test]
async fn cached_personal_usage_is_not_returned_after_membership_revocation() {
    let mut f = Fixture::new().await;
    let state = AppState::with_pool(DbRouter::single(f.db.clone()));
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(f.user.id, None, f.user.token_version, None, None, 3600)
        .unwrap();
    let context = state.auth.verify_token(&global).await.unwrap();
    let token = state
        .auth
        .select_tenant(&context, Some(f.a.id))
        .await
        .unwrap()
        .access_token;
    for _ in 0..2 {
        let stats = get(&state, &token, "/api/v1/usage/stats".into()).await;
        assert_eq!(stats["total_requests"], json!(1));
    }
    let cache_hits = state.display_cache.metrics()["hit"].as_u64().unwrap();
    assert!(
        cache_hits > 0,
        "the regression must exercise an actual cache hit"
    );
    let membership = TenantMembership::find(&f.db, f.a.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        f.a.id,
        f.user.id,
        MembershipStatus::Removed,
        membership.authz_version,
        &audit(&f.a),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let response = create_router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v1/usage/stats")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert!(body.get("total_requests").is_none());
    assert_eq!(
        state.display_cache.metrics()["hit"].as_u64().unwrap(),
        cache_hits,
        "rejected credentials must not reach the display cache"
    );
    // A's revocation must neither reveal A's cached result nor disable B.
    let b_token = state
        .auth
        .select_tenant(&context, Some(f.b.id))
        .await
        .unwrap()
        .access_token;
    let b_stats = get(&state, &b_token, "/api/v1/usage/stats".into()).await;
    assert_eq!(b_stats["total_cost"], json!(7.0));
    let removed = TenantMembership::find_any(&f.db, f.a.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let restore = TenantMembership::set_status(
        &tx,
        f.a.id,
        f.user.id,
        MembershipStatus::Active,
        removed.authz_version,
        &audit(&f.a),
    )
    .await;
    assert!(
        restore.is_err(),
        "removal requires a new invitation, not direct reactivation"
    );
    tx.rollback().await.unwrap();
    assert!(state.auth.verify_token(&token).await.is_err());
    assert!(
        state
            .auth
            .select_tenant(&context, Some(f.a.id))
            .await
            .is_err()
    );
    f.finish().await;
}

#[tokio::test]
async fn platform_aggregate_scope_rechecks_current_role_and_status() {
    let mut f = Fixture::new().await;
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let actor = AuditContext {
        actor_user_id: root.id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        request_id: Some(Uuid::new_v4()),
    };
    let from = Utc::now() - Duration::hours(1);
    let to = Utc::now() + Duration::hours(1);
    let scope =
        PlatformUsageScope::new(PlatformScope::checked(f.user.id, PlatformRole::Operator).unwrap());
    use keycompute_types::UserStatus;
    for (role, status, permitted) in [
        (PlatformRole::None, UserStatus::Active, false),
        (PlatformRole::Operator, UserStatus::Active, true),
        (PlatformRole::Operator, UserStatus::Suspended, false),
        (PlatformRole::None, UserStatus::Active, false),
        (PlatformRole::Root, UserStatus::Active, true),
    ] {
        let tx = f.db.begin().await.unwrap();
        User::set_security(&tx, f.user.id, role, status, &actor)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let rows = scope.stats(&f.db, from, to).await.unwrap();
        assert_eq!(
            !rows.is_empty(),
            permitted,
            "retained scope must follow current role/status: {role:?}/{status:?}"
        );
        for row in rows {
            let value = serde_json::to_value(row).unwrap();
            for forbidden in ["user_id", "request_id", "account_id", "produce_ai_key_id"] {
                assert!(value.get(forbidden).is_none());
            }
        }
    }
    f.finish().await;
}
