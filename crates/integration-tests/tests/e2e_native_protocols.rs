//! Protocol-fidelity regressions through the real router, database and task service.
//! Every account and worker is local to the fixture. No real inference is used.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::db::{
    TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::models::{
    node::{CreateNodeRequest, Node},
    node_session::{CreateNodeSessionRequest, NodeSession},
    passthrough_binding::{CreatePassthroughBindingRequest, PassthroughBinding},
};
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, CreateUserRequest, DbRouter,
    ProduceAiKey, User, UserBalance,
};
use keycompute_server::{
    AppState, create_router,
    state::{AppStateConfig, RateLimitBackendConfig},
};
use keycompute_types::{
    UserRole,
    node::{NodeTaskEnvelope, NodeTaskResult},
    node_capability::{NativeFeature, NativeModelProfile},
    node_native::{NodeNativeHttpResult, NodeNativeOperation as Op},
};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;
#[derive(Clone, Debug)]
struct Call {
    path: String,
    body: Value,
    headers: HeaderMap,
}
#[derive(Default)]
struct Upstream {
    calls: Mutex<Vec<Call>>,
    status: AtomicU16,
}
fn sample_response(op: Op, model: &str) -> Value {
    match op {
        Op::Chat => {
            json!({"id":"chatcmpl-native","object":"chat.completion","created":1,"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-new","type":"function","function":{"name":"weather","arguments":"{\"city\":\"x\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10},"vendor":{"untouched":[null,1]}})
        }
        Op::Messages => {
            json!({"id":"msg-native","type":"message","role":"assistant","model":model,"content":[{"type":"tool_use","id":"call-new","name":"weather","input":{"city":"x"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":7,"output_tokens":3},"vendor":{"untouched":[null,1]}})
        }
        Op::Responses => {
            json!({"id":"resp-native","object":"response","created_at":1,"model":model,"status":"completed","error":null,"output":[{"id":"fc-new","type":"function_call","call_id":"call-new","name":"weather","arguments":"{\"city\":\"x\"}","status":"completed"}],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10},"vendor":{"untouched":[null,1]}})
        }
    }
}
async fn upstream(
    State(s): State<Arc<Upstream>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    s.calls.lock().unwrap().push(Call {
        path: path.clone(),
        body: body.clone(),
        headers,
    });
    let op = if path.ends_with("messages") {
        Op::Messages
    } else if path.ends_with("responses") {
        Op::Responses
    } else {
        Op::Chat
    };
    let status = s.status.load(Ordering::Relaxed);
    let response = if status == 200 {
        sample_response(op, body["model"].as_str().unwrap())
    } else {
        json!({"type":"error","error":{"type":"invalid_request_error","code":"isolated_rejection","message":"Fixture rejected the request","detail":[null,1]}})
    };
    (
        StatusCode::from_u16(status).unwrap(),
        [
            ("x-request-id", "fixture-request"),
            ("set-cookie", "do-not-forward"),
        ],
        Json(response),
    )
        .into_response()
}
#[derive(Debug)]
struct HttpResult {
    status: StatusCode,
    body: Value,
    headers: HeaderMap,
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
    if path.ends_with("/messages") {
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
    .expect("request timeout")
    .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 2 << 20).await.unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    HttpResult {
        status,
        body,
        headers,
    }
}
fn expect(r: HttpResult, status: StatusCode) -> Value {
    assert_eq!(r.status, status, "{}", r.body);
    r.body
}
struct Fixture {
    db: DatabaseConnection,
    cleanup: TestDataGuard,
    state: AppState,
    app: Router,
    user: User,
    key: String,
    model: String,
    node: Node,
    session: NodeSession,
    accounts: Vec<Account>,
    upstream: Arc<Upstream>,
    server: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let db = create_test_pool().await;
        let run = Uuid::new_v4().to_string();
        let cleanup = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "native-multi", &run).await;
        let user = User::create(
            &db,
            &CreateUserRequest {
                tenant_id: tenant.id,
                email: format!("native-{run}@example.invalid"),
                name: Some("Native protocol fixture".into()),
                role: Some(UserRole::Admin),
            },
        )
        .await
        .unwrap();
        UserBalance::recharge(
            &db,
            user.id,
            tenant.id,
            Decimal::from(100),
            None,
            Some("isolated protocol tests"),
        )
        .await
        .unwrap();
        let owner = create_test_user(&db, tenant.id, "native-worker", &run).await;
        keycompute_runtime::set_global_crypto(&keycompute_runtime::ApiKeyCrypto::generate_key())
            .unwrap();
        let model = format!("native-multi:{run}");
        let upstream = Arc::new(Upstream::default());
        upstream.status.store(200, Ordering::Relaxed);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mock = Router::new()
            .route("/{*path}", post(crate::upstream))
            .with_state(upstream.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        let mut accounts = Vec::new();
        for provider in ["openai", "anthropic"] {
            let account = Account::create(
                &db,
                &CreateAccountRequest {
                    tenant_id: tenant.id,
                    provider: provider.into(),
                    name: format!("native-{provider}-{run}"),
                    endpoint: format!("http://{address}/{provider}/v1"),
                    upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(
                        "upstream-only-fixture",
                    )
                    .unwrap()
                    .into_inner(),
                    upstream_api_key_preview: "test***".into(),
                    rpm_limit: Some(1000),
                    tpm_limit: Some(1000000),
                    priority: Some(0),
                    models_supported: vec![model.clone()],
                    api_capabilities: if provider == "openai" {
                        vec!["chat_completions".into(), "responses".into()]
                    } else {
                        vec!["messages".into()]
                    },
                    pool_enabled: Some(true),
                    visibility: Some("tenant".into()),
                },
            )
            .await
            .unwrap();
            PassthroughBinding::create(
                &db,
                &CreatePassthroughBindingRequest {
                    account_id: account.id,
                    tenant_id: tenant.id,
                    is_global: false,
                    pool_enabled: true,
                },
            )
            .await
            .unwrap();
            accounts.push(account);
        }
        let node = Node::create(
            &db,
            &CreateNodeRequest {
                owner_user_id: owner.id,
                client_instance_id: format!("native-{run}"),
                display_name: "Native fixture".into(),
                capabilities_json: json!({"runtime":"ollama","models":[{"model":model}]}),
            },
        )
        .await
        .unwrap();
        let profiles: Vec<_> = [Op::Chat, Op::Messages, Op::Responses]
            .into_iter()
            .map(|op| {
                NativeModelProfile::for_operation(model.clone(), op).with_features(vec![
                    NativeFeature::Tools,
                    NativeFeature::Vision,
                    NativeFeature::Thinking,
                    NativeFeature::StructuredOutput,
                ])
            })
            .collect();
        let session = NodeSession::create(
            &db,
            &CreateNodeSessionRequest {
                node_id: node.id,
                session_token_hash: format!("native-fixture-{}", Uuid::new_v4()),
                expires_at: Utc::now() + ChronoDuration::hours(1),
                accepted_models_json: json!([model]),
                native_operations_json: json!(["chat", "messages", "responses"]),
                native_profiles_json: json!(profiles),
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
                poll_timeout_secs: Some(1),
                task_deadline_secs: Some(10),
                complete_grace_secs: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let state = AppState::try_with_pool_and_config(DbRouter::single(db.clone()), config)
            .await
            .unwrap();
        let key = ProduceAiKeyValidator::generate_key();
        ProduceAiKey::create(
            &db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: user.id,
                name: "native-test".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "test***".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        let app = create_router(state.clone());
        Self {
            db,
            cleanup,
            state,
            app,
            user,
            key,
            model,
            node,
            session,
            accounts,
            upstream,
            server,
        }
    }
    fn body(&self, op: Op) -> Value {
        match op {
            Op::Chat => {
                json!({"model":self.model,"messages":[{"role":"user","content":"Hello"}],"max_tokens":32,"temperature":0.4,"top_p":0.9,"stop":["STOP"],"stream":false,"tools":[{"type":"function","function":{"name":"weather","parameters":{"type":"object"}}}],"vendor_extension":{"keep":[null,2]}})
            }
            Op::Messages => {
                json!({"model":self.model,"max_tokens":32,"system":[{"type":"text","text":"Keep semantics"}],"messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"temperature":0.4,"top_p":0.9,"top_k":20,"stop_sequences":["STOP"],"stream":false,"tools":[{"name":"weather","description":"Weather","input_schema":{"type":"object","properties":{"city":{"type":"string"}}}}],"vendor_extension":{"keep":[null,2]}})
            }
            Op::Responses => {
                json!({"model":self.model,"input":[{"role":"user","content":[{"type":"input_text","text":"Hello"}]}],"instructions":"Keep semantics","max_output_tokens":32,"temperature":0.4,"top_p":0.9,"stream":false,"store":false,"tools":[{"type":"function","name":"weather","description":"Weather","parameters":{"type":"object"}}],"vendor_extension":{"keep":[null,2]}})
            }
        }
    }
    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> HttpResult {
        http(self.app.clone(), method, path, Some(&self.key), body).await
    }
    fn worker(&self, status: u16) -> JoinHandle<NodeTaskEnvelope> {
        let service = self.state.node_gateway.as_ref().unwrap().clone();
        let node = self.node.id;
        let session = self.session.id;
        let model = self.model.clone();
        tokio::spawn(async move {
            let task = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(task) = service
                        .poll_task(node, session, vec![model.clone()])
                        .await
                        .unwrap()
                        .task
                    {
                        break task;
                    }
                }
            })
            .await
            .unwrap();
            let native = task.payload.native.as_ref().expect("native task required");
            let body = if status == 200 {
                sample_response(native.operation, &model)
            } else {
                json!({"type":"error","error":{"type":"invalid_request_error","code":"isolated_rejection","message":"Fixture rejected the request","detail":[null,1]}})
            };
            service
                .complete_task(
                    task.task_id,
                    task.lease_id,
                    node,
                    session,
                    NodeTaskResult::NativeSucceeded {
                        response: NodeNativeHttpResult {
                            status,
                            headers: vec![("x-request-id".into(), "node-fixture".into())],
                            body,
                        },
                    },
                )
                .await
                .unwrap();
            task
        })
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
    async fn finish(&mut self) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM node_tasks WHERE user_id=$1",
                [self.user.id.into()],
            ))
            .await
            .unwrap();
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM nodes WHERE id=$1",
                [self.node.id.into()],
            ))
            .await
            .unwrap();
        self.cleanup.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn passthrough_messages_and_responses_preserve_native_body_and_result() {
    let mut f = Fixture::new().await;
    for op in [Op::Messages, Op::Responses] {
        let body = f.body(op);
        let path = format!("/pt{}", op.local_path());
        let response = f.request(Method::POST, &path, Some(body.clone())).await;
        assert!(response.headers.get("set-cookie").is_none());
        assert_eq!(
            expect(response, StatusCode::OK),
            sample_response(op, &f.model)
        );
        let calls = f.upstream.calls.lock().unwrap();
        let call = calls.last().unwrap();
        assert_eq!(call.body, body);
        assert!(call.path.ends_with(op.local_path()));
        if op == Op::Messages {
            assert_eq!(call.headers.get("anthropic-version").unwrap(), "2023-06-01");
        }
        assert!(
            !call
                .headers
                .values()
                .any(|v| v.to_str().is_ok_and(|value| value.contains(&f.key)))
        );
    }
    assert_eq!(f.tasks().await, 0);
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 2);
    f.finish().await;
}
#[tokio::test]
async fn node_messages_and_responses_keep_operation_headers_and_account_isolation() {
    let mut f = Fixture::new().await;
    for op in [Op::Messages, Op::Responses] {
        let body = f.body(op);
        let worker = f.worker(200);
        let path = format!("/nt{}", op.local_path());
        let response = expect(
            f.request(Method::POST, &path, Some(body.clone())).await,
            StatusCode::OK,
        );
        let task = worker.await.unwrap();
        let native = task.payload.native.unwrap();
        assert_eq!(native.operation, op);
        assert_eq!(native.body, body);
        assert_eq!(response, sample_response(op, &f.model));
        assert!(task.payload.chat.is_none());
        assert!(
            !native
                .headers
                .iter()
                .any(|(name, _)| matches!(name.as_str(), "authorization" | "cookie" | "x-api-key"))
        );
        if op == Op::Messages {
            assert!(
                native
                    .headers
                    .iter()
                    .any(|(name, value)| name == "anthropic-version" && value == "2023-06-01")
            );
        }
    }
    assert_eq!(f.tasks().await, 2);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn protocol_http_errors_return_once_without_switching_family() {
    let mut f = Fixture::new().await;
    for family in ["pt", "nt"] {
        for op in [Op::Messages, Op::Responses] {
            for status in [400, 429, 500] {
                f.upstream.status.store(status, Ordering::Relaxed);
                for account in &f.accounts {
                    f.state.account_states.clear_cooldown(account.id);
                }
                // Isolate HTTP forwarding from the intentionally sticky model-health gate.
                for account in &f.accounts {
                    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE account_model_health SET status='healthy',reason_code=NULL,generation=generation+1 WHERE account_id=$1",
                [account.id.into()])).await.unwrap();
                }
                let before = f.upstream.calls.lock().unwrap().len();
                let worker = (family == "nt").then(|| f.worker(status));
                let response = expect(
                    f.request(
                        Method::POST,
                        &format!("/{family}{}", op.local_path()),
                        Some(f.body(op)),
                    )
                    .await,
                    StatusCode::from_u16(status).unwrap(),
                );
                if family == "pt" {
                    // Existing upstream error sanitization deliberately retains
                    // status and protocol fields, not arbitrary vendor metadata.
                    if op == Op::Messages {
                        assert_eq!(response["type"], "error");
                    }
                    assert!(response["error"]["type"].is_string());
                    assert!(response["error"]["message"].is_string());
                    assert!(response["error"].get("detail").is_none());
                } else {
                    assert_eq!(
                        response["error"]["code"], "isolated_rejection",
                        "{family} {op:?} {status}: {response}"
                    );
                }
                if let Some(worker) = worker {
                    worker.await.unwrap();
                }
                assert_eq!(
                    f.upstream.calls.lock().unwrap().len() - before,
                    usize::from(family == "pt")
                );
            }
        }
    }
    f.finish().await;
}
#[tokio::test]
async fn scoped_model_discovery_reports_each_supported_protocol() {
    let mut f = Fixture::new().await;
    for family in ["pt", "nt"] {
        for op in [Op::Chat, Op::Messages, Op::Responses] {
            let path = format!(
                "/{family}/v1/models?protocol={}&capability={}",
                op.protocol(),
                op.api_capability()
            );
            let response = expect(f.request(Method::GET, &path, None).await, StatusCode::OK);
            assert!(
                response["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["id"] == f.model),
                "{path}: {response}"
            );
        }
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn stateful_or_streaming_requests_are_not_silently_downgraded() {
    let mut f = Fixture::new().await;
    for family in ["pt", "nt"] {
        for op in [Op::Messages, Op::Responses] {
            let mut body = f.body(op);
            body["stream"] = true.into();
            expect(
                f.request(
                    Method::POST,
                    &format!("/{family}{}", op.local_path()),
                    Some(body),
                )
                .await,
                StatusCode::BAD_REQUEST,
            );
        }
        for (name, value) in [
            ("previous_response_id", json!("resp-do-not-drop")),
            ("conversation", json!("conv-do-not-drop")),
            ("background", json!(true)),
            ("store", json!(true)),
        ] {
            let mut body = f.body(Op::Responses);
            body[name] = value;
            expect(
                f.request(Method::POST, &format!("/{family}/v1/responses"), Some(body))
                    .await,
                StatusCode::BAD_REQUEST,
            );
        }
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}
#[tokio::test]
async fn incompatible_worker_profile_never_receives_a_different_operation() {
    let mut f = Fixture::new().await;
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE node_sessions SET native_operations_json='[\"chat\"]'::JSONB,native_profiles_json=$2 WHERE id=$1",
        [f.session.id.into(),json!([NativeModelProfile::plain_chat(f.model.clone())]).into()])).await.unwrap();
    for op in [Op::Messages, Op::Responses] {
        expect(
            f.request(
                Method::POST,
                &format!("/nt{}", op.local_path()),
                Some(f.body(op)),
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
    }
    assert_eq!(f.tasks().await, 0);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn user_tool_schemas_are_not_mistaken_for_protocol_control_fields() {
    let mut f = Fixture::new().await;
    for op in [Op::Messages, Op::Responses] {
        let mut body = f.body(op);
        let properties = json!({"file_id":{"type":"string"},"cache_control":{"type":"string"},"tool_choice":{"type":"object"},"thinking":{"type":"object","properties":{"budget_tokens":{"type":"integer"}}}});
        if op == Op::Messages {
            body["tools"][0]["input_schema"]["properties"] = properties;
        } else {
            body["tools"][0]["parameters"]["properties"] = properties;
        }
        body["vendor_extension"]["file_url"] = "this-is-user-data-not-a-download".into();
        let worker = f.worker(200);
        expect(
            f.request(
                Method::POST,
                &format!("/nt{}", op.local_path()),
                Some(body.clone()),
            )
            .await,
            StatusCode::OK,
        );
        let task = worker.await.unwrap();
        assert_eq!(task.payload.native.unwrap().body, body);
    }
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}
