//! Live primary identity checks remain mandatory despite console read optimizations.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{DbRouter, TenantMembership};
use keycompute_ratelimit::{RateLimitConfig, RateLimitKey};
use keycompute_server::{AppState, create_router};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;
struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    user: TenantActor,
    token: String,
    run: String,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "console-auth", &run).await;
        let user = create_test_user(&db, tenant.id, "console-auth", &run).await;
        let membership = TenantMembership::find(&db, tenant.id, user.id)
            .await
            .unwrap()
            .unwrap();
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        let token = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                user.id,
                Some(user.tenant_id),
                user.token_version,
                Some(tenant.authz_version),
                Some(membership.authz_version),
                3600,
            )
            .unwrap();
        Self {
            db,
            guard,
            state,
            user,
            token,
            run,
        }
    }
    // Exercise only the existing in-process router and this fixture's identity.
    // There are no network requests or new credentials in this helper.
    async fn dashboard_status(&self) -> StatusCode {
        create_router(self.state.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/overview")
                    .header("authorization", format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
    async fn change(&self, sql: &str) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                [self.user.id.into()],
            ))
            .await
            .unwrap();
    }
    async fn verify(&self) -> bool {
        tokio::time::timeout(
            Duration::from_secs(3),
            self.state.auth.verify_token(&self.token),
        )
        .await
        .unwrap()
        .is_ok()
    }
    async fn refresh_selected_token(&mut self) {
        let global = self
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                self.user.id,
                None,
                self.user.token_version,
                None,
                None,
                3600,
            )
            .unwrap();
        let context = self.state.auth.verify_token(&global).await.unwrap();
        self.token = self
            .state
            .auth
            .select_tenant(&context, Some(self.user.tenant_id))
            .await
            .unwrap()
            .access_token;
    }
}
#[tokio::test]
async fn jwt_revocation_is_observed_on_the_next_request() {
    let mut f = Fixture::new().await;
    assert!(f.verify().await);
    assert_eq!(f.dashboard_status().await, StatusCode::OK);
    assert_eq!(f.dashboard_status().await, StatusCode::OK);
    assert_eq!(f.state.display_cache.metrics()["hit"], 1);
    f.change("UPDATE users SET token_version=token_version+1 WHERE id=$1")
        .await;
    assert!(!f.verify().await);
    assert_eq!(f.dashboard_status().await, StatusCode::UNAUTHORIZED);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn role_change_does_not_keep_old_jwt_permissions() {
    let mut f = Fixture::new().await;
    assert!(f.verify().await);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$2 AND user_id=$1",
        [f.user.id.into(), f.user.tenant_id.into()],
    ))
    .await
    .unwrap();
    assert!(!f.verify().await);
    assert_eq!(f.dashboard_status().await, StatusCode::UNAUTHORIZED);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn tenant_reassignment_rejects_the_old_jwt_even_without_a_version_change() {
    let mut f = Fixture::new().await;
    let other = create_test_tenant(&f.db, "console-auth-other", &f.run).await;
    assert!(f.verify().await);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='removed' WHERE tenant_id=$2 AND user_id=$1",
        [f.user.id.into(), f.user.tenant_id.into()],
    ))
    .await
    .unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status) VALUES($2,$1,'member','active')",
        [f.user.id.into(), other.id.into()],
    )).await.unwrap();
    assert!(!f.verify().await);
    assert_eq!(f.dashboard_status().await, StatusCode::UNAUTHORIZED);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn joined_primary_identity_uses_committed_state_without_row_locks() {
    let mut f = Fixture::new().await;
    let tx = f.db.begin().await.unwrap();
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id FROM users WHERE id=$1 FOR UPDATE",
        [f.user.id.into()],
    ))
    .await
    .unwrap();
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET status='inactive' WHERE id=$1",
        [f.user.tenant_id.into()],
    ))
    .await
    .unwrap();
    assert!(
        f.verify().await,
        "uncommitted closure is invisible and ordinary identity SELECT does not wait for row locks"
    );
    tx.commit().await.unwrap();
    assert!(!f.verify().await);
    assert_eq!(f.dashboard_status().await, StatusCode::UNAUTHORIZED);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn real_console_http_reads_do_not_spend_generation_rpm() {
    let mut f = Fixture::new().await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET default_rpm_limit=1 WHERE id=$1",
        [f.user.tenant_id.into()],
    ))
    .await
    .unwrap();
    f.refresh_selected_token().await;
    let app = create_router(f.state.clone());
    for _ in 0..4 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/payments/balance")
                    .header("authorization", format!("Bearer {}", f.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let key = RateLimitKey::new(f.user.tenant_id, f.user.id, Uuid::nil());
    let cfg = RateLimitConfig::new(1, 1000);
    assert!(
        f.state
            .rate_limiter
            .check_and_record_with_config(&key, &cfg)
            .await
            .is_ok()
    );
    assert!(
        f.state
            .rate_limiter
            .check_and_record_with_config(&key, &cfg)
            .await
            .is_err()
    );
    f.guard.cleanup().await.unwrap();
}
