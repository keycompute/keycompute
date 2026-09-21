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
    TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user,
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::models::{
    node::{CreateNodeRequest, Node},
    node_session::{CreateNodeSessionRequest, NodeSession},
    passthrough_binding::{CreatePassthroughBindingRequest, PassthroughBinding},
};
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, DbRouter, ProduceAiKey, UserBalance,
};
use keycompute_server::{
    AppState, create_router,
    state::{AppStateConfig, RateLimitBackendConfig},
};
use keycompute_types::{
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
    stream_release: tokio::sync::Notify,
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
    if status == 200 && body["stream"] == true {
        return streaming_upstream(s, op, body).await;
    }
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
    let bytes = tokio::time::timeout(
        Duration::from_secs(20),
        to_bytes(response.into_body(), 2 << 20),
    )
    .await
    .expect("response body timeout")
    .unwrap();
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
    user: TenantActor,
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
        Self::with_sse(false).await
    }
    async fn with_sse(sse: bool) -> Self {
        let db = create_test_pool().await;
        let run = Uuid::new_v4().to_string();
        let cleanup = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "native-multi", &run).await;
        let user = create_test_user(&db, tenant.id, "native", &run).await;
        UserBalance::recharge(
            &db,
            tenant.id,
            user.id,
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
                tenant_id: owner.tenant_id,
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
                let mut features = vec![
                    NativeFeature::Tools,
                    NativeFeature::Vision,
                    NativeFeature::Thinking,
                    NativeFeature::StructuredOutput,
                ];
                if sse {
                    features.push(NativeFeature::Sse);
                }
                NativeModelProfile::for_operation(model.clone(), op).with_features(features)
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
    async fn wait_for_settlement(&self) {
        // HTTP error delivery/EOF is deliberately earlier than detached
        // financial finalization. Never delete fixture rows while a worker
        // can still append a balance transaction referencing its ledger.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let requests = self.state.generation_admission.requests.status().active;
                let accounts = self.state.generation_admission.accounts.status().active;
                let row = self.db.query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "SELECT (SELECT COUNT(*) FROM balance_reservations WHERE user_id=$1 AND status='active') + (SELECT COUNT(*) FROM response_affinities WHERE tenant_id=$2 AND (settlement IS NOT NULL OR is_reservation)) AS pending",
                    [self.user.id.into(), self.user.tenant_id.into()],
                )).await.unwrap().unwrap();
                let pending: i64 = row.try_get("", "pending").unwrap();
                if requests == 0 && accounts == 0 && pending == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("fixture cleanup must wait for completed settlement and released admission");
    }
    async fn finish(&mut self) {
        self.wait_for_settlement().await;
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
async fn invalid_state_controls_and_missing_references_are_not_silently_downgraded() {
    let mut f = Fixture::new().await;
    for family in ["pt", "nt"] {
        for op in [Op::Messages, Op::Responses] {
            let mut body = f.body(op);
            body["stream"] = "invalid-stream-flag".into();
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
        for (name, value, status) in [
            (
                "previous_response_id",
                json!("resp-do-not-drop"),
                StatusCode::NOT_FOUND,
            ),
            (
                "conversation",
                json!("conv-do-not-drop"),
                StatusCode::NOT_FOUND,
            ),
            (
                "background",
                json!("not-a-boolean"),
                StatusCode::BAD_REQUEST,
            ),
            ("store", json!([true]), StatusCode::BAD_REQUEST),
        ] {
            let mut body = f.body(Op::Responses);
            body[name] = value;
            expect(
                f.request(Method::POST, &format!("/{family}/v1/responses"), Some(body))
                    .await,
                status,
            );
        }
    }
    // The fixture's immutable worker profiles do not advertise SSE. New
    // native streaming requests must fail readiness instead of being sent to
    // these non-streaming executors; PT live-stream success is tested below.
    for op in [Op::Messages, Op::Responses] {
        let mut body = f.body(op);
        body["stream"] = true.into();
        expect(
            f.request(Method::POST, &format!("/nt{}", op.local_path()), Some(body))
                .await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
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

async fn streaming_upstream(s: Arc<Upstream>, op: Op, body: Value) -> Response {
    let (first, last) = stream_fixture(op, body["model"].as_str().unwrap());
    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::result::Result<bytes::Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        if tx.send(Ok(bytes::Bytes::from(first))).await.is_err() {
            return;
        }
        tokio::select! {_ = s.stream_release.notified()=>{},_ = tx.closed()=>return}
        let _ = tx.send(Ok(bytes::Bytes::from(last))).await;
    });
    (
        [
            ("content-type", "text/event-stream"),
            ("set-cookie", "do-not-forward"),
        ],
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response()
}

fn stream_fixture(op: Op, model: &str) -> (String, String) {
    let frame = |event: &str, data: Value| format!("event: {event}\r\ndata: {data}\r\n\r\n");
    match op {
        Op::Messages => (
            frame(
                "message_start",
                json!({"type":"message_start","message":{"id":"msg-stream","type":"message","role":"assistant","model":model,"content":[],"usage":{"input_tokens":7,"output_tokens":0}}}),
            ) + &frame(
                "content_block_start",
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            ) + &frame(
                "content_block_delta",
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"终端 preserved native text"},"vendor_extension":{"untouched":[null,1]}}),
            ),
            frame(
                "content_block_stop",
                json!({"type":"content_block_stop","index":0}),
            ) + &frame(
                "message_delta",
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}),
            ) + &frame("message_stop", json!({"type":"message_stop"})),
        ),
        Op::Responses => {
            let mut head = sample_response(op, model);
            head["id"] = "resp-stream".into();
            head["status"] = "in_progress".into();
            head["usage"] = Value::Null;
            head["output"] = json!([]);
            let mut completed = sample_response(op, model);
            completed["id"] = "resp-stream".into();
            (
                frame(
                    "response.created",
                    json!({"type":"response.created","sequence_number":0,"response":head}),
                ) + &frame(
                    "response.output_text.delta",
                    json!({"type":"response.output_text.delta","sequence_number":1,"output_index":0,"content_index":0,"item_id":"msg-stream","delta":"终端 preserved native text","vendor_extension":{"untouched":[null,1]}}),
                ),
                frame(
                    "response.completed",
                    json!({"type":"response.completed","sequence_number":2,"response":completed}),
                ),
            )
        }
        Op::Chat => unreachable!("PT Chat uses its existing tests"),
    }
}

