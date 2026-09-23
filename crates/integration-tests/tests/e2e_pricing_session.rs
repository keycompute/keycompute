//! Real HTTP regressions for pricing effects after the original console proof expires.
//! Deliberate lock/audit waits use independent disposable local databases only.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::db::{create_test_pool, create_test_tenant, create_test_user};
use keycompute_db::{DbRouter, Tenant, TenantMembership, User};
use keycompute_server::{AppState, create_router};
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, TransactionTrait,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    state: AppState,
    source: Tenant,
    target: Tenant,
    root: Uuid,
    database_url: String,
}
#[derive(Clone, Copy)]
enum Scope {
    Tenant,
    PlatformTenant,
    PlatformGlobal,
}
impl Scope {
    fn platform(self) -> bool {
        !matches!(self, Self::Tenant)
    }
    fn name(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::PlatformTenant => "platform-tenant",
            Self::PlatformGlobal => "platform-global",
        }
    }
}
impl Fixture {
    async fn new(db: DatabaseConnection, database_url: String) -> Self {
        keycompute_db::initialize_schema(&db).await.unwrap();
        let boot = db.begin().await.unwrap();
        User::bootstrap_root(&boot, "pricing-proof-bootstrap@fixture.invalid", None)
            .await
            .unwrap();
        boot.commit().await.unwrap();
        let run = Uuid::new_v4().to_string();
        let source = create_test_tenant(&db, "pricing-proof-source", &run).await;
        let target = create_test_tenant(&db, "pricing-proof-target", &run).await;
        let actor = create_test_user(&db, source.id, "pricing-proof-root", &run).await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [actor.id.into()],
        ))
        .await
        .unwrap();
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        db.execute_unprepared(r#"
            CREATE SEQUENCE pricing_proof_marker;
            CREATE TABLE pricing_proof_delay_state(expires BIGINT NOT NULL, enabled BOOLEAN NOT NULL);
            INSERT INTO pricing_proof_delay_state VALUES(0,FALSE);
            CREATE FUNCTION pricing_proof_delay() RETURNS TRIGGER LANGUAGE plpgsql AS $$
            DECLARE deadline BIGINT; active BOOLEAN; called BOOLEAN;
            BEGIN
                SELECT expires,enabled INTO deadline,active FROM pricing_proof_delay_state;
                IF active AND NEW.resource_type='pricing_model' THEN
                    SELECT is_called INTO called FROM pricing_proof_marker;
                    PERFORM nextval('pricing_proof_marker');
                    IF NOT called THEN
                        PERFORM pg_sleep(GREATEST(0::double precision,deadline::double precision-EXTRACT(EPOCH FROM clock_timestamp())::double precision)+0.05);
                    END IF;
                END IF;
                RETURN NEW;
            END $$;
            CREATE TRIGGER pricing_proof_delay BEFORE INSERT ON tenant_audit_events
              FOR EACH ROW EXECUTE FUNCTION pricing_proof_delay();
        "#).await.unwrap();
        Self {
            db,
            state,
            source,
            target,
            root: actor.id,
            database_url,
        }
    }
    async fn token(&self, user: Uuid, selected: Option<Uuid>, lifetime: i64) -> (String, i64) {
        let user = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        let (tenant_version, member_version) = if let Some(id) = selected {
            let t = Tenant::find_by_id(&self.db, id).await.unwrap().unwrap();
            let m = TenantMembership::find(&self.db, id, user.id)
                .await
                .unwrap()
                .unwrap();
            (Some(t.authz_version), Some(m.authz_version))
        } else {
            (None, None)
        };
        let validator = self.state.auth.get_jwt_validator().unwrap();
        let token = validator
            .generate_identity_token(
                user.id,
                selected,
                user.token_version,
                tenant_version,
                member_version,
                lifetime,
            )
            .unwrap();
        let expiry = validator.validate_claims(&token).unwrap().exp;
        (token, expiry)
    }
    fn actor(&self, scope: Scope) -> (Uuid, Option<Uuid>) {
        match scope {
            Scope::Tenant => (self.target.owner_user_id, Some(self.target.id)),
            Scope::PlatformTenant => (self.root, Some(self.source.id)),
            Scope::PlatformGlobal => (self.root, None),
        }
    }
    fn base(&self, scope: Scope) -> String {
        if scope.platform() {
            "/api/v1/platform/pricing".into()
        } else {
            format!("/api/v1/tenants/{}/pricing", self.target.id)
        }
    }
    fn target_query(&self, scope: Scope) -> String {
        match scope {
            Scope::Tenant => String::new(),
            Scope::PlatformTenant => format!("?scope_type=tenant&tenant_id={}", self.target.id),
            Scope::PlatformGlobal => "?scope_type=platform".into(),
        }
    }
    fn create_body(&self, scope: Scope, model: &str) -> Value {
        let mut body = json!({"model_name":model,"billing_dimension":"provideraccount","currency":"CNY",
            "input_price_per_1k":"0.0000000001","output_price_per_1k":"0.02","is_default":false});
        match scope {
            Scope::Tenant => {}
            Scope::PlatformTenant => {
                body["scope_type"] = json!("tenant");
                body["tenant_id"] = json!(self.target.id);
            }
            Scope::PlatformGlobal => body["scope_type"] = json!("platform"),
        }
        body
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        http(self.state.clone(), method, path, token, body).await
    }
    async fn seed(&self, scope: Scope) -> (Uuid, i64) {
        let (actor, selected) = self.actor(scope);
        let token = self.token(actor, selected, 3600).await.0;
        let (status, row) = self
            .request(
                "POST",
                &self.base(scope),
                &token,
                self.create_body(scope, &format!("fixture-{}", Uuid::new_v4())),
            )
            .await;
        assert!(status.is_success(), "seed {}: {status} {row}", scope.name());
        let id = row[if scope.platform() { "pricing_id" } else { "id" }]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        (id, row["version"].as_i64().unwrap())
    }
    async fn fingerprints(&self) -> Vec<String> {
        let mut digests = vec![];
        for table in [
            "pricing_models",
            "pricing_audit_events",
            "tenant_audit_events",
        ] {
            let row=self.db.query_one(Statement::from_string(DbBackend::Postgres,
                format!("SELECT md5(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r.id),'[]'::jsonb)::text) AS digest FROM {table} r"))).await.unwrap().unwrap();
            digests.push(row.try_get::<String>("", "digest").unwrap());
        }
        digests
    }
    async fn expired(&self, scope: Scope, operation: &str) {
        let (id, version) = self.seed(scope).await;
        let base = self.base(scope);
        let target = self.target_query(scope);
        let (method, path, body) = match operation {
            "create" => (
                "POST",
                base.clone(),
                self.create_body(scope, &format!("expired-create-{}", Uuid::new_v4())),
            ),
            "update" => (
                if scope.platform() { "PUT" } else { "PATCH" },
                format!("{base}/{id}{target}"),
                json!({"expected_version":version,"input_price_per_1k":"0.0000000002"}),
            ),
            "delete" => ("DELETE", format!("{base}/{id}{target}"), Value::Null),
            "default" => (
                "POST",
                format!("{base}/{id}/make-default{target}"),
                json!({}),
            ),
            "batch" => {
                let (other, _) = self.seed(scope).await;
                (
                    "POST",
                    format!("{base}/batch-defaults{target}"),
                    json!({"model_ids":[id,other]}),
                )
            }
            _ => panic!("invalid internal operation"),
        };
        let before = self.fingerprints().await;
        let (actor, selected) = self.actor(scope);
        let (token, expiry) = self.token(actor, selected, 2).await;
        self.db
            .execute_unprepared("SELECT setval('pricing_proof_marker',1,FALSE)")
            .await
            .unwrap();
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE pricing_proof_delay_state SET expires=$1,enabled=TRUE",
                [expiry.into()],
            ))
            .await
            .unwrap();
        let (status, result) = self.request(method, &path, &token, body.clone()).await;
        self.db
            .execute_unprepared("UPDATE pricing_proof_delay_state SET enabled=FALSE")
            .await
            .unwrap();
        let marker = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT is_called,last_value FROM pricing_proof_marker",
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(
            marker.try_get::<bool>("", "is_called").unwrap(),
            "must reach delayed audit, not fail early: {} {operation}",
            scope.name()
        );
        assert!(marker.try_get::<i64>("", "last_value").unwrap() > 0);
        assert!(chrono::Utc::now().timestamp() >= expiry);
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{} {operation} returned {result} after JWT expiry",
            scope.name()
        );
        assert_eq!(
            self.fingerprints().await,
            before,
            "pricing and both audits must rollback: {} {operation}",
            scope.name()
        );
        let fresh = self.token(actor, selected, 3600).await.0;
        let (status, result) = self.request(method, &path, &fresh, body).await;
        assert!(
            status.is_success(),
            "fresh {} {operation}: {status} {result}",
            scope.name()
        );
    }
    async fn expired_read(&self, scope: Scope, detail: bool) {
        use sea_orm::ConnectOptions;
        use std::time::Duration;
        let (id, _) = self.seed(scope).await;
        let path = if detail {
            format!("{}/{id}", self.base(scope))
        } else {
            format!("{}{}", self.base(scope), self.target_query(scope))
        };
        let mut options = ConnectOptions::new(self.database_url.clone());
        options
            .max_connections(1)
            .min_connections(1)
            .sqlx_logging(false);
        let connection = Database::connect(options).await.unwrap();
        let pid = connection
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i32>("", "pid")
            .unwrap();
        let mut state = self.state.clone();
        state.pool = Some(DbRouter::single(connection.clone()));
        let (actor, selected) = self.actor(scope);
        let (token, expiry) = self.token(actor, selected, 2).await;
        let before = self.fingerprints().await;
        let blocker = self.db.begin().await.unwrap();
        blocker
            .execute_unprepared("LOCK TABLE pricing_models IN ACCESS EXCLUSIVE MODE")
            .await
            .unwrap();
        let request_path = path.clone();
        let request =
            tokio::spawn(
                async move { http(state, "GET", &request_path, &token, Value::Null).await },
            );
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let row=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                    "SELECT wait_event_type='Lock' AND query LIKE '%pricing_models%' AS waiting FROM pg_stat_activity WHERE pid=$1",[pid.into()])).await.unwrap();
                if row.is_some_and(|r|r.try_get::<Option<bool>>("","waiting").unwrap()==Some(true)){break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("exact pricing read backend must reach the relation lock after authentication");
        self.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT pg_sleep(GREATEST(0::double precision,$1::double precision-EXTRACT(EPOCH FROM clock_timestamp())::double precision)+0.05)",
            [expiry.into()])).await.unwrap();
        blocker.commit().await.unwrap();
        let (status, body) = request.await.unwrap();
        connection.close().await.unwrap();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "expired {} read: {body}",
            scope.name()
        );
        assert!(body.get("pricing").is_none());
        assert_eq!(self.fingerprints().await, before);
        let fresh = self.token(actor, selected, 3600).await.0;
        assert!(
            self.request("GET", &path, &fresh, Value::Null)
                .await
                .0
                .is_success()
        );
    }

    async fn regranted_selected_membership(&self) {
        use sea_orm::ConnectOptions;
        use std::time::Duration;
        let mut options = ConnectOptions::new(self.database_url.clone());
        options
            .max_connections(1)
            .min_connections(1)
            .sqlx_logging(false);
        let connection = Database::connect(options).await.unwrap();
        let pid = connection
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT pg_backend_pid() AS pid",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i32>("", "pid")
            .unwrap();
        let mut state = self.state.clone();
        state.pool = Some(DbRouter::single(connection.clone()));
        let (token, _) = self.token(self.root, Some(self.source.id), 3600).await;
        let body = self.create_body(Scope::PlatformTenant, "queued-original-selected-proof");
        let before = self.fingerprints().await;
        let original = TenantMembership::find(&self.db, self.source.id, self.root)
            .await
            .unwrap()
            .unwrap();
        let blocker = self.db.begin().await.unwrap();
        blocker
            .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
            .await
            .unwrap();
        let request = tokio::spawn(async move {
            http(state, "POST", "/api/v1/platform/pricing", &token, body).await
        });
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let row=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT wait_event_type='Lock' AND query LIKE '%identity_admin_fence%' AS waiting FROM pg_stat_activity WHERE pid=$1",[pid.into()])).await.unwrap();
                if row.is_some_and(|r|r.try_get::<Option<bool>>("","waiting").unwrap()==Some(true)){break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("exact pricing backend must wait on identity fence after authenticating");
        for status in ["suspended", "active"] {
            blocker
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE tenant_memberships SET status=$3 WHERE tenant_id=$1 AND user_id=$2",
                    [self.source.id.into(), self.root.into(), status.into()],
                ))
                .await
                .unwrap();
        }
        let version = blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT authz_version FROM tenant_memberships WHERE tenant_id=$1 AND user_id=$2",
                [self.source.id.into(), self.root.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "authz_version")
            .unwrap();
        assert!(version > original.authz_version);
        blocker.commit().await.unwrap();
        let (status, value) = request.await.unwrap();
        connection.close().await.unwrap();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "selected membership regrant cannot renew a queued root proof: {value}"
        );
        assert_eq!(self.fingerprints().await, before);
        let fresh = self.token(self.root, Some(self.source.id), 3600).await.0;
        let (status, value) = self
            .request(
                "POST",
                "/api/v1/platform/pricing",
                &fresh,
                self.create_body(Scope::PlatformTenant, "fresh-selected-proof"),
            )
            .await;
        assert!(
            status.is_success(),
            "fresh cross-tenant root request: {status} {value}"
        );
        assert!(
            TenantMembership::find_any(&self.db, self.target.id, self.root)
                .await
                .unwrap()
                .is_none()
        );
    }
}
async fn http(
    state: AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(if body.is_null() {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap();
    let response = create_router(state).oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn isolated(scope: Scope, scenario: &'static str) {
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some()
    );
    let mut url = url::Url::parse(&integration_tests::common::resolve_database_url()).unwrap();
    assert!(matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    ));
    let parent = create_test_pool().await;
    let name = format!("kc_pricing_session_{}", Uuid::new_v4().simple());
    parent
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    url.set_path(&format!("/{name}"));
    let child = Database::connect(url.as_str()).await.unwrap();
    let db = child.clone();
    let outcome = tokio::spawn(async move {
        let f = Fixture::new(db, url.to_string()).await;
        if scenario == "race" {
            f.regranted_selected_membership().await;
        } else if scenario == "reads" {
            f.expired_read(scope, false).await;
            if !scope.platform() {
                f.expired_read(scope, true).await;
            }
        } else {
            for operation in ["create", "update", "default", "batch", "delete"] {
                if matches!(scope, Scope::PlatformGlobal) && operation == "delete" {
                    continue;
                }
                f.expired(scope, operation).await;
            }
        }
    })
    .await;
    child.close().await.unwrap();
    parent
        .execute_unprepared(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .await
        .unwrap();
    outcome.unwrap();
}
#[tokio::test]
async fn tenant_pricing_mutations_reject_expiry_after_audit_wait() {
    isolated(Scope::Tenant, "writes").await;
}
#[tokio::test]
async fn root_cross_tenant_pricing_mutations_reject_expiry_after_audit_wait() {
    isolated(Scope::PlatformTenant, "writes").await;
}
#[tokio::test]
async fn root_global_pricing_mutations_reject_expiry_after_audit_wait() {
    isolated(Scope::PlatformGlobal, "writes").await;
}
#[tokio::test]
async fn queued_root_pricing_does_not_adopt_a_regranted_selected_membership() {
    isolated(Scope::PlatformTenant, "race").await;
}

#[tokio::test]
async fn tenant_pricing_reads_reject_expiry_after_relation_lock_wait() {
    isolated(Scope::Tenant, "reads").await;
}
#[tokio::test]
async fn platform_pricing_reads_reject_expiry_after_relation_lock_wait() {
    isolated(Scope::PlatformGlobal, "reads").await;
    isolated(Scope::PlatformTenant, "reads").await;
}
