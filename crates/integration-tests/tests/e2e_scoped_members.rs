//! Real database and HTTP regressions for explicit user/member query scopes.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::models::user::TenantMemberRecord;
use keycompute_db::{
    AuditContext, CreateTenantMembershipRequest, DbRouter, Tenant, TenantMembership, User,
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{
    CredentialKind, MembershipStatus, PlatformRole, PlatformScope, TenantRole, TenantScope,
    UserStatus,
};
use sea_orm::{DatabaseConnection, TransactionTrait};
use tower::ServiceExt;
use uuid::Uuid;

fn scope(t: &Tenant) -> TenantScope {
    TenantScope::checked(t.id, t.owner_user_id, TenantRole::Admin).unwrap()
}
fn actor(id: Uuid) -> AuditContext {
    AuditContext {
        actor_user_id: id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(TenantRole::Admin),
        request_id: Some(Uuid::new_v4()),
    }
}
async fn root(db: &DatabaseConnection) -> User {
    User::find_by_email(db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap()
}
async fn add(db: &DatabaseConnection, t: &Tenant, user: Uuid, role: TenantRole) {
    let tx = db.begin().await.unwrap();
    TenantMembership::create(
        &tx,
        &CreateTenantMembershipRequest {
            tenant_id: t.id,
            user_id: user,
            role,
        },
        &actor(t.owner_user_id),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
async fn security(db: &DatabaseConnection, who: &User, role: PlatformRole) {
    let r = root(db).await;
    let tx = db.begin().await.unwrap();
    User::set_security(&tx, who.id, role, UserStatus::Active, &actor(r.id))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn tenant_member_projection_preserves_membership_and_hides_global_authority() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "member-a", &run).await;
    let b = create_test_tenant(&db, "member-b", &run).await;
    let user = create_test_user(&db, a.id, "shared", &run).await;
    add(&db, &b, user.id, TenantRole::Admin).await;
    security(&db, &user, PlatformRole::Operator).await;
    let in_a = TenantMemberRecord::find_in_tenant(&db, scope(&a), user.id)
        .await
        .unwrap()
        .unwrap();
    let in_b = TenantMemberRecord::find_in_tenant(&db, scope(&b), user.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(in_a.user_id, in_b.user_id);
    assert_eq!(in_a.role, "member");
    assert_eq!(in_b.role, "admin");
    assert_eq!(in_a.tenant_id, a.id);
    assert_eq!(in_b.tenant_id, b.id);
    let json = serde_json::to_value(&in_a).unwrap();
    for field in ["platform_role", "token_version", "password", "memberships"] {
        assert!(json.get(field).is_none());
    }
    assert!(
        TenantMemberRecord::find_in_tenant(&db, scope(&a), b.owner_user_id)
            .await
            .unwrap()
            .is_none()
    );
    let member = TenantScope::checked(a.id, user.id, TenantRole::Member).unwrap();
    assert!(
        TenantMemberRecord::list_in_tenant(&db, member, None, None, 20, 0)
            .await
            .is_err()
    );
    let fabricated = TenantScope::checked(a.id, b.owner_user_id, TenantRole::Admin).unwrap();
    assert_eq!(
        TenantMemberRecord::count_in_tenant(&db, fabricated, None, None)
            .await
            .unwrap(),
        0
    );
    assert!(
        TenantMemberRecord::find_in_tenant(&db, fabricated, user.id)
            .await
            .unwrap()
            .is_none()
    );
    guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn platform_user_http_queries_require_root_without_tenant_selection() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let tenant = create_test_tenant(&db, "platform-member", &run).await;
    let root = root(&db).await;
    let owner = User::find_by_id(&db, tenant.owner_user_id)
        .await
        .unwrap()
        .unwrap();
    let state = AppState::with_pool(DbRouter::single(db.clone()));
    for (user, expected) in [(&root, StatusCode::OK), (&owner, StatusCode::FORBIDDEN)] {
        let token = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
            .unwrap();
        let response = create_router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/users?search={run}&page_size=1"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["total"], 1);
            assert_eq!(value["users"][0]["id"], owner.id.to_string());
        }
    }
    let forged = PlatformScope::checked(owner.id, PlatformRole::Root).unwrap();
    assert!(
        User::find_platform(&db, forged, root.id)
            .await
            .unwrap()
            .is_none()
    );
    let operator = PlatformScope::checked(owner.id, PlatformRole::Operator).unwrap();
    assert!(User::find_platform(&db, operator, root.id).await.is_err());
    guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_query_pagination_and_literal_search_do_not_expand_scope() {
    let db = create_test_pool().await;
    let run = generate_test_id();
    let mut guard = TestDataGuard::new(db.clone(), run.clone());
    let a = create_test_tenant(&db, "query-a", &run).await;
    let b = create_test_tenant(&db, "query-b", &run).await;
    let literal = create_test_user(&db, a.id, "literal%_", &run).await;
    create_test_user(&db, b.id, "foreign%_", &run).await;
    let all = TenantMemberRecord::list_in_tenant(&db, scope(&a), None, None, 100, 0)
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(
        TenantMemberRecord::count_in_tenant(&db, scope(&a), None, None)
            .await
            .unwrap(),
        2
    );
    let first = TenantMemberRecord::list_in_tenant(&db, scope(&a), None, None, 1, 0)
        .await
        .unwrap();
    let second = TenantMemberRecord::list_in_tenant(&db, scope(&a), None, None, 1, 1)
        .await
        .unwrap();
    assert_ne!(first[0].user_id, second[0].user_id);
    let found = TenantMemberRecord::list_in_tenant(
        &db,
        scope(&a),
        Some(MembershipStatus::Active),
        Some("%_"),
        20,
        0,
    )
    .await
    .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].user_id, literal.id);
    assert_eq!(
        TenantMemberRecord::count_in_tenant(
            &db,
            scope(&a),
            Some(MembershipStatus::Active),
            Some("%_")
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        TenantMemberRecord::count_in_tenant(&db, scope(&a), Some(MembershipStatus::Revoked), None)
            .await
            .unwrap(),
        0
    );
    guard.cleanup().await.unwrap();
}
