//! Ordinary console reads must not materialize, lock, reclaim, or mutate funds.
//! All fixtures use the explicit integration-test DATABASE_URL, never production.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_billing::balance::BalanceService;
use keycompute_db::{DbError, DbRouter, Tenant, User, UserBalance};
use keycompute_server::{AppState, create_router};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

fn money(value: &serde_json::Value, field: &str) -> Decimal {
    Decimal::from_str_exact(value[field].as_str().expect("money is a decimal string")).unwrap()
}

struct Fixture {
    pool: DatabaseConnection,
    guard: TestDataGuard,
    tenant: Tenant,
    user: User,
    service: BalanceService,
    app: Router,
    token: String,
}
impl Fixture {
    async fn new(admin: bool) -> Self {
        let pool = create_test_pool().await;
        let id = generate_test_id();
        let guard = TestDataGuard::new(pool.clone(), id.clone());
        let tenant = create_test_tenant(&pool, "display", &id).await;
        let mut user = create_test_user(&pool, tenant.id, "display", &id).await;
        if admin {
            pool.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET role = 'admin' WHERE id = $1",
                [user.id.into()],
            ))
            .await
            .unwrap();
            user = User::find_by_id(&pool, user.id).await.unwrap().unwrap();
        }
        let service = BalanceService::new(DbRouter::single(pool.clone()));
        let state = AppState::with_pool(DbRouter::single(pool.clone()));
        let token = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_token_with_version(user.id, tenant.id, &user.role, user.token_version)
            .unwrap();
        Self {
            pool,
            guard,
            tenant,
            user,
            service,
            app: create_router(state),
            token,
        }
    }
    async fn fund(&self) {
        self.service
            .recharge(
                self.user.id,
                self.tenant.id,
                Decimal::new(12345678, 4),
                None,
                None,
            )
            .await
            .unwrap();
    }
    async fn tuple(&self) -> (String, String, chrono::DateTime<chrono::Utc>) {
        let row = self
            .pool
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT xmin::text, ctid::text, updated_at FROM user_balances WHERE user_id=$1",
                [self.user.id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        (
            row.try_get_by_index(0).unwrap(),
            row.try_get_by_index(1).unwrap(),
            row.try_get_by_index(2).unwrap(),
        )
    }
    async fn get(&self, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            self.app.clone().oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", format!("Bearer {}", self.token))
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("read must finish while writer locks are held")
        .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
}
#[tokio::test]
async fn repeated_http_balance_reads_leave_tuple_and_timestamp_unchanged() {
    let mut f = Fixture::new(false).await;
    f.fund().await;
    let before = f.tuple().await;
    for _ in 0..8 {
        let (status, value) = f.get("/api/v1/payments/balance").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(
            money(&value, "available_balance"),
            Decimal::new(12345678, 4)
        );
        assert_eq!(value["initialized"], true);
        chrono::DateTime::parse_from_rfc3339(value["as_of"].as_str().unwrap()).unwrap();
    }
    assert_eq!(
        before,
        f.tuple().await,
        "GET must not even update updated_at"
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn valid_uninitialized_user_stays_uninitialized_after_http_reads() {
    let mut f = Fixture::new(false).await;
    for _ in 0..3 {
        let (status, value) = f.get("/api/v1/payments/balance").await;
        assert_eq!(status, StatusCode::OK, "{value}");
        assert_eq!(value["initialized"], false);
        assert_eq!(value["available_balance"], "0");
        assert!(
            UserBalance::find_by_user(&f.pool, f.user.id)
                .await
                .unwrap()
                .is_none()
        );
    }
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn display_reads_use_committed_mvcc_snapshot_despite_user_and_balance_writer_locks() {
    let mut f = Fixture::new(false).await;
    f.fund().await;
    let tx = f.pool.begin().await.unwrap();
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id FROM users WHERE id=$1 FOR UPDATE",
        [f.user.id.into()],
    ))
    .await
    .unwrap();
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE user_balances SET available_balance=available_balance+1, total_recharged=total_recharged+1 WHERE user_id=$1",
        [f.user.id.into()])).await.unwrap();
    let (status, value) = f.get("/api/v1/payments/balance").await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        money(&value, "available_balance"),
        Decimal::new(12345678, 4),
        "uncommitted balance must be invisible"
    );
    assert_eq!(money(&value, "total_recharged"), Decimal::new(12345678, 4));
    tx.commit().await.unwrap();
    let (status, value) = f.get("/api/v1/payments/balance").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        money(&value, "available_balance"),
        Decimal::new(12355678, 4),
        "primary must observe commit"
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn display_does_not_reclaim_expired_reservations_but_money_helper_still_does() {
    let mut f = Fixture::new(false).await;
    f.fund().await;
    let reservation = f
        .service
        .reserve_request(
            f.user.id,
            f.tenant.id,
            Uuid::new_v4(),
            Decimal::from(5),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    f.pool
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE balance_reservations SET expires_at=NOW()-INTERVAL '1 second' WHERE id=$1",
            [reservation.id.into()],
        ))
        .await
        .unwrap();
    let before = f.tuple().await;
    let (status, value) = f.get("/api/v1/payments/balance").await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(money(&value, "frozen_balance"), Decimal::from(5));
    let row = f
        .pool
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT status FROM balance_reservations WHERE id=$1",
            [reservation.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "active");
    assert_eq!(before, f.tuple().await);
    let reclaimed = f.service.find_by_user(f.user.id).await.unwrap().unwrap();
    assert_eq!(reclaimed.frozen_balance, Decimal::ZERO);
    assert_eq!(reclaimed.available_balance, Decimal::new(12345678, 4));
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn snapshot_rejects_wrong_owner_missing_user_and_bounded_batch_without_zero_fallback() {
    let mut f = Fixture::new(false).await;
    assert!(matches!(
        UserBalance::find_display_snapshot(&f.pool, Uuid::new_v4(), f.user.id).await,
        Err(DbError::UserTenantMismatch { .. })
    ));
    assert!(
        UserBalance::find_display_snapshot(&f.pool, f.tenant.id, Uuid::new_v4())
            .await
            .unwrap_err()
            .is_not_found()
    );
    let snapshots = f
        .service
        .find_display_snapshots(&[f.user.id, f.user.id])
        .await
        .unwrap();
    assert_eq!(snapshots.len(), 1);
    assert!(!snapshots[&f.user.id].initialized);
    assert!(
        f.service
            .find_display_snapshots(&[f.user.id, Uuid::new_v4()])
            .await
            .is_err()
    );
    let unavailable = DatabaseConnection::Disconnected;
    let error = UserBalance::find_display_snapshots(&unavailable, &vec![f.user.id; 1001])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("1000"),
        "batch cap must apply before a DB query"
    );
    assert!(
        UserBalance::find_display_snapshot(&unavailable, f.tenant.id, f.user.id)
            .await
            .is_err()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn admin_user_list_and_detail_use_read_only_snapshots_and_bounded_pagination() {
    let mut f = Fixture::new(true).await;
    f.fund().await;
    let before = f.tuple().await;
    let tx = f.pool.begin().await.unwrap();
    tx.query_one(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id FROM user_balances WHERE user_id=$1 FOR UPDATE",
        [f.user.id.into()],
    ))
    .await
    .unwrap();
    let (status, page) = f
        .get(&format!(
            "/api/v1/users?tenant_id={}&page_size=9223372036854775807&page=1",
            f.tenant.id
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["page_size"], 100);
    assert_eq!(page["users"][0]["balance_initialized"], true);
    let (status, detail) = f.get(&format!("/api/v1/users/{}", f.user.id)).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["balance_initialized"], true);
    tx.rollback().await.unwrap();
    assert_eq!(before, f.tuple().await);
    f.guard.cleanup().await.unwrap();
}