async fn scoped_stream_request(f: &Fixture, op: Op) -> Response {
    let mut body = f.body(op);
    body["stream"] = true.into();
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/pt{}", op.local_path()))
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(body.to_string()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), f.app.clone().oneshot(request))
        .await
        .expect("HTTP head must not wait for terminal event")
        .unwrap()
}

#[tokio::test]
async fn passthrough_streams_deliver_before_completion_and_preserve_native_events() {
    use http_body_util::BodyExt;
    let mut f = Fixture::new().await;
    for op in [Op::Messages, Op::Responses] {
        let response = scoped_stream_request(&f, op).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        assert!(!response.headers().contains_key("set-cookie"));
        let mut body = response.into_body();
        let first = tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert!(!first.is_empty());
        f.upstream.stream_release.notify_one();
        let tail = tokio::time::timeout(Duration::from_secs(10), body.collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        let mut data = first.to_vec();
        data.extend_from_slice(&tail);
        let text = String::from_utf8(data).unwrap();
        assert!(text.contains("终端") && text.contains("vendor_extension"));
        assert!(text.contains(if op == Op::Messages {
            "message_stop"
        } else {
            "response.completed"
        }));
    }
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 2);
    assert_eq!(f.tasks().await, 0);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_stream_errors_preserve_status() {
    let mut f = Fixture::new().await;
    for op in [Op::Messages, Op::Responses] {
        f.upstream.status.store(429, Ordering::Relaxed);
        let response = scoped_stream_request(&f, op).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let bytes = to_bytes(response.into_body(), 2 << 20).await.unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["code"], "isolated_rejection");
    }
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_disconnect_settlement() {
    use http_body_util::BodyExt;
    let mut f = Fixture::new().await;
    let response = scoped_stream_request(&f, Op::Messages).await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let mut received = String::new();
    while !received.contains("终端") {
        let frame = tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Ok(bytes) = frame.into_data() {
            received.push_str(std::str::from_utf8(&bytes).unwrap());
        }
    }
    drop(body);
    let rows = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let rows = keycompute_db::models::usage_log::UserUsageScope::new(f.user.scope())
                .list(&f.db, None, None, 2, 0)
                .await
                .unwrap();
            if !rows.is_empty() {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("detached settlement timeout");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, 7);
    assert!(rows[0].output_tokens > 0);
    assert_ne!(rows[0].usage_source, "provider");
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn node_stream_generic_failure_after_head_closes_before_deadline() {
    use http_body_util::BodyExt;
    use keycompute_types::node::{NodeNativeStreamEvent, NodeTaskStreamEventRequest};
    let mut f = Fixture::with_sse(true).await;
    let gateway = f.state.node_gateway.as_ref().unwrap().clone();
    let node = f.node.id;
    let session = f.session.id;
    let model = f.model.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let worker_release = release.clone();
    let worker = tokio::spawn(async move {
        let task = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(task) = gateway
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
        let first = format!(
            "data: {}\n\n",
            json!({"id":"failure-stream","object":"chat.completion.chunk","model":model,"choices":[{"index":0,"delta":{"content":"observed partial output"},"finish_reason":null}]})
        );
        for (seq, event) in [
            NodeNativeStreamEvent::Start {
                status: 200,
                headers: vec![("content-type".into(), "text/event-stream".into())],
                body: None,
            },
            NodeNativeStreamEvent::Data { frame: first },
        ]
        .into_iter()
        .enumerate()
        {
            gateway
                .store
                .accept_native_stream_event(NodeTaskStreamEventRequest {
                    protocol_version: "node.v1".into(),
                    node_id: node,
                    session_id: session,
                    task_id: task.task_id,
                    lease_id: task.lease_id,
                    seq: seq as u64,
                    event,
                })
                .await
                .unwrap();
        }
        worker_release.notified().await;
        gateway
            .complete_task(
                task.task_id,
                task.lease_id,
                node,
                session,
                NodeTaskResult::Failed {
                    code: "event_delivery_rejected".into(),
                    message: "fixture terminal rejection".into(),
                    is_client_error: false,
                },
            )
            .await
            .unwrap();
        task
    });
    let mut body = f.body(Op::Chat);
    body["stream"] = true.into();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/nt/v1/chat/completions")
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), f.app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body();
    let first = tokio::time::timeout(Duration::from_secs(3), stream.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(first.data_ref().unwrap()).contains("observed partial output"));
    release.notify_one();
    let drained = tokio::time::timeout(Duration::from_secs(5), stream.collect())
        .await
        .expect("generic task failure must not wait for original 10-second task deadline");
    assert!(
        drained.is_err(),
        "failed native stream must not end as successful EOF"
    );
    let task = worker.await.unwrap();
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT state FROM node_native_streams WHERE task_id=$1",
            [task.task_id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "state").unwrap(), "failed");
    let summary = f
        .state
        .node_gateway
        .as_ref()
        .unwrap()
        .store
        .native_stream_summary(task.task_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        summary.usage.unwrap().output_tokens > 0,
        "observed partial output must retain accounting"
    );
    assert_eq!(f.tasks().await, 1);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.wait_for_settlement().await;
    let ledger = keycompute_db::models::usage_log::UserUsageScope::new(f.user.scope())
        .list(&f.db, None, None, 2, 0)
        .await
        .unwrap();
    assert_eq!(
        ledger.len(),
        1,
        "observed failed streams must settle exactly once"
    );
    assert!(ledger[0].output_tokens > 0);
    assert_eq!(ledger[0].status, "error");
    f.finish().await;
}

#[tokio::test]
async fn unclaimed_native_stream_returns_gateway_timeout_before_any_success_head() {
    let mut f = Fixture::with_sse(true).await;
    let mut payload = f.body(Op::Chat);
    payload["stream"] = true.into();
    let error = expect(
        f.request(Method::POST, "/nt/v1/chat/completions", Some(payload))
            .await,
        StatusCode::GATEWAY_TIMEOUT,
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    assert_eq!(f.tasks().await, 1);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT (SELECT COUNT(*) FROM balance_reservations WHERE user_id=$1 AND status='active')::BIGINT AS reserved,(SELECT COUNT(*) FROM usage_logs WHERE user_id=$1)::BIGINT AS billed",
        [f.user.id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "reserved").unwrap(), 0);
    assert_eq!(row.try_get::<i64>("", "billed").unwrap(), 0);
    f.finish().await;
}
