//! Raw platform monitoring authorization and audit regressions with isolated PostgreSQL.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    routing::{get, post},
};
use chrono::Utc;
use integration_tests::{
    common::resolve_database_url,
    db::{create_test_api_key, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    AuditContext, CreateProduceAiKeyRequest, DbRouter, User,
    models::{
        financial_scope::{FinancialScope, FinancialSession},
        platform_monitoring::{MonitoringAction, MonitoringRead},
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
    let name = format!("kc_raw_monitoring_{}", Uuid::new_v4().simple());
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

fn bare(state: AppState) -> Router {
    use keycompute_server::handlers::admin_monitoring as m;
    Router::new()
        .route("/overview", get(m::get_monitoring_overview))
        .route("/requests", get(m::list_monitoring_requests))
        .route("/requests/{request_id}", get(m::get_monitoring_request))
        .route("/summary", get(m::get_monitoring_summary))
        .route("/targets", get(m::get_monitoring_target_health))
        .route("/probe", post(m::probe_monitoring_targets))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            keycompute_server::middleware::trace_id_middleware,
        ))
        .with_state(state)
}
async fn trace(f: &Fixture, tenant: Uuid, owner: Uuid, model: &str, currency: &str) -> Uuid {
    let id = Uuid::new_v4();
    let key_id = Uuid::new_v4();
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO gateway_requests(request_id,tenant_id,user_id,produce_ai_key_id,protocol,request_path,requested_model,is_stream,route_type,status,received_at,finished_at,billing_status) VALUES($1,$2,$3,$4,'openai','/v1/responses',$5,FALSE,'provider_account','succeeded',NOW()-INTERVAL '2 seconds',NOW()-INTERVAL '1 second','succeeded')",
        [id.into(),tenant.into(),owner.into(),key_id.into(),model.into()])).await.unwrap();
    keycompute_db::UsageLog::create(
        &f.db,
        &keycompute_db::CreateUsageLogRequest {
            request_id: id,
            tenant_id: tenant,
            user_id: owner,
            produce_ai_key_id: key_id,
            model_name: model.into(),
            provider_name: "openai".into(),
            account_id: Uuid::new_v4(),
            input_tokens: 7,
            output_tokens: 3,
            input_unit_price_snapshot: 1.into(),
            output_unit_price_snapshot: 1.into(),
            user_amount: 2.into(),
            currency: currency.into(),
            usage_source: "upstream".into(),
            status: "success".into(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
        },
    )
    .await
    .unwrap();
    id
}
#[tokio::test]
async fn bare_monitoring_handlers_require_root_not_tenant_operator_or_inference_credentials() {
    isolated(|f|async move {
        let state=f.state();let app=bare(state.clone());
        let admin=f.token(&state,f.admin,true,3600).await;let member=f.token(&state,f.user,true,3600).await;let op=f.token(&state,f.operator,false,3600).await;
        for token in [&admin,&member,&op,&f.key] {
            for path in ["/overview".to_owned(),"/requests".into(),format!("/requests/{}",Uuid::new_v4()),"/summary".into(),"/targets".into()] {
                let response=call(app.clone(),"GET",&path,token,Value::Null).await;
                assert_eq!(response.0,StatusCode::FORBIDDEN,"{path}: {}",response.1);
            }
            let response=call(app.clone(),"POST","/probe",token,json!({"account_ids":[]})).await;
            assert_eq!(response.0,StatusCode::FORBIDDEN,"{}",response.1);
        }
        let row=f.db.query_one(Statement::from_string(DbBackend::Postgres,"SELECT COUNT(*)::BIGINT n FROM tenant_audit_events WHERE action LIKE 'monitoring.%'")).await.unwrap().unwrap();
        assert_eq!(row.try_get::<i64>("","n").unwrap(),0);
        let root=f.token(&state,f.root,false,3600).await;
        assert_eq!(call(app,"GET","/overview",&root,Value::Null).await.0,StatusCode::OK);
    }).await;
}
#[tokio::test]
async fn canonical_and_retained_root_reads_share_scope_currency_and_server_audit_ids() {
    isolated(|f|async move {
        let other=create_test_tenant(&f.db,"other","monitoring").await;
        let a=trace(&f,f.tenant,f.user,"RAW_A_MARKER","CNY").await;
        let b=trace(&f,other.id,other.owner_user_id,"RAW_B_MARKER","USD").await;
        let state=f.state();let root=f.token(&state,f.root,false,3600).await;let app=create_router(state);
        for base in ["/api/v1/platform/monitoring","/api/v1/admin/monitoring"] {
            let page=call(app.clone(),"GET",&format!("{base}/requests?tenant_id={}&limit=1",f.tenant),&root,Value::Null).await;
            assert_eq!(page.0,StatusCode::OK,"{}",page.1);
            assert_eq!(page.1["items"].as_array().unwrap().len(),1);
            assert_eq!(page.1["items"][0]["request_id"],a.to_string());
            assert!(!page.1.to_string().contains("RAW_B_MARKER"));
            assert!(page.2["cache-control"].to_str().unwrap().contains("no-store"));
            let canonical:Uuid=page.2["x-request-id"].to_str().unwrap().parse().unwrap();
            let count=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::BIGINT n FROM tenant_audit_events WHERE request_id=$1 AND actor_user_id=$2 AND action='monitoring.requests'",[canonical.into(),f.root.into()])).await.unwrap().unwrap();
            assert_eq!(count.try_get::<i64>("","n").unwrap(),1);
            let detail=ok(call(app.clone(),"GET",&format!("{base}/requests/{b}"),&root,Value::Null).await);
            assert_eq!(detail["request"]["tenant_id"],other.id.to_string());assert_eq!(detail["usage"]["currency"],"USD");
            let summary=ok(call(app.clone(),"GET",&format!("{base}/summary?tenant_id={}",f.tenant),&root,Value::Null).await);
            assert_eq!(summary["summary"]["request_count"],1);
            assert!(summary.to_string().contains("CNY"));assert!(!summary.to_string().contains("USD"));
            for suffix in ["overview","targets/health"] {assert_eq!(call(app.clone(),"GET",&format!("{base}/{suffix}"),&root,Value::Null).await.0,StatusCode::OK);}
        }
    }).await;
}
#[tokio::test]
async fn audit_failure_withholds_raw_data_and_probe_acceptance() {
    isolated(|f|async move {
        trace(&f,f.tenant,f.user,"NEVER_RETURN_THIS_RAW_MODEL","CNY").await;
        f.db.execute_unprepared("CREATE FUNCTION monitoring_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action LIKE 'monitoring.%' THEN RAISE EXCEPTION 'isolated monitoring audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER monitoring_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION monitoring_fault();").await.unwrap();
        let state=f.state();let root=f.token(&state,f.root,false,3600).await;let app=bare(state);
        for path in ["/overview","/requests","/summary","/targets"] {
            let result=call(app.clone(),"GET",path,&root,Value::Null).await;
            assert_eq!(result.0,StatusCode::SERVICE_UNAVAILABLE,"{}",result.1);
            assert!(!result.1.to_string().contains("NEVER_RETURN_THIS_RAW_MODEL"));
            assert!(!result.1.to_string().contains("isolated monitoring audit failure"));
        }
        assert_eq!(call(app,"POST","/probe",&root,json!({"account_ids":[]})).await.0,StatusCode::SERVICE_UNAVAILABLE);
    }).await;
}
#[tokio::test]
async fn scoped_read_guard_rejects_forged_actors_roles_and_old_versions_without_grant_cache() {
    isolated(|f| async move {
        let good = f.scope(f.root).await;
        assert!(
            MonitoringRead::begin(&f.db, good, &audit(f.user))
                .await
                .is_err()
        );
        for id in [f.user, f.admin, f.operator] {
            assert!(
                MonitoringRead::begin(&f.db, f.scope(id).await, &audit(id))
                    .await
                    .is_err()
            );
        }
        let mut invalid = audit(f.root);
        invalid.request_id = None;
        assert!(MonitoringRead::begin(&f.db, good, &invalid).await.is_err());
        for kind in [
            CredentialKind::ApiKey,
            CredentialKind::Node,
            CredentialKind::System,
        ] {
            let mut invalid = audit(f.root);
            invalid.credential_kind = kind;
            assert!(MonitoringRead::begin(&f.db, good, &invalid).await.is_err());
        }
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET token_version=token_version+1 WHERE id=$1",
            [f.root.into()],
        ))
        .await
        .unwrap();
        assert!(
            MonitoringRead::begin(&f.db, good, &audit(f.root))
                .await
                .is_err()
        );
        MonitoringRead::begin(&f.db, f.scope(f.root).await, &audit(f.root))
            .await
            .unwrap()
            .finish(MonitoringAction::Overview, None, json!({}))
            .await
            .unwrap();
    })
    .await;
}
#[tokio::test]
async fn raw_detail_never_joins_usage_owned_by_a_different_tenant() {
    isolated(|f| async move {
        let id = trace(&f, f.tenant, f.user, "RAW_ORIGINAL", "CNY").await;
        let other = create_test_tenant(&f.db, "foreign-evidence", "monitoring").await;
        // A legacy inconsistent association must not leak through a request-ID-only join.
        assert!(
            f.db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE usage_logs SET tenant_id=$2,user_id=$3 WHERE request_id=$1",
                [id.into(), other.id.into(), other.owner_user_id.into()]
            ))
            .await
            .is_err()
        );
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM usage_logs WHERE request_id=$1",
            [id.into()],
        ))
        .await
        .unwrap();
        keycompute_db::UsageLog::create(
            &f.db,
            &keycompute_db::CreateUsageLogRequest {
                request_id: id,
                tenant_id: other.id,
                user_id: other.owner_user_id,
                produce_ai_key_id: Uuid::new_v4(),
                model_name: "foreign usage".into(),
                provider_name: "openai".into(),
                account_id: Uuid::new_v4(),
                input_tokens: 7,
                output_tokens: 3,
                input_unit_price_snapshot: 1.into(),
                output_unit_price_snapshot: 1.into(),
                user_amount: 2.into(),
                currency: "USD".into(),
                usage_source: "upstream".into(),
                status: "success".into(),
                started_at: Utc::now(),
                finished_at: Utc::now(),
            },
        )
        .await
        .unwrap();
        let state = f.state();
        let root = f.token(&state, f.root, false, 3600).await;
        let response = ok(call(
            bare(state),
            "GET",
            &format!("/requests/{id}"),
            &root,
            Value::Null,
        )
        .await);
        assert!(response["usage"].is_null());
        assert!(response["request"]["total_tokens"].is_null());
    })
    .await;
}

