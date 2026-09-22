//! PostgreSQL + HTTP tenant-control regressions.
//!
//! These tests intentionally use the in-process Axum router, which exercises
//! the same extractor, middleware, SQL transaction, and response path as the
//! bound server without opening a production port.

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{DbRouter, Tenant, TenantMembership, User};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, UserStatus};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    tenant: Tenant,
    owner: User,
    member: User,
    owner_token: String,
    member_token: String,
    run: String,
}

impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "control", &run).await;
        let owner = User::find_by_id(&db, tenant.owner_user_id)
            .await
            .unwrap()
            .unwrap();
        let member_actor = create_test_user(&db, tenant.id, "member", &run).await;
        let member = member_actor.user;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        let owner_token = token(&state, &db, &tenant, &owner).await;
        let member_token = token(&state, &db, &tenant, &member).await;
        Self {
            db,
            guard,
            state,
            tenant,
            owner,
            member,
            owner_token,
            member_token,
            run,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: impl Into<String>,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path.into());
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = builder
            .header("content-type", "application/json")
            .body(Body::from(
                body.map(|value| value.to_string()).unwrap_or_default(),
            ))
            .unwrap();
        let response = create_router(self.state.clone())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }
}

async fn token(state: &AppState, db: &DatabaseConnection, tenant: &Tenant, user: &User) -> String {
    let membership = TenantMembership::find(db, tenant.id, user.id)
        .await
        .unwrap()
        .unwrap();
    state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            user.id,
            Some(tenant.id),
            user.token_version,
            Some(tenant.authz_version),
            Some(membership.authz_version),
            3600,
        )
        .unwrap()
}

