//! Personal payment URLs never inherit tenant-administration resource scope.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{CreatePaymentOrderRequest, DbRouter, PaymentMethod, PaymentOrder, User};
use keycompute_server::{AppState, create_router};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn order(db: &DatabaseConnection, tenant_id: Uuid, user_id: Uuid) -> PaymentOrder {
    PaymentOrder::create(
        db,
        &CreatePaymentOrderRequest {
            tenant_id,
            user_id,
            amount: Decimal::from(7),
            subject: "Private order".into(),
            body: Some("private payment body".into()),
            payment_method: PaymentMethod::WechatPay,
            payment_scene: "native".into(),
            expired_at: Utc::now() + Duration::minutes(30),
        },
        &format!("SCOPED{}", Uuid::new_v4().simple()),
        "https://pay.invalid/private",
    )
    .await
    .unwrap()
}
async fn token(state: &AppState, db: &DatabaseConnection, user: Uuid, tenant: Uuid) -> String {
    let user = User::find_by_id(db, user).await.unwrap().unwrap();
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
        .unwrap();
    let ctx = state.auth.verify_token(&global).await.unwrap();
    state
        .auth
        .select_tenant(&ctx, Some(tenant))
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

#[tokio::test]
async fn tenant_admin_cannot_read_other_members_or_foreign_payment_orders_through_personal_urls() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "payment-scope-a", &run).await;
    let b = create_test_tenant(&db, "payment-scope-b", &run).await;
    let admin = create_test_user(&db, a.id, "payment-admin", &run).await;
    let peer = create_test_user(&db, a.id, "payment-peer", &run).await;
    let foreign = create_test_user(&db, b.id, "payment-foreign", &run).await;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [a.id.into(), admin.id.into()],
    ))
    .await
    .unwrap();
    let own_order = order(&db, a.id, admin.id).await;
    let peer_order = order(&db, a.id, peer.id).await;
    let foreign_order = order(&db, b.id, foreign.id).await;
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let jwt = token(&state, &db, admin.id, a.id).await;
    assert_eq!(
        get(
            &state,
            &jwt,
            &format!("/api/v1/payments/orders/{}", own_order.id)
        )
        .await
        .0,
        StatusCode::OK
    );
    for denied in [peer_order.id, foreign_order.id, Uuid::new_v4()] {
        let (status, body) = get(&state, &jwt, &format!("/api/v1/payments/orders/{denied}")).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "personal order scope must not widen: {body}"
        );
        assert!(!body.to_string().contains("private payment body"));
        assert!(!body.to_string().contains("pay.invalid"));
    }
    let (status, page) = get(&state, &jwt, "/api/v1/payments/orders?page=1&page_size=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], json!(1));
    assert_eq!(page["orders"][0]["id"], own_order.id.to_string());
    guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn payment_dao_lists_counts_and_details_share_the_same_explicit_scope() {
    use keycompute_types::{PlatformRole, PlatformScope, TenantRole, TenantScope};
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "payment-dao-a", &run).await;
    let b = create_test_tenant(&db, "payment-dao-b", &run).await;
    let user = create_test_user(&db, a.id, "payment-dao-user", &run).await;
    let peer = create_test_user(&db, a.id, "payment-dao-peer", &run).await;
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,role) VALUES($1,$2,'member')",
        [b.id.into(), user.id.into()],
    ))
    .await
    .unwrap();
    let own = order(&db, a.id, user.id).await;
    let other = order(&db, a.id, peer.id).await;
    let same_user_foreign = order(&db, b.id, user.id).await;
    let self_scope = TenantScope::checked(a.id, user.id, TenantRole::Member).unwrap();
    let admin_scope = TenantScope::checked(a.id, a.owner_user_id, TenantRole::Admin).unwrap();
    assert_eq!(
        PaymentOrder::list_owned(&db, self_scope, None, 100, 0)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        PaymentOrder::count_owned(&db, self_scope, None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        PaymentOrder::stats_owned(&db, self_scope)
            .await
            .unwrap()
            .total_amount,
        Decimal::from(7)
    );
    for id in [other.id, same_user_foreign.id] {
        assert!(
            PaymentOrder::find_owned(&db, self_scope, id)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        PaymentOrder::find_owned_by_out_trade_no(&db, self_scope, &same_user_foreign.out_trade_no)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        PaymentOrder::find_owned_by_out_trade_no(&db, self_scope, &own.out_trade_no)
            .await
            .unwrap()
            .unwrap()
            .id,
        own.id
    );
    assert!(
        PaymentOrder::list_in_tenant(&db, self_scope, None, None, 100, 0)
            .await
            .is_err()
    );
    assert!(
        PaymentOrder::find_in_tenant(&db, self_scope, other.id)
            .await
            .is_err()
    );
    assert_eq!(
        PaymentOrder::count_in_tenant(&db, admin_scope, Some("pending"), None)
            .await
            .unwrap(),
        2
    );
    let page = PaymentOrder::list_in_tenant(&db, admin_scope, Some("pending"), None, 1, 0)
        .await
        .unwrap();
    let next = PaymentOrder::list_in_tenant(&db, admin_scope, Some("pending"), None, 1, 1)
        .await
        .unwrap();
    assert_ne!(page[0].id, next[0].id);
    assert_eq!(page[0].tenant_id, a.id);
    assert_eq!(next[0].tenant_id, a.id);
    assert!(
        PaymentOrder::find_in_tenant(&db, admin_scope, same_user_foreign.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        PaymentOrder::find_in_tenant(&db, admin_scope, other.id)
            .await
            .unwrap()
            .unwrap()
            .user_id,
        peer.id
    );
    // Explicit platform raw-order queries reject operators, even with an otherwise valid scope.
    let operator = PlatformScope::checked(user.id, PlatformRole::Operator).unwrap();
    assert!(
        PaymentOrder::list_platform(&db, operator, None, None, 100, 0)
            .await
            .is_err()
    );
    assert!(
        PaymentOrder::count_platform(&db, operator, None, None)
            .await
            .is_err()
    );
    assert!(
        PaymentOrder::find_platform(&db, operator, own.id)
            .await
            .is_err()
    );
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    let jwt = token(&state, &db, user.id, a.id).await;
    for reference in [
        same_user_foreign.id.to_string(),
        same_user_foreign.out_trade_no.clone(),
        other.id.to_string(),
    ] {
        let response = create_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/payments/sync/{reference}"))
                    .header("authorization", format!("Bearer {jwt}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "unauthorized sync must not reach provider setup"
        );
    }
    guard.cleanup().await.unwrap();
}