async fn single_connection(f: &Fixture) -> DatabaseConnection {
    let name: String =
        f.db.query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_database() AS name",
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
async fn wait_exact_lock(db: &DatabaseConnection, pid: i32, relation: &str) {
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT a.wait_event_type,EXISTS(SELECT 1 FROM pg_locks l WHERE l.pid=a.pid AND l.relation=to_regclass($2) AND l.granted) AS target FROM pg_stat_activity a WHERE a.pid=$1",[pid.into(),relation.into()])).await.unwrap().unwrap();
            if row.try_get::<Option<String>>("","wait_event_type").unwrap().as_deref()==Some("Lock") && row.try_get::<bool>("","target").unwrap(){break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("exact monitoring backend must reach the lock");
}
#[tokio::test]
async fn a_root_role_change_committed_while_read_lock_waits_cannot_release_a_stale_snapshot() {
    isolated(|f| async move {
        let single = single_connection(&f).await;
        let pid: i32 = single
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "pid")
            .unwrap();
        let scope = f.scope(f.root).await;
        let actor = audit(f.root);
        let hold = f.db.begin().await.unwrap();
        hold.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM users WHERE id=$1 FOR UPDATE",
            [f.root.into()],
        ))
        .await
        .unwrap();
        let db = single.clone();
        let request = tokio::spawn(async move { MonitoringRead::begin(&db, scope, &actor).await });
        wait_exact_lock(&f.db, pid, "users").await;
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
        let Err(error) = request.await.unwrap() else {
            panic!("stale root unexpectedly acquired diagnostic authority")
        };
        assert!(
            error.to_string().contains("could not serialize access"),
            "expected committed-role conflict, got {error}"
        );
        single.close().await.unwrap();
    })
    .await;
}
#[tokio::test]
async fn monitoring_expiry_after_audit_lock_wait_withholds_payload_and_rolls_back_audit() {
    isolated(|f|async move {
        let single=single_connection(&f).await;
        let pid:i32=single.query_one(Statement::from_string(DbBackend::Postgres,"SELECT pg_backend_pid() AS pid")).await.unwrap().unwrap().try_get("","pid").unwrap();
        let user=User::find_by_id(&f.db,f.root).await.unwrap().unwrap();
        let expires=Utc::now().timestamp()+4;
        let scope=FinancialScope::platform_global(PlatformScope::checked(f.root,PlatformRole::Root).unwrap(),FinancialSession{user_id:f.root,credential_kind:CredentialKind::Jwt,token_version:user.token_version,expires_at:expires,selected:None}).unwrap();
        let actor=audit(f.root);let request_id=actor.request_id.unwrap();
        let read=MonitoringRead::begin(&single,scope,&actor).await.unwrap();
        // Test only: outwait the lease rather than succeeding through lock timeout.
        read.connection().execute_unprepared("SET LOCAL lock_timeout='10s'; SET LOCAL statement_timeout='10s'").await.unwrap();
        // Hold only an isolated audit relation, not an authorization parent.
        let hold=f.db.begin().await.unwrap();hold.execute_unprepared("LOCK TABLE tenant_audit_events IN ACCESS EXCLUSIVE MODE").await.unwrap();
        let request=tokio::spawn(async move {read.finish(MonitoringAction::Requests,None,json!({})).await});
        // The pending RowExclusiveLock itself identifies the exact awaited relation.
        tokio::time::timeout(Duration::from_secs(5),async {loop {
            let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT EXISTS(SELECT 1 FROM pg_locks WHERE pid=$1 AND relation='tenant_audit_events'::regclass AND NOT granted) AS waiting",[pid.into()])).await.unwrap().unwrap();
            if row.try_get::<bool>("","waiting").unwrap(){break;}tokio::time::sleep(Duration::from_millis(10)).await;
        }}).await.unwrap();
        while Utc::now().timestamp()<expires {tokio::time::sleep(Duration::from_millis(10)).await;}
        hold.commit().await.unwrap();
        let error=request.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("financial_authority_invalid"),"expected post-audit expiry, got {error}");
        let count=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*)::BIGINT n FROM tenant_audit_events WHERE request_id=$1",[request_id.into()])).await.unwrap().unwrap();assert_eq!(count.try_get::<i64>("","n").unwrap(),0);
        single.close().await.unwrap();
    }).await;
}
#[tokio::test]
async fn diagnostic_reads_neither_acquire_global_write_fence_nor_wait_for_each_other() {
    isolated(|f| async move {
        let fence = f.db.begin().await.unwrap();
        fence
            .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
            .await
            .unwrap();
        let scope = f.scope(f.root).await;
        let first_actor = audit(f.root);
        let second_actor = audit(f.root);
        let (first, second) = tokio::join!(
            MonitoringRead::begin(&f.db, scope, &first_actor),
            MonitoringRead::begin(&f.db, scope, &second_actor)
        );
        first
            .unwrap()
            .finish(MonitoringAction::Overview, None, json!({}))
            .await
            .unwrap();
        second
            .unwrap()
            .finish(MonitoringAction::Summary, None, json!({}))
            .await
            .unwrap();
        fence.rollback().await.unwrap();
    })
    .await;
}

