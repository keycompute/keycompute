//! Current platform identity/lifecycle and fixed financial owners, with isolated PostgreSQL.
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
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope, UserStatus};
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
        let operator = User::create(
            &db,
            &CreateUserRequest {
                email: "settings-operator@fixture.invalid".into(),
                name: Some("settings-operator".into()),
            },
        )
        .await
        .unwrap()
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
        let mut state = AppState::with_pool(DbRouter::single(self.db.clone()));
        // Invitation acceptance tokens are returned only as fragment links.
        // Keep this HTTP lifecycle fixture capable of exercising the complete
        // create -> accept flow without weakening the production response.
        state.app_base_url = Some("http://localhost:3000".into());
        state
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
            assert_eq!(call(app.clone(),"GET",&path,&op,Value::Null).await.0,StatusCode::OK);
            for token in [&admin,&member,&f.key] {assert!(matches!(call(app.clone(),"GET",&path,token,Value::Null).await.0,StatusCode::FORBIDDEN|StatusCode::UNAUTHORIZED));}
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
        let filtered = PlatformIdentity::users(
            &f.db,
            scope,
            Some(PlatformRole::Operator),
            Some(UserStatus::Active),
            Some("%_"),
            1,
            0,
        )
        .await
        .unwrap();
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.items[0].id, f.operator);
        let out = PlatformIdentity::users(&f.db, scope, None, None, Some("%_"), 1, 1)
            .await
            .unwrap();
        assert_eq!(out.total, 1);
        assert!(out.items.is_empty());
        let tenants = PlatformIdentity::tenants(&f.db, scope, Some("settings"), 1, 0)
            .await
            .unwrap();
        assert_eq!(tenants.total, 1);
        assert_eq!(tenants.items[0].id, f.tenant);
        assert_eq!(tenants.items[0].user_count, 3);
        for id in [f.user, f.admin, f.operator] {
            let forged = f.scope(id).await;
            assert!(
                PlatformIdentity::users(&f.db, forged, None, None, None, 20, 0)
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
            PlatformIdentity::users(&f.db, scope, None, None, None, 0, 0)
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
    isolated(|f| async move {
        let state = f.state();
        let root = f.token(&state, f.root, false, 3600).await;
        let app = create_router(state.clone());
        let unowned = User::create(
            &f.db,
            &CreateUserRequest {
                email: "unowned-operator@fixture.invalid".into(),
                name: Some("Unowned".into()),
            },
        )
        .await
        .unwrap();
        let target = format!("/api/v1/platform/users/{}", unowned.id);

        let changed_response = call(
            app.clone(),
            "PATCH",
            &target,
            &root,
            json!({"name":"Changed","platform_role":"operator","reason":"operations assignment"}),
        )
        .await;
        let changed_request: Uuid = changed_response.2["x-request-id"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let changed = ok(changed_response);
        assert_eq!(changed["platform_role"], "operator");
        assert_eq!(changed["name"], "Changed");
        assert!(
            keycompute_db::TenantMembership::list_active_for_user(&f.db, unowned.id)
                .await
                .unwrap()
                .is_empty()
        );
        let rows = f.db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT scope_type,tenant_id,action,metadata FROM tenant_audit_events WHERE request_id=$1 ORDER BY id",
            [changed_request.into()],
        )).await.unwrap();
        assert_eq!(rows.len(), 1, "one PATCH must produce one operation audit");
        assert_eq!(rows[0].try_get::<String>("", "scope_type").unwrap(), "platform");
        assert!(rows[0].try_get::<Option<Uuid>>("", "tenant_id").unwrap().is_none());
        assert_eq!(rows[0].try_get::<String>("", "action").unwrap(), "user.update");
        let metadata = rows[0].try_get::<Value>("", "metadata").unwrap();
        assert_eq!(metadata["reason"], "operations assignment");
        assert_eq!(metadata["name_changed"], true);

        let profile_response = call(
            app.clone(),
            "PATCH",
            &target,
            &root,
            json!({"name":"Profile only"}),
        )
        .await;
        let profile_request: Uuid = profile_response.2["x-request-id"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(ok(profile_response)["name"], "Profile only");
        let rows = f.db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT scope_type,tenant_id,action,metadata FROM tenant_audit_events WHERE request_id=$1 ORDER BY id",
            [profile_request.into()],
        )).await.unwrap();
        assert_eq!(rows.len(), 1, "profile-only PATCH must not emit user.security");
        assert_eq!(rows[0].try_get::<String>("", "scope_type").unwrap(), "platform");
        assert!(rows[0].try_get::<Option<Uuid>>("", "tenant_id").unwrap().is_none());
        assert_eq!(rows[0].try_get::<String>("", "action").unwrap(), "user.update");

        let create_response = call(
            app.clone(),
            "POST",
            "/api/v1/platform/tenants",
            &root,
            json!({"name":"Ownerless business","slug":"ownerless-business"}),
        )
        .await;
        let create_request: Uuid = create_response.2["x-request-id"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        let created = ok(create_response);
        let tenant = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
        assert_eq!(created["user_count"], 0);
        assert!(created["owner_user_id"].is_null());
        let rows = f.db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT scope_type,tenant_id,action,metadata FROM tenant_audit_events WHERE request_id=$1 ORDER BY id",
            [create_request.into()],
        )).await.unwrap();
        assert_eq!(rows.len(), 1, "tenant creation must have one semantic audit event");
        assert_eq!(rows[0].try_get::<String>("", "scope_type").unwrap(), "platform");
        assert_eq!(rows[0].try_get::<Uuid>("", "tenant_id").unwrap(), tenant);
        assert_eq!(rows[0].try_get::<String>("", "action").unwrap(), "tenant.create");
        assert_eq!(
            keycompute_db::Tenant::count_users(&f.db, tenant)
                .await
                .unwrap(),
            0
        );

        let assigned_owner = keycompute_db::Tenant::find_by_id(&f.db, f.tenant)
            .await
            .unwrap()
            .unwrap()
            .owner_user_id
            .expect("owned fixture tenant must retain its owner");
        assert_eq!(
            call(
                app.clone(),
                "PATCH",
                &format!("/api/v1/platform/users/{assigned_owner}"),
                &root,
                json!({"status":"suspended","reason":"invalid owner suspension"}),
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let deactivated = ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/tenants/{tenant}"),
            &root,
            json!({"status":"inactive"}),
        )
        .await);
        assert_eq!(deactivated["status"], "inactive");
        let rows = f.db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT scope_type,tenant_id FROM tenant_audit_events WHERE action='tenant.update' AND resource_id=$1 ORDER BY id DESC LIMIT 1",
            [tenant.to_string().into()],
        )).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].try_get::<String>("", "scope_type").unwrap(), "platform");
        assert_eq!(rows[0].try_get::<Uuid>("", "tenant_id").unwrap(), tenant);
        ok(call(
            app,
            "DELETE",
            &format!("/api/v1/platform/tenants/{tenant}"),
            &root,
            Value::Null,
        )
        .await);
        let rows = f.db.query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT scope_type,tenant_id,metadata FROM tenant_audit_events WHERE action='tenant.delete' AND resource_id=$1 ORDER BY id DESC LIMIT 1",
            [tenant.to_string().into()],
        )).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].try_get::<String>("", "scope_type").unwrap(), "platform");
        assert_eq!(rows[0].try_get::<Uuid>("", "tenant_id").unwrap(), tenant);
        let metadata = rows[0].try_get::<Value>("", "metadata").unwrap();
        assert_eq!(metadata["deleted_tenant_id"], tenant.to_string());
    })
    .await;
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
        tx.execute_unprepared("CREATE FUNCTION lifecycle_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='tenant.create' OR (NEW.scope_type='platform' AND NEW.action IN ('tenant.update','tenant.delete')) THEN RAISE EXCEPTION 'isolated lifecycle audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER lifecycle_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION lifecycle_fault();").await.unwrap();
        let request=CreateTenantRequest {name:"must roll back".into(),slug:"must-rollback".into(),description:None,default_rpm_limit:None,default_tpm_limit:None};
        assert!(PlatformIdentity::create_tenant(&tx,scope,&audit(f.root),&request).await.is_err());
        assert!(keycompute_db::Tenant::find_by_slug(&tx,"must-rollback").await.unwrap().is_none());
        let old=keycompute_db::Tenant::find_by_id(&tx,f.tenant).await.unwrap().unwrap();
        let change=UpdateTenantRequest {name:Some("not applied".into()),description:None,status:Some(keycompute_types::TenantStatus::Inactive),default_rpm_limit:None,default_tpm_limit:None};
        assert!(PlatformIdentity::update_tenant(&tx,scope,&audit(f.root),f.tenant,&change).await.is_err());
        let current=keycompute_db::Tenant::find_by_id(&tx,f.tenant).await.unwrap().unwrap();assert_eq!(old.name,current.name);assert_eq!(old.status,current.status);assert_eq!(old.authz_version,current.authz_version);
        tx.execute_unprepared("DROP TRIGGER lifecycle_fault ON tenant_audit_events; DROP FUNCTION lifecycle_fault();").await.unwrap();tx.commit().await.unwrap();
    }).await;
}
#[tokio::test]
async fn root_is_immutable_and_only_root_can_change_an_operator() {
    isolated(|f| async move {
        let state = f.state();
        let root = f.token(&state, f.root, false, 3600).await;
        let operator = f.token(&state, f.operator, false, 3600).await;
        let app = create_router(state.clone());
        assert_eq!(
            call(
                app.clone(),
                "PATCH",
                &format!("/api/v1/platform/users/{}", f.root),
                &operator,
                json!({"name":"operator cannot rename root"}),
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                app.clone(),
                "PATCH",
                &format!("/api/v1/platform/users/{}", f.operator),
                &operator,
                json!({"name":"operator cannot rename operator"}),
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let renamed = ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/users/{}", f.root),
            &root,
            json!({"name":"Root self-managed profile"}),
        )
        .await);
        assert_eq!(renamed["name"], "Root self-managed profile");
        assert_eq!(renamed["platform_role"], "root");
        assert_eq!(renamed["status"], "active");
        assert_eq!(
            call(
                app.clone(),
                "PATCH",
                &format!("/api/v1/platform/users/{}", f.user),
                &root,
                json!({"platform_role":"root","reason":"second root is forbidden"}),
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        let demoted = ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/users/{}", f.operator),
            &root,
            json!({"platform_role":"none","reason":"root revokes operator role"}),
        )
        .await);
        assert_eq!(demoted["platform_role"], "none");
        assert!(matches!(
            call(
                app.clone(),
                "GET",
                "/api/v1/platform/users",
                &operator,
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
                json!({"platform_role":"none","reason":"root cannot be demoted"})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        let database_delete =
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM users WHERE id=$1",
                [f.root.into()],
            ))
            .await
            .unwrap_err();
        assert!(
            database_delete
                .to_string()
                .contains("root identity cannot be deleted"),
            "{database_delete}"
        );
    })
    .await;
}

#[tokio::test]
async fn operator_can_manage_tenants_without_acquiring_tenant_membership() {
    isolated(|f| async move {
        let state = f.state();
        let operator = f.token(&state, f.operator, false, 3600).await;
        let app = create_router(state.clone());
        let created = ok(call(
            app.clone(),
            "POST",
            "/api/v1/platform/tenants",
            &operator,
            json!({
                "name": "Operator managed tenant",
                "slug": "operator-managed-tenant"
            }),
        )
        .await);
        let tenant_id = created["id"].as_str().unwrap();
        let tenant_id = Uuid::parse_str(tenant_id).unwrap();
        let stored = keycompute_db::Tenant::find_by_id(&f.db, tenant_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner_user_id, None);
        assert_eq!(
            keycompute_db::Tenant::count_users(&f.db, tenant_id)
                .await
                .unwrap(),
            0
        );
        let updated = ok(call(
            app.clone(),
            "PUT",
            &format!("/api/v1/platform/tenants/{tenant_id}"),
            &operator,
            json!({"name": "Operator updated tenant"}),
        )
        .await);
        assert_eq!(updated["name"], "Operator updated tenant");
        let context = ok(call(
            app.clone(),
            "GET",
            &format!("/api/v1/platform/tenants/{tenant_id}/control"),
            &operator,
            Value::Null,
        )
        .await);
        assert!(context["owner_user_id"].is_null());
        assert_eq!(context["tenant_role"], "admin");

        let invitee = User::find_by_id(&f.db, f.user).await.unwrap().unwrap();
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO user_credentials(user_id,password_hash,email_verified,email_verified_at) VALUES($1,'fixture',TRUE,clock_timestamp()) ON CONFLICT(user_id) DO UPDATE SET email_verified=TRUE,email_verified_at=clock_timestamp()",
            [invitee.id.into()],
        ))
        .await
        .unwrap();
        let invitation_response = call(
            app.clone(),
            "POST",
            &format!("/api/v1/platform/tenants/{tenant_id}/control/invitations"),
            &operator,
            json!({
                "email": invitee.email,
                "tenant_role": "admin",
                "expires_in_seconds": 3600
            }),
        )
        .await;
        assert_eq!(
            invitation_response.0,
            StatusCode::CREATED,
            "{}",
            invitation_response.1
        );
        let invitation = invitation_response.1;
        let invitation_token = invitation["acceptance_link"]
            .as_str()
            .unwrap()
            .split_once("#token=")
            .unwrap()
            .1;
        let invitee_token = f.token(&state, f.user, false, 3600).await;
        let accepted = ok(call(
            app.clone(),
            "POST",
            &format!("/api/v1/invitations/{invitation_token}/accept"),
            &invitee_token,
            json!({}),
        )
        .await);
        assert_eq!(accepted["tenant"]["owner_user_id"], f.user.to_string());
        assert_eq!(accepted["membership"]["tenant_role"], "admin");
        let stored = keycompute_db::Tenant::find_by_id(&f.db, tenant_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner_user_id, Some(f.user));

        // Platform authority also reaches tenant resources without acquiring a
        // membership. The owner still performs the one-time secret claim.
        let requested = call(
            app.clone(),
            "POST",
            &format!("/api/v1/platform/tenants/{}/keys/issuance", f.tenant),
            &operator,
            json!({"owner_user_id":f.user,"name":"operator requested key"}),
        )
        .await;
        assert_eq!(requested.0, StatusCode::ACCEPTED, "{}", requested.1);
        let intent_id = requested.1["intent"]["id"].as_str().unwrap();
        let owner_token = f.token(&state, f.user, true, 3600).await;
        let claimed = ok(call(
            app.clone(),
            "POST",
            &format!("/api/v1/me/key-issuance/{intent_id}/claim"),
            &owner_token,
            json!({}),
        )
        .await);
        assert_eq!(claimed["outcome"], "claimed");
        assert_eq!(claimed["secret_returned_once"], true);
        assert!(
            claimed["key"]
                .as_str()
                .is_some_and(|key| key.starts_with("sk-"))
        );
        let keys = ok(call(
            app.clone(),
            "GET",
            &format!(
                "/api/v1/platform/tenants/{}/keys?owner_user_id={}",
                f.tenant, f.user
            ),
            &operator,
            Value::Null,
        )
        .await);
        assert_eq!(keys["total"], 2);
        assert!(!keys.to_string().contains(claimed["key"].as_str().unwrap()));

        // Tenant administrators see only tenant-authority events. The
        // platform-target route retains the complete timeline, including the
        // operator's key issuance request above, and its count uses the same
        // visibility boundary as the returned page.
        let tenant_admin = f.token(&state, f.admin, true, 3600).await;
        let tenant_audit = ok(call(
            app.clone(),
            "GET",
            &format!(
                "/api/v1/tenants/{}/audit-events?page=1&page_size=100",
                f.tenant
            ),
            &tenant_admin,
            Value::Null,
        )
        .await);
        let tenant_events = tenant_audit["items"].as_array().unwrap();
        assert!(
            tenant_events
                .iter()
                .all(|event| event["scope_type"] == "tenant")
        );
        assert_eq!(
            tenant_audit["total"].as_u64().unwrap(),
            tenant_events.len() as u64
        );

        let platform_audit = ok(call(
            app.clone(),
            "GET",
            &format!(
                "/api/v1/platform/tenants/{}/control/audit-events?page=1&page_size=100",
                f.tenant
            ),
            &operator,
            Value::Null,
        )
        .await);
        assert!(platform_audit["items"].as_array().unwrap().iter().any(|event| {
            event["scope_type"] == "platform" && event["action"] == "key_issuance.request"
        }));
        assert!(platform_audit["total"].as_u64() > tenant_audit["total"].as_u64());

        for path in [
            format!("/api/v1/platform/tenants/{tenant_id}/usage"),
            format!("/api/v1/platform/tenants/{tenant_id}/payments/orders"),
            format!("/api/v1/platform/tenants/{tenant_id}/tips"),
        ] {
            assert_eq!(
                call(app.clone(), "GET", &path, &operator, Value::Null)
                    .await
                    .0,
                StatusCode::OK,
                "{path}"
            );
        }
        let blocked_delete = call(
            app.clone(),
            "DELETE",
            &format!("/api/v1/platform/tenants/{tenant_id}"),
            &operator,
            Value::Null,
        )
        .await;
        assert_eq!(
            blocked_delete.0,
            StatusCode::CONFLICT,
            "{}",
            blocked_delete.1
        );
        let membership = keycompute_db::TenantMembership::find(&f.db, tenant_id, f.user)
            .await
            .unwrap()
            .unwrap();
        let removed = ok(call(
            app.clone(),
            "DELETE",
            &format!(
                "/api/v1/platform/tenants/{tenant_id}/control/members/{}",
                f.user
            ),
            &operator,
            json!({"expected_authz_version":membership.authz_version}),
        )
        .await);
        assert_eq!(removed["membership_status"], "removed");

        let retained_history_delete = call(
            app.clone(),
            "DELETE",
            &format!("/api/v1/platform/tenants/{tenant_id}"),
            &operator,
            Value::Null,
        )
        .await;
        assert_eq!(
            retained_history_delete.0,
            StatusCode::CONFLICT,
            "{}",
            retained_history_delete.1
        );

        let empty = ok(call(
            app.clone(),
            "POST",
            "/api/v1/platform/tenants",
            &operator,
            json!({
                "name": "Operator disposable tenant",
                "slug": "operator-disposable-tenant"
            }),
        )
        .await);
        let empty_id = empty["id"].as_str().unwrap();
        let deleted = call(
            app,
            "DELETE",
            &format!("/api/v1/platform/tenants/{empty_id}"),
            &operator,
            Value::Null,
        )
        .await;
        assert_eq!(deleted.0, StatusCode::OK, "{}", deleted.1);
        assert!(
            keycompute_db::TenantMembership::list_active_for_user(&f.db, f.operator)
                .await
                .unwrap()
                .is_empty()
        );
    })
    .await;
}

