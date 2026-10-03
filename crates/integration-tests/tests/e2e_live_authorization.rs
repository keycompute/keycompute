//! Live authorization checks use production verifiers/extractors and isolated DB fixtures.
//! Probe routes test the phase-two boundary; tenant CRUD acceptance is a later gate.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Path,
    http::{Request, StatusCode},
    routing::get,
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_auth::{
    AuthorizationAction as Action, AuthorizationDecision, Permission, ProduceAiKeyValidator,
    ResourceScope, authorize,
};
use keycompute_db::{
    AuditContext, CreateTenantMembershipRequest, CreateUserRequest, DbRouter, Tenant,
    TenantMembership, User, models::api_key::CreateProduceAiKeyRequest,
};
use keycompute_server::{
    AppState, create_router,
    error::ApiError,
    extractors::{AuthExtractor, ConsoleAuth, GlobalConsoleAuth},
};
use keycompute_types::{CredentialKind, MembershipStatus, PlatformRole, TenantRole};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn platform_health(auth: GlobalConsoleAuth) -> Result<StatusCode, ApiError> {
    auth.require_platform(Action::ReadTenantHealth)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn platform_manage(auth: GlobalConsoleAuth) -> Result<StatusCode, ApiError> {
    auth.require_platform(Action::ManagePlatform)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn tenant_admin(auth: ConsoleAuth) -> Result<StatusCode, ApiError> {
    auth.require_tenant(Action::ManageMembers)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn personal(auth: ConsoleAuth, Path(owner): Path<Uuid>) -> Result<StatusCode, ApiError> {
    auth.require_owner(owner, Action::ManagePersonalResource)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn tenant_resource(
    auth: ConsoleAuth,
    Path((tenant, owner)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let decision = authorize(
        auth.credential_kind,
        auth.authorization_context().authorization_subject(),
        Action::ManageTenantResource,
        ResourceScope::UserOwned {
            tenant_id: tenant,
            owner_user_id: owner,
        },
    );
    if decision != AuthorizationDecision::Allow {
        return Err(ApiError::Forbidden("resource scope denied".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}
async fn inference_identity(auth: AuthExtractor) -> Result<Json<Value>, ApiError> {
    auth.require_tenant(Action::Use)?;
    Ok(Json(
        json!({"tenant_id":auth.tenant_id,"user_id":auth.user_id}),
    ))
}
fn probes(state: AppState) -> Router {
    Router::new()
        .route("/health", get(platform_health))
        .route("/platform", get(platform_manage))
        .route("/admin", get(tenant_admin))
        .route("/personal/{owner}", get(personal))
        .route("/resource/{tenant}/{owner}", get(tenant_resource))
        .route("/identity", get(inference_identity))
        .with_state(state)
}
async fn call(
    app: Router,
    token: &str,
    method: &str,
    path: &str,
    body: Value,
    tenant_header: Option<Uuid>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(id) = tenant_header {
        req = req.header("x-tenant-id", id.to_string());
    }
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        app.oneshot(
            req.body(if body.is_null() {
                Body::empty()
            } else {
                Body::from(body.to_string())
            })
            .unwrap(),
        ),
    )
    .await
    .expect("authorization request timeout")
    .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn audit(tenant: &Tenant) -> AuditContext {
    AuditContext {
        actor_user_id: tenant.owner_user_id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(TenantRole::Admin),
        request_id: Some(Uuid::new_v4()),
    }
}
struct Fixture {
    db: DatabaseConnection,
    state: AppState,
    a: Tenant,
    b: Tenant,
    run: String,
    guard: TestDataGuard,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "live-auth-a", &run).await;
        let b = create_test_tenant(&db, "live-auth-b", &run).await;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        Self {
            db,
            state,
            a,
            b,
            run,
            guard,
        }
    }
    async fn platform_role(&self, id: Uuid, role: PlatformRole) -> User {
        // Test fixture assignment still executes the real version/invariant triggers.
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET platform_role=$2 WHERE id=$1",
                [id.into(), role.as_str().into()],
            ))
            .await
            .unwrap();
        User::find_by_id(&self.db, id).await.unwrap().unwrap()
    }
    async fn member(&self, suffix: &str, platform: PlatformRole) -> User {
        let member = create_test_user(&self.db, self.a.id, suffix, &self.run).await;
        let tx = self.db.begin().await.unwrap();
        TenantMembership::set_role(
            &tx,
            self.a.id,
            member.id,
            TenantRole::Admin,
            1,
            &audit(&self.a),
        )
        .await
        .unwrap();
        TenantMembership::create(
            &tx,
            &CreateTenantMembershipRequest {
                tenant_id: self.b.id,
                user_id: member.id,
                tenant_role: TenantRole::Member,
            },
            &audit(&self.b),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        self.platform_role(member.id, platform).await
    }
    async fn global_token(&self, id: Uuid) -> String {
        let user = User::find_by_id(&self.db, id).await.unwrap().unwrap();
        self.state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(id, None, user.token_version, None, None, 3600)
            .unwrap()
    }
    async fn selected(&self, global: &str, tenant: Uuid) -> String {
        let ctx = self.state.auth.verify_token(global).await.unwrap();
        self.state
            .auth
            .select_tenant(&ctx, Some(tenant))
            .await
            .unwrap()
            .access_token
    }
    async fn key(&self, user: Uuid, tenant: Uuid) -> String {
        let key = ProduceAiKeyValidator::generate_key();
        integration_tests::db::create_test_api_key(
            &self.db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant,
                user_id: user,
                name: "live-authorization-test".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "test-key".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        key
    }
    async fn request(&self, token: &str, path: &str) -> StatusCode {
        call(
            probes(self.state.clone()),
            token,
            "GET",
            path,
            Value::Null,
            None,
        )
        .await
        .0
    }
    async fn finish(&mut self) {
        self.guard.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn credential_and_orthogonal_role_matrix_is_enforced_over_http() {
    let mut f = Fixture::new().await;
    for platform in [
        PlatformRole::Root,
        PlatformRole::Operator,
        PlatformRole::None,
    ] {
        let user = f.member(platform.as_str(), platform).await;
        let global = f.global_token(user.id).await;
        let a = f.selected(&global, f.a.id).await;
        let b = f.selected(&global, f.b.id).await;
        assert_eq!(
            f.request(&global, "/platform").await,
            if platform == PlatformRole::Root {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::FORBIDDEN
            }
        );
        assert_eq!(
            f.request(&global, "/health").await,
            if platform == PlatformRole::None {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::NO_CONTENT
            }
        );
        assert_eq!(f.request(&global, "/admin").await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            f.request(&a, "/admin").await,
            StatusCode::NO_CONTENT,
            "platform roles must not suppress tenant admin"
        );
        assert_eq!(
            f.request(&b, "/admin").await,
            StatusCode::FORBIDDEN,
            "platform roles must not create tenant admin"
        );
        for token in [&a, &b] {
            assert_eq!(
                f.request(token, &format!("/personal/{}", user.id)).await,
                StatusCode::NO_CONTENT
            );
            assert_eq!(
                f.request(token, &format!("/personal/{}", f.a.owner_user_id))
                    .await,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            f.request(&a, &format!("/resource/{}/{}", f.a.id, f.a.owner_user_id))
                .await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            f.request(&a, &format!("/resource/{}/{}", f.b.id, user.id))
                .await,
            StatusCode::FORBIDDEN
        );
        let key = f.key(user.id, f.a.id).await;
        for path in ["/platform", "/health", "/admin"] {
            assert_eq!(f.request(&key, path).await, StatusCode::FORBIDDEN);
        }
        let (status, identity) = call(
            probes(f.state.clone()),
            &key,
            "GET",
            "/identity",
            Value::Null,
            Some(f.b.id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            identity["tenant_id"],
            f.a.id.to_string(),
            "headers cannot retarget inference credentials"
        );
        assert_eq!(
            call(
                create_router(f.state.clone()),
                &key,
                "GET",
                "/api/v1/me",
                Value::Null,
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let context = f.state.auth.verify_token(&key).await.unwrap();
        let extracted = AuthExtractor::from_auth_context(context.clone()).unwrap();
        for p in Permission::all() {
            assert_eq!(context.has_permission(p), extracted.has_permission(p));
        }
    }
    f.finish().await;
}

#[tokio::test]
async fn global_identity_can_select_only_existing_memberships_and_clear_selection() {
    let mut f = Fixture::new().await;
    for platform in [
        PlatformRole::Root,
        PlatformRole::Operator,
        PlatformRole::None,
    ] {
        let user = User::create(
            &f.db,
            &CreateUserRequest {
                email: format!("global-{}-{}@example.com", platform.as_str(), f.run),
                name: None,
            },
        )
        .await
        .unwrap();
        let user = f.platform_role(user.id, platform).await;
        let token = f.global_token(user.id).await;
        let (status, session) = call(
            create_router(f.state.clone()),
            &token,
            "GET",
            "/api/v1/me",
            Value::Null,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(session["selected_tenant"].is_null());
        assert_eq!(session["memberships"], json!([]));
        assert_eq!(
            call(
                create_router(f.state.clone()),
                &token,
                "POST",
                "/api/v1/me/tenant",
                json!({"tenant_id":f.a.id}),
                None
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let user = f.member("switch", PlatformRole::None).await;
    let global = f.global_token(user.id).await;
    let (status, a) = call(
        create_router(f.state.clone()),
        &global,
        "POST",
        "/api/v1/me/tenant",
        json!({"tenant_id":f.a.id}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(a["selected_tenant"]["tenant_role"], "admin");
    let (status, b) = call(
        create_router(f.state.clone()),
        a["access_token"].as_str().unwrap(),
        "POST",
        "/api/v1/me/tenant",
        json!({"tenant_id":f.b.id}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(b["selected_tenant"]["tenant_role"], "member");
    let (status, global_again) = call(
        create_router(f.state.clone()),
        b["access_token"].as_str().unwrap(),
        "POST",
        "/api/v1/me/tenant",
        json!({"tenant_id":null}),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(global_again["selected_tenant"].is_null());
    assert_eq!(global_again["capabilities"]["tenant"], json!([]));
    f.finish().await;
}

#[tokio::test]
async fn membership_and_tenant_versions_revoke_only_their_own_scope() {
    let mut f = Fixture::new().await;
    let user = f.member("versions", PlatformRole::Operator).await;
    let global = f.global_token(user.id).await;
    let a = f.selected(&global, f.a.id).await;
    let b = f.selected(&global, f.b.id).await;
    let key_a = f.key(user.id, f.a.id).await;
    let key_b = f.key(user.id, f.b.id).await;
    let before = TenantMembership::find(&f.db, f.a.id, user.id)
        .await
        .unwrap()
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let lowered = TenantMembership::set_role(
        &tx,
        f.a.id,
        user.id,
        TenantRole::Member,
        before.authz_version,
        &audit(&f.a),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(f.state.auth.verify_token(&a).await.is_err());
    f.state.auth.verify_token(&b).await.unwrap();
    f.state.auth.verify_token(&global).await.unwrap();
    let refreshed = f.selected(&global, f.a.id).await;
    assert_eq!(f.request(&refreshed, "/admin").await, StatusCode::FORBIDDEN);
    let tx = f.db.begin().await.unwrap();
    let paused = TenantMembership::set_status(
        &tx,
        f.a.id,
        user.id,
        MembershipStatus::Suspended,
        lowered.authz_version,
        &audit(&f.a),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(f.state.auth.verify_token(&refreshed).await.is_err());
    assert!(f.state.auth.verify_token(&key_a).await.is_err());
    f.state.auth.verify_token(&key_b).await.unwrap();
    let tx = f.db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        f.a.id,
        user.id,
        MembershipStatus::Active,
        paused.authz_version,
        &audit(&f.a),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        f.state.auth.verify_token(&key_a).await.is_err(),
        "reactivation must not revive old keys"
    );
    assert!(
        f.state.auth.verify_token(&a).await.is_err(),
        "reactivation must not revive old JWTs"
    );
    let current = f.selected(&global, f.a.id).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET default_rpm_limit=default_rpm_limit+1 WHERE id=$1",
        [f.a.id.into()],
    ))
    .await
    .unwrap();
    assert!(f.state.auth.verify_token(&current).await.is_err());
    f.state.auth.verify_token(&b).await.unwrap();
    let new_key = f.key(user.id, f.a.id).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET status='inactive' WHERE id=$1",
        [f.a.id.into()],
    ))
    .await
    .unwrap();
    assert!(f.state.auth.verify_token(&new_key).await.is_err());
    assert_eq!(f.request(&b, "/identity").await, StatusCode::OK);
    f.finish().await;
}

#[tokio::test]
async fn live_global_security_changes_invalidate_all_old_sessions_without_cross_scope_escalation() {
    let mut f = Fixture::new().await;
    let user = f.member("global-versions", PlatformRole::Operator).await;
    let global = f.global_token(user.id).await;
    let a = f.selected(&global, f.a.id).await;
    let b = f.selected(&global, f.b.id).await;
    let key = f.key(user.id, f.a.id).await;
    f.platform_role(user.id, PlatformRole::None).await;
    for token in [&global, &a, &b] {
        assert!(f.state.auth.verify_token(token).await.is_err());
    }
    let key_context = f.state.auth.verify_token(&key).await.unwrap();
    assert_eq!(key_context.platform_role, PlatformRole::None);
    assert_eq!(key_context.permissions, vec![Permission::UseApi]);
    let current_global = f.global_token(user.id).await;
    let current_a = f.selected(&current_global, f.a.id).await;
    assert_eq!(
        f.request(&current_a, "/admin").await,
        StatusCode::NO_CONTENT
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET status='suspended' WHERE id=$1",
        [user.id.into()],
    ))
    .await
    .unwrap();
    for token in [&current_global, &current_a, &key] {
        assert!(f.state.auth.verify_token(token).await.is_err());
    }
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET status='active' WHERE id=$1",
        [user.id.into()],
    ))
    .await
    .unwrap();
    for token in [&current_global, &current_a] {
        assert!(f.state.auth.verify_token(token).await.is_err());
    }
    f.finish().await;
}

#[tokio::test]
async fn missing_authentication_storage_is_not_reported_as_a_bad_credential() {
    let state = AppState::default();
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(Uuid::new_v4(), None, 0, None, None, 3600)
        .unwrap();
    let key = ProduceAiKeyValidator::generate_key();
    for (token, path) in [(&global, "/health"), (&key, "/identity")] {
        let (status, _) = call(probes(state.clone()), token, "GET", path, Value::Null, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
    let (status, _) = call(
        probes(state),
        "invalid-token",
        "GET",
        "/health",
        Value::Null,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn independent_go_cookies_and_role_headers_never_authenticate_rust_routes() {
    let mut f = Fixture::new().await;
    let user = f.member("foreign-cookie", PlatformRole::None).await;
    let app = create_router(f.state.clone());
    let routes = [
        "/api/v1/me".to_owned(),
        "/api/v1/platform/users".to_owned(),
        format!("/api/v1/tenants/{}/members", f.a.id),
    ];
    for path in &routes {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("cookie", "new_api_refresh=test-only; new_api_has_session=1")
                    .header("new-api-user", "1")
                    .header("x-user-id", user.id.to_string())
                    .header("x-tenant-id", f.a.id.to_string())
                    .header("x-role", "Root")
                    .header("x-platform-role", "root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    // A separately issued Rust inference key is the only permitted bridge;
    // Go cookies/headers cannot turn that key into a console credential.
    let key = f.key(user.id, f.a.id).await;
    let response = probes(f.state.clone())
        .oneshot(
            Request::builder()
                .uri("/identity")
                .header("authorization", format!("Bearer {key}"))
                .header("cookie", "new_api_refresh=test-only; new_api_has_session=1")
                .header("x-platform-role", "root")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(body["tenant_id"], f.a.id.to_string());
    assert_eq!(body["user_id"], user.id.to_string());
    for path in &routes {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("authorization", format!("Bearer {key}"))
                    .header("cookie", "new_api_refresh=test-only; new_api_has_session=1")
                    .header("x-role", "Root")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
    }
    f.guard.cleanup().await.unwrap();
}
