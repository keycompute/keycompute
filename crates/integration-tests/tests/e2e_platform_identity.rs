//! Current root identity/lifecycle and fixed financial owners, with isolated PostgreSQL.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    routing::get,
};
use chrono::Utc;
use integration_tests::{
    common::resolve_database_url,
    db::{create_test_api_key, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    AuditContext, CreateProduceAiKeyRequest, CreateTenantRequest, CreateUserRequest, DbRouter,
    UpdateTenantRequest, User,
    models::{
        financial_scope::{FinancialScope, FinancialSession},
        platform_identity::{PlatformIdentity, PlatformUserPatch},
    },
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    root: Uuid,
    admin: Uuid,
    user: Uuid,
    operator: Uuid,
    tenant: Uuid,
    key: String,
}
impl Fixture {
    async fn new(db: DatabaseConnection) -> Self {
        keycompute_db::initialize_schema(&db).await.unwrap();
        let tx = db.begin().await.unwrap();
        let root = User::bootstrap_root(&tx, "settings-root@fixture.invalid", None)
            .await
            .unwrap()
            .id;
        tx.commit().await.unwrap();
        let tenant = create_test_tenant(&db, "settings", "isolated").await.id;
        let admin = create_test_user(&db, tenant, "settings-admin", "isolated")
            .await
            .id;
        let user = create_test_user(&db, tenant, "settings-user", "isolated")
            .await
            .id;
        let operator = create_test_user(&db, tenant, "settings-operator", "isolated")
            .await
            .id;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
            [tenant.into(), admin.into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='operator' WHERE id=$1",
            [operator.into()],
        ))
        .await
        .unwrap();
        let key = keycompute_auth::ProduceAiKeyValidator::generate_key();
        create_test_api_key(
            &db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant,
                user_id: user,
                name: "settings inference fixture".into(),
                produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "fixture***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        Self {
            db,
            root,
            admin,
            user,
            operator,
            tenant,
            key,
        }
    }
    fn state(&self) -> AppState {
        AppState::with_pool(DbRouter::single(self.db.clone()))
    }
    async fn scope(&self, id: Uuid) -> FinancialScope {
        let user = User::find_by_id(&self.db, id).await.unwrap().unwrap();
        FinancialScope::platform_global(
            PlatformScope::checked(id, PlatformRole::Root).unwrap(),
            FinancialSession {
                user_id: id,
                credential_kind: CredentialKind::Jwt,
                token_version: user.token_version,
                expires_at: Utc::now().timestamp() + 3600,
                selected: None,
            },
        )
        .unwrap()
    }
    async fn token(&self, state: &AppState, id: Uuid, selected: bool, seconds: i64) -> String {
        let user = User::find_by_id(&self.db, id).await.unwrap().unwrap();
        let (tenant, tv, mv) = if selected {
            let tenant = keycompute_db::Tenant::find_by_id(&self.db, self.tenant)
                .await
                .unwrap()
                .unwrap();
            let member = keycompute_db::TenantMembership::find(&self.db, tenant.id, id)
                .await
                .unwrap()
                .unwrap();
            (
                Some(tenant.id),
                Some(tenant.authz_version),
                Some(member.authz_version),
            )
        } else {
            (None, None, None)
        };
        state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(id, tenant, user.token_version, tv, mv, seconds)
            .unwrap()
    }
}
async fn isolated<F, Fut>(case: F)
where
    F: FnOnce(Fixture) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some(),
        "isolated test environment required"
    );
    let url = resolve_database_url();
    assert!(
        url.contains("@127.0.0.1:") || url.contains("@localhost:"),
        "test database must be local"
    );
    let parent = create_test_pool().await;
    let name = format!("kc_identity_{}", Uuid::new_v4().simple());
    parent
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let base = url.rsplit_once('/').unwrap().0;
    let db = Database::connect(format!("{base}/{name}")).await.unwrap();
    let owned = db.clone();
    let result = tokio::spawn(async move {
        case(Fixture::new(owned).await).await;
    })
    .await;
    db.close().await.unwrap();
    parent
        .execute_unprepared(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    result.unwrap();
}
fn audit(id: Uuid) -> AuditContext {
    AuditContext {
        actor_user_id: id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::Root,
        actor_tenant_role: None,
        request_id: Some(Uuid::new_v4()),
    }
}
async fn call(
    app: Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value, HeaderMap) {
    let response = tokio::time::timeout(
        Duration::from_secs(20),
        app.oneshot(
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
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}
fn ok(r: (StatusCode, Value, HeaderMap)) -> Value {
    assert_eq!(r.0, StatusCode::OK, "{}", r.1);
    r.1
}

#[tokio::test]
async fn canonical_root_identity_reads_are_current_scoped_and_never_load_credential_secrets() {
    isolated(|f|async move {
        let state=f.state();let root=f.token(&state,f.root,false,3600).await;let admin=f.token(&state,f.admin,true,3600).await;
        let op=f.token(&state,f.operator,false,3600).await;let member=f.token(&state,f.user,true,3600).await;let app=create_router(state.clone());
        f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO user_credentials(user_id,password_hash,email_verified,last_login_at) VALUES($1,'NEVER-READ-PASSWORD',TRUE,clock_timestamp()) ON CONFLICT(user_id) DO UPDATE SET password_hash='NEVER-READ-PASSWORD',last_login_at=clock_timestamp()",[f.user.into()])).await.unwrap();
        for path in ["/api/v1/platform/users".to_owned(),format!("/api/v1/platform/users/{}",f.user),"/api/v1/platform/tenants".to_owned(),format!("/api/v1/platform/tenants/{}",f.tenant)] {
            let r=call(app.clone(),"GET",&path,&root,Value::Null).await;assert_eq!(r.0,StatusCode::OK,"{path}: {}",r.1);assert!(r.2["cache-control"].to_str().unwrap().contains("no-store"));
            for word in ["password_hash","NEVER-READ-PASSWORD","token_version","refresh_token"] {assert!(!r.1.to_string().contains(word),"{word}");}
            for token in [&admin,&op,&member,&f.key] {assert!(matches!(call(app.clone(),"GET",&path,token,Value::Null).await.0,StatusCode::FORBIDDEN|StatusCode::UNAUTHORIZED));}
        }
        let bare=Router::new().route("/users",get(keycompute_server::handlers::admin_user::list_all_users)).with_state(state);
        assert_eq!(call(bare,"GET","/users",&admin,Value::Null).await.0,StatusCode::FORBIDDEN);
        let detail=ok(call(app,"GET",&format!("/api/v1/platform/users/{}",f.user),&root,Value::Null).await);assert!(detail["last_login_at"].is_string());
    }).await;
}
#[tokio::test]
async fn root_platform_user_and_tenant_lists_have_one_literal_paging_scope() {
    isolated(|f| async move {
        let scope = f.scope(f.root).await;
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET name='literal%_operator' WHERE id=$1",
            [f.operator.into()],
        ))
        .await
        .unwrap();
        let filtered =
            PlatformIdentity::users(&f.db, scope, Some(PlatformRole::Operator), Some("%_"), 1, 0)
                .await
                .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].id, f.operator);
        let out = PlatformIdentity::users(&f.db, scope, None, Some("%_"), 1, 1)
            .await
            .unwrap();
        assert_eq!(out.total, 1);
        assert!(out.items.is_empty());
        let tenants = PlatformIdentity::tenants(&f.db, scope, Some("settings"), 1, 0)
            .await
            .unwrap();
        assert_eq!(tenants.total, 1);
        assert_eq!(tenants.items[0].id, f.tenant);
        assert_eq!(tenants.items[0].user_count, 4);
        for id in [f.user, f.admin, f.operator] {
            let forged = f.scope(id).await;
            assert!(
                PlatformIdentity::users(&f.db, forged, None, None, 20, 0)
                    .await
                    .is_err()
            );
            assert!(
                PlatformIdentity::tenants(&f.db, forged, None, 20, 0)
                    .await
                    .is_err()
            );
        }
        assert!(
            PlatformIdentity::users(&f.db, scope, None, None, 0, 0)
                .await
                .is_err()
        );
        assert!(
            PlatformIdentity::tenant(&f.db, scope, Uuid::nil())
                .await
                .is_err()
        );
    })
    .await;
}
#[tokio::test]
async fn root_mutations_are_audited_with_server_request_ids_and_keep_active_owner_invariants() {
    isolated(|f|async move {
        let state=f.state();let root=f.token(&state,f.root,false,3600).await;let app=create_router(state);
        let target=format!("/api/v1/platform/users/{}",f.user);
        let changed=ok(call(app.clone(),"PATCH",&target,&root,json!({"name":"Changed","platform_role":"operator","reason":"operations assignment"})).await);
        assert_eq!(changed["platform_role"],"operator");assert_eq!(changed["name"],"Changed");
        let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT request_id,metadata FROM tenant_audit_events WHERE actor_user_id=$1 AND resource_id=$2 AND action='user.update'",[f.root.into(),f.user.to_string().into()])).await.unwrap().unwrap();assert!(!row.try_get::<Uuid>("","request_id").unwrap().is_nil());assert_eq!(row.try_get::<Value>("","metadata").unwrap()["reason"],"operations assignment");
        let created=ok(call(app.clone(),"POST","/api/v1/platform/tenants",&root,json!({"owner_user_id":f.user,"name":"Owned business","slug":"owned-business"})).await);
        let tenant=created["id"].as_str().unwrap();assert_eq!(created["user_count"],1);
        let owner=keycompute_db::TenantMembership::find(&f.db,Uuid::parse_str(tenant).unwrap(),f.user).await.unwrap().unwrap();assert_eq!(owner.tenant_role,"admin");
        assert_eq!(call(app.clone(),"PATCH",&target,&root,json!({"status":"suspended","reason":"invalid owner suspension"})).await.0,StatusCode::CONFLICT);
        let deactivated=ok(call(app.clone(),"PATCH",&format!("/api/v1/platform/tenants/{tenant}"),&root,json!({"status":"inactive"})).await);assert_eq!(deactivated["status"],"inactive");
        ok(call(app,"DELETE",&format!("/api/v1/platform/tenants/{tenant}"),&root,Value::Null).await);
    }).await;
}
#[tokio::test]
async fn user_security_audit_failure_rolls_back_profile_and_privilege_with_outer_commit() {
    isolated(|f|async move {
        let scope=f.scope(f.root).await;let before=User::find_by_id(&f.db,f.user).await.unwrap().unwrap();
        let tx=f.db.begin().await.unwrap();tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE").await.unwrap();
        tx.execute_unprepared("CREATE FUNCTION identity_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='user.update' THEN RAISE EXCEPTION 'isolated identity audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER identity_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION identity_fault();").await.unwrap();
        let command=PlatformUserPatch {name:Some("never committed".into()),platform_role:Some(PlatformRole::Operator),status:None};
        assert!(PlatformIdentity::update_user(&tx,scope,&audit(f.root),f.user,&command,"audit must persist").await.is_err());
        let current=User::find_by_id(&tx,f.user).await.unwrap().unwrap();assert_eq!(current.name,before.name);assert_eq!(current.platform_role,before.platform_role);assert_eq!(current.token_version,before.token_version);
        tx.execute_unprepared("DROP TRIGGER identity_fault ON tenant_audit_events; DROP FUNCTION identity_fault();").await.unwrap();tx.commit().await.unwrap();
    }).await;
}
#[tokio::test]
async fn lifecycle_audit_failure_keeps_tenant_memberships_and_configuration_atomic() {
    isolated(|f|async move {
        let scope=f.scope(f.root).await;let tx=f.db.begin().await.unwrap();tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE").await.unwrap();
        tx.execute_unprepared("CREATE FUNCTION lifecycle_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.scope_type='platform' AND NEW.action IN ('tenant.create','tenant.update','tenant.delete') THEN RAISE EXCEPTION 'isolated lifecycle audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER lifecycle_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION lifecycle_fault();").await.unwrap();
        let request=CreateTenantRequest {name:"must roll back".into(),slug:"must-rollback".into(),description:None,default_rpm_limit:None,default_tpm_limit:None};
        assert!(PlatformIdentity::create_tenant(&tx,scope,&audit(f.root),&request,f.user).await.is_err());
        assert!(keycompute_db::Tenant::find_by_slug(&tx,"must-rollback").await.unwrap().is_none());
        let old=keycompute_db::Tenant::find_by_id(&tx,f.tenant).await.unwrap().unwrap();
        let change=UpdateTenantRequest {name:Some("not applied".into()),description:None,status:Some(keycompute_types::TenantStatus::Inactive),default_rpm_limit:None,default_tpm_limit:None};
        assert!(PlatformIdentity::update_tenant(&tx,scope,&audit(f.root),f.tenant,&change).await.is_err());
        let current=keycompute_db::Tenant::find_by_id(&tx,f.tenant).await.unwrap().unwrap();assert_eq!(old.name,current.name);assert_eq!(old.status,current.status);assert_eq!(old.authz_version,current.authz_version);
        tx.execute_unprepared("DROP TRIGGER lifecycle_fault ON tenant_audit_events; DROP FUNCTION lifecycle_fault();").await.unwrap();tx.commit().await.unwrap();
    }).await;
}
#[tokio::test]
async fn authorized_self_role_change_and_selected_tenant_changes_commit_then_invalidate_old_jwt() {
    isolated(|f| async move {
        let state = f.state();
        let root = f.token(&state, f.root, false, 3600).await;
        let app = create_router(state.clone());
        ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/users/{}", f.admin),
            &root,
            json!({"platform_role":"root","reason":"second root for invariants"}),
        )
        .await);
        let admin_global = f.token(&state, f.admin, false, 3600).await;
        let selected = f.token(&state, f.admin, true, 3600).await;
        let changed = ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/tenants/{}", f.tenant),
            &selected,
            json!({"status":"inactive"}),
        )
        .await);
        assert_eq!(changed["status"], "inactive");
        assert!(matches!(
            call(
                app.clone(),
                "GET",
                "/api/v1/platform/tenants",
                &selected,
                Value::Null
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ));
        let selfdemotion = ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/users/{}", f.admin),
            &admin_global,
            json!({"platform_role":"none","reason":"relinquish platform role"}),
        )
        .await);
        assert_eq!(selfdemotion["platform_role"], "none");
        assert!(matches!(
            call(
                app.clone(),
                "GET",
                "/api/v1/platform/users",
                &admin_global,
                Value::Null
            )
            .await
            .0,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ));
        assert_eq!(
            call(
                app,
                "PATCH",
                &format!("/api/v1/platform/users/{}", f.root),
                &root,
                json!({"platform_role":"none","reason":"last root cannot go"})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
    })
    .await;
}
#[tokio::test]
async fn no_stale_or_forged_root_scope_can_create_update_or_delete_tenants() {
    isolated(|f| async move {
        let request = CreateTenantRequest {
            name: "unauthorized".into(),
            slug: "unauthorized".into(),
            description: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        };
        let patch = UpdateTenantRequest {
            name: Some("unauthorized".into()),
            description: None,
            status: None,
            default_rpm_limit: None,
            default_tpm_limit: None,
        };
        for id in [f.user, f.admin, f.operator] {
            let scope = f.scope(id).await;
            assert!(
                PlatformIdentity::create_tenant(&f.db, scope, &audit(id), &request, id)
                    .await
                    .is_err()
            );
            assert!(
                PlatformIdentity::update_tenant(&f.db, scope, &audit(id), f.tenant, &patch)
                    .await
                    .is_err()
            );
            assert!(
                PlatformIdentity::delete_tenant(&f.db, scope, &audit(id), f.tenant)
                    .await
                    .is_err()
            );
        }
        let scope = f.scope(f.root).await;
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET token_version=token_version+1 WHERE id=$1",
            [f.root.into()],
        ))
        .await
        .unwrap();
        assert!(
            PlatformIdentity::create_tenant(&f.db, scope, &audit(f.root), &request, f.user)
                .await
                .is_err()
        );
        assert!(
            PlatformIdentity::update_user(
                &f.db,
                scope,
                &audit(f.root),
                f.user,
                &PlatformUserPatch {
                    name: Some("bad".into()),
                    ..Default::default()
                },
                "stale request"
            )
            .await
            .is_err()
        );
        assert!(
            keycompute_db::Tenant::find_by_slug(&f.db, "unauthorized")
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}
async fn single_connection(f: &Fixture) -> DatabaseConnection {
    let name: String =
        f.db.query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_database() AS name".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "name")
        .unwrap();
    let url = resolve_database_url();
    let base = url.rsplit_once('/').unwrap().0;
    let mut options = ConnectOptions::new(format!("{base}/{name}"));
    options.max_connections(1).min_connections(1);
    Database::connect(options).await.unwrap()
}
async fn wait_for_exact_backend(db: &DatabaseConnection, pid: i32, relation: &str) {
    // pg_stat_activity.query can be truncated before a long safe projection's
    // FROM clause. Check the exact backend's granted relation lock instead.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT a.wait_event_type,EXISTS(SELECT 1 FROM pg_locks l WHERE l.pid=a.pid AND l.relation=to_regclass($2) AND l.granted) AS target_relation FROM pg_stat_activity a WHERE a.pid=$1",
                [pid.into(),relation.into()])).await.unwrap().unwrap();
            if row.try_get::<Option<String>>("","wait_event_type").unwrap().as_deref()==Some("Lock")
                && row.try_get::<bool>("","target_relation").unwrap() {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("the exact test request must reach the expected relation lock");
}