#[derive(Clone)]
struct ProbeUpstream {
    db: DatabaseConnection,
    actor: Uuid,
    revoke: bool,
    chat_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    response_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
struct ProbeServer {
    url: String,
    shared: ProbeUpstream,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for ProbeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn chat_probe(
    axum::extract::State(state): axum::extract::State<ProbeUpstream>,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    state
        .chat_calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if state.revoke {
        state
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET token_version=token_version+1 WHERE id=$1",
                [state.actor.into()],
            ))
            .await
            .unwrap();
    }
    axum::Json(
        json!({"id":"chatcmpl-probe","object":"chat.completion","created":1,"model":body["model"],"choices":[{"index":0,"message":{"role":"assistant","content":"pong"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
    )
}
async fn responses_probe(
    axum::extract::State(state): axum::extract::State<ProbeUpstream>,
) -> axum::Json<Value> {
    state
        .response_calls
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    axum::Json(
        json!({"id":"resp_probe","object":"response","created_at":1,"status":"completed","model":"probe-model","output":[],"usage":{"input_tokens":1,"output_tokens":0,"total_tokens":1}}),
    )
}
async fn probe_server(f: &Fixture, revoke: bool) -> ProbeServer {
    let shared = ProbeUpstream {
        db: f.db.clone(),
        actor: f.root,
        revoke,
        chat_calls: Default::default(),
        response_calls: Default::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/v1/chat/completions", post(chat_probe))
        .route("/v1/responses", post(responses_probe))
        .with_state(shared.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    ProbeServer {
        url: format!("http://{addr}/v1"),
        shared,
        task,
    }
}
async fn create_probe_account(
    f: &Fixture,
    state: &AppState,
    server: &ProbeServer,
    dual: bool,
) -> Uuid {
    let token = f.token(state, f.root, false, 3600).await;
    let result=call(create_router(state.clone()),"POST","/api/v1/accounts",&token,json!({
        "tenant_id":f.tenant,"name":"isolated probe","provider":"openai","api_key":"PROBE_SECRET_NOT_FOR_CLIENTS",
        "api_base":server.url,"models":["probe-model"],"api_capabilities":if dual {vec!["chat_completions","responses"]} else {vec!["chat_completions"]},
        "rpm_limit":100,"tpm_limit":100000,"priority":1,"pool_enabled":true,
    })).await;
    ok(result)["id"].as_str().unwrap().parse().unwrap()
}
#[tokio::test]
async fn batch_probe_revalidates_original_session_between_physical_capability_requests() {
    isolated(|f| async move {
        let state = f.state();
        let server = probe_server(&f, true).await;
        let account = create_probe_account(&f, &state, &server, true).await;
        let token = f.token(&state, f.root, false, 3600).await;
        let result = call(
            create_router(state),
            "POST",
            "/api/v1/platform/monitoring/targets/probe",
            &token,
            json!({"account_ids":[account]}),
        )
        .await;
        assert_eq!(result.0, StatusCode::FORBIDDEN, "{}", result.1);
        assert_eq!(
            server
                .shared
                .chat_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            server
                .shared
                .response_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let stored = keycompute_db::Account::find_by_id(&f.db, account)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.health_failure_count, 0);
        assert!(stored.last_probe_at.is_none());
        assert!(
            !result
                .1
                .to_string()
                .contains("PROBE_SECRET_NOT_FOR_CLIENTS")
        );
    })
    .await;
}
#[tokio::test]
async fn probe_intent_audit_failure_prevents_network_and_repeated_ids_probe_once() {
    isolated(|f|async move {
        let state=f.state();let server=probe_server(&f,false).await;let account=create_probe_account(&f,&state,&server,false).await;
        let token=f.token(&state,f.root,false,3600).await;let app=create_router(state);
        f.db.execute_unprepared("CREATE FUNCTION probe_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='monitoring.probe_request' THEN RAISE EXCEPTION 'isolated probe audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER probe_fault BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION probe_fault();").await.unwrap();
        assert_eq!(call(app.clone(),"POST","/api/v1/platform/monitoring/targets/probe",&token,json!({"account_ids":[account]})).await.0,StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(server.shared.chat_calls.load(std::sync::atomic::Ordering::SeqCst),0);
        f.db.execute_unprepared("DROP TRIGGER probe_fault ON tenant_audit_events; DROP FUNCTION probe_fault();").await.unwrap();
        let response=call(app.clone(),"POST","/api/v1/platform/monitoring/targets/probe",&token,json!({"account_ids":[account,account]})).await;
        assert_eq!(response.0,StatusCode::OK,"{}",response.1);assert_eq!(response.1["results"][0]["success"],true);
        assert_eq!(server.shared.chat_calls.load(std::sync::atomic::Ordering::SeqCst),1);
        assert!(!response.1.to_string().contains("PROBE_SECRET_NOT_FOR_CLIENTS"));
        assert_eq!(call(app.clone(),"POST","/api/v1/platform/monitoring/targets/probe",&token,json!({"account_ids":[Uuid::nil()]})).await.0,StatusCode::BAD_REQUEST);
        assert_eq!(call(app,"POST","/api/v1/platform/monitoring/targets/probe",&token,json!({"account_ids":(0..51).map(|_|Uuid::new_v4()).collect::<Vec<_>>()})).await.0,StatusCode::BAD_REQUEST);
    }).await;
}

#[tokio::test]
async fn target_health_filter_uses_resource_owner_tenant_instead_of_returning_every_tenant() {
    isolated(|f|async move {
        let state=f.state();let server=probe_server(&f,false).await;
        let own=create_probe_account(&f,&state,&server,false).await;
        let other=create_test_tenant(&f.db,"health-foreign","monitoring").await;
        let token=f.token(&state,f.root,false,3600).await;let app=create_router(state);
        let foreign=ok(call(app.clone(),"POST","/api/v1/accounts",&token,json!({"tenant_id":other.id,"name":"foreign provider","provider":"openai","api_key":"foreign-secret","api_base":server.url,"models":["probe-model"],"api_capabilities":["chat_completions"],"rpm_limit":60,"tpm_limit":100000})).await)["id"].as_str().unwrap().to_owned();
        let own_page=ok(call(app.clone(),"GET",&format!("/api/v1/platform/monitoring/targets/health?tenant_id={}",f.tenant),&token,Value::Null).await);
        assert_eq!(own_page["providers"].as_array().unwrap().len(),1);assert_eq!(own_page["providers"][0]["id"],own.to_string());assert!(!own_page.to_string().contains(&foreign));
        let all=ok(call(app.clone(),"GET","/api/v1/platform/monitoring/targets/health",&token,Value::Null).await);assert_eq!(all["providers"].as_array().unwrap().len(),2);
        assert_eq!(call(app,"GET","/api/v1/platform/monitoring/targets/health?tenant_id=00000000-0000-0000-0000-000000000000",&token,Value::Null).await.0,StatusCode::BAD_REQUEST);
    }).await;
}

#[tokio::test]
async fn console_probe_material_checks_selected_tenant_owner_and_current_config_without_shared_secret_grants()
 {
    isolated(|f|async move {
        use keycompute_db::models::financial_scope::FinancialMembership;
        let state=f.state();let server=probe_server(&f,false).await;let id=create_probe_account(&f,&state,&server,false).await;
        let tenant=keycompute_db::Tenant::find_by_id(&f.db,f.tenant).await.unwrap().unwrap();
        let member=keycompute_db::TenantMembership::find(&f.db,f.tenant,f.admin).await.unwrap().unwrap();
        let user=User::find_by_id(&f.db,f.admin).await.unwrap().unwrap();
        let selected=FinancialSession{user_id:f.admin,credential_kind:CredentialKind::Jwt,token_version:user.token_version,expires_at:Utc::now().timestamp()+3600,selected:Some(FinancialMembership{tenant_id:f.tenant,tenant_role:keycompute_types::TenantRole::Admin,tenant_authz_version:tenant.authz_version,membership_authz_version:member.authz_version})};
        let scope=FinancialScope::tenant_admin(keycompute_types::TenantScope::checked(f.tenant,f.admin,keycompute_types::TenantRole::Admin).unwrap(),selected).unwrap();
        let own=keycompute_db::Account::load_console_probe(&f.db,scope,id).await.unwrap().unwrap();
        assert!(keycompute_db::Account::console_probe_is_current(&f.db,scope,&own,false).await.unwrap());
        let other=create_test_tenant(&f.db,"foreign-probe-owner","monitoring").await;
        let token=f.token(&state,f.root,false,3600).await;
        let other_id:Uuid=ok(call(create_router(state),"POST","/api/v1/accounts",&token,json!({"tenant_id":other.id,"name":"shared but not owned","provider":"openai","api_key":"secret","api_base":server.url,"models":["probe-model"],"api_capabilities":["chat_completions"],"visibility":"global","rpm_limit":60,"tpm_limit":100000})).await)["id"].as_str().unwrap().parse().unwrap();
        assert!(keycompute_db::Account::load_console_probe(&f.db,scope,other_id).await.unwrap().is_none());
        f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE accounts SET endpoint='http://127.0.0.1:1/v1' WHERE id=$1",[id.into()])).await.unwrap();
        assert!(!keycompute_db::Account::console_probe_is_current(&f.db,scope,&own,false).await.unwrap());
        f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",[f.tenant.into(),f.admin.into()])).await.unwrap();
        assert!(keycompute_db::Account::load_console_probe(&f.db,scope,id).await.unwrap().is_none());
    }).await;
}
