//! Explicit node ingress tested through the actual HTTP router, PostgreSQL,
//! Redis task transport and a deterministic in-process worker. No paid model.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::db::{
    TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::models::{
    node::{CreateNodeRequest, Node},
    node_session::{CreateNodeSessionRequest, NodeSession},
    passthrough_binding::{CreatePassthroughBindingRequest, PassthroughBinding},
    pricing_model::{BillingDimension, CreatePricingRequest, PricingModel, PricingScopeType},
};
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, DbRouter, UserBalance,
};
use keycompute_ratelimit::RateLimitKey;
use keycompute_server::{
    AppState, create_router,
    state::{AppStateConfig, RateLimitBackendConfig},
};
use keycompute_types::node::{NodeTaskEnvelope, NodeTaskResult};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;

const NT: &str = "/nt/v1/chat/completions";
const PT: &str = "/pt/v1/chat/completions";
const POOL: &str = "/v1/chat/completions";
#[derive(Clone)]
struct UpstreamCall {
    channel: String,
    body: Value,
}
type Calls = Arc<Mutex<Vec<UpstreamCall>>>;
fn completion(model: &str, content: &str) -> Value {
    json!({"id":"chatcmpl-nt-test","object":"chat.completion","created":1,"model":model,
        "choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}})
}
async fn mock_upstream(
    State(calls): State<Calls>,
    Path(channel): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    calls.lock().unwrap().push(UpstreamCall {
        channel: channel.clone(),
        body: body.clone(),
    });
    Json(completion(body["model"].as_str().unwrap(), &channel)).into_response()
}
#[derive(Debug)]
struct HttpResult {
    status: StatusCode,
    body: Value,
    request_id: Option<Uuid>,
}
async fn http(
    app: Router,
    method: Method,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> HttpResult {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    if path == "/v1/messages" {
        request = request.header("anthropic-version", "2023-06-01");
    }
    let response = tokio::time::timeout(
        Duration::from_secs(20),
        app.oneshot(
            request
                .body(
                    body.map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        ),
    )
    .await
    .expect("HTTP node fixture timed out")
    .unwrap();
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let bytes = tokio::time::timeout(
        Duration::from_secs(20),
        to_bytes(response.into_body(), 2 << 20),
    )
    .await
    .unwrap()
    .unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    HttpResult {
        status,
        body,
        request_id,
    }
}
fn expect(r: HttpResult, status: StatusCode) -> Value {
    assert_eq!(r.status, status, "{}", r.body);
    r.body
}

async fn scoped_jwt(state: &AppState, user: &TenantActor) -> String {
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(
            user.id,
            Some(user.tenant_id),
            user.token_version,
            Some(1),
            Some(1),
            3600,
        )
        .unwrap();
    let context = state.auth.verify_token(&global).await.unwrap();
    state
        .auth
        .select_tenant(&context, Some(user.tenant_id))
        .await
        .unwrap()
        .access_token
}

struct Fixture {
    db: DatabaseConnection,
    cleanup: TestDataGuard,
    state: AppState,
    app: Router,
    user: TenantActor,
    key: String,
    key_id: Uuid,
    admin: String,
    node: Node,
    session: NodeSession,
    owner: TenantActor,
    shared: String,
    node_only: String,
    pool_only: String,
    calls: Calls,
    server: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_model_prefix("").await
    }
    async fn with_model_prefix(prefix: &str) -> Self {
        let db = create_test_pool().await;
        let run = Uuid::new_v4().to_string();
        let cleanup = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "nt-consumer", &run).await;
        let mut user = create_test_user(&db, tenant.id, "nt-consumer", &run).await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET platform_role='root' WHERE id=$1",
            [user.id.into()],
        ))
        .await
        .unwrap();
        user.user = keycompute_db::User::find_by_id(&db, user.id)
            .await
            .unwrap()
            .unwrap();
        UserBalance::recharge(
            &db,
            user.tenant_id,
            user.id,
            Decimal::from(1000),
            None,
            Some("isolated node test credit"),
        )
        .await
        .unwrap();
        let owner_tenant = tenant.clone(); // Private nodes serve their own tenant only.
        let owner = create_test_user(&db, owner_tenant.id, "nt-worker", &run).await;
        keycompute_runtime::set_global_crypto("LXmXUgcaoZePsWayXJN2E5Wa9/zpkl/vOTwnLNy/oLc=")
            .unwrap();
        let shared = format!("{prefix}gemma3:{run}");
        let node_only = format!("node-only-{run}");
        let pool_only = format!("pool-only-{run}");
        let calls: Calls = Arc::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/{channel}/v1/chat/completions", post(mock_upstream))
            .with_state(calls.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut accounts = Vec::new();
        for channel in ["pool", "bound"] {
            accounts.push(
                Account::create(
                    &db,
                    &CreateAccountRequest {
                        tenant_id: user.tenant_id,
                        provider: "openai".into(),
                        name: format!("nt-{channel}-{run}"),
                        endpoint: format!("http://{address}/{channel}/v1"),
                        upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(
                            "local-test-upstream",
                        )
                        .unwrap()
                        .into_inner(),
                        upstream_api_key_preview: "test****".into(),
                        rpm_limit: Some(1000),
                        tpm_limit: Some(1_000_000),
                        priority: Some(0),
                        models_supported: vec![shared.clone(), pool_only.clone()],
                        api_capabilities: vec!["chat_completions".into()],
                        pool_enabled: Some(true),
                        visibility: Some("tenant".into()),
                    },
                )
                .await
                .unwrap(),
            );
        }
        let _pool_account = accounts.remove(0);
        let bound_account = accounts.remove(0);
        PassthroughBinding::create(
            &db,
            &CreatePassthroughBindingRequest {
                account_id: bound_account.id,
                tenant_id: user.tenant_id,
                is_global: false,
                pool_enabled: false,
            },
        )
        .await
        .unwrap();
        for (billing_dimension, price) in [
            (BillingDimension::Node, 1),
            (BillingDimension::ProviderAccount, 10),
        ] {
            PricingModel::create(
                &db,
                &CreatePricingRequest {
                    scope_type: PricingScopeType::Tenant,
                    tenant_id: Some(user.tenant_id),
                    model_name: shared.clone(),
                    billing_dimension,
                    currency: Some("CNY".into()),
                    input_price_per_1k: price.into(),
                    output_price_per_1k: (price * 2).into(),
                    is_default: Some(false),
                    effective_from: None,
                    effective_until: None,
                },
            )
            .await
            .unwrap();
        }
        let node=Node::create(&db,&CreateNodeRequest{tenant_id:owner.tenant_id,owner_user_id:owner.id,client_instance_id:format!("nt-{run}"),
            display_name:"isolated NT worker".into(),capabilities_json:json!({"runtime":"ollama","models":[{"model":shared},{"model":node_only}]})}).await.unwrap();
        let session = NodeSession::create(
            &db,
            &CreateNodeSessionRequest {
                native_profiles_json: serde_json::json!([
                    keycompute_types::node_capability::NativeModelProfile::plain_chat(
                        shared.clone()
                    ),
                    keycompute_types::node_capability::NativeModelProfile::plain_chat(
                        node_only.clone()
                    )
                ]),
                native_operations_json: serde_json::json!(["chat"]),
                node_id: node.id,
                session_token_hash: hex::encode(Sha256::digest(Uuid::new_v4().as_bytes())),
                expires_at: Utc::now() + ChronoDuration::hours(1),
                accepted_models_json: json!([shared, node_only]),
            },
        )
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET status='online',last_heartbeat_at=NOW() WHERE id=$1",
            [node.id.into()],
        ))
        .await
        .unwrap();
        let config = AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(keycompute_config::RedisConfig {
                url: integration_tests::common::resolve_redis_url(),
                pool_size: 8,
                node_poll_pool_size: 4,
                node_result_pool_size: 4,
                ..Default::default()
            }),
            node_gateway: Some(keycompute_config::NodeGatewayConfig {
                poll_timeout_secs: Some(2),
                task_deadline_secs: Some(10),
                complete_grace_secs: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let state = AppState::try_with_pool_and_config(DbRouter::single(db.clone()), config)
            .await
            .unwrap();
        assert!(
            state.node_gateway.is_some(),
            "this fixture requires the redis node backend"
        );
        let admin = scoped_jwt(&state, &user).await;
        let key = ProduceAiKeyValidator::generate_key();
        let key_id = integration_tests::db::create_test_api_key(
            &db,
            &CreateProduceAiKeyRequest {
                tenant_id: user.tenant_id,
                user_id: user.id,
                name: "nt-test".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "sk-test****".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap()
        .id;
        let app = create_router(state.clone());
        Self {
            db,
            cleanup,
            state,
            app,
            user,
            key,
            key_id,
            admin,
            node,
            session,
            owner,
            shared,
            node_only,
            pool_only,
            calls,
            server,
        }
    }
    fn body(&self, model: &str, stream: bool) -> Value {
        json!({"model":model,"messages":[{"role":"user","content":"Hello"}],"stream":stream,"max_tokens":16})
    }
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> HttpResult {
        http(self.app.clone(), method, path, Some(&self.key), body).await
    }
    async fn tasks(&self) -> i64 {
        self.db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT n FROM node_tasks WHERE user_id=$1",
                [self.user.id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "n")
            .unwrap()
    }
    fn worker(&self) -> JoinHandle<NodeTaskEnvelope> {
        let service = self.state.node_gateway.as_ref().unwrap().clone();
        let node = self.node.id;
        let session = self.session.id;
        let models = vec![self.shared.clone(), self.node_only.clone()];
        tokio::spawn(async move {
            let task = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(task) = service
                        .poll_task(node, session, models.clone())
                        .await
                        .unwrap()
                        .task
                    {
                        break task;
                    }
                }
            })
            .await
            .expect("no Node task arrived");
            let response = keycompute_types::node_native::NodeNativeHttpResult {
                status: 200,
                headers: vec![],
                body: completion(&task.model, "node-worker"),
            };
            service
                .complete_task(
                    task.task_id,
                    task.lease_id,
                    node,
                    session,
                    NodeTaskResult::NativeSucceeded { response },
                )
                .await
                .unwrap();
            task
        })
    }
    async fn finish(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while self.state.generation_admission.requests.status().active > 0
                || self.state.generation_admission.accounts.status().active > 0
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("request settlement must finish before fixture cleanup");
        self.cleanup.cleanup().await.unwrap();
    }
    async fn node_models(&self) -> Vec<String> {
        let list = expect(
            self.request(Method::GET, "/nt/v1/models", None).await,
            StatusCode::OK,
        );
        list["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_owned())
            .collect()
    }
    async fn assert_ledger(&self, id: Uuid, price: &str, provider: &str) {
        let row=tokio::time::timeout(Duration::from_secs(5),async{loop{
            if let Some(row)=self.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT model_name,provider_name,input_unit_price_snapshot::TEXT AS price,input_tokens,output_tokens FROM usage_logs WHERE request_id=$1",[id.into()])).await.unwrap(){break row;}
            tokio::task::yield_now().await;
        }}).await.unwrap();
        assert_eq!(
            row.try_get::<String>("", "model_name").unwrap(),
            self.shared
        );
        assert_eq!(
            row.try_get::<String>("", "provider_name").unwrap(),
            provider
        );
        assert_eq!(
            row.try_get::<String>("", "price")
                .unwrap()
                .parse::<Decimal>()
                .unwrap(),
            price.parse::<Decimal>().unwrap()
        );
        assert_eq!(row.try_get::<i32>("", "input_tokens").unwrap(), 5);
        assert_eq!(row.try_get::<i32>("", "output_tokens").unwrap(), 3);
    }
}

#[tokio::test]
async fn urls_isolate_same_model_and_keep_node_pricing_raw() {
    let mut f = Fixture::new().await;
    let worker = f.worker();
    let result = f
        .request(Method::POST, NT, Some(f.body(&f.shared, false)))
        .await;
    let id = result.request_id.unwrap();
    let body = expect(result, StatusCode::OK);
    assert_eq!(body["model"], f.shared);
    assert_eq!(body["choices"][0]["message"]["content"], "node-worker");
    let task = worker.await.unwrap();
    assert_eq!(task.model, f.shared);
    assert_eq!(task.payload.native.unwrap().body["model"], f.shared);
    assert!(f.calls.lock().unwrap().is_empty());
    assert_eq!(f.tasks().await, 1);
    f.assert_ledger(id, "1", "node").await;
    let trace=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT request_path,requested_model,route_type FROM gateway_requests WHERE request_id=$1",[id.into()])).await.unwrap().unwrap();
    assert_eq!(trace.try_get::<String>("", "request_path").unwrap(), NT);
    assert_eq!(
        trace.try_get::<String>("", "requested_model").unwrap(),
        f.shared
    );
    assert_eq!(trace.try_get::<String>("", "route_type").unwrap(), "node");
    for (path, channel) in [(POOL, "pool"), (PT, "bound")] {
        let result = f
            .request(Method::POST, path, Some(f.body(&f.shared, false)))
            .await;
        let id = result.request_id.unwrap();
        let body = expect(result, StatusCode::OK);
        assert_eq!(body["choices"][0]["message"]["content"], channel);
        f.assert_ledger(id, "10", "openai").await;
    }
    assert_eq!(f.tasks().await, 1);
    let calls = f.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].channel, "pool");
    assert_eq!(calls[1].channel, "bound");
    assert!(calls.iter().all(|c| c.body["model"] == f.shared));
    let rpm = f
        .state
        .rate_limiter
        .get_rpm_count(&RateLimitKey::new(f.user.tenant_id, f.user.id, f.key_id))
        .await
        .unwrap();
    assert_eq!(
        rpm, 3,
        "each generation consumes one RPM, not middleware plus handler twice"
    );
    f.finish().await;
}

