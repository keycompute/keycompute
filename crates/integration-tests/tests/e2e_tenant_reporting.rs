//! Real tenant-reporting routes: stable filters, currency groups and safe fields.
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
use keycompute_db::{
    CreatePaymentOrderRequest, CreateUsageLogRequest, DbRouter, PaymentMethod, PaymentOrder,
    Tenant, UsageLog, User, UserBalance,
};
use keycompute_server::{AppState, create_router};

use rust_decimal::Decimal;
use sea_orm::DatabaseConnection;
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    member: User,
    admin_token: String,
    member_token: String,
}
async fn token(
    state: &AppState,
    db: &DatabaseConnection,
    id: Uuid,
    tenant: Option<Uuid>,
) -> String {
    let user = User::find_by_id(db, id).await.unwrap().unwrap();
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(id, None, user.token_version, None, None, 3600)
        .unwrap();
    let ctx = state.auth.verify_token(&global).await.unwrap();
    state
        .auth
        .select_tenant(&ctx, tenant)
        .await
        .unwrap()
        .access_token
}
async fn get(state: &AppState, token: &str, path: &str) -> (StatusCode, Value) {
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
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "reports-a", &run).await;
        let b = create_test_tenant(&db, "reports-b", &run).await;
        let member = create_test_user(&db, a.id, "report-member", &run)
            .await
            .user;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        let admin_token = token(&state, &db, a.owner_user_id, Some(a.id)).await;
        let member_token = token(&state, &db, member.id, Some(a.id)).await;
        Self {
            db,
            guard,
            state,
            a,
            b,
            member,
            admin_token,
            member_token,
        }
    }
    fn path(&self, suffix: &str) -> String {
        format!("/api/v1/tenants/{}/{suffix}", self.a.id)
    }
    async fn usage(&self, tenant: Uuid, user: Uuid, currency: &str, amount: i64) -> UsageLog {
        let now = Utc::now();
        UsageLog::create(
            &self.db,
            &CreateUsageLogRequest {
                request_id: Uuid::new_v4(),
                tenant_id: tenant,
                user_id: user,
                produce_ai_key_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                model_name: "report-model".into(),
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
    async fn order(&self, tenant: Uuid, user: Uuid) -> PaymentOrder {
        PaymentOrder::create(
            &self.db,
            &CreatePaymentOrderRequest {
                tenant_id: tenant,
                user_id: user,
                amount: Decimal::from(7),
                subject: "private-subject-marker".into(),
                body: Some("private-order-body-marker".into()),
                payment_method: PaymentMethod::WechatPay,
                payment_scene: "native".into(),
                expired_at: Utc::now() + Duration::minutes(30),
            },
            &format!("REPORT{}", Uuid::new_v4().simple()),
            "https://payment.invalid/secret-capability-marker",
        )
        .await
        .unwrap()
    }
}
#[tokio::test]
async fn tenant_usage_lists_details_and_currency_totals_have_identical_boundaries() {
    let mut f = Fixture::new().await;
    let cny = f.usage(f.a.id, f.member.id, "CNY", 7).await;
    let usd = f.usage(f.a.id, f.a.owner_user_id, "USD", 11).await;
    let foreign = f.usage(f.b.id, f.b.owner_user_id, "CNY", 999).await;
    let (status, list) = get(&f.state, &f.admin_token, &f.path("usage?page_size=1")).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 2);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (_, next) = get(
        &f.state,
        &f.admin_token,
        &f.path("usage?page_size=1&page=2"),
    )
    .await;
    assert_ne!(next["items"][0]["id"], list["items"][0]["id"]);
    let (_, filtered) = get(
        &f.state,
        &f.admin_token,
        &f.path(&format!("billing/records?owner_user_id={}", f.member.id)),
    )
    .await;
    assert_eq!(filtered["total"], 1);
    assert_eq!(filtered["items"][0]["id"], cny.id.to_string());
    let (status, stats) = get(&f.state, &f.admin_token, &f.path("billing/stats")).await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    let groups = stats["currencies"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    for (currency, amount) in [("CNY", 7), ("USD", 11)] {
        let g = groups.iter().find(|g| g["currency"] == currency).unwrap();
        let actual: BigDecimal = serde_json::from_value(g["total_amount"].clone()).unwrap();
        assert_eq!(actual, BigDecimal::from(amount));
        assert_eq!(g["total_requests"], 1);
    }
    for (suffix, id) in [("usage", cny.id), ("billing/records", usd.id)] {
        assert_eq!(
            get(&f.state, &f.admin_token, &f.path(&format!("{suffix}/{id}")))
                .await
                .0,
            StatusCode::OK
        );
        assert_eq!(
            get(
                &f.state,
                &f.admin_token,
                &f.path(&format!("{suffix}/{}", foreign.id))
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    let from = cny.created_at - chrono::Duration::seconds(1);
    let to = cny.created_at;
    let path = f.path(&format!(
        "usage?from={}&to={}",
        from.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        to.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    ));
    let (status, excluded) = get(&f.state, &f.admin_token, &path).await;
    assert_eq!(status, StatusCode::OK, "{excluded}");
    assert_eq!(excluded["total"], 0);
    assert_eq!(
        get(
            &f.state,
            &f.admin_token,
            &f.path("usage?tenant_id=00000000-0000-0000-0000-000000000000")
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn tenant_payment_reports_are_scoped_and_metadata_only() {
    let mut f = Fixture::new().await;
    let own = f.order(f.a.id, f.member.id).await;
    let _foreign = f.order(f.b.id, f.b.owner_user_id).await;
    let (status, list) = get(
        &f.state,
        &f.admin_token,
        &f.path("payments/orders?status=pending"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["id"], own.id.to_string());
    let (status, detail) = get(
        &f.state,
        &f.admin_token,
        &f.path(&format!("payments/orders/{}", own.id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    for field in [
        "pay_url",
        "notify_data",
        "provider_payload",
        "body",
        "subject",
        "remarks",
        "last_error_message",
    ] {
        assert!(
            detail.get(field).is_none(),
            "{field} must not be in tenant reports"
        );
    }
    assert!(!detail.to_string().contains("private-"));
    assert!(!detail.to_string().contains("secret-capability-marker"));
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn ordinary_member_reports_are_denied_and_wallet_views_stay_read_only() {
    let mut f = Fixture::new().await;
    for suffix in [
        "usage",
        "usage/stats",
        "billing/records",
        "billing/stats",
        "payments/orders",
    ] {
        assert_eq!(
            get(&f.state, &f.member_token, &f.path(suffix)).await.0,
            StatusCode::FORBIDDEN
        );
    }
    assert!(
        UserBalance::find_by_user(&f.db, f.a.id, f.member.id)
            .await
            .unwrap()
            .is_none()
    );
    let path = f.path(&format!("balances/{}", f.member.id));
    let (status, view) = get(&f.state, &f.admin_token, &path).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["initialized"], false);
    assert!(
        UserBalance::find_by_user(&f.db, f.a.id, f.member.id)
            .await
            .unwrap()
            .is_none(),
        "a reporting read cannot create a wallet"
    );
    assert_eq!(
        get(&f.state, &f.member_token, &path).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(
            &f.state,
            &f.admin_token,
            &f.path(&format!("balances/{}", f.b.owner_user_id))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    UserBalance::recharge(&f.db, f.a.id, f.member.id, Decimal::from(19), None, None)
        .await
        .unwrap();
    let before = UserBalance::find_by_user(&f.db, f.a.id, f.member.id)
        .await
        .unwrap()
        .unwrap();
    let (status, view) = get(&f.state, &f.admin_token, &path).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["initialized"], true);
    let after = UserBalance::find_by_user(&f.db, f.a.id, f.member.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.updated_at, after.updated_at);
    assert_eq!(before.available_balance, after.available_balance);
    f.guard.cleanup().await.unwrap();
}
