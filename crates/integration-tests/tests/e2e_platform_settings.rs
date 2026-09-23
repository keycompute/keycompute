//! Real platform authority and policy transactions. Each case has a disposable DB.
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
    AuditContext, CreateProduceAiKeyRequest, DbRouter, SystemSetting, User,
    models::{
        financial_scope::{FinancialScope, FinancialSession},
        node_tip_setting::{TipRatioSetting, UpdateTipRatio},
        system_setting::SettingsPolicy,
    },
};
use keycompute_server::{AppState, create_router};
use keycompute_types::{CredentialKind, PlatformRole, PlatformScope};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};
use std::{collections::HashMap, future::Future, time::Duration};
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
    let name = format!("kc_settings_{}", Uuid::new_v4().simple());
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
fn policy() -> SettingsPolicy {
    SettingsPolicy {
        public_base_url_configured: true,
    }
}
fn changes(entries: &[(&str, &str)]) -> HashMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
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
async fn global_root_and_bare_handlers_reject_tenant_roles_keys_and_operators() {
    isolated(|f| async move {
        let state = f.state();
        let app = create_router(state.clone());
        let root = f.token(&state, f.root, false, 3600).await;
        let admin = f.token(&state, f.admin, true, 3600).await;
        let member = f.token(&state, f.user, true, 3600).await;
        let operator = f.token(&state, f.operator, false, 3600).await;
        let bare = Router::new()
            .route(
                "/settings",
                get(keycompute_server::handlers::admin_settings::get_system_settings)
                    .put(keycompute_server::handlers::admin_settings::update_system_settings),
            )
            .with_state(state.clone());
        for path in [
            "/api/v1/platform/settings",
            "/api/v1/settings",
            "/api/v1/platform/tips/settings/ratio",
            "/api/v1/admin/tips/settings/ratio",
        ] {
            let current = call(app.clone(), "GET", path, &root, Value::Null).await;
            assert_eq!(current.0, StatusCode::OK, "{} {path}", current.1);
            assert!(
                current.2["cache-control"]
                    .to_str()
                    .unwrap()
                    .contains("no-store")
            );
            for token in [&admin, &member, &operator, &f.key] {
                let denied = call(app.clone(), "GET", path, token, Value::Null).await;
                assert!(
                    matches!(denied.0, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
                    "{path} {} {}",
                    denied.0,
                    denied.1
                );
            }
        }
        ok(call(bare.clone(), "GET", "/settings", &root, Value::Null).await);
        for token in [&admin, &member, &operator, &f.key] {
            assert!(matches!(
                call(bare.clone(), "GET", "/settings", token, Value::Null)
                    .await
                    .0,
                StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
            ));
        }
    })
    .await;
}

#[tokio::test]
async fn settings_projection_masks_secrets_before_reading_and_rejects_arbitrary_keys() {
    isolated(|f|async move {
    let state=f.state();let root=f.token(&state,f.root,false,3600).await;let app=create_router(state);
    f.db.execute_unprepared("INSERT INTO system_settings(key,value,value_type,description,is_sensitive) VALUES('fixture_secret','NEVER-RETURN-SECRET','bool','NEVER-RETURN-DESCRIPTION',TRUE)").await.unwrap();
    f.db.execute_unprepared("INSERT INTO system_settings(key,value,value_type,is_sensitive) VALUES('unclassified_secret','NEVER-RETURN-UNFLAGGED','string',FALSE)").await.unwrap();
    let scope=f.scope(f.root).await;
    let rows=SystemSetting::find_all_platform(&f.db,scope).await.unwrap();
    assert!(!serde_json::to_string(&rows).unwrap().contains("NEVER-RETURN"));
    let list=ok(call(app.clone(),"GET","/api/v1/platform/settings",&root,Value::Null).await);
    assert_eq!(list["fixture_secret"],"[REDACTED]");
    assert_eq!(list["unclassified_secret"],"[REDACTED]");
    let detail=ok(call(app.clone(),"GET","/api/v1/platform/settings/fixture_secret",&root,Value::Null).await);
    assert_eq!(detail["value"],"[REDACTED]");assert!(detail["description"].is_null());
    assert_eq!(call(app.clone(),"PUT","/api/v1/platform/settings/fixture_secret",&root,json!({"value":"replacement"})).await.0,StatusCode::BAD_REQUEST);
    f.db.execute_unprepared("UPDATE system_settings SET is_sensitive=TRUE WHERE key='site_name'").await.unwrap();
    assert_eq!(call(app,"PUT","/api/v1/platform/settings/site_name",&root,json!({"value":"replacement"})).await.0,StatusCode::FORBIDDEN);
    let row=SystemSetting::find_by_key(&f.db,"site_name").await.unwrap().unwrap();assert_eq!(row.value,"KeyCompute");
}).await;
}

#[tokio::test]
async fn setting_batches_validate_inside_the_dao_and_preserve_atomic_payment_limits() {
    isolated(|f|async move {
    let scope=f.scope(f.root).await;let actor=audit(f.root);
    let invalid=changes(&[("site_name","must roll back"),("min_recharge_amount","100"),("max_recharge_amount","10")]);
    assert!(SystemSetting::update_platform_batch(&f.db,scope,&actor,&invalid,policy()).await.is_err());
    assert_eq!(SystemSetting::find_by_key(&f.db,"site_name").await.unwrap().unwrap().value,"KeyCompute");
    for invalid in [changes(&[("node_tip_ratio","0.1")]),changes(&[("unregistered_field","secret")]),changes(&[("default_currency","USD")]),changes(&[("default_user_role","root")]),changes(&[("login_failed_limit","0")])] {
        assert!(SystemSetting::update_platform_batch(&f.db,scope,&actor,&invalid,policy()).await.is_err());
    }
    assert!(SystemSetting::update_platform_batch(&f.db,scope,&actor,&changes(&[("distribution_enabled","true")]),SettingsPolicy{public_base_url_configured:false}).await.is_err());
    let valid=changes(&[("site_name","Tenant platform"),("min_recharge_amount","1.23"),("max_recharge_amount","999.99")]);
    let first=SystemSetting::update_platform_batch(&f.db,scope,&actor,&valid,policy()).await.unwrap();
    let second=SystemSetting::update_platform_batch(&f.db,scope,&actor,&valid,policy()).await.unwrap();
    assert_eq!(first.iter().map(|r|r.updated_at).collect::<Vec<_>>(),second.iter().map(|r|r.updated_at).collect::<Vec<_>>());
    let n=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE actor_user_id=$1 AND action='settings.update'",[f.root.into()])).await.unwrap().unwrap().try_get::<i64>("","n").unwrap();assert_eq!(n,3);
}).await;
}

#[tokio::test]
async fn generic_settings_audit_failure_rolls_back_all_keys_even_after_outer_commit() {
    isolated(|f|async move {
    let scope=f.scope(f.root).await;let tx=f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE").await.unwrap();
    tx.execute_unprepared("CREATE FUNCTION settings_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='settings.update' AND NEW.resource_id='site_name' THEN RAISE EXCEPTION 'isolated settings failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER settings_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION settings_fault();").await.unwrap();
    let old=SystemSetting::find_by_key(&tx,"site_description").await.unwrap().unwrap();
    assert!(SystemSetting::update_platform_batch(&tx,scope,&audit(f.root),&changes(&[("site_description","first changed key"),("site_name","second changed key")]),policy()).await.is_err());
    let after=SystemSetting::find_by_key(&tx,"site_description").await.unwrap().unwrap();assert_eq!(after.value,old.value);assert_eq!(after.updated_at,old.updated_at);
    tx.execute_unprepared("DROP TRIGGER settings_fault ON tenant_audit_events; DROP FUNCTION settings_fault();").await.unwrap();tx.commit().await.unwrap();
    assert_eq!(SystemSetting::find_by_key(&f.db,"site_name").await.unwrap().unwrap().value,"KeyCompute");
}).await;
}

#[tokio::test]
async fn ratio_cas_and_generic_paths_cannot_bypass_policy_or_audit() {
    isolated(|f|async move {
    let scope=f.scope(f.root).await;let old=TipRatioSetting::read(&f.db,scope).await.unwrap();
    let state=f.state();let root=f.token(&state,f.root,false,3600).await;let app=create_router(state);
    let path="/api/v1/platform/tips/settings/ratio";
    let changed=ok(call(app.clone(),"PUT",path,&root,json!({"ratio":"0.1234","expected_updated_at":old.updated_at,"reason":"ratio validation"})).await);
    let noop=ok(call(app.clone(),"PUT",path,&root,json!({"ratio":"0.12340","expected_updated_at":changed["updated_at"],"reason":"equivalent ratio"})).await);assert_eq!(noop["updated_at"],changed["updated_at"]);
    assert_eq!(call(app.clone(),"PUT",path,&root,json!({"ratio":"0.8","expected_updated_at":old.updated_at,"reason":"stale version"})).await.0,StatusCode::CONFLICT);
    for ratio in ["0","-1","1.0001","0.12345"] {
        assert_eq!(call(app.clone(),"PUT",path,&root,json!({"ratio":ratio,"expected_updated_at":changed["updated_at"],"reason":"invalid ratio"})).await.0,StatusCode::BAD_REQUEST);
    }
    for base in ["/api/v1/settings","/api/v1/platform/settings"] {
        assert_eq!(call(app.clone(),"PUT",base,&root,json!({"node_tip_ratio":"0.8"})).await.0,StatusCode::BAD_REQUEST);
        assert_eq!(call(app.clone(),"PUT",&format!("{base}/node_tip_ratio"),&root,json!({"value":"0.8"})).await.0,StatusCode::BAD_REQUEST);
    }
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT request_id,metadata FROM tenant_audit_events WHERE actor_user_id=$1 AND action='settings.node_tip_ratio'",[f.root.into()])).await.unwrap().unwrap();
    assert!(!row.try_get::<Uuid>("","request_id").unwrap().is_nil());assert_eq!(row.try_get::<Value>("","metadata").unwrap()["after"]["ratio"],"0.1234");
}).await;
}

#[tokio::test]
async fn concurrent_root_ratio_commands_have_one_version_winner() {
    isolated(|f| async move {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [f.admin.into()],
        ))
        .await
        .unwrap();
        let a = f.scope(f.root).await;
        let b = f.scope(f.admin).await;
        let old = TipRatioSetting::read(&f.db, a).await.unwrap();
        let ac = UpdateTipRatio {
            ratio: "0.1234".into(),
            expected_updated_at: old.updated_at,
            reason: "first contender".into(),
        };
        let bc = UpdateTipRatio {
            ratio: "0.5678".into(),
            expected_updated_at: old.updated_at,
            reason: "second contender".into(),
        };
        let aa = audit(f.root);
        let ba = audit(f.admin);
        let (one, two) = tokio::join!(
            TipRatioSetting::update(&f.db, a, &aa, &ac),
            TipRatioSetting::update(&f.db, b, &ba, &bc)
        );
        assert_ne!(one.is_ok(), two.is_ok());
        let current = TipRatioSetting::read(&f.db, a).await.unwrap();
        assert!(current.updated_at > old.updated_at);
    })
    .await;
}

