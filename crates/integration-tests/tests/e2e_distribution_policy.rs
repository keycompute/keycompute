//! Actual PostgreSQL and router acceptance for scoped, audited policy mutations.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::post,
};
use chrono::{DateTime, Utc};
use integration_tests::{
    common::generate_test_id,
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::models::{distribution_policy as dao, tenant_control::TenantAuthzSnapshot};
use keycompute_db::{
    AuditContext, BeneficiaryScope, CreateDistributionRuleRequest, DbRouter, Tenant,
    TenantDistributionRule, TenantMembership, User,
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, TenantRole, TenantScope};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    member: User,
    root: User,
    run: String,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "policy-a", &run).await;
        let b = create_test_tenant(&db, "policy-b", &run).await;
        let member = create_test_user(&db, a.id, "policy-member", &run)
            .await
            .user;
        let mut root = create_test_user(&db, b.id, "policy-root", &run).await.user;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [root.id.into()],
        ))
        .await
        .unwrap();
        root = User::find_by_id(&db, root.id).await.unwrap().unwrap();
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        Self {
            db,
            guard,
            state,
            a,
            b,
            member,
            root,
            run,
        }
    }
    async fn token(&self, u: Uuid, t: Option<Uuid>) -> String {
        let u = User::find_by_id(&self.db, u).await.unwrap().unwrap();
        let global = self
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(u.id, None, u.token_version, None, None, 3600)
            .unwrap();
        let ctx = self.state.auth.verify_token(&global).await.unwrap();
        self.state
            .auth
            .select_tenant(&ctx, t)
            .await
            .unwrap()
            .access_token
    }
    fn path(&self, t: Uuid) -> String {
        format!("/api/v1/tenants/{t}/distribution/rules")
    }
    fn input(&self, t: Uuid) -> CreateDistributionRuleRequest {
        CreateDistributionRuleRequest {
            tenant_id: t,
            beneficiary_scope: BeneficiaryScope::Everyone,
            beneficiary_id: None,
            name: format!("租户规则_{}", self.run),
            description: Some("private-description-marker".into()),
            commission_rate: "0.1".parse().unwrap(),
            priority: Some(1),
            effective_from: None,
            effective_until: None,
        }
    }
    async fn authority(&self, t: Uuid, u: Uuid) -> (dao::PolicyActor, AuditContext) {
        let tenant = Tenant::find_by_id(&self.db, t).await.unwrap().unwrap();
        let user = User::find_by_id(&self.db, u).await.unwrap().unwrap();
        let member = TenantMembership::find(&self.db, t, u)
            .await
            .unwrap()
            .unwrap();
        let who = dao::PolicyActor::tenant(
            TenantScope::checked(t, u, TenantRole::Admin).unwrap(),
            TenantAuthzSnapshot {
                token_version: user.token_version,
                tenant_authz_version: tenant.authz_version,
                membership_authz_version: member.authz_version,
            },
        )
        .unwrap();
        let audit = AuditContext {
            actor_user_id: u,
            credential_kind: CredentialKind::Jwt,
            actor_platform_role: user.platform_role().unwrap(),
            actor_tenant_role: Some(TenantRole::Admin),
            request_id: Some(Uuid::new_v4()),
        };
        (who, audit)
    }
    async fn create(&self, t: Uuid) -> TenantDistributionRule {
        let tenant = Tenant::find_by_id(&self.db, t).await.unwrap().unwrap();
        let (who, audit) = self.authority(t, tenant.owner_user_id).await;
        dao::create(
            &self.db,
            who,
            &audit,
            &self.input(t),
            "test policy creation",
        )
        .await
        .unwrap()
    }
}
async fn request(
    app: Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value, String) {
    let r = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("x-request-id", "a1111111-1111-4111-8111-111111111111")
                .body(Body::from(if body.is_null() {
                    String::new()
                } else {
                    body.to_string()
                }))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let id = r
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let bytes = to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        id,
    )
}
fn create_body() -> Value {
    json!({"name":"租户规则_%","description":"private-description-marker","commission_rate":"0.1250","priority":1,"reason":"configure allocation"})
}
#[tokio::test]
async fn real_tenant_crud_keeps_identity_revision_audit_and_nullable_fields() {
    let mut f = Fixture::new().await;
    let token = f.token(f.a.owner_user_id, Some(f.a.id)).await;
    let base = f.path(f.a.id);
    let (s, row, request_id) = request(
        create_router(f.state.clone()),
        "POST",
        &base,
        &token,
        create_body(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{row}");
    assert_ne!(request_id, "a1111111-1111-4111-8111-111111111111");
    let id = row["id"].as_str().unwrap();
    let path = format!("{base}/{id}");
    let (s,next,_)=request(create_router(f.state.clone()),"PATCH",&path,&token,json!({"expected_updated_at":row["updated_at"],"commission_rate":"0.25","description":null,"reason":"change allocation"})).await;
    assert_eq!(s, StatusCode::OK, "{next}");
    assert!(next["description"].is_null());
    assert_eq!(next["tenant_id"], row["tenant_id"]);
    assert_eq!(next["beneficiary_id"], row["beneficiary_id"]);
    assert!(next["updated_at"].as_str().unwrap() > row["updated_at"].as_str().unwrap());
    assert_eq!(
        request(
            create_router(f.state.clone()),
            "PATCH",
            &path,
            &token,
            json!({"expected_updated_at":row["updated_at"],"name":"stale","reason":"stale write"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(request(create_router(f.state.clone()),"PATCH",&path,&token,json!({"expected_updated_at":next["updated_at"],"tenant_id":f.b.id,"reason":"attempt transfer"})).await.0,StatusCode::UNPROCESSABLE_ENTITY);
    let audit=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT metadata,request_id FROM tenant_audit_events WHERE tenant_id=$1 AND resource_id=$2 AND action='distribution.rule.create'",[f.a.id.into(),id.into()])).await.unwrap().unwrap();
    let metadata: Value = audit.try_get("", "metadata").unwrap();
    assert_eq!(metadata["after"]["commission_rate"], "0.1250");
    assert!(!metadata.to_string().contains("private-description-marker"));
    assert_eq!(
        audit.try_get::<Uuid>("", "request_id").unwrap().to_string(),
        request_id
    );
    let (s, _, _) = request(
        create_router(f.state.clone()),
        "DELETE",
        &path,
        &token,
        json!({"expected_updated_at":next["updated_at"],"reason":"remove obsolete policy"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        request(
            create_router(f.state.clone()),
            "GET",
            &path,
            &token,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let n =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM tenant_audit_events WHERE tenant_id=$1 AND resource_id=$2",
            [f.a.id.into(), id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(n, 3);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn credentials_foreign_ids_and_root_without_membership_follow_distinct_routes() {
    let mut f = Fixture::new().await;
    let row = f.create(f.b.id).await;
    let admin = f.token(f.a.owner_user_id, Some(f.a.id)).await;
    let member = f.token(f.member.id, Some(f.a.id)).await;
    let root = f.token(f.root.id, None).await;
    for token in [&member, &root] {
        let s = request(
            create_router(f.state.clone()),
            "POST",
            &f.path(f.a.id),
            token,
            create_body(),
        )
        .await
        .0;
        assert!(
            s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED,
            "{s}"
        );
    }
    assert_eq!(
        request(
            create_router(f.state.clone()),
            "PATCH",
            &format!("{}/{}", f.path(f.a.id), row.id),
            &admin,
            json!({"expected_updated_at":row.updated_at,"name":"stolen","reason":"foreign target"})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (s, created, _) = request(
        create_router(f.state.clone()),
        "POST",
        &format!("/api/v1/platform/distribution/tenants/{}/rules", f.a.id),
        &root,
        create_body(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{created}");
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET platform_role='operator' WHERE id=$1",
        [f.root.id.into()],
    ))
    .await
    .unwrap();
    let operator = f.token(f.root.id, None).await;
    assert_eq!(
        request(
            create_router(f.state.clone()),
            "POST",
            &format!("/api/v1/platform/distribution/tenants/{}/rules", f.a.id),
            &operator,
            create_body()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let raw = keycompute_auth::ProduceAiKeyValidator::generate_key();
    integration_tests::db::create_test_api_key(
        &f.db,
        &keycompute_db::CreateProduceAiKeyRequest {
            tenant_id: f.a.id,
            user_id: f.a.owner_user_id,
            name: "inference only".into(),
            produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&raw),
            produce_ai_key_preview: "sk-fixture-****".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        request(
            create_router(f.state.clone()),
            "POST",
            &f.path(f.a.id),
            &raw,
            create_body()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    // Handler-level root gate is effective without the outer admin middleware.
    let bare = Router::new()
        .route(
            "/api/v1/distribution/rules",
            post(keycompute_server::handlers::distribution::create_distribution_rule),
        )
        .layer(axum::Extension(
            keycompute_server::extractors::RequestId::new(),
        ))
        .with_state(f.state.clone());
    assert_eq!(
        request(
            bare,
            "POST",
            &format!("/api/v1/distribution/rules?tenant_id={}", f.b.id),
            &admin,
            json!({"name":"forbidden","commission_rate":0.1})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn direct_dao_rejects_forged_actors_bad_rates_and_foreign_beneficiaries() {
    let mut f = Fixture::new().await;
    let (who, audit) = f.authority(f.a.id, f.a.owner_user_id).await;
    for kind in [
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        assert!(
            dao::create(
                &f.db,
                who,
                &AuditContext {
                    credential_kind: kind,
                    ..audit
                },
                &f.input(f.a.id),
                "invalid credential"
            )
            .await
            .is_err()
        );
    }
    assert!(
        dao::create(
            &f.db,
            who,
            &AuditContext {
                actor_user_id: f.member.id,
                ..audit
            },
            &f.input(f.a.id),
            "mismatched actor"
        )
        .await
        .is_err()
    );
    for rate in ["-0.1", "1.01", "0.00001"] {
        let mut req = f.input(f.a.id);
        req.commission_rate = rate.parse().unwrap();
        assert!(
            dao::create(&f.db, who, &audit, &req, "bad rate")
                .await
                .is_err()
        );
    }
    let mut foreign = f.input(f.a.id);
    foreign.beneficiary_scope = BeneficiaryScope::TenantMember;
    foreign.beneficiary_id = Some(f.b.owner_user_id);
    assert!(
        dao::create(&f.db, who, &audit, &foreign, "foreign beneficiary")
            .await
            .is_err()
    );
    let mut member = f.input(f.a.id);
    member.beneficiary_scope = BeneficiaryScope::TenantMember;
    member.beneficiary_id = Some(f.member.id);
    let row = dao::create(&f.db, who, &audit, &member, "member override")
        .await
        .unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), f.member.id.into()],
    ))
    .await
    .unwrap();
    let mut patch = dao::PolicyPatch::empty(row.updated_at);
    patch.is_active = Some(false);
    let disabled = dao::update(
        &f.db,
        who,
        &audit,
        row.id,
        &patch,
        "disable unavailable member rule",
    )
    .await
    .unwrap();
    assert!(!disabled.is_active);
    patch.expected_updated_at = disabled.updated_at;
    patch.is_active = Some(true);
    assert!(
        dao::update(
            &f.db,
            who,
            &audit,
            row.id,
            &patch,
            "cannot reactivate suspended beneficiary"
        )
        .await
        .is_err()
    );
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_distribution_rules SET tenant_id=$1 WHERE id=$2",
            [f.b.id.into(), row.id.into()]
        ))
        .await
        .is_err()
    );
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_distribution_rules SET commission_rate=-1 WHERE id=$1",
            [row.id.into()]
        ))
        .await
        .is_err()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn audit_failure_rolls_back_create_update_delete_and_default_inside_outer_transaction() {
    let mut f = Fixture::new().await;
    let row = f.create(f.a.id).await;
    let (who, audit) = f.authority(f.a.id, f.a.owner_user_id).await;
    let tx = f.db.begin().await.unwrap();
    // Identical lock order to production, avoiding DDL/authority lock inversion.
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("reject_policy_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION pg_temp.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid AND NEW.resource_type='distribution_rule' THEN RAISE EXCEPTION 'policy audit test failure'; END IF; RETURN NEW; END $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION pg_temp.{name}();",f.a.id)).await.unwrap();
    assert!(
        dao::create(&tx, who, &audit, &f.input(f.a.id), "rolled back create")
            .await
            .is_err()
    );
    let mut patch = dao::PolicyPatch::empty(row.updated_at);
    patch.commission_rate = Some("0.2".parse().unwrap());
    assert!(
        dao::update(&tx, who, &audit, row.id, &patch, "rolled back update")
            .await
            .is_err()
    );
    assert!(
        dao::delete(
            &tx,
            who,
            &audit,
            row.id,
            row.updated_at,
            "rolled back delete"
        )
        .await
        .is_err()
    );
    assert!(
        dao::upsert_default(
            &tx,
            who,
            &audit,
            "default",
            "0.15".parse().unwrap(),
            "rolled back default"
        )
        .await
        .is_err()
    );
    tx.execute_unprepared(&format!("DROP TRIGGER {name} ON tenant_audit_events"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let found=f.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id,commission_rate,updated_at FROM tenant_distribution_rules WHERE tenant_id=$1",[f.a.id.into()])).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].try_get::<Uuid>("", "id").unwrap(), row.id);
    assert_eq!(
        found[0]
            .try_get::<bigdecimal::BigDecimal>("", "commission_rate")
            .unwrap(),
        row.commission_rate
    );
    assert_eq!(
        found[0].try_get::<DateTime<Utc>>("", "updated_at").unwrap(),
        row.updated_at
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn same_revision_race_and_concurrent_defaults_are_atomic() {
    let mut f = Fixture::new().await;
    let row = f.create(f.a.id).await;
    let (who, audit) = f.authority(f.a.id, f.a.owner_user_id).await;
    let mut one = dao::PolicyPatch::empty(row.updated_at);
    one.name = Some("first change".into());
    let mut two = one.clone();
    two.name = Some("second change".into());
    let (a, b) = tokio::join!(
        dao::update(&f.db, who, &audit, row.id, &one, "concurrent edit"),
        dao::update(&f.db, who, &audit, row.id, &two, "concurrent edit")
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let platform = dao::PolicyActor::platform(
        PlatformScope::checked(f.root.id, PlatformRole::Root).unwrap(),
        f.a.id,
        f.root.token_version,
    )
    .unwrap();
    let ra = AuditContext {
        actor_user_id: f.root.id,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        ..audit
    };
    let (a, b) = tokio::join!(
        dao::upsert_default(
            &f.db,
            who,
            &audit,
            "admin default",
            "0.2".parse().unwrap(),
            "set default"
        ),
        dao::upsert_default(
            &f.db,
            platform,
            &ra,
            "root default",
            "0.2".parse().unwrap(),
            "set default"
        )
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.id, b.id);
    let c = dao::upsert_default(
        &f.db,
        who,
        &audit,
        "stable",
        "0.2".parse().unwrap(),
        "set default",
    )
    .await
    .unwrap();
    let d = dao::upsert_default(
        &f.db,
        who,
        &audit,
        "stable",
        "0.2000".parse().unwrap(),
        "set default",
    )
    .await
    .unwrap();
    assert_eq!(c.updated_at, d.updated_at);
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn queued_mutation_rechecks_exact_signed_authority_before_writing() {
    let mut f = Fixture::new().await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), f.member.id.into()],
    ))
    .await
    .unwrap();
    let token = f.token(f.member.id, Some(f.a.id)).await;
    let mut options = ConnectOptions::new(std::env::var("DATABASE_URL").unwrap());
    options.max_connections(1).min_connections(1);
    let request_db = Database::connect(options).await.unwrap();
    let pid = request_db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid() AS pid",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i32>("", "pid")
        .unwrap();
    let state = AppState::with_pool(DbRouter::single(request_db.clone()));
    let path = f.path(f.a.id);
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        request(create_router(state), "POST", &path, &token, create_body()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{
        let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT 1 AS waiting FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock' AND query LIKE '%identity_admin_fence%'",[pid.into()])).await.unwrap();
        if row.is_some(){break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }}).await.expect("the tested connection must actually wait");
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='member' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), f.member.id.into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let (status, body, _) = task.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let n =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM tenant_distribution_rules WHERE tenant_id=$1",
            [f.a.id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(n, 0);
    request_db.close().await.unwrap();
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn renewed_roles_and_tokens_do_not_revalidate_old_policy_authority() {
    let mut f = Fixture::new().await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), f.member.id.into()],
    ))
    .await
    .unwrap();
    let (old, audit) = f.authority(f.a.id, f.member.id).await;
    for role in ["member", "admin"] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role=$3 WHERE tenant_id=$1 AND user_id=$2",
            [f.a.id.into(), f.member.id.into(), role.into()],
        ))
        .await
        .unwrap();
    }
    assert!(
        dao::create(
            &f.db,
            old,
            &audit,
            &f.input(f.a.id),
            "old regranted authority"
        )
        .await
        .is_err()
    );
    let stale = dao::PolicyActor::platform(
        PlatformScope::checked(f.root.id, PlatformRole::Root).unwrap(),
        f.a.id,
        f.root.token_version,
    )
    .unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
        [f.root.id.into()],
    ))
    .await
    .unwrap();
    let actor = AuditContext {
        actor_user_id: f.root.id,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        ..audit
    };
    assert!(
        dao::create(
            &f.db,
            stale,
            &actor,
            &f.input(f.a.id),
            "invalidated root token"
        )
        .await
        .is_err()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn changing_future_policy_cannot_rewrite_existing_distribution_or_money() {
    let mut f = Fixture::new().await;
    let row = f.create(f.a.id).await;
    let now = Utc::now();
    let usage = keycompute_db::UsageLog::create(
        &f.db,
        &keycompute_db::CreateUsageLogRequest {
            request_id: Uuid::new_v4(),
            tenant_id: f.a.id,
            user_id: f.member.id,
            produce_ai_key_id: Uuid::new_v4(),
            model_name: "policy-history-model".into(),
            provider_name: "openai".into(),
            account_id: Uuid::new_v4(),
            input_tokens: 10,
            output_tokens: 20,
            input_unit_price_snapshot: 1.into(),
            output_unit_price_snapshot: 1.into(),
            user_amount: 10.into(),
            currency: "USD".into(),
            usage_source: "gateway_accumulated".into(),
            status: "success".into(),
            started_at: now,
            finished_at: now,
        },
    )
    .await
    .unwrap();
    let historical = keycompute_db::DistributionRecord::create(
        &f.db,
        &keycompute_db::CreateDistributionRecordRequest {
            usage_log_id: usage.id,
            tenant_id: f.a.id,
            beneficiary_scope: "tenant_member".into(),
            beneficiary_id: f.member.id,
            share_amount: 1.into(),
            share_ratio: "0.1".parse().unwrap(),
            level: "level1".into(),
        },
    )
    .await
    .unwrap();
    let (who, audit) = f.authority(f.a.id, f.a.owner_user_id).await;
    let mut patch = dao::PolicyPatch::empty(row.updated_at);
    patch.commission_rate = Some("0.9".parse().unwrap());
    let changed = dao::update(&f.db, who, &audit, row.id, &patch, "future allocation only")
        .await
        .unwrap();
    dao::delete(
        &f.db,
        who,
        &audit,
        row.id,
        changed.updated_at,
        "remove future allocation",
    )
    .await
    .unwrap();
    let same=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT tenant_id,beneficiary_id,usage_log_id,share_amount,share_ratio,status FROM distribution_records WHERE id=$1",[historical.id.into()])).await.unwrap().unwrap();
    assert_eq!(same.try_get::<Uuid>("", "tenant_id").unwrap(), f.a.id);
    assert_eq!(
        same.try_get::<Uuid>("", "beneficiary_id").unwrap(),
        f.member.id
    );
    assert_eq!(same.try_get::<Uuid>("", "usage_log_id").unwrap(), usage.id);
    assert_eq!(
        same.try_get::<bigdecimal::BigDecimal>("", "share_amount")
            .unwrap(),
        historical.share_amount
    );
    assert_eq!(
        same.try_get::<bigdecimal::BigDecimal>("", "share_ratio")
            .unwrap(),
        historical.share_ratio
    );
    assert_eq!(same.try_get::<String>("", "status").unwrap(), "pending");
    let count =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT count(*) AS n FROM balance_transactions WHERE tenant_id=$1",
            [f.a.id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(
        count, 0,
        "policy operations do not credit/debit historical money"
    );
    f.guard.cleanup().await.unwrap();
}
