//! SQL ownership, current authority, auditing and HTTP key-management boundaries.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::{
    common::generate_test_id,
    db::{
        TenantActor, TestDataGuard, create_test_api_key, create_test_pool, create_test_tenant,
        create_test_user,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::models::api_key::KeyRemoval;
use keycompute_db::{
    AuditContext, CreateProduceAiKeyRequest, CreateTenantMembershipRequest, DbRouter, ProduceAiKey,
    Tenant, TenantMembership, User,
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
fn actor(scope: TenantScope) -> AuditContext {
    AuditContext {
        actor_user_id: scope.user_id(),
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: Some(scope.tenant_role()),
        request_id: Some(Uuid::new_v4()),
    }
}
fn request(t: Uuid, u: Uuid) -> CreateProduceAiKeyRequest {
    CreateProduceAiKeyRequest {
        tenant_id: t,
        user_id: u,
        name: "scope fixture".into(),
        produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&ProduceAiKeyValidator::generate_key()),
        produce_ai_key_preview: "sk-test****".into(),
        expires_at: None,
    }
}
struct Fixture {
    db: DatabaseConnection,
    a: Tenant,
    b: Tenant,
    user: TenantActor,
    peer: TenantActor,
    own: ProduceAiKey,
    other: ProduceAiKey,
    foreign: ProduceAiKey,
    guard: TestDataGuard,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "key-scope-a", &run).await;
        let b = create_test_tenant(&db, "key-scope-b", &run).await;
        let user = create_test_user(&db, a.id, "key-owner", &run).await;
        let peer = create_test_user(&db, a.id, "key-peer", &run).await;
        let tx = db.begin().await.unwrap();
        TenantMembership::create(
            &tx,
            &CreateTenantMembershipRequest {
                tenant_id: b.id,
                user_id: user.id,
                tenant_role: TenantRole::Member,
            },
            &actor(admin(&b)),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let own = create_test_api_key(&db, &request(a.id, user.id))
            .await
            .unwrap();
        let other = create_test_api_key(&db, &request(a.id, peer.id))
            .await
            .unwrap();
        let foreign = create_test_api_key(&db, &request(b.id, user.id))
            .await
            .unwrap();
        Self {
            db,
            a,
            b,
            user,
            peer,
            own,
            other,
            foreign,
            guard,
        }
    }
    async fn audits(&self) -> i64 {
        self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*) FROM tenant_audit_events WHERE tenant_id=$1 AND resource_type='produce_ai_key'",[self.a.id.into()])).await.unwrap().unwrap().try_get_by_index(0).unwrap()
    }
}
#[tokio::test]
async fn key_queries_bind_tenant_owner_and_current_actor_without_secret_projection() {
    let mut f = Fixture::new().await;
    let personal = ProduceAiKey::list_owned(&f.db, f.user.scope(), true, 100, 0)
        .await
        .unwrap();
    assert_eq!(personal.len(), 1);
    assert_eq!(personal[0].id, f.own.id);
    assert_eq!(
        ProduceAiKey::count_owned(&f.db, f.user.scope(), true)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        ProduceAiKey::count_owned(&f.db, admin(&f.a), true)
            .await
            .unwrap(),
        0
    );
    for id in [f.other.id, f.foreign.id, Uuid::new_v4()] {
        assert!(
            ProduceAiKey::find_owned(&f.db, f.user.scope(), id)
                .await
                .unwrap()
                .is_none()
        );
    }
    let rows = ProduceAiKey::list_in_tenant(&f.db, admin(&f.a), None, true, 100, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        ProduceAiKey::count_in_tenant(&f.db, admin(&f.a), None, true)
            .await
            .unwrap(),
        2
    );
    assert!(
        ProduceAiKey::find_in_tenant(&f.db, admin(&f.a), f.foreign.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        ProduceAiKey::list_in_tenant(&f.db, f.user.scope(), None, true, 100, 0)
            .await
            .is_err()
    );
    let forged = TenantScope::checked(f.a.id, f.b.owner_user_id, TenantRole::Admin).unwrap();
    assert_eq!(
        ProduceAiKey::count_in_tenant(&f.db, forged, None, true)
            .await
            .unwrap(),
        0
    );
    let first = ProduceAiKey::list_in_tenant(&f.db, admin(&f.a), None, true, 1, 0)
        .await
        .unwrap();
    let second = ProduceAiKey::list_in_tenant(&f.db, admin(&f.a), None, true, 1, 1)
        .await
        .unwrap();
    assert_ne!(first[0].id, second[0].id);
    for row in rows {
        let value = serde_json::to_value(row).unwrap();
        for secret in ["produce_ai_key_hash", "key", "password"] {
            assert!(value.get(secret).is_none());
        }
    }
    assert!(
        serde_json::to_value(&f.own)
            .unwrap()
            .get("produce_ai_key_hash")
            .is_none()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn scoped_key_mutations_preserve_owners_and_commit_audit_with_the_change() {
    let mut f = Fixture::new().await;
    let before = f.audits().await;
    for id in [f.other.id, f.foreign.id] {
        assert!(
            ProduceAiKey::remove_owned(&f.db, f.user.scope(), id, &actor(f.user.scope()))
                .await
                .is_err()
        );
    }
    assert!(
        ProduceAiKey::revoke_in_tenant(&f.db, admin(&f.a), f.foreign.id, &actor(admin(&f.a)))
            .await
            .is_err()
    );
    assert_eq!(f.audits().await, before);
    let mut bad = actor(admin(&f.a));
    bad.credential_kind = CredentialKind::ApiKey;
    assert!(
        ProduceAiKey::revoke_in_tenant(&f.db, admin(&f.a), f.other.id, &bad)
            .await
            .is_err()
    );
    assert!(
        ProduceAiKey::create_in_tenant(&f.db, admin(&f.a), &request(f.a.id, f.peer.id), &bad)
            .await
            .is_err()
    );
    assert!(
        ProduceAiKey::create_owned(
            &f.db,
            f.user.scope(),
            &request(f.a.id, f.peer.id),
            &actor(f.user.scope())
        )
        .await
        .is_err()
    );
    assert!(
        ProduceAiKey::create_owned(
            &f.db,
            f.user.scope(),
            &request(f.b.id, f.user.id),
            &actor(f.user.scope())
        )
        .await
        .is_err()
    );
    assert_eq!(f.audits().await, before);
    let result =
        ProduceAiKey::revoke_in_tenant(&f.db, admin(&f.a), f.other.id, &actor(admin(&f.a)))
            .await
            .unwrap();
    let KeyRemoval::Revoked(revoked) = result else {
        panic!("expected revocation")
    };
    assert_eq!(revoked.user_id, f.peer.id);
    assert_eq!(revoked.tenant_id, f.a.id);
    assert!(revoked.revoked);
    assert!(
        !ProduceAiKey::find_by_hash(&f.db, &f.foreign.produce_ai_key_hash)
            .await
            .unwrap()
            .unwrap()
            .revoked
    );
    assert_eq!(f.audits().await, before + 1);
    assert!(
        matches!(ProduceAiKey::remove_in_tenant(&f.db,admin(&f.a),f.other.id,&actor(admin(&f.a))).await.unwrap(),KeyRemoval::Deleted(id) if id==f.other.id)
    );
    assert_eq!(f.audits().await, before + 2);
    assert!(
        ProduceAiKey::find_by_hash(&f.db, &f.other.produce_ai_key_hash)
            .await
            .unwrap()
            .is_none()
    );
    let key = ProduceAiKey::create_in_tenant(
        &f.db,
        admin(&f.a),
        &request(f.a.id, f.peer.id),
        &actor(admin(&f.a)),
    )
    .await
    .unwrap();
    assert_eq!(key.user_id, f.peer.id);
    assert_eq!(f.audits().await, before + 3);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn stale_key_scopes_are_rejected_after_demotion_and_revocation() {
    let mut f = Fixture::new().await;
    let member = TenantMembership::find(&f.db, f.a.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let elevated = TenantMembership::set_role(
        &tx,
        f.a.id,
        f.user.id,
        TenantRole::Admin,
        member.authz_version,
        &actor(admin(&f.a)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let scope = TenantScope::checked(f.a.id, f.user.id, TenantRole::Admin).unwrap();
    assert_eq!(
        ProduceAiKey::count_in_tenant(&f.db, scope, None, true)
            .await
            .unwrap(),
        2
    );
    let tx = f.db.begin().await.unwrap();
    let lowered = TenantMembership::set_role(
        &tx,
        f.a.id,
        f.user.id,
        TenantRole::Member,
        elevated.authz_version,
        &actor(admin(&f.a)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        ProduceAiKey::revoke_in_tenant(&f.db, scope, f.other.id, &actor(scope))
            .await
            .is_err()
    );
    assert!(
        ProduceAiKey::create_in_tenant(&f.db, scope, &request(f.a.id, f.peer.id), &actor(scope))
            .await
            .is_err()
    );
    assert_eq!(
        ProduceAiKey::count_in_tenant(&f.db, scope, None, true)
            .await
            .unwrap(),
        0
    );
    let tx = f.db.begin().await.unwrap();
    TenantMembership::set_status(
        &tx,
        f.a.id,
        f.user.id,
        MembershipStatus::Suspended,
        lowered.authz_version,
        &actor(admin(&f.a)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        ProduceAiKey::create_owned(
            &f.db,
            f.user.scope(),
            &request(f.a.id, f.user.id),
            &actor(f.user.scope())
        )
        .await
        .is_err()
    );
    assert!(
        ProduceAiKey::remove_owned(&f.db, f.user.scope(), f.own.id, &actor(f.user.scope()))
            .await
            .is_err()
    );
    assert_eq!(
        ProduceAiKey::count_owned(&f.db, f.user.scope(), true)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        ProduceAiKey::count_owned(
            &f.db,
            TenantScope::checked(f.b.id, f.user.id, TenantRole::Member).unwrap(),
            true
        )
        .await
        .unwrap(),
        1
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn platform_key_access_requires_real_root_and_explicit_tenant() {
    let mut f = Fixture::new().await;
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let scope = PlatformScope::checked(root.id, PlatformRole::Root).unwrap();
    let audit = AuditContext {
        actor_user_id: root.id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        request_id: Some(Uuid::new_v4()),
    };
    assert_eq!(
        ProduceAiKey::count_platform(&f.db, scope, f.a.id, None, true)
            .await
            .unwrap(),
        2
    );
    assert!(
        ProduceAiKey::find_platform(&f.db, scope, f.a.id, f.foreign.id)
            .await
            .unwrap()
            .is_none()
    );
    let forged = PlatformScope::checked(f.user.id, PlatformRole::Root).unwrap();
    assert_eq!(
        ProduceAiKey::count_platform(&f.db, forged, f.a.id, None, true)
            .await
            .unwrap(),
        0
    );
    assert!(
        ProduceAiKey::count_platform(
            &f.db,
            PlatformScope::checked(root.id, PlatformRole::Operator).unwrap(),
            f.a.id,
            None,
            true
        )
        .await
        .is_err()
    );
    assert!(
        ProduceAiKey::count_platform(&f.db, scope, Uuid::nil(), None, true)
            .await
            .is_err()
    );
    assert!(
        ProduceAiKey::revoke_platform(&f.db, scope, f.a.id, f.own.id, &audit, "")
            .await
            .is_err()
    );
    ProduceAiKey::revoke_platform(
        &f.db,
        scope,
        f.a.id,
        f.own.id,
        &audit,
        "security incident review",
    )
    .await
    .unwrap();
    let after = ProduceAiKey::find_platform(&f.db, scope, f.a.id, f.own.id)
        .await
        .unwrap()
        .unwrap();
    assert!(after.revoked);
    assert_eq!(after.user_id, f.user.id);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn key_http_create_revoke_delete_rejects_foreign_ids_and_inference_credentials() {
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
    for id in [f.other.id, f.foreign.id] {
        let response = create_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/keys/{id}"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let created = create_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/keys")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"name":"roundtrip"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let request_id = Uuid::parse_str(created.headers()["x-request-id"].to_str().unwrap()).unwrap();
    let value: Value =
        serde_json::from_slice(&to_bytes(created.into_body(), 1 << 20).await.unwrap()).unwrap();
    let id = value["key_id"].as_str().unwrap();
    let inference = value["key"].as_str().unwrap();
    let audit = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT request_id,metadata FROM tenant_audit_events WHERE tenant_id=$1 AND resource_id=$2 AND action='key.create'",
        [f.a.id.into(), id.into()])).await.unwrap().unwrap();
    assert_eq!(audit.try_get::<Uuid>("", "request_id").unwrap(), request_id);
    let metadata: Value = audit.try_get("", "metadata").unwrap();
    assert!(!metadata.to_string().contains(inference));
    assert!(
        !metadata
            .to_string()
            .contains(&ProduceAiKeyValidator::hash_key(inference))
    );
    let denied = create_router(state.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/keys/{id}"))
                .header("authorization", format!("Bearer {inference}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    for deleted in [false, true] {
        let response = create_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/keys/{id}"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap())
                .unwrap();
        assert_eq!(body["deleted"], json!(deleted));
        assert!(state.auth.verify_token(inference).await.is_err());
    }
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn failed_key_audit_rolls_back_creation_and_revocation() {
    let mut f = Fixture::new().await;
    let mut ctx = actor(f.user.scope());
    ctx.request_id = Some(Uuid::new_v4());
    let tx = f.db.begin().await.unwrap();
    // The failure injector exists only inside this test transaction. Rolling
    // back the outer transaction removes both trigger and temporary function.
    tx.execute_unprepared(&format!(
        "CREATE FUNCTION pg_temp.reject_test_key_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test audit failure'; END $$;          CREATE TRIGGER kc_test_key_audit_failure BEFORE INSERT ON tenant_audit_events FOR EACH ROW          WHEN (NEW.request_id='{}'::uuid) EXECUTE FUNCTION pg_temp.reject_test_key_audit()",
        ctx.request_id.unwrap()
    )).await.unwrap();
    let req = request(f.a.id, f.user.id);
    assert!(
        ProduceAiKey::create_owned(&tx, f.user.scope(), &req, &ctx)
            .await
            .is_err()
    );
    assert!(
        ProduceAiKey::find_by_hash(&tx, &req.produce_ai_key_hash)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        ProduceAiKey::revoke_owned(&tx, f.user.scope(), f.own.id, &ctx)
            .await
            .is_err()
    );
    assert!(
        !ProduceAiKey::find_by_hash(&tx, &f.own.produce_ai_key_hash)
            .await
            .unwrap()
            .unwrap()
            .revoked
    );
    tx.rollback().await.unwrap();
    f.guard.cleanup().await.unwrap();
}

struct PendingKeyChange(tokio::task::JoinHandle<Result<KeyRemoval, keycompute_db::DbError>>);
impl Drop for PendingKeyChange {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn queued_key_mutation_rechecks_authority_after_concurrent_demotion() {
    let mut f = Fixture::new().await;
    let member = TenantMembership::find(&f.db, f.a.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let elevated = TenantMembership::set_role(
        &tx,
        f.a.id,
        f.user.id,
        TenantRole::Admin,
        member.authz_version,
        &actor(admin(&f.a)),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let stale = TenantScope::checked(f.a.id, f.user.id, TenantRole::Admin).unwrap();
    let gate = f.db.begin().await.unwrap();
    Tenant::find_by_id_for_update(&gate, f.a.id)
        .await
        .unwrap()
        .unwrap();
    let gate_pid: i32 = gate
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid()".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    let db = f.db.clone();
    let target = f.other.id;
    let mut pending = PendingKeyChange(tokio::spawn(async move {
        ProduceAiKey::revoke_in_tenant(&db, stale, target, &actor(stale)).await
    }));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1=ANY(pg_blocking_pids(pid)) AND query LIKE '%FROM tenants WHERE id = $1 FOR SHARE%')",
                [gate_pid.into()])).await.unwrap().unwrap();
            if row.try_get_by_index::<bool>(0).unwrap() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("key mutation must be blocked by the specific tenant gate");
    TenantMembership::set_role(
        &gate,
        f.a.id,
        f.user.id,
        TenantRole::Member,
        elevated.authz_version,
        &actor(admin(&f.a)),
    )
    .await
    .unwrap();
    gate.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), &mut pending.0)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.is_err(),
        "old administrator snapshot must not authorize the write"
    );
    assert!(
        !ProduceAiKey::find_by_hash(&f.db, &f.other.produce_ai_key_hash)
            .await
            .unwrap()
            .unwrap()
            .revoked
    );
    f.guard.cleanup().await.unwrap();
}