#[tokio::test]
async fn queued_tenant_update_rechecks_root_after_identity_fence_wait() {
    isolated(|f| async move {
        let single = single_connection(&f).await;
        let pid: i32 = single
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "pid")
            .unwrap();
        let state = AppState::with_pool(DbRouter::single(single.clone()));
        let token = f.token(&state, f.root, false, 3600).await;
        let hold = f.db.begin().await.unwrap();
        hold.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
            .await
            .unwrap();
        let path = format!("/api/v1/platform/tenants/{}", f.tenant);
        let request = tokio::spawn(async move {
            call(
                create_router(state),
                "PATCH",
                &path,
                &token,
                json!({"name":"stale root cannot rename"}),
            )
            .await
        });
        wait_for_exact_backend(&f.db, pid, "identity_admin_fence").await;
        hold.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [f.admin.into()],
        ))
        .await
        .unwrap();
        hold.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='operator' WHERE id=$1",
            [f.root.into()],
        ))
        .await
        .unwrap();
        hold.commit().await.unwrap();
        let response = request.await.unwrap();
        assert_eq!(response.0, StatusCode::FORBIDDEN, "{}", response.1);
        assert_ne!(
            keycompute_db::Tenant::find_by_id(&f.db, f.tenant)
                .await
                .unwrap()
                .unwrap()
                .name,
            "stale root cannot rename"
        );
        single.close().await.unwrap();
    })
    .await;
}
#[tokio::test]
async fn platform_identity_expiry_after_target_row_wait_rolls_back_security_and_profile() {
    isolated(|f| async move {
        let single = single_connection(&f).await;
        let pid: i32 = single
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "pid")
            .unwrap();
        let root = User::find_by_id(&f.db, f.root).await.unwrap().unwrap();
        let expires = Utc::now().timestamp() + 2;
        let scope = FinancialScope::platform_global(
            PlatformScope::checked(f.root, PlatformRole::Root).unwrap(),
            FinancialSession {
                user_id: f.root,
                credential_kind: CredentialKind::Jwt,
                token_version: root.token_version,
                expires_at: expires,
                selected: None,
            },
        )
        .unwrap();
        let hold = f.db.begin().await.unwrap();
        hold.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM users WHERE id=$1 FOR UPDATE",
            [f.user.into()],
        ))
        .await
        .unwrap();
        let db = single.clone();
        let actor = audit(f.root);
        let target = f.user;
        let request = tokio::spawn(async move {
            PlatformIdentity::update_user(
                &db,
                scope,
                &actor,
                target,
                &PlatformUserPatch {
                    name: Some("expired mutation".into()),
                    platform_role: Some(PlatformRole::Operator),
                    status: None,
                },
                "expired command",
            )
            .await
        });
        wait_for_exact_backend(&f.db, pid, "users").await;
        while Utc::now().timestamp() < expires {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        hold.commit().await.unwrap();
        let error = request.await.unwrap().unwrap_err();
        assert!(
            error.to_string().contains("financial_authority_invalid"),
            "{error}"
        );
        let user = User::find_by_id(&f.db, f.user).await.unwrap().unwrap();
        assert_eq!(user.platform_role, "none");
        assert_ne!(user.name.as_deref(), Some("expired mutation"));
        single.close().await.unwrap();
    })
    .await;
}
#[tokio::test]
async fn identity_and_empty_tenant_deletion_roll_back_if_their_final_audit_fails() {
    isolated(|f|async move {
        let scope=f.scope(f.root).await;
        let target=User::create(&f.db,&CreateUserRequest {email:"unowned@fixture.invalid".into(),name:Some("retain on audit failure".into())}).await.unwrap();
        let request=CreateTenantRequest {name:"empty owned tenant".into(),slug:"empty-owned".into(),description:None,default_rpm_limit:None,default_tpm_limit:None};
        let created=PlatformIdentity::create_tenant(&f.db,scope,&audit(f.root),&request,f.user).await.unwrap();
        let tx=f.db.begin().await.unwrap();tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE").await.unwrap();
        tx.execute_unprepared("CREATE FUNCTION delete_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('user.delete','tenant.delete') THEN RAISE EXCEPTION 'isolated deletion audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER delete_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION delete_fault();").await.unwrap();
        assert!(PlatformIdentity::delete_user(&tx,scope,&audit(f.root),target.id).await.is_err());
        assert_eq!(User::find_by_id(&tx,target.id).await.unwrap().unwrap().status,"active");
        assert!(PlatformIdentity::delete_tenant(&tx,scope,&audit(f.root),created.id).await.is_err());
        assert!(keycompute_db::Tenant::find_by_id(&tx,created.id).await.unwrap().is_some());
        assert_eq!(keycompute_db::TenantMembership::find(&tx,created.id,f.user).await.unwrap().unwrap().tenant_role,"admin");
        tx.execute_unprepared("DROP TRIGGER delete_fault ON tenant_audit_events; DROP FUNCTION delete_fault();").await.unwrap();tx.commit().await.unwrap();
    }).await;
}