#[tokio::test]
async fn promoted_removed_member_keeps_implicit_platform_key_authority() {
    isolated(|f| async move {
        let state = f.state();
        let app = create_router(state.clone());
        let root = f.token(&state, f.root, false, 3600).await;
        let membership = keycompute_db::TenantMembership::find(&f.db, f.tenant, f.user)
            .await
            .unwrap()
            .unwrap();

        let removed = ok(call(
            app.clone(),
            "DELETE",
            &format!(
                "/api/v1/platform/tenants/{}/control/members/{}",
                f.tenant, f.user
            ),
            &root,
            json!({"expected_authz_version":membership.authz_version}),
        )
        .await);
        assert_eq!(removed["membership_status"], "removed");

        let promoted = ok(call(
            app.clone(),
            "PATCH",
            &format!("/api/v1/platform/users/{}", f.user),
            &root,
            json!({
                "platform_role":"operator",
                "reason":"promote a former tenant member"
            }),
        )
        .await);
        assert_eq!(promoted["platform_role"], "operator");
        let retained = keycompute_db::TenantMembership::find_any(&f.db, f.tenant, f.user)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.status, "removed");

        // A retained removal record is historical only. It must not cancel the
        // operator's implicit administrator authority over every tenant.
        let operator = f.token(&state, f.user, false, 3600).await;
        let requested = call(
            app.clone(),
            "POST",
            &format!("/api/v1/platform/tenants/{}/keys/issuance", f.tenant),
            &operator,
            json!({"owner_user_id":f.admin,"name":"former member requested key"}),
        )
        .await;
        assert_eq!(requested.0, StatusCode::ACCEPTED, "{}", requested.1);
        let intent_id = requested.1["intent"]["id"].as_str().unwrap();

        // Claiming revalidates the requester's authority, so this also covers
        // the second authorization check after the request is persisted.
        let owner = f.token(&state, f.admin, true, 3600).await;
        let claimed = ok(call(
            app,
            "POST",
            &format!("/api/v1/me/key-issuance/{intent_id}/claim"),
            &owner,
            json!({}),
        )
        .await);
        assert_eq!(claimed["outcome"], "claimed");
        assert_eq!(claimed["secret_returned_once"], true);
    })
    .await;
}

