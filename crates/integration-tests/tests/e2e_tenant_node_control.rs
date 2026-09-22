//! Actual PostgreSQL/router node-control isolation; workers and credentials are fixture-only.
use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use chrono::{Duration, Utc};
use integration_tests::{
    common::{generate_test_id, resolve_redis_url},
    db::{TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_db::{
    AuditContext, DbRouter, Tenant, User,
    models::{
        node::{CreateNodeRequest, Node},
        node_control::{
            self as dao, NodeAction, NodeControlScope, NodeFilter, NodePatch, NodeResource,
        },
        node_session::{CreateNodeSessionRequest, NodeSession},
        node_task::{CreateNodeTaskRequest, NodeTask},
        tenant_control::TenantAuthzSnapshot,
        user_node_gateway_token::UserNodeGatewayToken,
    },
};
use keycompute_server::{
    AppState, create_router,
    extractors::RequestId,
    state::{AppStateConfig, RateLimitBackendConfig},
};
use keycompute_types::{
    CredentialKind, PlatformRole, PlatformScope, TenantRole, TenantScope, node::NodeTaskResult,
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};
use std::time::Duration as StdDuration;
use tower::ServiceExt;
use uuid::Uuid;
const SECRET: &str = "isolated-node-control-registration-secret-not-production";
async fn state(db: DatabaseConnection) -> AppState {
    AppState::try_with_pool_and_config(
        DbRouter::single(db),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(keycompute_config::RedisConfig {
                url: resolve_redis_url(),
                pool_size: 4,
                node_poll_pool_size: 2,
                node_result_pool_size: 2,
                ..Default::default()
            }),
            node_gateway: Some(keycompute_config::NodeGatewayConfig {
                registration_token_secret: Some(SECRET.into()),
                poll_timeout_secs: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
async fn jwt(
    state: &AppState,
    db: &DatabaseConnection,
    user: Uuid,
    tenant: Option<Uuid>,
) -> String {
    let u = User::find_by_id(db, user).await.unwrap().unwrap();
    let raw = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user, None, u.token_version, None, None, 3600)
        .unwrap();
    let ctx = state.auth.verify_token(&raw).await.unwrap();
    state
        .auth
        .select_tenant(&ctx, tenant)
        .await
        .unwrap()
        .access_token
}
async fn call(
    app: Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value, HeaderMap) {
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
    let response = tokio::time::timeout(StdDuration::from_secs(20), app.oneshot(req))
        .await
        .unwrap()
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 2 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}
fn audit(id: Uuid, role: Option<TenantRole>) -> AuditContext {
    AuditContext {
        actor_user_id: id,
        credential_kind: CredentialKind::Jwt,
        actor_platform_role: PlatformRole::None,
        actor_tenant_role: role,
        request_id: Some(Uuid::new_v4()),
    }
}
struct Fixture {
    db: DatabaseConnection,
    guard: TestDataGuard,
    state: AppState,
    a: Tenant,
    b: Tenant,
    member: User,
    root: User,
    admin: String,
    member_token: String,
    root_token: String,
    operator_token: String,
    run: String,
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = generate_test_id();
        let guard = TestDataGuard::new(db.clone(), run.clone());
        let a = create_test_tenant(&db, "node-control-a", &run).await;
        let b = create_test_tenant(&db, "node-control-b", &run).await;
        let member = create_test_user(&db, a.id, "node-control-member", &run)
            .await
            .user;
        let root = create_test_user(&db, b.id, "node-control-root", &run)
            .await
            .user;
        let operator = create_test_user(&db, b.id, "node-control-operator", &run)
            .await
            .user;
        for (id, role) in [(root.id, "root"), (operator.id, "operator")] {
            db.execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE users SET platform_role=$2 WHERE id=$1",
                [id.into(), role.into()],
            ))
            .await
            .unwrap();
        }
        let state = state(db.clone()).await;
        let admin = jwt(&state, &db, a.owner_user_id, Some(a.id)).await;
        let member_token = jwt(&state, &db, member.id, Some(a.id)).await;
        let root_token = jwt(&state, &db, root.id, None).await;
        let operator_token = jwt(&state, &db, operator.id, None).await;
        Self {
            db,
            guard,
            state,
            a,
            b,
            member,
            root,
            admin,
            member_token,
            root_token,
            operator_token,
            run,
        }
    }
    fn path(&self, suffix: &str) -> String {
        format!("/api/v1/tenants/{}/{}", self.a.id, suffix)
    }
    fn platform(&self, suffix: &str) -> String {
        format!("/api/v1/platform/tenants/{}/{}", self.a.id, suffix)
    }
    async fn node(&self, t: Uuid, u: Uuid, name: &str) -> Node {
        Node::create(&self.db,&CreateNodeRequest{tenant_id:t,owner_user_id:u,client_instance_id:Uuid::new_v4().to_string(),display_name:name.into(),capabilities_json:json!({"runtime":"ollama","secret":"never-leak-node-config-marker"})}).await.unwrap()
    }
    async fn task(&self, t: Uuid, u: Uuid, name: &str) -> NodeTask {
        let request_id = Uuid::new_v4();
        NodeTask::create(&self.db,&CreateNodeTaskRequest{tenant_id:t,user_id:u,request_id,model:name.into(),payload_json:json!({"request_id":request_id,"chat":{"model":name,"messages":[{"role":"user","content":"never-leak-task-payload-marker"}]},"native":null}),deadline_at:Utc::now()+Duration::minutes(3),complete_grace_until:Utc::now()+Duration::minutes(5)}).await.unwrap()
    }
    async fn registration(&self, t: Uuid, u: Uuid) -> UserNodeGatewayToken {
        let (id, _, hash, preview) = UserNodeGatewayToken::generate_hmac_token(SECRET.as_bytes());
        UserNodeGatewayToken::create_with_id(&self.db, id, t, u, &hash, &preview)
            .await
            .unwrap()
    }
    async fn scope(&self, user: Uuid, tenant: Uuid, owned: bool) -> NodeControlScope {
        let u = User::find_by_id(&self.db, user).await.unwrap().unwrap();
        let t = Tenant::find_by_id(&self.db, tenant).await.unwrap().unwrap();
        let m = keycompute_db::TenantMembership::find(&self.db, tenant, user)
            .await
            .unwrap()
            .unwrap();
        let s = TenantScope::checked(tenant, user, m.tenant_role().unwrap()).unwrap();
        let snapshot = TenantAuthzSnapshot {
            token_version: u.token_version,
            tenant_authz_version: t.authz_version,
            membership_authz_version: m.authz_version,
        };
        if owned {
            NodeControlScope::owned(s, snapshot, CredentialKind::Jwt)
        } else {
            NodeControlScope::tenant(s, snapshot, CredentialKind::Jwt)
        }
        .unwrap()
    }
    async fn get(&self, token: &str, path: &str) -> (StatusCode, Value, HeaderMap) {
        call(
            create_router(self.state.clone()),
            "GET",
            path,
            token,
            Value::Null,
        )
        .await
    }
}
#[tokio::test]
async fn metadata_lists_details_counts_and_personal_views_are_tenant_bound() {
    let mut f = Fixture::new().await;
    let n = f.node(f.a.id, f.member.id, "节点_%").await;
    let own = f.node(f.a.id, f.a.owner_user_id, "other").await;
    let foreign = f.node(f.b.id, f.b.owner_user_id, "foreign").await;
    let task = f.task(f.a.id, f.member.id, "safe-model").await;
    f.task(f.b.id, f.b.owner_user_id, "foreign-model").await;
    let reg = f.registration(f.a.id, f.member.id).await;
    let (status, page, _) = f.get(&f.admin, &f.path("nodes?page_size=1")).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["total"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let (_, next, _) = f.get(&f.admin, &f.path("nodes?page_size=1&page=2")).await;
    assert_ne!(page["items"][0]["id"], next["items"][0]["id"]);
    let search = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("search", "节点_%")
        .finish();
    let (_, found, _) = f.get(&f.admin, &f.path(&format!("nodes?{search}"))).await;
    assert_eq!(found["total"], 1);
    assert_eq!(found["items"][0]["id"], n.id.to_string());
    assert_eq!(
        f.get(&f.admin, &f.path(&format!("nodes/{}", foreign.id)))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.get(&f.member_token, &f.path("nodes")).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, mine, _) = f.get(&f.member_token, "/api/v1/me/nodes").await;
    assert_eq!(mine["total"], 1);
    assert_eq!(mine["items"][0]["id"], n.id.to_string());
    let (_, admin_own, _) = f.get(&f.admin, "/api/v1/me/nodes").await;
    assert_eq!(admin_own["total"], 1);
    assert_eq!(admin_own["items"][0]["id"], own.id.to_string());
    let (_, tasks, _) = f.get(&f.admin, &f.path("tasks")).await;
    assert_eq!(tasks["total"], 1);
    assert_eq!(tasks["items"][0]["id"], task.id.to_string());
    assert!(!tasks.to_string().contains("private_prompt"));
    assert!(!tasks.to_string().contains("payload_json"));
    let (_, tokens, _) = f.get(&f.admin, &f.path("node-registrations")).await;
    assert_eq!(tokens["items"][0]["id"], reg.id.to_string());
    assert!(!tokens.to_string().contains(&reg.token_hash));
    assert!(!tokens.to_string().contains("token_hash"));
    assert!(!page.to_string().contains("never-leak"));
    let scope = f.scope(f.a.owner_user_id, f.a.id, false).await;
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("SET TRANSACTION READ ONLY")
        .await
        .unwrap();
    assert_eq!(
        dao::nodes(&tx, scope, &NodeFilter::default(), 100, 0)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        dao::count(&tx, scope, NodeResource::Node, &NodeFilter::default())
            .await
            .unwrap(),
        2
    );
    tx.rollback().await.unwrap();
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn platform_operator_actions_are_allowlisted_and_bare_handlers_enforce_authority() {
    let mut f = Fixture::new().await;
    let n = f.node(f.a.id, f.member.id, "node").await;
    assert_eq!(
        f.get(&f.root_token, &f.path("nodes")).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.get(&f.operator_token, &f.platform("nodes")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.get(&f.operator_token, &f.platform("node-registrations"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let command = json!({"expected_updated_at":n.updated_at,"reason":"operator maintenance"});
    let (status, paused, _) = call(
        create_router(f.state.clone()),
        "POST",
        &f.platform(&format!("nodes/{}/exclude", n.id)),
        &f.operator_token,
        command.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{paused}");
    assert_eq!(paused["node"]["status"], "excluded");
    for action in ["revoke"] {
        assert_eq!(
            call(
                create_router(f.state.clone()),
                "POST",
                &f.platform(&format!("nodes/{}/{action}", n.id)),
                &f.operator_token,
                json!({"expected_updated_at":paused["node"]["updated_at"],"reason":"forbidden"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let bare = Router::new()
        .route(
            "/api/v1/admin/nodes/{id}/exclude",
            post(keycompute_server::handlers::admin_node_gateway::exclude_node),
        )
        .layer(Extension(RequestId::new()))
        .with_state(f.state.clone());
    assert_eq!(
        call(
            bare,
            "POST",
            &format!("/api/v1/admin/nodes/{}/exclude?tenant_id={}", n.id, f.a.id),
            &f.admin,
            command
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        f.get(&f.root_token, "/api/v1/admin/node-gateway/nodes")
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut bad = json!({"expected_updated_at":paused["node"]["updated_at"],"reason":"bad","owner_user_id":f.root.id});
    assert_eq!(
        call(
            create_router(f.state.clone()),
            "PATCH",
            &f.path(&format!("nodes/{}", n.id)),
            &f.admin,
            bad.take()
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn approval_secrets_and_replacement_requests_remain_owner_only() {
    let mut f = Fixture::new().await;
    let (status, pending, _) = call(
        create_router(f.state.clone()),
        "POST",
        "/api/v1/me/node-gateway/token",
        &f.member_token,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{pending}");
    let id = Uuid::parse_str(pending["token"]["id"].as_str().unwrap()).unwrap();
    assert!(pending["registration_token"].is_null());
    let (_, duplicate, _) = call(
        create_router(f.state.clone()),
        "POST",
        "/api/v1/me/node-gateway/token",
        &f.member_token,
        Value::Null,
    )
    .await;
    assert_eq!(duplicate["token"]["id"], pending["token"]["id"]);
    let(status,approved,_)=call(create_router(f.state.clone()),"POST",&f.path(&format!("node-registrations/{id}")),&f.admin,json!({"action":"approve","expected_updated_at":pending["token"]["updated_at"],"reason":"approve owner registration"})).await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert!(!approved.to_string().contains("registration_token"));
    assert!(!approved.to_string().contains("token_hash"));
    let (status, secret, headers) = f
        .get(&f.member_token, "/api/v1/me/node-gateway/token")
        .await;
    assert_eq!(status, StatusCode::OK, "{secret}");
    assert_eq!(headers["cache-control"], "private, no-store");
    let raw = secret["registration_token"].as_str().unwrap();
    assert_eq!(
        UserNodeGatewayToken::validate_hmac_token(raw, SECRET.as_bytes()).unwrap(),
        id
    );
    let (_, history, _) = f
        .get(&f.member_token, "/api/v1/me/node-gateway/tokens")
        .await;
    assert!(history[0]["registration_token"].is_null());
    assert!(!history.to_string().contains(raw));
    let command = json!({"action":"revoke","expected_updated_at":approved["token"]["updated_at"],"reason":"rotate compromised registration"});
    let (status, _, _) = call(
        create_router(f.state.clone()),
        "POST",
        &f.path(&format!("node-registrations/{id}")),
        &f.admin,
        command.clone(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "owner reveal changes the resource revision"
    );
    let (status, current, _) = f
        .get(&f.admin, &f.path(&format!("node-registrations/{id}")))
        .await;
    assert_eq!(status, StatusCode::OK, "{current}");
    let revision = current["updated_at"].clone();
    assert!(revision.is_string());
    // Repeating an already-revealed read writes only audit, not a new resource revision.
    assert_eq!(
        f.get(&f.member_token, "/api/v1/me/node-gateway/token")
            .await
            .0,
        StatusCode::OK
    );
    let (_, unchanged, _) = f
        .get(&f.admin, &f.path(&format!("node-registrations/{id}")))
        .await;
    assert_eq!(unchanged["updated_at"], revision);
    let mut command = command;
    command["expected_updated_at"] = revision;
    let (status, rejected, _) = call(
        create_router(f.state.clone()),
        "POST",
        &f.path(&format!("node-registrations/{id}")),
        &f.admin,
        command,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rejected}");
    let (_, new, _) = call(
        create_router(f.state.clone()),
        "POST",
        "/api/v1/me/node-gateway/token",
        &f.member_token,
        Value::Null,
    )
    .await;
    assert_ne!(new["token"]["id"], id.to_string());
    assert_eq!(new["token"]["status"], "pending");
    assert!(new["registration_token"].is_null());
    assert_eq!(
        UserNodeGatewayToken::find_by_id(&f.db, id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "rejected"
    );
    let events=f.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT action,metadata FROM tenant_audit_events WHERE tenant_id=$1 AND resource_type='node_registration' ORDER BY created_at",[f.a.id.into()])).await.unwrap();
    assert!(
        events
            .iter()
            .any(|r| r.try_get::<String>("", "action").unwrap() == "node.registration.reveal")
    );
    for e in events {
        assert!(
            !e.try_get::<Value>("", "metadata")
                .unwrap()
                .to_string()
                .contains(raw)
        );
    }
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn node_revocation_drains_accepted_lease_without_restoring_credentials_or_erasing_evidence() {
    let mut f = Fixture::new().await;
    let original = f.node(f.a.id, f.member.id, "worker").await;
    let n = Node::update_status(&f.db, original.id, "online")
        .await
        .unwrap();
    let model = "control-drain";
    let session_secret = format!("fixture-node-session-{}", Uuid::new_v4());
    let session = NodeSession::create(
        &f.db,
        &CreateNodeSessionRequest {
            node_id: n.id,
            session_token_hash: UserNodeGatewayToken::hash_token(&session_secret),
            expires_at: Utc::now() + Duration::minutes(1),
            accepted_models_json: json!([model]),
            native_operations_json: json!([]),
            native_profiles_json: json!([]),
        },
    )
    .await
    .unwrap();
    let token = f.registration(f.a.id, f.member.id).await;
    token.approve(&f.db, f.a.owner_user_id).await.unwrap();
    assert!(
        UserNodeGatewayToken::consume(&f.db, token.id, n.id)
            .await
            .unwrap()
    );
    let task = f.task(f.a.id, f.a.owner_user_id, model).await;
    let lease = Uuid::new_v4();
    assert!(
        NodeTask::claim(&f.db, task.id, n.id, session.id, lease)
            .await
            .unwrap()
            .is_some()
    );
    let (status, revoked, _) = call(
        create_router(f.state.clone()),
        "POST",
        &f.path(&format!("nodes/{}/revoke", n.id)),
        &f.admin,
        json!({"expected_updated_at":n.updated_at,"reason":"revoke node credential"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{revoked}");
    let draining = NodeSession::find_by_id(&f.db, session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!draining.accepting_tasks);
    assert!(draining.revoked_at.is_none());
    assert!(draining.expires_at >= task.complete_grace_until);
    let next = f.task(f.a.id, f.member.id, model).await;
    assert!(
        NodeTask::claim(&f.db, next.id, n.id, session.id, Uuid::new_v4())
            .await
            .unwrap()
            .is_none()
    );
    let response:keycompute_types::ChatCompletionResponse=serde_json::from_value(json!({"id":"chatcmpl-control","object":"chat.completion","created":1,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":"complete accepted work"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":3,"total_tokens":5}})).unwrap();
    let body = json!({"protocol_version":"node.v1","node_id":n.id,"session_id":session.id,"task_id":task.id,"lease_id":lease,"result":NodeTaskResult::Succeeded{response}});
    let (status, completion, _) = call(
        create_router(f.state.clone()),
        "POST",
        &format!("/node/v1/tasks/{}/complete", task.id),
        &session_secret,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{completion}");
    let saved = NodeTask::find_by_id(&f.db, task.id).await.unwrap().unwrap();
    assert_eq!(saved.tenant_id, task.tenant_id);
    assert_eq!(saved.user_id, task.user_id);
    assert_eq!(saved.lease_id, Some(lease));
    assert_eq!(saved.status, "succeeded");
    assert_eq!(
        call(
            create_router(f.state.clone()),
            "POST",
            &format!("/node/v1/tasks/{}/complete", task.id),
            &session_secret,
            body
        )
        .await
        .0,
        StatusCode::OK
    );
    let current = Node::find_by_id(&f.db, n.id).await.unwrap().unwrap();
    let (status, recovered, _) = call(
        create_router(f.state.clone()),
        "POST",
        &f.path(&format!("nodes/{}/recover", n.id)),
        &f.admin,
        json!({"expected_updated_at":current.updated_at,"reason":"recover operational state only"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{recovered}");
    assert_eq!(recovered["node"]["status"], "offline");
    assert!(
        !NodeSession::find_by_id(&f.db, session.id)
            .await
            .unwrap()
            .unwrap()
            .accepting_tasks
    );
    assert_eq!(
        UserNodeGatewayToken::find_by_id(&f.db, token.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "rejected"
    );
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(
            "expected_updated_at",
            recovered["node"]["updated_at"].as_str().unwrap(),
        )
        .append_pair("reason", "retained evidence deletion attempt")
        .finish();
    assert_eq!(
        call(
            create_router(f.state.clone()),
            "DELETE",
            &f.path(&format!("nodes/{}?{query}", n.id)),
            &f.admin,
            Value::Null
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        NodeTask::find_by_id(&f.db, task.id)
            .await
            .unwrap()
            .is_some()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn stale_revisions_foreign_ids_and_audit_failures_leave_nodes_unchanged() {
    let mut f = Fixture::new().await;
    let node = f.node(f.a.id, f.member.id, "before").await;
    let foreign = f.node(f.b.id, f.b.owner_user_id, "foreign").await;
    let scope = f.scope(f.a.owner_user_id, f.a.id, false).await;
    let a = audit(f.a.owner_user_id, Some(TenantRole::Admin));
    assert!(
        dao::change_node(
            &f.db,
            scope,
            &a,
            dao::NodeMutation {
                id: foreign.id,
                expected_updated_at: foreign.updated_at,
                action: NodeAction::Exclude,
                patch: &NodePatch::default(),
                reason: "foreign"
            }
        )
        .await
        .unwrap_err()
        .is_not_found()
    );
    let first = dao::change_node(
        &f.db,
        scope,
        &a,
        dao::NodeMutation {
            id: node.id,
            expected_updated_at: node.updated_at,
            action: NodeAction::Configure,
            patch: &NodePatch {
                display_name: Some("after".into()),
                failure_threshold: Some(5),
            },
            reason: "edit",
        },
    )
    .await
    .unwrap();
    assert!(first.node.updated_at > node.updated_at);
    assert!(
        dao::change_node(
            &f.db,
            scope,
            &a,
            dao::NodeMutation {
                id: node.id,
                expected_updated_at: node.updated_at,
                action: NodeAction::Exclude,
                patch: &NodePatch::default(),
                reason: "stale"
            }
        )
        .await
        .unwrap_err()
        .is_optimistic_conflict()
    );
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let tag = Uuid::new_v4().simple().to_string();
    let function = format!("node_audit_failure_{tag}");
    let trigger = format!("node_audit_trigger_{tag}");
    tx.execute_unprepared(&format!("CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid THEN RAISE EXCEPTION 'fixture node audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {trigger} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {function}();",f.a.id)).await.unwrap();
    assert!(
        dao::change_node(
            &tx,
            scope,
            &a,
            dao::NodeMutation {
                id: node.id,
                expected_updated_at: first.node.updated_at,
                action: NodeAction::Exclude,
                patch: &NodePatch::default(),
                reason: "audit failure"
            }
        )
        .await
        .is_err()
    );
    let current = dao::node(&tx, scope, node.id).await.unwrap().unwrap();
    assert_eq!(current.status, first.node.status);
    assert_eq!(current.updated_at, first.node.updated_at);
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {trigger} ON tenant_audit_events; DROP FUNCTION {function}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let saved = dao::node(&f.db, scope, node.id).await.unwrap().unwrap();
    assert_eq!(saved.display_name, "after");
    assert_eq!(saved.status, "offline");
    let mut forged = a;
    forged.actor_user_id = f.member.id;
    assert!(
        dao::change_node(
            &f.db,
            scope,
            &forged,
            dao::NodeMutation {
                id: node.id,
                expected_updated_at: saved.updated_at,
                action: NodeAction::Exclude,
                patch: &NodePatch::default(),
                reason: "mismatched actor"
            }
        )
        .await
        .is_err()
    );
    for credential in [
        CredentialKind::ApiKey,
        CredentialKind::Node,
        CredentialKind::System,
    ] {
        let mut forged = a;
        forged.credential_kind = credential;
        assert!(
            dao::change_node(
                &f.db,
                scope,
                &forged,
                dao::NodeMutation {
                    id: node.id,
                    expected_updated_at: saved.updated_at,
                    action: NodeAction::Exclude,
                    patch: &NodePatch::default(),
                    reason: "invalid credential"
                }
            )
            .await
            .is_err()
        );
    }
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn queued_approval_rechecks_the_exact_administrator_grant_after_lock_wait() {
    let mut f = Fixture::new().await;
    let deputy = create_test_user(&f.db, f.a.id, "node-deputy", &f.run)
        .await
        .user;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [f.a.id.into(), deputy.id.into()],
    ))
    .await
    .unwrap();
    let auth = jwt(&f.state, &f.db, deputy.id, Some(f.a.id)).await;
    let reg = f.registration(f.a.id, f.member.id).await;
    let mut options = ConnectOptions::new(std::env::var("DATABASE_URL").unwrap());
    options.max_connections(1).min_connections(1);
    let isolated = Database::connect(options).await.unwrap();
    let pid: i32 = isolated
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT pg_backend_pid() AS pid".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "pid")
        .unwrap();
    let local_state = state(isolated.clone()).await;
    let blocker = f.db.begin().await.unwrap();
    blocker
        .execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let path = f.path(&format!("node-registrations/{}", reg.id));
    let body =
        json!({"action":"approve","expected_updated_at":reg.updated_at,"reason":"queued approval"});
    let pending =
        tokio::spawn(
            async move { call(create_router(local_state), "POST", &path, &auth, body).await },
        );
    tokio::time::timeout(StdDuration::from_secs(2),async{loop{let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock' AND query LIKE 'UPDATE identity_admin_fence%'",[pid.into()])).await.unwrap();if row.is_some(){break;}tokio::time::sleep(StdDuration::from_millis(10)).await;}}).await.unwrap();
    blocker
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='member' WHERE tenant_id=$1 AND user_id=$2",
            [f.a.id.into(), deputy.id.into()],
        ))
        .await
        .unwrap();
    blocker.commit().await.unwrap();
    let (status, body, _) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        UserNodeGatewayToken::find_by_id(&f.db, reg.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
    isolated.close().await.unwrap();
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn inference_credentials_foreign_owners_and_stale_versions_cannot_reuse_control_authority() {
    let mut f = Fixture::new().await;
    let n = f.node(f.a.id, f.member.id, "credential-check").await;
    let scope = f.scope(f.a.owner_user_id, f.a.id, false).await;
    let raw = keycompute_auth::ProduceAiKeyValidator::generate_key();
    integration_tests::db::create_test_api_key(
        &f.db,
        &keycompute_db::CreateProduceAiKeyRequest {
            tenant_id: f.a.id,
            user_id: f.a.owner_user_id,
            name: "control denied".into(),
            produce_ai_key_hash: keycompute_auth::ProduceAiKeyValidator::hash_key(&raw),
            produce_ai_key_preview: "test***".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(f.get(&raw, &f.path("nodes")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        f.get(&raw, "/api/v1/me/node-gateway/token").await.0,
        StatusCode::FORBIDDEN
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET token_version=token_version+1 WHERE id=$1",
        [f.a.owner_user_id.into()],
    ))
    .await
    .unwrap();
    assert!(
        dao::nodes(&f.db, scope, &NodeFilter::default(), 100, 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        dao::change_node(
            &f.db,
            scope,
            &audit(f.a.owner_user_id, Some(TenantRole::Admin)),
            dao::NodeMutation {
                id: n.id,
                expected_updated_at: n.updated_at,
                action: NodeAction::Exclude,
                patch: &NodePatch::default(),
                reason: "old token"
            }
        )
        .await
        .is_err()
    );
    let forged = NodeControlScope::platform(
        PlatformScope::checked(f.member.id, PlatformRole::Root).unwrap(),
        f.a.id,
        f.member.token_version,
        CredentialKind::Jwt,
    )
    .unwrap();
    assert!(
        dao::nodes(&f.db, forged, &NodeFilter::default(), 100, 0)
            .await
            .unwrap()
            .is_empty()
    );
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO tenant_memberships(tenant_id,user_id,tenant_role,status) VALUES($1,$2,'member','active')",[f.b.id.into(),f.member.id.into()])).await.unwrap();
    let rejected = f.registration(f.b.id, f.member.id).await;
    rejected.reject(&f.db, f.b.owner_user_id).await.unwrap();
    // A member can belong to two tenants; the first tenant still cannot erase
    // the other tenant's rejected registration through its personal URL.
    assert_eq!(
        call(
            create_router(f.state.clone()),
            "DELETE",
            &format!("/api/v1/me/node-gateway/token/{}", rejected.id),
            &f.member_token,
            Value::Null
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert!(
        UserNodeGatewayToken::find_by_id(&f.db, rejected.id)
            .await
            .unwrap()
            .is_some()
    );
    f.guard.cleanup().await.unwrap();
}
#[tokio::test]
async fn database_rejects_node_credential_reassignment_and_terminal_reactivation() {
    let mut f = Fixture::new().await;
    let n = f.node(f.a.id, f.member.id, "guard").await;
    let foreign = f.node(f.b.id, f.b.owner_user_id, "foreign").await;
    let reg = f.registration(f.a.id, f.member.id).await;
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_node_gateway_tokens SET consumed_node_id=$2 WHERE id=$1",
            [reg.id.into(), foreign.id.into()]
        ))
        .await
        .is_err()
    );
    reg.reject(&f.db, f.a.owner_user_id).await.unwrap();
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE user_node_gateway_tokens SET status='approved' WHERE id=$1",
            [reg.id.into()]
        ))
        .await
        .is_err()
    );
    let session = NodeSession::create(
        &f.db,
        &CreateNodeSessionRequest {
            node_id: n.id,
            session_token_hash: format!("session-{}", Uuid::new_v4()),
            expires_at: Utc::now() + Duration::hours(1),
            accepted_models_json: json!([]),
            native_operations_json: json!([]),
            native_profiles_json: json!([]),
        },
    )
    .await
    .unwrap();
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET node_id=$2 WHERE id=$1",
            [session.id.into(), foreign.id.into()]
        ))
        .await
        .is_err()
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_sessions SET accepting_tasks=FALSE WHERE id=$1",
        [session.id.into()],
    ))
    .await
    .unwrap();
    assert!(
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET accepting_tasks=TRUE WHERE id=$1",
            [session.id.into()]
        ))
        .await
        .is_err()
    );
    let scope = f.scope(f.a.owner_user_id, f.a.id, false).await;
    let deleted = dao::change_node(
        &f.db,
        scope,
        &audit(f.a.owner_user_id, Some(TenantRole::Admin)),
        dao::NodeMutation {
            id: n.id,
            expected_updated_at: n.updated_at,
            action: NodeAction::Delete,
            patch: &NodePatch::default(),
            reason: "unused offline node",
        },
    )
    .await
    .unwrap();
    assert!(deleted.deleted);
    assert!(Node::find_by_id(&f.db, n.id).await.unwrap().is_none());
    assert!(
        NodeSession::find_by_id(&f.db, session.id)
            .await
            .unwrap()
            .is_none()
    );
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn registration_approval_revocation_and_reveal_audit_failures_roll_back() {
    let mut f = Fixture::new().await;
    let reg = f.registration(f.a.id, f.member.id).await;
    let admin = f.scope(f.a.owner_user_id, f.a.id, false).await;
    let own = f.scope(f.member.id, f.a.id, true).await;
    let a = audit(f.a.owner_user_id, Some(TenantRole::Admin));
    let o = audit(f.member.id, Some(TenantRole::Member));
    let tx = f.db.begin().await.unwrap();
    tx.execute_unprepared("UPDATE identity_admin_fence SET version=version+1 WHERE id=TRUE")
        .await
        .unwrap();
    let name = format!("node_registration_audit_{}", Uuid::new_v4().simple());
    tx.execute_unprepared(&format!("CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.tenant_id='{}'::uuid THEN RAISE EXCEPTION 'fixture audit failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER {name} BEFORE INSERT ON tenant_audit_events FOR EACH ROW EXECUTE FUNCTION {name}();",f.a.id)).await.unwrap();
    assert!(
        dao::change_token(
            &tx,
            admin,
            reg.id,
            reg.updated_at,
            NodeAction::ApproveToken,
            &a,
            "approval rollback"
        )
        .await
        .is_err()
    );
    assert_eq!(
        UserNodeGatewayToken::find_by_id(&tx, reg.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "pending"
    );
    // Prepare approval inside this isolated outer transaction only, then test
    // that an audited administrative revocation leaves it approved on failure.
    reg.approve(&tx, f.a.owner_user_id).await.unwrap();
    let approved = UserNodeGatewayToken::find_by_id(&tx, reg.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        dao::change_token(
            &tx,
            admin,
            reg.id,
            approved.updated_at,
            NodeAction::RevokeToken,
            &a,
            "revocation rollback"
        )
        .await
        .is_err()
    );
    assert_eq!(
        UserNodeGatewayToken::find_by_id(&tx, reg.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "approved"
    );
    // Reveal is intentionally caller-transactional; HTTP must roll its own
    // transaction back if audit fails before any credential is returned.
    let reveal = tx.begin().await.unwrap();
    assert!(
        dao::owner_registration_for_reveal(&reveal, own, &o)
            .await
            .is_err()
    );
    reveal.rollback().await.unwrap();
    assert!(
        !UserNodeGatewayToken::find_by_id(&tx, reg.id)
            .await
            .unwrap()
            .unwrap()
            .is_revealed
    );
    tx.execute_unprepared(&format!(
        "DROP TRIGGER {name} ON tenant_audit_events; DROP FUNCTION {name}();"
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let saved = UserNodeGatewayToken::find_by_id(&f.db, reg.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.status, "approved");
    assert!(!saved.is_revealed);
    f.guard.cleanup().await.unwrap();
}

#[tokio::test]
async fn old_runtime_transactions_cannot_regress_node_control_revisions() {
    let mut f = Fixture::new().await;
    let node = f.node(f.a.id, f.member.id, "revision-node").await;
    let scope = f.scope(f.a.owner_user_id, f.a.id, false).await;
    let actor = audit(f.a.owner_user_id, Some(TenantRole::Admin));
    // Start the runtime transaction before another connection updates the row.
    // PostgreSQL NOW() remains this older time even after it sees a newer row.
    let delayed = f.db.begin().await.unwrap();
    delayed
        .query_one(Statement::from_string(DbBackend::Postgres, "SELECT NOW()"))
        .await
        .unwrap();
    let changed = dao::change_node(
        &f.db,
        scope,
        &actor,
        dao::NodeMutation {
            id: node.id,
            expected_updated_at: node.updated_at,
            action: NodeAction::Configure,
            patch: &NodePatch {
                display_name: Some("new-name".into()),
                failure_threshold: None,
            },
            reason: "revision regression",
        },
    )
    .await
    .unwrap();
    let heartbeat = Node::update_heartbeat(&delayed, node.id).await.unwrap();
    delayed.commit().await.unwrap();
    let registration = f.registration(f.a.id, f.member.id).await;
    let delayed = f.db.begin().await.unwrap();
    delayed
        .query_one(Statement::from_string(DbBackend::Postgres, "SELECT NOW()"))
        .await
        .unwrap();
    let approved = dao::change_token(
        &f.db,
        scope,
        registration.id,
        registration.updated_at,
        NodeAction::ApproveToken,
        &actor,
        "approve fixture",
    )
    .await
    .unwrap();
    let revealed = delayed.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE user_node_gateway_tokens SET is_revealed=TRUE,updated_at=NOW() WHERE tenant_id=$1 AND user_id=$2 AND id=$3 RETURNING updated_at",
        [f.a.id.into(), f.member.id.into(), registration.id.into()])).await.unwrap().unwrap();
    let revealed_at: chrono::DateTime<Utc> = revealed.try_get_by_index(0).unwrap();
    delayed.commit().await.unwrap();
    f.guard.cleanup().await.unwrap();
    assert!(
        heartbeat.updated_at > changed.node.updated_at,
        "older runtime transaction regressed node revision: {} <= {}",
        heartbeat.updated_at,
        changed.node.updated_at
    );
    assert!(
        revealed_at > approved.token.updated_at,
        "older runtime transaction regressed registration revision: {} <= {}",
        revealed_at,
        approved.token.updated_at
    );
}
