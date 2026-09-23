//! Post-audit credential expiry must not commit a key change or reveal a one-time secret.
//! Every delay runs in a fresh, explicitly isolated local test database.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use integration_tests::db::{
    create_test_api_key, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{CreateProduceAiKeyRequest, DbRouter, Tenant, TenantMembership, User};
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
    tenant: Tenant,
    owner: Uuid,
    database_url: String,
}
impl Fixture {
    async fn new(db: DatabaseConnection, database_url: String) -> Self {
        keycompute_db::initialize_schema(&db).await.unwrap();
        let boot = db.begin().await.unwrap();
        User::bootstrap_root(&boot, "key-expiry-bootstrap@fixture.invalid", None)
            .await
            .unwrap();
        boot.commit().await.unwrap();
        let run = Uuid::new_v4().to_string();
        let tenant = create_test_tenant(&db, "key-expiry", &run).await;
        let owner = create_test_user(&db, tenant.id, "key-expiry-owner", &run).await;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        db.execute_unprepared(&format!(r#"
            CREATE SEQUENCE key_expiry_marker;
            CREATE TABLE key_expiry_deadline (expires_at BIGINT NOT NULL, enabled BOOLEAN NOT NULL);
            INSERT INTO key_expiry_deadline VALUES(0,FALSE);
            CREATE FUNCTION key_expiry_delay() RETURNS trigger LANGUAGE plpgsql AS $$
            DECLARE deadline BIGINT; active BOOLEAN; already_called BOOLEAN;
            BEGIN
                SELECT expires_at,enabled INTO deadline,active FROM key_expiry_deadline;
                IF active AND NEW.tenant_id='{0}'::uuid THEN
                    SELECT is_called INTO already_called FROM key_expiry_marker;
                    PERFORM nextval('key_expiry_marker');
                    IF NOT already_called THEN
                        PERFORM pg_sleep(GREATEST(0::double precision,deadline::double precision-EXTRACT(EPOCH FROM clock_timestamp())::double precision)+0.05);
                    END IF;
                END IF;
                RETURN NEW;
            END $$;
            CREATE TRIGGER key_expiry_delay BEFORE INSERT ON tenant_audit_events
                FOR EACH ROW EXECUTE FUNCTION key_expiry_delay();
        "#,tenant.id)).await.unwrap();
        Self {
            db,
            state,
            tenant,
            owner: owner.id,
            database_url,
        }
    }
    async fn token(&self, user: Uuid, lifetime: i64) -> (String, i64) {
        let u = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        let m = TenantMembership::find(&self.db, self.tenant.id, user)
            .await
            .unwrap()
            .unwrap();
        let tenant = Tenant::find_by_id(&self.db, self.tenant.id)
            .await
            .unwrap()
            .unwrap();
        let validator = self.state.auth.get_jwt_validator().unwrap();
        let token = validator
            .generate_identity_token(
                user,
                Some(self.tenant.id),
                u.token_version,
                Some(tenant.authz_version),
                Some(m.authz_version),
                lifetime,
            )
            .unwrap();
        let expiry = validator.validate_claims(&token).unwrap().exp;
        (token, expiry)
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let req = Request::builder()
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
        let response = create_router(self.state.clone())
            .oneshot(req)
            .await
            .unwrap();
        let status = response.status();
        if status.is_success()
            && method == "POST"
            && (path == "/api/v1/keys" || path.ends_with("/claim"))
        {
            assert!(
                response
                    .headers()
                    .get("cache-control")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("no-store")
            );
            assert_eq!(response.headers().get("pragma").unwrap(), "no-cache");
        }
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn key(&self) -> Uuid {
        let secret = ProduceAiKeyValidator::generate_key();
        create_test_api_key(
            &self.db,
            &CreateProduceAiKeyRequest {
                tenant_id: self.tenant.id,
                user_id: self.owner,
                name: "expiry-fixture".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&secret),
                produce_ai_key_preview: "fixture***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap()
        .id
    }
    async fn fingerprints(&self) -> Vec<String> {
        self.fingerprints_in(&self.db).await
    }
    async fn fingerprints_in(&self, db: &impl ConnectionTrait) -> Vec<String> {
        let mut out = Vec::new();
        for table in [
            "produce_ai_keys",
            "tenant_key_issuance_intents",
            "tenant_audit_events",
        ] {
            let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                format!("SELECT md5(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY r.id),'[]'::jsonb)::text) AS digest FROM {table} r WHERE tenant_id=$1"),[self.tenant.id.into()])).await.unwrap().unwrap();
            out.push(row.try_get::<String>("", "digest").unwrap());
        }
        out
    }
    async fn version_case(&self, change: &str) {
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
        let token = self.token(self.owner, 300).await.0;
        let original = self
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .validate_claims(&token)
            .unwrap();
        let blocker = self.db.begin().await.unwrap();
        blocker
            .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
            .await
            .unwrap();
        blocker
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT id FROM tenants WHERE id=$1 FOR UPDATE",
                [self.tenant.id.into()],
            ))
            .await
            .unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/keys")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"name":"must-revalidate-original-session"}).to_string(),
            ))
            .unwrap();
        let request = tokio::spawn(async move {
            let response = create_router(state).oneshot(req).await.unwrap();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        });
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                let row=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                    "SELECT wait_event_type='Lock' AND query ILIKE '%FROM tenants%' AND query ILIKE '%FOR SHARE%' AS waiting FROM pg_stat_activity WHERE pid=$1",[pid.into()])).await.unwrap();
                if row.is_some_and(|r|r.try_get::<Option<bool>>("","waiting").unwrap()==Some(true)){break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("the exact owner-creation backend must reach its authority lock");
        let statements = match change {
            "token" => vec![Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET token_version=token_version+1 WHERE id=$1",
                [self.owner.into()],
            )],
            "tenant" => vec![Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE tenants SET authz_version=authz_version+1 WHERE id=$1",
                [self.tenant.id.into()],
            )],
            "member" => vec![Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE tenant_memberships SET tenant_role=CASE WHEN tenant_role='member' THEN 'admin' ELSE 'member' END WHERE tenant_id=$1 AND user_id=$2",
                [self.tenant.id.into(), self.owner.into()],
            )],
            "regrant" => vec![
                Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE tenant_memberships SET status='suspended' WHERE tenant_id=$1 AND user_id=$2",
                    [self.tenant.id.into(), self.owner.into()],
                ),
                Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE tenant_memberships SET status='active' WHERE tenant_id=$1 AND user_id=$2",
                    [self.tenant.id.into(), self.owner.into()],
                ),
            ],
            _ => panic!("unknown internal identity change"),
        };
        for statement in statements {
            blocker.execute(statement).await.unwrap();
        }
        // Version fields are trigger-owned: a real role/status transition must
        // advance the member version, not a manual UPDATE that the trigger ignores.
        let live=blocker.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT u.token_version,t.authz_version AS tenant_version,m.authz_version AS member_version FROM users u JOIN tenant_memberships m ON m.user_id=u.id JOIN tenants t ON t.id=m.tenant_id WHERE u.id=$1 AND t.id=$2",
            [self.owner.into(),self.tenant.id.into()])).await.unwrap().unwrap();
        match change {
            "token" => {
                assert!(live.try_get::<i32>("", "token_version").unwrap() > original.token_version)
            }
            "tenant" => assert!(
                live.try_get::<i64>("", "tenant_version").unwrap()
                    > original.authz_version.unwrap()
            ),
            "member" | "regrant" => assert!(
                live.try_get::<i64>("", "member_version").unwrap()
                    > original.membership_authz_version.unwrap()
            ),
            _ => unreachable!(),
        }
        // Legitimate role/suspension changes may revoke existing keys via the
        // schema trigger. Preserve that authorized state, not the pre-change state.
        let before = self.fingerprints_in(&blocker).await;
        blocker.commit().await.unwrap();
        let (status, body) = request.await.unwrap();
        connection.close().await.unwrap();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "queued personal key creation reused old {change} authority"
        );
        assert!(body.get("key").is_none());
        assert_eq!(self.fingerprints().await, before);
        let current = self.token(self.owner, 300).await.0;
        assert!(
            self.request(
                "POST",
                "/api/v1/keys",
                &current,
                json!({"name":"fresh-after-authority-change"})
            )
            .await
            .0
            .is_success()
        );
    }

    async fn case(&self, operation: &str) {
        let id = self.key().await;
        let admin = self.token(self.tenant.owner_user_id, 3600).await.0;
        let base = format!("/api/v1/tenants/{}", self.tenant.id);
        let (status, key) = self
            .request("GET", &format!("{base}/keys/{id}"), &admin, Value::Null)
            .await;
        assert_eq!(status, StatusCode::OK);
        let mut intent = Uuid::nil();
        if matches!(operation, "cancel" | "decline" | "claim") {
            let (status, response) = self
                .request(
                    "POST",
                    &format!("{base}/keys/{id}/rotate"),
                    &admin,
                    json!({"name":"expiry-rotation"}),
                )
                .await;
            assert_eq!(status, StatusCode::ACCEPTED);
            intent = Uuid::parse_str(response["intent"]["id"].as_str().unwrap()).unwrap();
        }
        let (method, path, body, actor) = match operation {
            "request" => (
                "POST",
                format!("{base}/keys/issuance"),
                json!({"owner_user_id":self.owner,"name":"expires-during-request"}),
                self.tenant.owner_user_id,
            ),
            "rotate" => (
                "POST",
                format!("{base}/keys/{id}/rotate"),
                json!({"name":"expires-during-rotation"}),
                self.tenant.owner_user_id,
            ),
            "patch" => (
                "PATCH",
                format!("{base}/keys/{id}"),
                json!({"name":"must-roll-back","expected_updated_at":key["updated_at"]}),
                self.tenant.owner_user_id,
            ),
            "revoke" => (
                "POST",
                format!("{base}/keys/{id}/revoke"),
                json!({}),
                self.tenant.owner_user_id,
            ),
            "delete" => (
                "DELETE",
                format!("{base}/keys/{id}"),
                Value::Null,
                self.tenant.owner_user_id,
            ),
            "cancel" => (
                "POST",
                format!("{base}/key-issuance/{intent}/cancel"),
                json!({}),
                self.tenant.owner_user_id,
            ),
            "decline" => (
                "POST",
                format!("/api/v1/me/key-issuance/{intent}/decline"),
                json!({}),
                self.owner,
            ),
            "claim" => (
                "POST",
                format!("/api/v1/me/key-issuance/{intent}/claim"),
                json!({}),
                self.owner,
            ),
            "personal_create" => (
                "POST",
                "/api/v1/keys".into(),
                json!({"name":"expiry-personal-key","never_expires":true}),
                self.owner,
            ),
            "personal_delete" => (
                "DELETE",
                format!("/api/v1/keys/{id}"),
                Value::Null,
                self.owner,
            ),
            _ => panic!("unknown internal test case"),
        };
        let before = self.fingerprints().await;
        self.db
            .execute_unprepared("SELECT setval('key_expiry_marker',1,FALSE)")
            .await
            .unwrap();
        let (short, expiry) = self.token(actor, 2).await;
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE key_expiry_deadline SET expires_at=$1,enabled=TRUE",
                [expiry.into()],
            ))
            .await
            .unwrap();
        let (status, body_out) = self.request(method, &path, &short, body.clone()).await;
        self.db
            .execute_unprepared("UPDATE key_expiry_deadline SET enabled=FALSE")
            .await
            .unwrap();
        let marker = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                "SELECT is_called,last_value FROM key_expiry_marker",
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(
            marker.try_get::<bool>("", "is_called").unwrap(),
            "request must reach the delayed audit: {operation}"
        );
        assert!(marker.try_get::<i64>("", "last_value").unwrap() >= 1);
        assert!(chrono::Utc::now().timestamp() >= expiry);
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "operation {operation} returned a success after original JWT expiry"
        );
        assert!(
            body_out.get("key").is_none(),
            "an expired claim must never expose a one-time secret"
        );
        assert_eq!(
            self.fingerprints().await,
            before,
            "all key, intent and audit changes must roll back for {operation}"
        );
        // A current identity can retry the operation exactly once after rollback.
        let fresh = self.token(actor, 3600).await.0;
        let (status, result) = self.request(method, &path, &fresh, body).await;
        assert!(status.is_success(), "fresh {operation} failed: {status}");
        if operation == "claim" {
            assert_eq!(result["secret_returned_once"], true);
            assert!(result["key"].as_str().is_some_and(|s| !s.is_empty()));
            let (status, old) = self
                .request("GET", &format!("{base}/keys/{id}"), &admin, Value::Null)
                .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(old["revoked"], true);
        }
    }
}
async fn isolated(cases: &'static [&'static str]) {
    assert!(
        std::env::var("KC_TENANT_TEST_ACK_ISOLATED").as_deref() == Ok("1")
            || std::env::var_os("CI").is_some()
    );
    let url = integration_tests::common::resolve_database_url();
    let parsed = url::Url::parse(&url).unwrap();
    assert!(matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    ));
    let parent = create_test_pool().await;
    let name = format!("kc_key_expiry_{}", Uuid::new_v4().simple());
    parent
        .execute_unprepared(&format!("CREATE DATABASE {name}"))
        .await
        .unwrap();
    let mut child_url = parsed;
    child_url.set_path(&format!("/{name}"));
    let child = Database::connect(child_url.as_str()).await.unwrap();
    let db = child.clone();
    let outcome = tokio::spawn(async move {
        let f = Fixture::new(db, child_url.to_string()).await;
        for operation in cases {
            if let Some(change) = operation.strip_prefix("version_") {
                f.version_case(change).await;
            } else {
                f.case(operation).await;
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
async fn expired_owner_claim_rolls_back_after_audit_wait() {
    isolated(&["claim"]).await;
}
#[tokio::test]
async fn expired_key_admin_and_owner_commands_roll_back_after_audit_wait() {
    isolated(&[
        "request", "rotate", "patch", "revoke", "delete", "cancel", "decline",
    ])
    .await;
}

#[tokio::test]
async fn expired_personal_key_commands_roll_back_after_audit_wait() {
    isolated(&["personal_create", "personal_delete"]).await;
}
#[tokio::test]
async fn queued_personal_key_creation_rejects_changed_and_regranted_authority() {
    isolated(&[
        "version_token",
        "version_tenant",
        "version_member",
        "version_regrant",
    ])
    .await;
}