#[tokio::test]
async fn root_and_operator_create_ownerless_tenants_without_memberships() {
    isolated(|f| async move {
        let state = f.state();
        let app = create_router(state.clone());
        for (actor, slug) in [
            (f.root, "root-created-ownerless"),
            (f.operator, "operator-created-ownerless"),
        ] {
            let token = f.token(&state, actor, false, 3600).await;
            let created = ok(call(
                app.clone(),
                "POST",
                "/api/v1/platform/tenants",
                &token,
                json!({
                    "name": "Ownerless platform tenant",
                    "slug": slug
                }),
            )
            .await);
            let tenant_id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
            let tenant = keycompute_db::Tenant::find_by_id(&f.db, tenant_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(tenant.owner_user_id, None);
            assert_eq!(
                keycompute_db::Tenant::count_users(&f.db, tenant_id)
                    .await
                    .unwrap(),
                0
            );
            assert!(
                keycompute_db::TenantMembership::list_active_for_user(&f.db, actor)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
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
                PlatformIdentity::create_tenant(&f.db, scope, &audit(id), &request)
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
            PlatformIdentity::create_tenant(&f.db, scope, &audit(f.root), &request)
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
async fn queued_tenant_update_rechecks_platform_token_after_identity_fence_wait() {
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
            "UPDATE users SET token_version=token_version+1 WHERE id=$1",
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
        let request=CreateTenantRequest {name:"empty ownerless tenant".into(),slug:"empty-ownerless".into(),description:None,default_rpm_limit:None,default_tpm_limit:None};
        let created=PlatformIdentity::create_tenant(&f.db,scope,&audit(f.root),&request).await.unwrap();
        let tx=f.db.begin().await.unwrap();tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE").await.unwrap();
        tx.execute_unprepared("CREATE FUNCTION delete_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action IN ('user.delete','tenant.delete') THEN RAISE EXCEPTION 'isolated deletion audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER delete_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION delete_fault();").await.unwrap();
        assert!(PlatformIdentity::delete_user(&tx,scope,&audit(f.root),target.id).await.is_err());
        assert_eq!(User::find_by_id(&tx,target.id).await.unwrap().unwrap().status,"active");
        assert!(PlatformIdentity::delete_tenant(&tx,scope,&audit(f.root),created.id).await.is_err());
        assert!(keycompute_db::Tenant::find_by_id(&tx,created.id).await.unwrap().is_some());
        assert!(keycompute_db::TenantMembership::find(&tx,created.id,f.user).await.unwrap().is_none());
        tx.execute_unprepared("DROP TRIGGER delete_fault ON tenant_audit_events; DROP FUNCTION delete_fault();").await.unwrap();tx.commit().await.unwrap();
    }).await;
}