#[tokio::test]
async fn forged_scopes_audit_actors_and_revoked_sessions_cannot_read_or_write_settings() {
    isolated(|f| async move {
        let change = changes(&[("site_name", "forbidden")]);
        for id in [f.user, f.admin, f.operator] {
            let scope = f.scope(id).await;
            assert!(
                SystemSetting::find_all_platform(&f.db, scope)
                    .await
                    .is_err()
            );
            assert!(
                SystemSetting::update_platform_batch(&f.db, scope, &audit(id), &change, policy())
                    .await
                    .is_err()
            );
        }
        let scope = f.scope(f.root).await;
        assert!(
            SystemSetting::update_platform_batch(&f.db, scope, &audit(f.admin), &change, policy())
                .await
                .is_err()
        );
        for credential in [
            CredentialKind::ApiKey,
            CredentialKind::Node,
            CredentialKind::System,
        ] {
            let mut actor = audit(f.root);
            actor.credential_kind = credential;
            assert!(
                SystemSetting::update_platform_batch(&f.db, scope, &actor, &change, policy())
                    .await
                    .is_err()
            );
        }
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET token_version=token_version+1 WHERE id=$1",
            [f.root.into()],
        ))
        .await
        .unwrap();
        assert!(
            SystemSetting::find_all_platform(&f.db, scope)
                .await
                .is_err()
        );
        assert!(
            SystemSetting::update_platform_batch(&f.db, scope, &audit(f.root), &change, policy())
                .await
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn system_setting_version_is_monotonic_for_old_runtime_transactions_and_noops() {
    isolated(|f|async move {
    let older=f.db.begin().await.unwrap();older.execute_unprepared("SELECT transaction_timestamp()").await.unwrap();
    let row=SystemSetting::update_platform_batch(&f.db,f.scope(f.root).await,&audit(f.root),&changes(&[("site_name","new root value")]),policy()).await.unwrap().remove(0);
    older.execute_unprepared("UPDATE system_settings SET value='later runtime value',updated_at=NOW() WHERE key='site_name'").await.unwrap();older.commit().await.unwrap();
    let current=SystemSetting::find_by_key(&f.db,"site_name").await.unwrap().unwrap();assert!(current.updated_at>row.updated_at);
    f.db.execute_unprepared("UPDATE system_settings SET updated_at=clock_timestamp() WHERE key='site_name'").await.unwrap();
    assert_eq!(SystemSetting::find_by_key(&f.db,"site_name").await.unwrap().unwrap().updated_at,current.updated_at);
    assert!(f.db.execute_unprepared("UPDATE system_settings SET key='changed-key' WHERE key='site_name'").await.is_err());
    SystemSetting::init_default_settings(&f.db).await.unwrap();SystemSetting::init_default_settings(&f.db).await.unwrap();
    assert!(SystemSetting::find_by_key(&f.db,"default_user_role").await.unwrap().is_none());
}).await;
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
async fn queued_settings_write_rechecks_root_role_before_modifying_any_key() {
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
        let request = tokio::spawn(async move {
            call(
                create_router(state),
                "PUT",
                "/api/v1/platform/settings",
                &token,
                json!({"site_name":"must not apply"}),
            )
            .await
        });
        wait_for_exact_backend(&f.db, pid, "identity_admin_fence").await;
        // Keep another root, preserving the database's last-root invariant.
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
        let result = request.await.unwrap();
        assert_eq!(result.0, StatusCode::FORBIDDEN, "{}", result.1);
        assert_eq!(
            SystemSetting::find_by_key(&f.db, "site_name")
                .await
                .unwrap()
                .unwrap()
                .value,
            "KeyCompute"
        );
        single.close().await.unwrap();
    })
    .await;
}
#[tokio::test]
async fn settings_expiry_after_row_wait_rolls_back_configuration_and_audit() {
    isolated(|f|async move {
    let single=single_connection(&f).await;
    let pid:i32=single.query_one(Statement::from_string(DbBackend::Postgres,"SELECT pg_backend_pid() AS pid".to_owned())).await.unwrap().unwrap().try_get("","pid").unwrap();
    let user=User::find_by_id(&f.db,f.root).await.unwrap().unwrap();let expires=Utc::now().timestamp()+2;
    let scope=FinancialScope::platform_global(PlatformScope::checked(f.root,PlatformRole::Root).unwrap(),FinancialSession {user_id:f.root,credential_kind:CredentialKind::Jwt,token_version:user.token_version,expires_at:expires,selected:None}).unwrap();
    let hold=f.db.begin().await.unwrap();hold.execute_unprepared("SELECT key FROM system_settings WHERE key='site_name' FOR UPDATE").await.unwrap();
    let worker_db=single.clone();let actor=audit(f.root);
    let request=tokio::spawn(async move {SystemSetting::update_platform_batch(&worker_db,scope,&actor,&changes(&[("site_name","expired command")]),policy()).await});
    wait_for_exact_backend(&f.db,pid,"system_settings").await;
    while Utc::now().timestamp()<expires {tokio::time::sleep(Duration::from_millis(20)).await;}
    hold.commit().await.unwrap();
    let error=request.await.unwrap().unwrap_err();assert!(error.to_string().contains("financial_authority_invalid"),"{error}");
    assert_eq!(SystemSetting::find_by_key(&f.db,"site_name").await.unwrap().unwrap().value,"KeyCompute");
    let n:i64=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::bigint AS n FROM tenant_audit_events WHERE actor_user_id=$1 AND action='settings.update'",[f.root.into()])).await.unwrap().unwrap().try_get("","n").unwrap();assert_eq!(n,0);
    single.close().await.unwrap();
}).await;
}
#[tokio::test]
async fn ratio_audit_failure_rolls_back_the_policy_even_after_outer_commit() {
    isolated(|f|async move {
    let scope=f.scope(f.root).await;let old=TipRatioSetting::read(&f.db,scope).await.unwrap();
    let tx=f.db.begin().await.unwrap();tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE").await.unwrap();
    tx.execute_unprepared("CREATE FUNCTION ratio_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='settings.node_tip_ratio' THEN RAISE EXCEPTION 'isolated ratio audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER ratio_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION ratio_fault();").await.unwrap();
    assert!(TipRatioSetting::update(&tx,scope,&audit(f.root),&UpdateTipRatio {ratio:"0.1234".into(),expected_updated_at:old.updated_at,reason:"audit atomicity".into()}).await.is_err());
    let untouched=TipRatioSetting::read(&tx,scope).await.unwrap();assert_eq!(untouched.ratio,old.ratio);assert_eq!(untouched.updated_at,old.updated_at);
    tx.execute_unprepared("DROP TRIGGER ratio_fault ON tenant_audit_events; DROP FUNCTION ratio_fault();").await.unwrap();tx.commit().await.unwrap();
}).await;
}
#[tokio::test]
async fn concurrent_partial_payment_limit_updates_cannot_commit_an_invalid_pair() {
    isolated(|f| async move {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [f.admin.into()],
        ))
        .await
        .unwrap();
        let a = f.scope(f.root).await;
        let b = f.scope(f.admin).await;
        let aa = audit(f.root);
        let ba = audit(f.admin);
        let av = changes(&[("min_recharge_amount", "100")]);
        let bv = changes(&[("max_recharge_amount", "50")]);
        let (one, two) = tokio::join!(
            SystemSetting::update_platform_batch(&f.db, a, &aa, &av, policy()),
            SystemSetting::update_platform_batch(&f.db, b, &ba, &bv, policy())
        );
        assert_ne!(one.is_ok(), two.is_ok());
        let min = SystemSetting::find_by_key(&f.db, "min_recharge_amount")
            .await
            .unwrap()
            .unwrap()
            .value
            .parse::<rust_decimal::Decimal>()
            .unwrap();
        let max = SystemSetting::find_by_key(&f.db, "max_recharge_amount")
            .await
            .unwrap()
            .unwrap()
            .value
            .parse::<rust_decimal::Decimal>()
            .unwrap();
        assert!(min <= max);
    })
    .await;
}
