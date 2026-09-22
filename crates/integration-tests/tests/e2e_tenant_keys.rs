//! Production HTTP/SQL checks for owner-only key provisioning and tenant metadata.
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use chrono::{Duration, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{
        TenantActor, TestDataGuard, create_test_api_key, create_test_pool, create_test_tenant,
        create_test_user,
    },
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::models::{
    key_issuance::{self, KeyIssuanceIntent},
    tenant_control::TenantAuthzSnapshot,
};
use keycompute_db::{
    AuditContext, CreateProduceAiKeyRequest, DbRouter, ProduceAiKey, Tenant, TenantMembership, User,
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, TenantScope};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, FromQueryResult, Statement, TransactionTrait,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
struct Fixture {
    db: DatabaseConnection,
    state: AppState,
    guard: TestDataGuard,
    run: String,
    a: Tenant,
    b: Tenant,
    owner: TenantActor,
    peer: TenantActor,
    foreign: TenantActor,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "issuance-a", &run).await;
        let b = create_test_tenant(&db, "issuance-b", &run).await;
        let owner = create_test_user(&db, a.id, "issuance-owner", &run).await;
        let peer = create_test_user(&db, a.id, "issuance-peer", &run).await;
        let foreign = create_test_user(&db, b.id, "issuance-foreign", &run).await;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        Self {
            db,
            state,
            guard,
            run,
            a,
            b,
            owner,
            peer,
            foreign,
        }
    }
    async fn token(&self, tenant: Uuid, user: Uuid) -> String {
        let u = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        let global = self
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(user, None, u.token_version, None, None, 3600)
            .unwrap();
        let auth = self.state.auth.verify_token(&global).await.unwrap();
        self.state
            .auth
            .select_tenant(&auth, Some(tenant))
            .await
            .unwrap()
            .access_token
    }
    async fn authority(
        &self,
        tenant: Uuid,
        user: Uuid,
    ) -> (TenantScope, TenantAuthzSnapshot, AuditContext) {
        let t = Tenant::find_by_id(&self.db, tenant).await.unwrap().unwrap();
        let u = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        let m = TenantMembership::find_any(&self.db, tenant, user)
            .await
            .unwrap()
            .unwrap();
        let role = m.tenant_role().unwrap();
        (
            TenantScope::checked(tenant, user, role).unwrap(),
            TenantAuthzSnapshot {
                token_version: u.token_version,
                tenant_authz_version: t.authz_version,
                membership_authz_version: m.authz_version,
            },
            AuditContext {
                actor_user_id: user,
                credential_kind: CredentialKind::Jwt,
                actor_platform_role: u.platform_role().unwrap(),
                actor_tenant_role: Some(role),
                request_id: Some(Uuid::new_v4()),
            },
        )
    }
    async fn issue(&self) -> KeyIssuanceIntent {
        let (scope, snapshot, actor) = self.authority(self.a.id, self.a.owner_user_id).await;
        key_issuance::request_new(
            &self.db,
            scope,
            snapshot,
            self.owner.id,
            "Owner only",
            None,
            &actor,
        )
        .await
        .unwrap()
        .0
    }
    async fn key(&self, tenant: Uuid, owner: Uuid) -> (ProduceAiKey, String) {
        let raw = ProduceAiKeyValidator::generate_key();
        let saved = create_test_api_key(
            &self.db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant,
                user_id: owner,
                name: "Existing key".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&raw),
                produce_ai_key_preview: "sk-test****".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        (saved, raw)
    }
}
async fn request(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value, HeaderMap) {
    let response = create_router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(if body.is_null() {
                    Body::empty()
                } else {
                    Body::from(body.to_string())
                })
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
        headers,
    )
}
fn id(value: &Value) -> Uuid {
    Uuid::parse_str(value.as_str().unwrap()).unwrap()
}
fn no_secrets(value: &Value) {
    let text = value.to_string();
    for name in [
        "produce_ai_key_hash",
        "secret",
        "requested_by_token_version",
        "owner_authz_version",
        "tenant_authz_version",
    ] {
        assert!(!text.contains(name), "sensitive field exposed: {name}");
    }
}
#[tokio::test]
async fn administrator_requests_but_only_the_owner_receives_a_noncacheable_one_time_key() {
    let mut f = Fixture::new().await;
    let admin = f.token(f.a.id, f.a.owner_user_id).await;
    let owner = f.token(f.a.id, f.owner.id).await;
    let base = format!("/api/v1/tenants/{}/keys", f.a.id);
    let (status, body, _) = request(
        &f.state,
        "POST",
        &format!("{base}/issuance"),
        &admin,
        json!({"owner_user_id":f.owner.id,"name":"Owned key"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    no_secrets(&body);
    assert_eq!(body["message"], "awaiting_owner_claim");
    let intent = id(&body["intent"]["id"]);
    let claim = format!("/api/v1/me/key-issuance/{intent}/claim");
    assert_eq!(
        request(&f.state, "POST", &claim, &admin, Value::Null)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, listed, _) = request(
        &f.state,
        "GET",
        "/api/v1/me/key-issuance",
        &owner,
        Value::Null,
    )
    .await;
    assert_eq!(listed["total"], 1);
    no_secrets(&listed);
    let (status, claimed, headers) = request(&f.state, "POST", &claim, &owner, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert_eq!(headers["pragma"], "no-cache");
    let raw = claimed["key"].as_str().unwrap();
    assert!(ProduceAiKeyValidator::is_valid_format(raw));
    let authenticated = f.state.auth.verify_api_key(raw).await.unwrap();
    assert_eq!(authenticated.user_id, f.owner.id);
    assert_eq!(authenticated.platform_role, PlatformRole::None);
    assert_eq!(authenticated.credential_kind, CredentialKind::ApiKey);
    let (_, keys, _) = request(&f.state, "GET", &base, &admin, Value::Null).await;
    assert_eq!(keys["total"], 1);
    no_secrets(&keys);
    assert!(!keys.to_string().contains(raw));
    assert_eq!(keys["keys"][0]["owner_user_id"], f.owner.id.to_string());
    assert_eq!(
        request(&f.state, "POST", &claim, &owner, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let saved = KeyIssuanceIntent::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT * FROM tenant_key_issuance_intents WHERE tenant_id=$1 AND id=$2",
        [f.a.id.into(), intent.into()],
    ))
    .one(&f.db)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(saved.status, "claimed");
    assert_eq!(saved.created_key_id, Some(id(&claimed["key_id"])));
    assert!(!format!("{saved:?}").contains(raw));
    let audit=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT actor_user_id,request_id,metadata FROM tenant_audit_events WHERE tenant_id=$1 AND action='key_issuance.claim' AND resource_id=$2",
        [f.a.id.into(),intent.to_string().into()])).await.unwrap().unwrap();
    assert_eq!(audit.try_get_by_index::<Uuid>(0).unwrap(), f.owner.id);
    assert_eq!(
        audit.try_get_by_index::<Uuid>(1).unwrap().to_string(),
        headers["x-request-id"].to_str().unwrap()
    );
    assert!(
        !audit
            .try_get_by_index::<Value>(2)
            .unwrap()
            .to_string()
            .contains(raw)
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn two_claims_of_one_new_issuance_return_exactly_one_credential() {
    let mut f = Fixture::new().await;
    let intent = f.issue().await;
    let token = f.token(f.a.id, f.owner.id).await;
    let path = format!("/api/v1/me/key-issuance/{}/claim", intent.id);
    let (a, b) = tokio::join!(
        request(&f.state, "POST", &path, &token, Value::Null),
        request(&f.state, "POST", &path, &token, Value::Null)
    );
    let replies = [a, b];
    assert_eq!(replies.iter().filter(|r| r.0 == StatusCode::OK).count(), 1);
    assert_eq!(
        replies
            .iter()
            .filter(|r| r.0 == StatusCode::CONFLICT)
            .count(),
        1
    );
    assert!(
        replies
            .iter()
            .find(|r| r.0 == StatusCode::CONFLICT)
            .unwrap()
            .1
            .get("key")
            .is_none()
    );
    let count =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) FROM produce_ai_keys WHERE tenant_id=$1 AND user_id=$2",
            [f.a.id.into(), f.owner.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count.try_get_by_index::<i64>(0).unwrap(), 1);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn members_can_decline_their_own_issuance_but_not_a_peers_request() {
    let mut f = Fixture::new().await;
    let intent = f.issue().await;
    let owner = f.token(f.a.id, f.owner.id).await;
    let peer = f.token(f.a.id, f.peer.id).await;
    let path = format!("/api/v1/me/key-issuance/{}/decline", intent.id);
    assert_eq!(
        request(&f.state, "POST", &path, &peer, Value::Null).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, value, _) = request(&f.state, "POST", &path, &owner, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["intent"]["status"], "cancelled");
    assert_eq!(
        request(
            &f.state,
            "POST",
            &path.replace("decline", "claim"),
            &owner,
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn tenant_key_metadata_enforces_scope_strict_payloads_and_optimistic_revisions() {
    let mut f = Fixture::new().await;
    let (key, _) = f.key(f.a.id, f.owner.id).await;
    let (foreign, _) = f.key(f.b.id, f.foreign.id).await;
    let admin = f.token(f.a.id, f.a.owner_user_id).await;
    let member = f.token(f.a.id, f.owner.id).await;
    let path = format!("/api/v1/tenants/{}/keys/{}", f.a.id, key.id);
    assert_eq!(
        request(&f.state, "GET", &path, &member, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &f.state,
            "GET",
            &format!("/api/v1/tenants/{}/keys/{}", f.a.id, foreign.id),
            &admin,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &f.state,
            "GET",
            &format!("/api/v1/tenants/{}/keys", f.b.id),
            &admin,
            Value::Null
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let future = Utc::now() + Duration::hours(2);
    let (status, changed, _) = request(
        &f.state,
        "PATCH",
        &path,
        &admin,
        json!({"expected_updated_at":key.updated_at,"name":"  renamed  ","expires_at":future}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["name"], "renamed");
    assert_eq!(changed["owner_user_id"], f.owner.id.to_string());
    no_secrets(&changed);
    assert_eq!(
        request(
            &f.state,
            "PATCH",
            &path,
            &admin,
            json!({"expected_updated_at":key.updated_at,"name":"stale"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (status, cleared, _) = request(
        &f.state,
        "PATCH",
        &path,
        &admin,
        json!({"expected_updated_at":changed["updated_at"],"expires_at":null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(cleared["expires_at"].is_null());
    assert_ne!(cleared["updated_at"], changed["updated_at"]);
    for extra in [
        "tenant_id",
        "user_id",
        "owner_user_id",
        "platform_role",
        "produce_ai_key_hash",
    ] {
        let mut body = json!({"expected_updated_at":cleared["updated_at"],"name":"forbidden"});
        body[extra] = json!(Uuid::new_v4());
        assert_eq!(
            request(&f.state, "PATCH", &path, &admin, body).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    assert_eq!(request(&f.state,"PATCH",&path,&admin,json!({"expected_updated_at":cleared["updated_at"],"expires_at":Utc::now()-Duration::seconds(1)})).await.0,StatusCode::BAD_REQUEST);
    let (_, personal, _) = request(&f.state, "GET", "/api/v1/keys", &admin, Value::Null).await;
    assert!(!personal.to_string().contains(&key.id.to_string()));
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn rotation_request_is_inert_until_its_owner_claims() {
    let mut f = Fixture::new().await;
    let (old, old_raw) = f.key(f.a.id, f.owner.id).await;
    let admin = f.token(f.a.id, f.a.owner_user_id).await;
    let owner = f.token(f.a.id, f.owner.id).await;
    let rotate = format!("/api/v1/tenants/{}/keys/{}/rotate", f.a.id, old.id);
    let (status, created, _) = request(
        &f.state,
        "POST",
        &rotate,
        &admin,
        json!({"name":"replacement"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{created}");
    no_secrets(&created);
    assert!(f.state.auth.verify_api_key(&old_raw).await.is_ok());
    let (status, repeated, _) = request(
        &f.state,
        "POST",
        &rotate,
        &admin,
        json!({"name":"replacement"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated["outcome"], "already_pending");
    assert_eq!(created["intent"]["id"], repeated["intent"]["id"]);
    assert_eq!(
        request(
            &f.state,
            "POST",
            &rotate,
            &admin,
            json!({"name":"different"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let claim = format!(
        "/api/v1/me/key-issuance/{}/claim",
        id(&created["intent"]["id"])
    );
    let (status, claimed, _) = request(&f.state, "POST", &claim, &owner, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(id(&claimed["key_id"]), old.id);
    assert!(f.state.auth.verify_api_key(&old_raw).await.is_err());
    assert_eq!(
        f.state
            .auth
            .verify_api_key(claimed["key"].as_str().unwrap())
            .await
            .unwrap()
            .user_id,
        f.owner.id
    );
    let original = ProduceAiKey::find_by_hash(&f.db, &ProduceAiKeyValidator::hash_key(&old_raw))
        .await
        .unwrap()
        .unwrap();
    assert!(original.revoked);
    assert_eq!(original.tenant_id, f.a.id);
    assert_eq!(original.user_id, f.owner.id);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn regranted_administrator_or_owner_role_does_not_reanimate_old_issuance() {
    let mut f = Fixture::new().await;
    let delegate = create_test_user(&f.db, f.a.id, "issuance-delegate", &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), delegate.id.into()],
    ))
    .await
    .unwrap();
    let (scope, snap, actor) = f.authority(f.a.id, delegate.id).await;
    let intent = key_issuance::request_new(
        &f.db,
        scope,
        snap,
        f.owner.id,
        "requester test",
        None,
        &actor,
    )
    .await
    .unwrap()
    .0;
    let owner = f.token(f.a.id, f.owner.id).await;
    let claim = format!("/api/v1/me/key-issuance/{}/claim", intent.id);
    for role in ["member", "admin"] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role=$3 WHERE tenant_id=$1 AND user_id=$2",
            [f.a.id.into(), delegate.id.into(), role.into()],
        ))
        .await
        .unwrap();
        assert_eq!(
            request(&f.state, "POST", &claim, &owner, Value::Null)
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    let fresh = f.issue().await;
    for status in ["suspended", "active"] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET status=$3 WHERE tenant_id=$1 AND user_id=$2",
            [f.a.id.into(), f.owner.id.into(), status.into()],
        ))
        .await
        .unwrap();
    }
    let current = f.token(f.a.id, f.owner.id).await;
    assert_eq!(
        request(
            &f.state,
            "POST",
            &format!("/api/v1/me/key-issuance/{}/claim", fresh.id),
            &current,
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        ProduceAiKey::count_owned(&f.db, f.owner.scope(), true)
            .await
            .unwrap(),
        0
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn credential_and_snapshot_checks_cannot_be_supplied_by_request_payloads() {
    let mut f = Fixture::new().await;
    let (scope, snapshot, actor) = f.authority(f.a.id, f.a.owner_user_id).await;
    for kind in [
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        let forged = AuditContext {
            credential_kind: kind,
            ..actor
        };
        assert!(
            key_issuance::request_new(&f.db, scope, snapshot, f.owner.id, "denied", None, &forged)
                .await
                .is_err()
        );
    }
    let wrong_actor = AuditContext {
        actor_user_id: f.peer.id,
        ..actor
    };
    assert!(
        key_issuance::request_new(
            &f.db,
            scope,
            snapshot,
            f.owner.id,
            "denied",
            None,
            &wrong_actor
        )
        .await
        .is_err()
    );
    let stale = TenantAuthzSnapshot {
        membership_authz_version: snapshot.membership_authz_version + 1,
        ..snapshot
    };
    assert!(
        key_issuance::request_new(&f.db, scope, stale, f.owner.id, "denied", None, &actor)
            .await
            .is_err()
    );
    assert!(
        key_issuance::request_new(
            &f.db,
            scope,
            snapshot,
            f.foreign.id,
            "foreign",
            None,
            &actor
        )
        .await
        .is_err()
    );
    assert_eq!(
        key_issuance::count_in_tenant(&f.db, scope, None)
            .await
            .unwrap(),
        0
    );
    let (_, inference) = f.key(f.a.id, f.owner.id).await;
    let base = format!("/api/v1/tenants/{}/keys", f.a.id);
    assert_eq!(
        request(&f.state, "GET", &base, &inference, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let owner = f.token(f.a.id, f.owner.id).await;
    assert_eq!(
        request(
            &f.state,
            "GET",
            "/api/v1/me/key-issuance?owner_user_id=00000000-0000-0000-0000-000000000000",
            &owner,
            Value::Null
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let root = User::find_by_email(&f.db, "tenant-test-root@fixture.invalid")
        .await
        .unwrap()
        .unwrap();
    let global = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(root.id, None, root.token_version, None, None, 3600)
        .unwrap();
    assert_eq!(
        request(&f.state, "GET", &base, &global, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn failed_claim_audit_rolls_back_the_entire_rotation_savepoint() {
    let mut f = Fixture::new().await;
    let (old, old_raw) = f.key(f.a.id, f.owner.id).await;
    let (admin, admin_version, admin_actor) = f.authority(f.a.id, f.a.owner_user_id).await;
    let intent = key_issuance::request_rotation(
        &f.db,
        admin,
        admin_version,
        old.id,
        "audited",
        None,
        &admin_actor,
    )
    .await
    .unwrap()
    .0;
    let (owner, version, actor) = f.authority(f.a.id, f.owner.id).await;
    let outer = f.db.begin().await.unwrap();
    outer
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    outer.execute_unprepared(&format!(
        "CREATE FUNCTION pg_temp.reject_keypool_claim() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'keypool audit unavailable'; END $$;
         CREATE TRIGGER keypool_claim_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW
         WHEN (NEW.action='key_issuance.claim' AND NEW.request_id='{}'::uuid) EXECUTE FUNCTION pg_temp.reject_keypool_claim()",actor.request_id.unwrap()
    )).await.unwrap();
    let error = key_issuance::claim(&outer, owner, version, intent.id, &actor)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("keypool audit unavailable"));
    assert_eq!(
        ProduceAiKey::count_owned(&outer, owner, true)
            .await
            .unwrap(),
        1
    );
    let retained = ProduceAiKey::find_by_hash(&outer, &ProduceAiKeyValidator::hash_key(&old_raw))
        .await
        .unwrap()
        .unwrap();
    assert!(!retained.revoked);
    let state=outer.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT status,created_key_id FROM tenant_key_issuance_intents WHERE tenant_id=$1 AND owner_user_id=$2 AND id=$3",
        [f.a.id.into(),f.owner.id.into(),intent.id.into()])).await.unwrap().unwrap();
    assert_eq!(state.try_get_by_index::<String>(0).unwrap(), "pending");
    assert!(state.try_get_by_index::<Option<Uuid>>(1).unwrap().is_none());
    outer
        .execute_unprepared("DROP TRIGGER keypool_claim_fault ON tenant_audit_events")
        .await
        .unwrap();
    outer.commit().await.unwrap();
    assert!(f.state.auth.verify_api_key(&old_raw).await.is_ok());
    f.guard.cleanup().await.unwrap();
}

async fn rejected_sql(db: &DatabaseConnection, statement: Statement, expected: &str) {
    let tx = db.begin().await.unwrap();
    let result = tx.execute(statement).await;
    tx.rollback().await.unwrap();
    assert!(result.unwrap_err().to_string().contains(expected));
}
#[tokio::test]
async fn database_rejects_intent_reassignment_foreign_keys_and_terminal_replay() {
    let mut f = Fixture::new().await;
    let intent = f.issue().await;
    rejected_sql(
        &f.db,
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_key_issuance_intents SET owner_user_id=$3 WHERE tenant_id=$1 AND id=$2",
            [f.a.id.into(), intent.id.into(), f.peer.id.into()],
        ),
        "key issuance identity is immutable",
    )
    .await;
    let (peer_key, _) = f.key(f.a.id, f.peer.id).await;
    rejected_sql(&f.db,Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE tenant_key_issuance_intents SET status='claimed',claimed_at=clock_timestamp(),created_key_id=$3 WHERE tenant_id=$1 AND id=$2",
        [f.a.id.into(),intent.id.into(),peer_key.id.into()]),"claim requires a new owner-scoped key").await;
    let (scope, version, actor) = f.authority(f.a.id, f.owner.id).await;
    let claimed = key_issuance::claim(&f.db, scope, version, intent.id, &actor)
        .await
        .unwrap();
    assert!(!format!("{claimed:?}").contains(&claimed.secret));
    assert!(
        !serde_json::to_string(&claimed)
            .unwrap()
            .contains(&claimed.secret)
    );
    rejected_sql(&f.db,Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE tenant_key_issuance_intents SET status='pending',claimed_at=NULL,created_key_id=NULL WHERE tenant_id=$1 AND id=$2",
        [f.a.id.into(),intent.id.into()]),"terminal key issuance is immutable").await;
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn expired_issuance_never_creates_a_credential() {
    let mut f = Fixture::new().await;
    let original = f.issue().await;
    let expired = Uuid::new_v4();
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO tenant_key_issuance_intents
         (id,tenant_id,owner_user_id,requested_by_user_id,requested_name,status,expires_at,created_at,
          requested_by_token_version,requested_by_authz_version,owner_token_version,owner_authz_version,tenant_authz_version)
         SELECT $3,tenant_id,owner_user_id,requested_by_user_id,'Expired fixture','pending',
          clock_timestamp()-INTERVAL '1 minute',clock_timestamp()-INTERVAL '1 hour',
          requested_by_token_version,requested_by_authz_version,owner_token_version,owner_authz_version,tenant_authz_version
         FROM tenant_key_issuance_intents WHERE tenant_id=$1 AND id=$2",
        [f.a.id.into(),original.id.into(),expired.into()])).await.unwrap();
    let owner = f.token(f.a.id, f.owner.id).await;
    let (status, body, _) = request(
        &f.state,
        "POST",
        &format!("/api/v1/me/key-issuance/{expired}/claim"),
        &owner,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.get("key").is_none());
    let (_, list, _) = request(
        &f.state,
        "GET",
        "/api/v1/me/key-issuance",
        &owner,
        Value::Null,
    )
    .await;
    assert_eq!(list["total"], 1);
    assert!(!list.to_string().contains(&expired.to_string()));
    assert_eq!(
        ProduceAiKey::count_owned(&f.db, f.owner.scope(), true)
            .await
            .unwrap(),
        0
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn committed_key_claim_and_admin_revocation_refresh_cached_owner_dashboard() {
    let mut f = Fixture::new().await;
    let intent = f.issue().await;
    let owner = f.token(f.a.id, f.owner.id).await;
    let admin = f.token(f.a.id, f.a.owner_user_id).await;
    let dashboard = "/api/v1/dashboard/overview";
    let (status, initial, _) = request(&f.state, "GET", dashboard, &owner, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["active_key_count"], 0);
    let (_, cached, _) = request(&f.state, "GET", dashboard, &owner, Value::Null).await;
    assert_eq!(cached["active_key_count"], 0);
    assert!(f.state.display_cache.metrics()["hit"].as_u64().unwrap() > 0);
    let peer = f.token(f.a.id, f.peer.id).await;
    let origin_before = f.state.display_cache.metrics()["origin"].clone();
    assert_eq!(
        request(
            &f.state,
            "POST",
            &format!("/api/v1/me/key-issuance/{}/claim", intent.id),
            &peer,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    request(&f.state, "GET", dashboard, &owner, Value::Null).await;
    assert_eq!(
        f.state.display_cache.metrics()["origin"],
        origin_before,
        "denied claims must not flush cached snapshots"
    );
    let (status, claimed, _) = request(
        &f.state,
        "POST",
        &format!("/api/v1/me/key-issuance/{}/claim", intent.id),
        &owner,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, current, _) = request(&f.state, "GET", dashboard, &owner, Value::Null).await;
    assert_eq!(
        current["active_key_count"], 1,
        "committed claim must fence the cached owner view"
    );
    let key_id = id(&claimed["key_id"]);
    let key_path = format!("/api/v1/tenants/{}/keys/{key_id}", f.a.id);
    let (_, metadata, _) = request(&f.state, "GET", &key_path, &admin, Value::Null).await;
    let (status, value, _) = request(
        &f.state,
        "PATCH",
        &key_path,
        &admin,
        json!({"expected_updated_at":metadata["updated_at"],"name":"Updated dashboard key"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let (_, current, _) = request(&f.state, "GET", dashboard, &owner, Value::Null).await;
    assert_eq!(current["active_keys"][0]["name"], "Updated dashboard key");

    let (status, _, _) = request(
        &f.state,
        "POST",
        &format!("/api/v1/tenants/{}/keys/{key_id}/revoke", f.a.id),
        &admin,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, current, _) = request(&f.state, "GET", dashboard, &owner, Value::Null).await;
    assert_eq!(
        current["active_key_count"], 0,
        "committed revocation must fence the cached owner view"
    );
    f.guard.cleanup().await.unwrap();
}