#[tokio::test]
async fn native_streaming_requires_an_sse_capable_native_profile() {
    let mut f = Fixture::new().await;
    let body = expect(
        f.request(Method::POST, NT, Some(f.body(&f.shared, true)))
            .await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert!(body.to_string().contains("No available node") || body.to_string().contains("node"));
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn families_have_independent_model_discovery_without_prefixes() {
    let mut f = Fixture::new().await;
    let nodes = f.node_models().await;
    assert!(nodes.contains(&f.shared));
    assert!(nodes.contains(&f.node_only));
    assert!(!nodes.contains(&f.pool_only));
    let entry = expect(
        f.request(Method::GET, &format!("/nt/v1/models/{}", f.shared), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(entry["id"], f.shared);
    for path in ["/v1/models", "/pt/v1/models"] {
        let list = expect(f.request(Method::GET, path, None).await, StatusCode::OK);
        let ids: Vec<_> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert!(ids.contains(&f.shared.as_str()));
        assert!(ids.contains(&f.pool_only.as_str()));
        assert!(!ids.contains(&f.node_only.as_str()));
    }
    for path in [
        "/v1/models?mode=node_dispatch",
        "/nt/v1/models?mode=account_pool",
        "/pt/v1/models?mode=node_dispatch",
    ] {
        expect(
            f.request(Method::GET, path, None).await,
            StatusCode::BAD_REQUEST,
        );
    }
    for path in [
        "/nt/v1/models?protocol=anthropic",
        "/nt/v1/models?capability=responses",
    ] {
        let listed = expect(f.request(Method::GET, path, None).await, StatusCode::OK);
        assert!(
            !listed["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == f.shared),
            "Chat-only worker must not advertise new operations"
        );
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn undeclared_node_prefix_returns_the_same_errors_as_other_missing_models() {
    let mut f = Fixture::new().await;
    for (path, status) in [
        (POOL, StatusCode::NOT_FOUND),
        (PT, StatusCode::NOT_FOUND),
        (NT, StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let missing = format!("missing-{}", f.shared);
        let normal = expect(
            f.request(Method::POST, path, Some(f.body(&missing, false)))
                .await,
            status,
        );
        for prefix in ["node:", "NODE:", "Node:"] {
            let model = format!("{prefix}{}", f.shared);
            let error = expect(
                f.request(Method::POST, path, Some(f.body(&model, false)))
                    .await,
                status,
            );
            assert_eq!(
                error.to_string().replace(&model, "<model>"),
                normal.to_string().replace(&missing, "<model>")
            );
            let text = error.to_string().to_lowercase();
            assert!(
                !text.contains("migration")
                    && !text.contains("routing prefix")
                    && !text.contains("no longer supported")
            );
            let base = path.strip_suffix("/chat/completions").unwrap();
            let absent = expect(
                f.request(Method::GET, &format!("{base}/models/{missing}"), None)
                    .await,
                StatusCode::NOT_FOUND,
            );
            let detail = expect(
                f.request(Method::GET, &format!("{base}/models/{model}"), None)
                    .await,
                StatusCode::NOT_FOUND,
            );
            assert_eq!(
                detail.to_string().replace(&model, "<model>"),
                absent.to_string().replace(&missing, "<model>")
            );
        }
    }
    expect(
        f.request(Method::POST, POOL, Some(f.body(&f.node_only, false)))
            .await,
        StatusCode::NOT_FOUND,
    );
    expect(
        f.request(Method::POST, PT, Some(f.body(&f.node_only, false)))
            .await,
        StatusCode::NOT_FOUND,
    );
    expect(
        f.request(Method::POST, NT, Some(f.body(&f.pool_only, false)))
            .await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn mode_body_and_query_cannot_override_the_generation_url() {
    let mut f = Fixture::new().await;
    let mut body = f.body(&f.shared, false);
    body["access_mode"] = "node_dispatch".into();
    body["mode"] = "node_dispatch".into();
    let reply = expect(
        f.request(
            Method::POST,
            "/v1/chat/completions?mode=node_dispatch",
            Some(body),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(reply["choices"][0]["message"]["content"], "pool");
    assert_eq!(f.tasks().await, 0);
    let worker = f.worker();
    let mut body = f.body(&f.shared, false);
    body["access_mode"] = "account_pool".into();
    let reply = expect(
        f.request(
            Method::POST,
            "/nt/v1/chat/completions?mode=account_pool",
            Some(body),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(reply["choices"][0]["message"]["content"], "node-worker");
    worker.await.unwrap();
    assert_eq!(f.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn nt_authentication_and_unsupported_endpoints_never_create_work() {
    let mut f = Fixture::new().await;
    expect(
        http(
            f.app.clone(),
            Method::POST,
            NT,
            None,
            Some(f.body(&f.shared, false)),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    );
    expect(
        http(f.app.clone(), Method::GET, "/nt/v1/models", None, None).await,
        StatusCode::UNAUTHORIZED,
    );
    for path in ["/nt/v1/images/generations", "/nt/v1/embeddings"] {
        let response = f
            .request(Method::POST, path, Some(f.body(&f.shared, false)))
            .await;
        assert!(matches!(
            response.status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ));
        assert!(
            response.body.is_object(),
            "unsupported Node API must not be SPA HTML: {}",
            response.body
        );
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn node_readiness_revocation_changes_discovery_and_dispatch_not_pool() {
    let mut f = Fixture::new().await;
    assert!(f.node_models().await.contains(&f.shared));
    for action in ["offline", "session_expired", "owner_inactive"] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE nodes SET status=$2 WHERE id=$1",
            [
                f.node.id.into(),
                if action == "offline" {
                    "offline"
                } else {
                    "online"
                }
                .into(),
            ],
        ))
        .await
        .unwrap();
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE node_sessions SET expires_at=$2 WHERE id=$1",
            [
                f.session.id.into(),
                (Utc::now()
                    + ChronoDuration::seconds(if action == "session_expired" {
                        -60
                    } else {
                        3600
                    }))
                .into(),
            ],
        ))
        .await
        .unwrap();
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET status=$2 WHERE user_id=$1 AND tenant_id=$3",
            [
                f.owner.id.into(),
                if action == "owner_inactive" {
                    "suspended"
                } else {
                    "active"
                }
                .into(),
                f.owner.tenant_id.into(),
            ],
        ))
        .await
        .unwrap();
        assert!(!f.node_models().await.contains(&f.shared), "{action}");
        expect(
            f.request(Method::POST, NT, Some(f.body(&f.shared, false)))
                .await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
    }
    expect(
        f.request(Method::POST, POOL, Some(f.body(&f.shared, false)))
            .await,
        StatusCode::OK,
    );
    assert_eq!(f.tasks().await, 0);
    assert_eq!(f.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn pricing_preview_uses_mode_not_the_raw_model_name() {
    let mut f = Fixture::new().await;
    for (mode, expected) in [
        ("node_dispatch", "3"),
        ("account_pool", "30"),
        ("passthrough", "30"),
    ] {
        let result = http(
            f.app.clone(),
            Method::POST,
            "/api/v1/pricing/calculate",
            Some(&f.admin),
            Some(json!({"model":f.shared,"mode":mode,"input_tokens":1000,"output_tokens":1000})),
        )
        .await;
        let response = expect(result, StatusCode::OK);
        let amount = response["total_cost"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| response["total_cost"].to_string());
        assert_eq!(
            amount.parse::<Decimal>().unwrap(),
            expected.parse::<Decimal>().unwrap()
        );
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn cancel_node_request_releases_admission_without_account_fallback() {
    let mut f = Fixture::new().await;
    let app = f.app.clone();
    let key = f.key.clone();
    let body = f.body(&f.shared, false);
    let request =
        tokio::spawn(async move { http(app, Method::POST, NT, Some(&key), Some(body)).await });
    tokio::time::timeout(Duration::from_secs(8), async {
        while f.tasks().await == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(15),async {loop {
        let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT n FROM balance_reservations WHERE user_id=$1 AND status='active'",[f.user.id.into()])).await.unwrap().unwrap();
        if row.try_get::<i64>("","n").unwrap()==0 && f.state.generation_admission.requests.status().active==0 {break;}
        tokio::time::sleep(Duration::from_millis(30)).await;
    }}).await.expect("Node cancellation must release request permits and settle unused reservation");
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn disabled_node_service_fails_before_execution_budgets() {
    let mut f = Fixture::new().await;
    let mut disabled = f.state.clone();
    disabled.node_gateway = None;
    let app = create_router(disabled);
    let limits = RateLimitKey::new(f.user.tenant_id, f.user.id, f.key_id);
    let before = f.state.rate_limiter.get_rpm_count(&limits).await.unwrap();
    let result = http(
        app.clone(),
        Method::POST,
        NT,
        Some(&f.key),
        Some(f.body(&f.shared, false)),
    )
    .await;
    expect(result, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        f.state.rate_limiter.get_rpm_count(&limits).await.unwrap(),
        before
    );
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT n FROM balance_reservations WHERE user_id=$1 AND status='active'",
        [f.user.id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    let models = expect(
        http(
            app.clone(),
            Method::GET,
            "/nt/v1/models",
            Some(&f.key),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert!(models["data"].as_array().unwrap().is_empty());
    let debug = format!(
        "/api/v1/debug/routing?mode=node_dispatch&model={}",
        f.shared
    );
    expect(
        http(app, Method::GET, &debug, Some(&f.admin), None).await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    f.finish().await;
}

#[tokio::test]
async fn declared_node_prefixes_are_normal_account_model_names() {
    let mut f = Fixture::new().await;
    let names = [
        format!("node:{}", f.shared),
        format!("NODE:{}", f.shared),
        format!("Node:{}", f.shared),
    ];
    let mut models = names.to_vec();
    models.push(f.shared.clone());
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET models_supported=$2::TEXT[] WHERE tenant_id=$1",
        [f.user.tenant_id.into(), models.into()],
    ))
    .await
    .unwrap();
    for (base, channel) in [("/v1", "pool"), ("/pt/v1", "bound")] {
        let listed = expect(
            f.request(Method::GET, &format!("{base}/models"), None)
                .await,
            StatusCode::OK,
        );
        let data = listed["data"].as_array().unwrap();
        assert!(data.iter().any(|m| m["id"] == f.shared));
        for model in &names {
            assert!(data.iter().any(|m| m["id"] == *model));
            let detail = expect(
                f.request(Method::GET, &format!("{base}/models/{model}"), None)
                    .await,
                StatusCode::OK,
            );
            assert_eq!(detail["id"], *model);
            let result = expect(
                f.request(
                    Method::POST,
                    &format!("{base}/chat/completions"),
                    Some(f.body(model, false)),
                )
                .await,
                StatusCode::OK,
            );
            assert_eq!(result["model"], *model);
            assert_eq!(result["choices"][0]["message"]["content"], channel);
        }
    }
    assert_eq!(f.tasks().await, 0);
    assert_eq!(f.calls.lock().unwrap().len(), 6);
    f.finish().await;
}

#[tokio::test]
async fn routing_debug_validates_mode_and_protocol_before_planning() {
    let mut f = Fixture::new().await;
    for mode in ["node_dispatch", "passthrough"] {
        for entry in ["invalid"] {
            let path = format!(
                "/api/v1/debug/routing?mode={mode}&entry={entry}&model={}",
                f.shared
            );
            expect(
                http(f.app.clone(), Method::GET, &path, Some(&f.admin), None).await,
                StatusCode::BAD_REQUEST,
            );
        }
    }
    for mode in ["node_dispatch", "passthrough"] {
        let path = format!(
            "/api/v1/debug/routing?mode={mode}&entry=anthropic&model={}",
            f.shared
        );
        let result = expect(
            http(f.app.clone(), Method::GET, &path, Some(&f.admin), None).await,
            StatusCode::OK,
        );
        assert_eq!(
            result["routed"], false,
            "the fixture has no Messages execution capability"
        );
    }
    let path = format!(
        "/api/v1/debug/routing?mode=node_dispatch&entry=openai&model={}",
        f.shared
    );
    let response = expect(
        http(f.app.clone(), Method::GET, &path, Some(&f.admin), None).await,
        StatusCode::OK,
    );
    assert_eq!(response["routed"], true);
    assert_eq!(response["primary"]["provider"], "node");
    assert_eq!(response["primary"]["model"], f.shared);
    assert!(response["primary"]["account_id"].is_null());
    assert!(response["fallback_chain"].as_array().unwrap().is_empty());
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn literal_node_prefixed_name_uses_the_url_selected_execution_family() {
    let mut f = Fixture::with_model_prefix("node:").await;
    for base in ["/v1", "/pt/v1", "/nt/v1"] {
        let listed = expect(
            f.request(Method::GET, &format!("{base}/models"), None)
                .await,
            StatusCode::OK,
        );
        assert!(
            listed["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == f.shared)
        );
        let detail = expect(
            f.request(Method::GET, &format!("{base}/models/{}", f.shared), None)
                .await,
            StatusCode::OK,
        );
        assert_eq!(detail["id"], f.shared);
    }
    for (path, channel) in [(POOL, "pool"), (PT, "bound")] {
        let response = expect(
            f.request(Method::POST, path, Some(f.body(&f.shared, false)))
                .await,
            StatusCode::OK,
        );
        assert_eq!(response["model"], f.shared);
        assert_eq!(response["choices"][0]["message"]["content"], channel);
    }
    assert_eq!(f.tasks().await, 0);
    let worker = f.worker();
    let response = expect(
        f.request(Method::POST, NT, Some(f.body(&f.shared, false)))
            .await,
        StatusCode::OK,
    );
    let task = worker.await.unwrap();
    assert_eq!(response["model"], f.shared);
    assert_eq!(task.model, f.shared);
    assert_eq!(
        task.payload.native.as_ref().unwrap().body["model"],
        f.shared
    );
    assert_eq!(f.tasks().await, 1);
    assert_eq!(f.calls.lock().unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn unknown_prefixed_models_use_normal_responses_and_messages_errors() {
    let mut f = Fixture::new().await;
    let missing = format!("missing-{}", f.shared);
    let prefixed = format!("node:{}", f.shared);
    for path in ["/v1/responses", "/v1/messages"] {
        let payload = |model: &str| {
            if path == "/v1/responses" {
                json!({"model":model,"input":"Hello","max_output_tokens":16,"store":false})
            } else {
                json!({"model":model,"messages":[{"role":"user","content":"Hello"}],"max_tokens":16})
            }
        };
        let ordinary = expect(
            f.request(Method::POST, path, Some(payload(&missing))).await,
            StatusCode::NOT_FOUND,
        );
        let response = expect(
            f.request(Method::POST, path, Some(payload(&prefixed)))
                .await,
            StatusCode::NOT_FOUND,
        );
        assert_eq!(
            response.to_string().replace(&prefixed, "<model>"),
            ordinary.to_string().replace(&missing, "<model>")
        );
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn native_claim_uses_immutable_session_permission_not_node_metadata() {
    let mut f = Fixture::new().await;
    let service = f.state.node_gateway.as_ref().unwrap();
    let old = NodeSession::create(
        &f.db,
        &CreateNodeSessionRequest {
            native_profiles_json: json!([]),
            node_id: f.node.id,
            session_token_hash: hex::encode(Sha256::digest(Uuid::new_v4().as_bytes())),
            accepted_models_json: json!([f.shared]),
            native_operations_json: json!([]),
            expires_at: Utc::now() + ChronoDuration::hours(1),
        },
    )
    .await
    .unwrap();
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE nodes SET capabilities_json=capabilities_json || '{\"native_operations\":[\"chat\"]}'::jsonb WHERE id=$1",[f.node.id.into()])).await.unwrap();
    let payload = keycompute_types::node::NodeTaskPayload {
        request_id: Uuid::new_v4(),
        chat: None,
        image_generation: None,
        image_edit: None,
        native: Some(keycompute_types::node_native::NodeNativeRequest {
            operation: keycompute_types::node_native::NodeNativeOperation::Chat,
            body: f.body(&f.shared, false),
            headers: vec![],
        }),
    };
    let task = service
        .store
        .create_and_enqueue_task(f.user.tenant_id, f.user.id, f.shared.clone(), payload)
        .await
        .unwrap();
    assert!(
        service
            .store
            .claim_task(task.id, f.node.id, old.id)
            .await
            .unwrap()
            .is_none()
    );
    let (_, envelope) = service
        .store
        .claim_task(task.id, f.node.id, f.session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(envelope.payload.native.is_some());
    let response = keycompute_types::node_native::NodeNativeHttpResult {
        status: 200,
        headers: vec![],
        body: completion(&f.shared, "native"),
    };
    let result = NodeTaskResult::NativeSucceeded { response };
    service
        .complete_task(
            task.id,
            envelope.lease_id,
            f.node.id,
            f.session.id,
            result.clone(),
        )
        .await
        .unwrap();
    let repeated = service
        .complete_task(task.id, envelope.lease_id, f.node.id, f.session.id, result)
        .await
        .unwrap();
    assert_eq!(repeated.task_status, "succeeded");
    f.finish().await;
}

#[tokio::test]
async fn uncertain_native_execution_failure_is_terminal_and_cannot_be_requeued() {
    let mut f = Fixture::new().await;
    let service = f.state.node_gateway.as_ref().unwrap();
    let task = service
        .store
        .create_and_enqueue_task(
            f.user.tenant_id,
            f.user.id,
            f.shared.clone(),
            keycompute_types::node::NodeTaskPayload {
                request_id: Uuid::new_v4(),
                chat: None,
                image_generation: None,
                image_edit: None,
                native: Some(keycompute_types::node_native::NodeNativeRequest {
                    operation: keycompute_types::node_native::NodeNativeOperation::Chat,
                    body: f.body(&f.shared, false),
                    headers: vec![],
                }),
            },
        )
        .await
        .unwrap();
    let (_, envelope) = service
        .store
        .claim_task(task.id, f.node.id, f.session.id)
        .await
        .unwrap()
        .unwrap();
    let response = service
        .complete_task(
            task.id,
            envelope.lease_id,
            f.node.id,
            f.session.id,
            NodeTaskResult::Failed {
                code: "native_timeout".into(),
                message: "completion unknown".into(),
                is_client_error: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(response.task_status, "failed");
    assert_eq!(
        response.action,
        keycompute_types::node::NodeTaskCompleteAction::Failed
    );
    assert!(
        keycompute_db::models::node_task::NodeTask::requeue(&f.db, task.id)
            .await
            .is_err()
    );
    assert!(
        service
            .store
            .claim_task(task.id, f.node.id, f.session.id)
            .await
            .unwrap()
            .is_none()
    );
    let row = keycompute_db::models::node_task::NodeTask::find_by_id(&f.db, task.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "failed");
    assert!(row.finished_at.is_some());
    f.finish().await;
}

#[tokio::test]
async fn native_capable_worker_can_claim_legacy_work_without_native_queue_starvation() {
    let mut f = Fixture::new().await;
    let service = f.state.node_gateway.as_ref().unwrap().clone();
    let model = f.shared.clone();
    let user = f.user.id;
    let tenant = f.user.tenant_id;
    let producer = service.clone();
    let pending = tokio::spawn(async move {
        producer
            .enqueue_and_wait(
                tenant,
                user,
                model.clone(),
                keycompute_types::node::NodeTaskPayload {
                    request_id: Uuid::new_v4(),
                    native: None,
                    image_generation: None,
                    image_edit: None,
                    chat: Some(keycompute_types::ChatCompletionRequest::new(model, vec![])),
                },
            )
            .await
    });
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if f.tasks().await > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(ready.is_ok());
    let envelope = tokio::time::timeout(
        Duration::from_secs(3),
        service.poll_task(f.node.id, f.session.id, vec![f.shared.clone()]),
    )
    .await
    .unwrap()
    .unwrap()
    .task
    .unwrap();
    assert!(envelope.payload.native.is_none());
    assert!(envelope.payload.chat.is_some());
    service
        .complete_task(
            envelope.task_id,
            envelope.lease_id,
            f.node.id,
            f.session.id,
            NodeTaskResult::Succeeded {
                response: serde_json::from_value(completion(&f.shared, "legacy")).unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(pending.await.unwrap().unwrap().model, f.shared);
    f.finish().await;
}

fn native_caps(
    model: &str,
    features: Vec<keycompute_types::node_capability::NativeFeature>,
) -> keycompute_types::node::NodeCapabilities {
    let mut profile = keycompute_types::node_capability::NativeModelProfile::plain_chat(model);
    profile.features = features;
    keycompute_types::node::NodeCapabilities {
        runtime: "ollama".into(),
        models: vec![keycompute_types::node::NodeModelCapability {
            model: model.into(),
        }],
        native_operations: vec![keycompute_types::node_native::NodeNativeOperation::Chat],
        native_profiles: vec![profile],
        runtime_version: Some("0.16.0-test".into()),
    }
}
#[tokio::test]
async fn native_tool_requirements_are_checked_before_route_and_again_at_claim() {
    let mut f = Fixture::new().await;
    let service = f.state.node_gateway.as_ref().unwrap();
    let mut body = f.body(&f.shared, false);
    body["tools"] =
        json!([{"type":"function","function":{"name":"f","parameters":{"type":"object"}}}]);
    expect(
        f.request(Method::POST, NT, Some(body.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert_eq!(f.tasks().await, 0);
    let task = service
        .store
        .create_and_enqueue_task(
            f.user.tenant_id,
            f.user.id,
            f.shared.clone(),
            keycompute_types::node::NodeTaskPayload {
                request_id: Uuid::new_v4(),
                chat: None,
                image_generation: None,
                image_edit: None,
                native: Some(keycompute_types::node_native::NodeNativeRequest {
                    operation: keycompute_types::node_native::NodeNativeOperation::Chat,
                    body: body.clone(),
                    headers: vec![],
                }),
            },
        )
        .await
        .unwrap();
    assert!(
        service
            .store
            .claim_task(task.id, f.node.id, f.session.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .store
            .claim_next_native_task(f.node.id, f.session.id)
            .await
            .unwrap()
            .is_none()
    );
    let caps = native_caps(
        &f.shared,
        vec![keycompute_types::node_capability::NativeFeature::Tools],
    );
    let upgraded = service
        .store
        .negotiate_capabilities(f.node.id, f.session.id, &caps)
        .await
        .unwrap();
    let (selected, envelope) = service
        .store
        .claim_next_native_task(f.node.id, upgraded.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(selected.id, task.id);
    assert_eq!(envelope.payload.native.as_ref().unwrap().body, body);
    service
        .complete_task(
            task.id,
            envelope.lease_id,
            f.node.id,
            upgraded.session_id,
            NodeTaskResult::NativeSucceeded {
                response: keycompute_types::node_native::NodeNativeHttpResult {
                    status: 200,
                    headers: vec![],
                    body: completion(&f.shared, "tools"),
                },
            },
        )
        .await
        .unwrap();
    assert!(f.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn capability_renewal_is_retry_safe_and_old_session_only_finishes_existing_work() {
    let mut f = Fixture::new().await;
    let secret = format!("isolated-session-{}", Uuid::new_v4());
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_sessions SET session_token_hash=$2 WHERE id=$1",
        [
            f.session.id.into(),
            hex::encode(Sha256::digest(secret.as_bytes())).into(),
        ],
    ))
    .await
    .unwrap();
    let service = f.state.node_gateway.as_ref().unwrap();
    let task = service
        .store
        .create_and_enqueue_task(
            f.user.tenant_id,
            f.user.id,
            f.shared.clone(),
            keycompute_types::node::NodeTaskPayload {
                request_id: Uuid::new_v4(),
                chat: None,
                image_generation: None,
                image_edit: None,
                native: Some(keycompute_types::node_native::NodeNativeRequest {
                    operation: keycompute_types::node_native::NodeNativeOperation::Chat,
                    body: f.body(&f.shared, false),
                    headers: vec![],
                }),
            },
        )
        .await
        .unwrap();
    let (_, envelope) = service
        .store
        .claim_task(task.id, f.node.id, f.session.id)
        .await
        .unwrap()
        .unwrap();
    // The predecessor is about to expire, while its issued lease remains valid.
    // Renewal must retain completion authentication until the lease grace ends.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_sessions SET expires_at=NOW()+INTERVAL '1 second' WHERE id=$1",
        [f.session.id.into()],
    ))
    .await
    .unwrap();
    let old = f.session.native_profiles_json.clone();
    let caps = native_caps(
        &f.shared,
        vec![keycompute_types::node_capability::NativeFeature::Tools],
    );
    let request = json!({"protocol_version":"node.v1","node_id":f.node.id,"session_id":f.session.id,"capabilities":caps});
    let renewed = expect(
        http(
            f.app.clone(),
            Method::POST,
            "/node/v1/capabilities",
            Some(&secret),
            Some(request.clone()),
        )
        .await,
        StatusCode::OK,
    );
    let repeated = expect(
        http(
            f.app.clone(),
            Method::POST,
            "/node/v1/capabilities",
            Some(&secret),
            Some(request),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(renewed, repeated);
    assert_ne!(renewed["session_id"], f.session.id.to_string());
    let stored = NodeSession::find_by_id(&f.db, f.session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.native_profiles_json, old);
    assert!(!stored.accepting_tasks);
    assert!(stored.expires_at >= task.complete_grace_until);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    expect(
        http(
            f.app.clone(),
            Method::POST,
            "/node/v1/tasks/poll",
            Some(&secret),
            Some(
                json!({"protocol_version":"node.v1","node_id":f.node.id,"session_id":f.session.id}),
            ),
        )
        .await,
        StatusCode::UNAUTHORIZED,
    );
    let result = NodeTaskResult::NativeSucceeded {
        response: keycompute_types::node_native::NodeNativeHttpResult {
            status: 200,
            headers: vec![],
            body: completion(&f.shared, "completed-old-lease"),
        },
    };
    let complete = json!({"protocol_version":"node.v1","node_id":f.node.id,"session_id":f.session.id,"task_id":task.id,"lease_id":envelope.lease_id,"result":result});
    expect(
        http(
            f.app.clone(),
            Method::POST,
            &format!("/node/v1/tasks/{}/complete", task.id),
            Some(&secret),
            Some(complete),
        )
        .await,
        StatusCode::OK,
    );
    f.finish().await;
}

#[tokio::test]
async fn native_byte_and_output_limits_are_enforced_by_sql_claim() {
    let mut f = Fixture::new().await;
    let service = f.state.node_gateway.as_ref().unwrap();
    let task = service
        .store
        .create_and_enqueue_task(
            f.user.tenant_id,
            f.user.id,
            f.shared.clone(),
            keycompute_types::node::NodeTaskPayload {
                request_id: Uuid::new_v4(),
                chat: None,
                image_generation: None,
                image_edit: None,
                native: Some(keycompute_types::node_native::NodeNativeRequest {
                    operation: keycompute_types::node_native::NodeNativeOperation::Chat,
                    body: f.body(&f.shared, false),
                    headers: vec![],
                }),
            },
        )
        .await
        .unwrap();
    let mut caps = native_caps(&f.shared, vec![]);
    caps.native_profiles[0].max_request_bytes = 16;
    let narrow = service
        .store
        .negotiate_capabilities(f.node.id, f.session.id, &caps)
        .await
        .unwrap();
    assert!(
        service
            .store
            .claim_next_native_task(f.node.id, narrow.session_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .store
            .claim_task(task.id, f.node.id, narrow.session_id)
            .await
            .unwrap()
            .is_none()
    );
    caps.native_profiles[0].max_request_bytes = 1024 * 1024;
    caps.native_profiles[0].max_output_tokens = Some(1);
    let narrow = service
        .store
        .negotiate_capabilities(f.node.id, narrow.session_id, &caps)
        .await
        .unwrap();
    assert!(
        service
            .store
            .claim_next_native_task(f.node.id, narrow.session_id)
            .await
            .unwrap()
            .is_none()
    );
    caps.native_profiles[0].max_output_tokens = Some(32);
    let ready = service
        .store
        .negotiate_capabilities(f.node.id, narrow.session_id, &caps)
        .await
        .unwrap();
    let (claimed, envelope) = service
        .store
        .claim_next_native_task(f.node.id, ready.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, task.id);
    service
        .complete_task(
            task.id,
            envelope.lease_id,
            f.node.id,
            ready.session_id,
            NodeTaskResult::NativeSucceeded {
                response: keycompute_types::node_native::NodeNativeHttpResult {
                    status: 200,
                    headers: vec![],
                    body: completion(&f.shared, "bounded"),
                },
            },
        )
        .await
        .unwrap();
    f.finish().await;
}
#[tokio::test]
async fn heartbeat_cannot_expand_immutable_registered_model_scope() {
    let mut f = Fixture::new().await;
    let other = format!("not-registered-{}", f.shared);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE nodes SET capabilities_json=$2 WHERE id=$1",
        [
            f.node.id.into(),
            json!({"runtime":"ollama","models":[{"model":f.shared},{"model":other}]}).into(),
        ],
    ))
    .await
    .unwrap();
    let result = f
        .state
        .node_gateway
        .as_ref()
        .unwrap()
        .heartbeat(f.node.id, f.session.id, vec![other])
        .await;
    assert!(result.is_err());
    let stored = NodeSession::find_by_id(&f.db, f.session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.accepted_models_json, f.session.accepted_models_json);
    f.finish().await;
}

#[tokio::test]
async fn a_native_backlog_does_not_starve_queued_legacy_work() {
    let mut f = Fixture::new().await;
    let service = f.state.node_gateway.as_ref().unwrap();
    let redis = node_gateway::NodeGatewayRedis::new(
        Arc::new(
            keycompute_runtime::redis_store::RedisRuntimeStore::new(
                &std::env::var("REDIS_URL").unwrap(),
            )
            .unwrap(),
        ),
        &keycompute_config::RedisConfig {
            url: std::env::var("REDIS_URL").unwrap(),
            ..Default::default()
        },
    )
    .unwrap();
    for native in [true, true, false] {
        let payload = keycompute_types::node::NodeTaskPayload {
            request_id: Uuid::new_v4(),
            image_generation: None,
            image_edit: None,
            chat: (!native)
                .then(|| keycompute_types::ChatCompletionRequest::new(&f.shared, vec![])),
            native: native.then(|| keycompute_types::node_native::NodeNativeRequest {
                operation: keycompute_types::node_native::NodeNativeOperation::Chat,
                body: f.body(&f.shared, false),
                headers: vec![],
            }),
        };
        let task = service
            .store
            .create_and_enqueue_task(f.user.tenant_id, f.user.id, f.shared.clone(), payload)
            .await
            .unwrap();
        if !native {
            redis.push_to_model_queue(&f.shared, task.id).await.unwrap();
        }
    }
    let first = service
        .poll_task(f.node.id, f.session.id, vec![f.shared.clone()])
        .await
        .unwrap()
        .task
        .unwrap();
    let second = service
        .poll_task(f.node.id, f.session.id, vec![f.shared.clone()])
        .await
        .unwrap()
        .task
        .unwrap();
    assert!(first.payload.native.is_some());
    assert!(
        second.payload.chat.is_some(),
        "legacy work must win the next alternating poll even with native backlog"
    );
    f.finish().await;
}