async fn promote_member_to_admin(db: &DatabaseConnection, tenant_id: Uuid, user_id: Uuid) {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [tenant_id.into(), user_id.into()],
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn tenant_context_requires_the_selected_membership_and_safe_fields() {
    let mut f = Fixture::new().await;
    let (status, body) = f
        .request(
            Method::GET,
            format!("/api/v1/tenants/{}", f.tenant.id),
            Some(&f.member_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], f.tenant.id.to_string());
    assert_eq!(body["tenant_role"], "member");
    for forbidden in ["platform_role", "token_version", "password", "memberships"] {
        assert!(body.get(forbidden).is_none(), "{forbidden} must not leak");
    }

    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let root_without_selection = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(root.id, None, root.token_version, None, None, 3600)
        .unwrap();
    let (status, _) = f
        .request(
            Method::GET,
            format!("/api/v1/tenants/{}", f.tenant.id),
            Some(&root_without_selection),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_operator_and_foreign_object_requests_are_denied_without_cross_tenant_effects() {
    let mut f = Fixture::new().await;
    let foreign = create_test_tenant(&f.db, "foreign", &f.run).await;
    let foreign_member = create_test_user(&f.db, foreign.id, "foreign-member", &f.run).await;

    let (status, _) = f
        .request(
            Method::GET,
            format!("/api/v1/tenants/{}/members", f.tenant.id),
            Some(&f.member_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = f
        .request(
            Method::GET,
            format!(
                "/api/v1/tenants/{}/members/{}",
                f.tenant.id, foreign_member.id
            ),
            Some(&f.owner_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = f
        .request(
            Method::GET,
            format!("/api/v1/tenants/{}/members/not-a-uuid", f.tenant.id),
            Some(&f.owner_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn member_update_is_versioned_atomic_and_rejects_unknown_global_fields() {
    let mut f = Fixture::new().await;
    let membership = TenantMembership::find(&f.db, f.tenant.id, f.member.id)
        .await
        .unwrap()
        .unwrap();
    let (status, _) = f
        .request(
            Method::PATCH,
            format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.member.id),
            Some(&f.owner_token),
            Some(json!({
                "expected_authz_version": membership.authz_version,
                "tenant_role": "admin",
                "status": "suspended",
                "platform_role": "root"
            })),
        )
        .await;
    // Axum rejects a typed JSON shape mismatch with 422 before the handler.
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let unchanged = TenantMembership::find(&f.db, f.tenant.id, f.member.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.authz_version, membership.authz_version);
    assert_eq!(unchanged.tenant_role, "member");
    assert_eq!(unchanged.status, "active");

    let (status, body) = f
        .request(
            Method::PATCH,
            format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.member.id),
            Some(&f.owner_token),
            Some(json!({
                "expected_authz_version": membership.authz_version,
                "tenant_role": "admin",
                "status": "active"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tenant_role"], "admin");
    assert_eq!(body["membership_status"], "active");
    assert_eq!(
        body["authz_version"].as_i64().unwrap(),
        membership.authz_version + 1
    );

    let (status, _) = f
        .request(
            Method::PATCH,
            format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.member.id),
            Some(&f.owner_token),
            Some(json!({
                "expected_authz_version": membership.authz_version,
                "status": "suspended"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn last_admin_and_owner_protection_are_conflicts() {
    let mut f = Fixture::new().await;
    let owner_membership = TenantMembership::find(&f.db, f.tenant.id, f.owner.id)
        .await
        .unwrap()
        .unwrap();
    let (status, _) = f
        .request(
            Method::DELETE,
            format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.owner.id),
            Some(&f.owner_token),
            Some(json!({
                "expected_authz_version": owner_membership.authz_version
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    promote_member_to_admin(&f.db, f.tenant.id, f.member.id).await;
    let member_membership = TenantMembership::find(&f.db, f.tenant.id, f.member.id)
        .await
        .unwrap()
        .unwrap();
    let (status, _) = f
        .request(
            Method::PATCH,
            format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.member.id),
            Some(&f.owner_token),
            Some(json!({
                "expected_authz_version": member_membership.authz_version,
                "tenant_role": "member",
                "status": "removed"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn operator_without_tenant_membership_cannot_use_a_tenant_url() {
    let mut f = Fixture::new().await;
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let operator = User::create(
        &f.db,
        &keycompute_db::CreateUserRequest {
            email: format!("test-operator-{}@example.com", f.run),
            name: Some("Test Operator".into()),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    keycompute_db::User::set_security(
        &tx,
        operator.id,
        PlatformRole::Operator,
        UserStatus::Active,
        &keycompute_db::AuditContext {
            actor_user_id: root.id,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: PlatformRole::Root,
            actor_tenant_role: None,
            request_id: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let token = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            operator.id,
            Some(f.tenant.id),
            operator.token_version + 1,
            Some(f.tenant.authz_version),
            Some(1),
            3600,
        )
        .unwrap();
    let (status, _) = f
        .request(
            Method::GET,
            format!("/api/v1/tenants/{}", f.tenant.id),
            Some(&token),
            None,
        )
        .await;
    assert!(matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
    ));
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn denied_member_command_keeps_cache_and_successful_change_invalidates_old_token() {
    let mut f = Fixture::new().await;
    let (status, _) = f
        .request(
            Method::GET,
            "/api/v1/usage/stats",
            Some(&f.member_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let entries = f.state.display_cache.metrics()["entries"].as_u64().unwrap();
    assert!(entries > 0);
    let path = format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.member.id);
    let body = json!({"expected_authz_version":1,"tenant_role":"admin"});
    let (status, _) = f
        .request(
            Method::PATCH,
            &path,
            Some(&f.member_token),
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(f.state.display_cache.metrics()["entries"], entries);
    let (status, _) = f
        .request(Method::PATCH, &path, Some(&f.owner_token), Some(body))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(f.state.display_cache.metrics()["entries"], 0);
    let (status, _) = f
        .request(
            Method::GET,
            "/api/v1/usage/stats",
            Some(&f.member_token),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn membership_change_rolls_back_when_audit_insert_fails() {
    let mut f = Fixture::new().await;
    let name = format!("control_audit_{}", Uuid::new_v4().simple());
    f.db.execute_unprepared(&format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.actor_user_id='{}' AND NEW.action='membership.update' THEN \
         RAISE EXCEPTION 'isolated audit failure'; END IF; RETURN NEW; END $$; \
         CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events \
         FOR EACH ROW EXECUTE FUNCTION {name}();",
        f.owner.id,
    ))
    .await
    .unwrap();
    let (status, _) = f
        .request(
            Method::PATCH,
            format!("/api/v1/tenants/{}/members/{}", f.tenant.id, f.member.id),
            Some(&f.owner_token),
            Some(json!({
                "expected_authz_version":1,"tenant_role":"admin"
            })),
        )
        .await;
    f.db.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    assert!(
        status.is_server_error(),
        "audit failure must reject the command: {status}"
    );
    let member = TenantMembership::find(&f.db, f.tenant.id, f.member.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(member.tenant_role, "member");
    assert_eq!(member.authz_version, 1);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn queued_member_change_revalidates_administrator_after_demotion() {
    let mut f = Fixture::new().await;
    promote_member_to_admin(&f.db, f.tenant.id, f.member.id).await;
    let admin_token = token(&f.state, &f.db, &f.tenant, &f.member).await;
    let target = create_test_user(&f.db, f.tenant.id, "queued-target", &f.run).await;
    let blocker = f.db.begin().await.unwrap();
    blocker
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let blocker_pid: i32 = blocker
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid()".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let command = f.request(
        Method::PATCH,
        format!("/api/v1/tenants/{}/members/{}", f.tenant.id, target.id),
        Some(&admin_token),
        Some(json!({"expected_authz_version":1,"tenant_role":"admin"})),
    );
    let demotion = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let waiting: bool = f.db.query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)))",
                    [blocker_pid.into()],
                )).await.unwrap().unwrap().try_get_by_index(0).unwrap();
                if waiting { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.expect("request must reach the real authority lock");
        blocker.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='member' WHERE tenant_id=$1 AND user_id=$2",
            [f.tenant.id.into(), f.member.id.into()],
        )).await.unwrap();
        blocker.commit().await.unwrap();
    };
    let ((status, _), ()) = tokio::join!(command, demotion);
    assert_eq!(status, StatusCode::CONFLICT);
    let untouched = TenantMembership::find(&f.db, f.tenant.id, target.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(untouched.tenant_role, "member");
    assert_eq!(untouched.authz_version, 1);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn bare_tenant_routes_reject_inference_keys_and_keep_personal_authority_narrow() {
    let mut f = Fixture::new().await;
    let raw = keycompute_auth::ProduceAiKeyValidator::generate_key();
    integration_tests::db::create_test_api_key(
        &f.db,
        &keycompute_db::CreateProduceAiKeyRequest {
            tenant_id: f.tenant.id,
            user_id: f.owner.id,
            name: "bare-control-test".into(),
            produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&raw),
            produce_ai_key_preview: "test-only".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let bare = keycompute_server::handlers::tenant_control::router().with_state(f.state.clone());
    let path = format!("/api/v1/tenants/{}/members", f.tenant.id);
    for (credential, expected) in [
        (raw.as_str(), StatusCode::FORBIDDEN),
        (f.member_token.as_str(), StatusCode::FORBIDDEN),
        (f.owner_token.as_str(), StatusCode::OK),
    ] {
        let response = bare
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&path)
                    .header("authorization", format!("Bearer {credential}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    f.guard.cleanup().await.unwrap();
}
