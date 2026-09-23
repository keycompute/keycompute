//! Scoped Responses resource semantics through the actual router and isolated database.
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
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, DbRouter, UserBalance,
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

async fn scoped_jwt(state: &AppState, user: &TenantActor) -> String {
    let global = state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(user.id, None, user.token_version, None, None, 3600)
        .unwrap();
    let context = state.auth.verify_token(&global).await.unwrap();
    state
        .auth
        .select_tenant(&context, Some(user.tenant_id))
        .await
        .unwrap()
        .access_token
}
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
    jwt_replay_probe: tokio::sync::Notify,
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
            json!({"id":"resp-native","object":"response","created_at":1,"model":model,"status":"completed","error":null,"metadata":null,"output":[{"id":"fc-new","type":"function_call","call_id":"call-new","name":"weather","arguments":"{\"city\":\"x\"}","status":"completed"}],"usage":{"input_tokens":7,"output_tokens":3,"total_tokens":10},"vendor":{"untouched":[null,1]}})
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
    if body.get("hold_test").and_then(Value::as_bool) == Some(true) {
        s.stream_release.notified().await;
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
    run: String,
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
        Self::with_pool(create_test_pool().await, sse).await
    }
    async fn with_pool(db: DatabaseConnection, sse: bool) -> Self {
        let run = Uuid::new_v4().to_string();
        let cleanup = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "resource-state", &run).await;
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
        let model = format!("resource-state:{run}");
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
                    NativeFeature::Cancellation,
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
                scope: keycompute_db::NodeSessionScope::for_node(&node),
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
        integration_tests::db::create_test_api_key(
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
            run,
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
        self.cleanup.cleanup().await.unwrap();
    }
}

async fn streaming_upstream(s: Arc<Upstream>, op: Op, body: Value) -> Response {
    let (first, mut last) = stream_fixture(op, body["model"].as_str().unwrap());
    if op == Op::Responses && body["late_delta_after_revoke"] == true {
        // Force a nonterminal event after revocation, rather than relying on
        // the scheduler to race the initial response.created/delta batch.
        last = format!(
            "event: response.output_text.delta\r\ndata: {}\r\n\r\n",
            json!({"type":"response.output_text.delta","sequence_number":2,
                "output_index":0,"content_index":0,"item_id":"msg-stream",
                "delta":" accepted late event"})
        ) + &last.replace("\"sequence_number\":2", "\"sequence_number\":3");
    }
    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::result::Result<bytes::Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        if tx.send(Ok(bytes::Bytes::from(first))).await.is_err() {
            return;
        }
        if body["jwt_replay_probe"] == true {
            tokio::select! {_ = s.jwt_replay_probe.notified()=>{},_ = tx.closed()=>return}
            let probe = format!(
                "event: response.output_text.delta\r\ndata: {}\r\n\r\n",
                json!({"type":"response.output_text.delta","sequence_number":2,
                    "output_index":0,"content_index":0,"item_id":"msg-stream",
                    "delta":" live-jwt-boundary-probe"})
            );
            if tx.send(Ok(bytes::Bytes::from(probe))).await.is_err() {
                return;
            }
            last = last.replace("\"sequence_number\":2", "\"sequence_number\":3");
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

async fn call_with_key(f: &Fixture, path: &str, body: Value, idem: &str) -> HttpResult {
    let request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .header("idempotency-key", idem)
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(12), f.app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 2 << 20).await.unwrap();
    HttpResult {
        status,
        headers,
        body: serde_json::from_slice(&bytes).unwrap(),
    }
}
async fn wait_state(f: &Fixture, path: &str, status: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let result = f.request(Method::GET, path, None).await;
            assert_eq!(result.status, StatusCode::OK, "{}", result.body);
            if result.body["status"] == status {
                break result.body;
            }
            assert!(
                !matches!(result.body["status"].as_str(), Some("failed" | "cancelled")),
                "{}",
                result.body
            );
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    })
    .await
    .expect("managed response did not reach expected status")
}
async fn wait_calls(f: &Fixture, count: usize) {
    tokio::time::timeout(Duration::from_secs(8), async {
        while f.upstream.calls.lock().unwrap().len() < count {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("inference dispatch timeout");
}

#[tokio::test]
async fn stored_previous_response_replays_items_once_with_current_instructions() {
    let mut f = Fixture::new().await;
    let mut first = f.body(Op::Responses);
    first.as_object_mut().unwrap().remove("store");
    first["instructions"] = "first instructions".into();
    let one = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(first.clone()))
            .await,
        StatusCode::OK,
    );
    let id = one["id"].as_str().unwrap();
    assert_ne!(id, "resp-native");
    assert!(id.starts_with("resp_"));
    assert_eq!(one["vendor"], json!({"untouched":[null,1]}));
    assert_eq!(one["status"], "completed");
    let saved = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(saved, one);
    let mut second = f.body(Op::Responses);
    second["store"] = true.into();
    second["previous_response_id"] = id.into();
    second["instructions"] = "second instructions".into();
    second["input"] =
        json!([{"type":"function_call_output","call_id":"call-new","output":"tool result"}]);
    let two = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(second.clone()))
            .await,
        StatusCode::OK,
    );
    assert_ne!(two["id"], one["id"]);
    let calls = f.upstream.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].body["instructions"], "second instructions");
    let expected: Vec<Value> = first["input"]
        .as_array()
        .unwrap()
        .iter()
        .chain(one["output"].as_array().unwrap())
        .chain(second["input"].as_array().unwrap())
        .cloned()
        .collect();
    assert_eq!(calls[1].body["input"], json!(expected));
    assert!(calls[1].body.get("previous_response_id").is_none());
    assert_eq!(calls[1].body["store"], false);
    assert_eq!(calls[1].body["tools"], second["tools"]);
    assert_eq!(
        calls[1].body["vendor_extension"],
        second["vendor_extension"]
    );
    let items = expect(
        f.request(
            Method::GET,
            &format!(
                "/pt/v1/responses/{}/input_items?order=asc",
                two["id"].as_str().unwrap()
            ),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(items["data"].as_array().unwrap().len(), expected.len());
    assert_eq!(f.tasks().await, 0);
    f.finish().await;
}

#[tokio::test]
async fn resource_scope_is_user_tenant_family_and_current_passthrough_grant() {
    let mut f = Fixture::new().await;
    let mut payload = f.body(Op::Responses);
    payload["store"] = true.into();
    let one = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(payload))
            .await,
        StatusCode::OK,
    );
    let id = one["id"].as_str().unwrap();
    let path = format!("/pt/v1/responses/{id}");
    let user = create_test_user(
        &f.db,
        f.user.tenant_id,
        "other-resource-user",
        &Uuid::new_v4().to_string(),
    )
    .await;
    let token = scoped_jwt(&f.state, &user).await;
    expect(
        http(f.app.clone(), Method::GET, &path, Some(&token), None).await,
        StatusCode::NOT_FOUND,
    );
    // Raising the peer's platform role cannot grant access to private content.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
        [user.tenant_id.into(), user.id.into()],
    ))
    .await
    .unwrap();
    let admin = user.clone();
    let token = scoped_jwt(&f.state, &admin).await;
    for (method, target) in [
        (Method::GET, path.clone()),
        (Method::DELETE, path.clone()),
        (Method::POST, format!("{path}/cancel")),
        (Method::GET, format!("{path}/input_items")),
    ] {
        expect(
            http(f.app.clone(), method, &target, Some(&token), None).await,
            StatusCode::NOT_FOUND,
        );
    }
    let mut continuation = f.body(Op::Responses);
    continuation["previous_response_id"] = json!(id);
    expect(
        http(
            f.app.clone(),
            Method::POST,
            "/pt/v1/responses",
            Some(&token),
            Some(continuation),
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    // Rotating only the owning user's inference key keeps the stable owner.
    let rotated = ProduceAiKeyValidator::generate_key();
    integration_tests::db::create_test_api_key(
        &f.db,
        &CreateProduceAiKeyRequest {
            tenant_id: f.user.tenant_id,
            user_id: f.user.id,
            name: "rotated owner".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&rotated),
            produce_ai_key_preview: "sk-test****".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    expect(
        http(f.app.clone(), Method::GET, &path, Some(&rotated), None).await,
        StatusCode::OK,
    );
    expect(
        f.request(Method::GET, &format!("/nt/v1/responses/{id}"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    expect(
        f.request(Method::GET, &format!("/v1/responses/{id}"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM passthrough_bindings WHERE account_id=$1",
        [f.accounts[0].id.into()],
    ))
    .await
    .unwrap();
    expect(
        f.request(Method::GET, &path, None).await,
        StatusCode::NOT_FOUND,
    );
    expect(
        f.request(Method::DELETE, &path, None).await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[path = "support/tenant_response_control.rs"]
mod tenant_control;

#[tokio::test]
async fn explicit_stateless_response_does_not_create_a_retrievable_resource() {
    let mut f = Fixture::new().await;
    let body = f.body(Op::Responses);
    let response = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body.clone()))
            .await,
        StatusCode::OK,
    );
    assert_eq!(response["id"], "resp-native");
    assert_eq!(f.upstream.calls.lock().unwrap()[0].body, body);
    expect(
        f.request(Method::GET, "/pt/v1/responses/resp-native", None)
            .await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn conversation_items_are_scoped_paginated_and_appended_once() {
    let mut f = Fixture::new().await;
    let initial = json!({"type":"message","role":"user","content":"Conversation seed"});
    let conv = expect(
        f.request(
            Method::POST,
            "/pt/v1/conversations",
            Some(json!({"metadata":{"purpose":"test"},"items":[initial]})),
        )
        .await,
        StatusCode::OK,
    );
    let id = conv["id"].as_str().unwrap();
    let path = format!("/pt/v1/conversations/{id}");
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    body["conversation"] = id.into();
    let response = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body.clone()))
            .await,
        StatusCode::OK,
    );
    let call = f.upstream.calls.lock().unwrap()[0].body.clone();
    assert_eq!(call["input"][0], initial);
    assert_eq!(call["input"].as_array().unwrap().len(), 2);
    assert!(call.get("conversation").is_none());
    let items = expect(
        f.request(
            Method::GET,
            &format!("{path}/items?order=asc&limit=1"),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(items["data"].as_array().unwrap().len(), 1);
    assert_eq!(items["has_more"], true);
    let cursor = items["last_id"].as_str().unwrap();
    let rest = expect(
        f.request(
            Method::GET,
            &format!("{path}/items?order=asc&after={cursor}"),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(
        rest["data"].as_array().unwrap().len(),
        body["input"].as_array().unwrap().len() + response["output"].as_array().unwrap().len()
    );
    assert_eq!(rest["has_more"], false);
    let item = rest["data"][0]["id"].as_str().unwrap();
    expect(
        f.request(Method::GET, &format!("{path}/items/{item}"), None)
            .await,
        StatusCode::OK,
    );
    expect(
        f.request(
            Method::POST,
            &path,
            Some(json!({"metadata":{"purpose":"updated"}})),
        )
        .await,
        StatusCode::OK,
    );
    expect(
        f.request(Method::DELETE, &format!("{path}/items/{item}"), None)
            .await,
        StatusCode::OK,
    );
    expect(
        f.request(Method::GET, &format!("/nt/v1/conversations/{id}"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    expect(f.request(Method::DELETE, &path, None).await, StatusCode::OK);
    expect(
        f.request(Method::GET, &path, None).await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn background_returns_early_and_idempotency_never_repeats_inference() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    body["background"] = true.into();
    body["hold_test"] = true.into();
    let key = Uuid::new_v4().to_string();
    let first = expect(
        tokio::time::timeout(
            Duration::from_secs(3),
            call_with_key(&f, "/pt/v1/responses", body.clone(), &key),
        )
        .await
        .expect("background must return before held model"),
        StatusCode::OK,
    );
    let id = first["id"].as_str().unwrap();
    assert!(matches!(
        first["status"].as_str(),
        Some("queued" | "in_progress")
    ));
    wait_calls(&f, 1).await;
    let replay = expect(
        call_with_key(&f, "/pt/v1/responses", body.clone(), &key).await,
        StatusCode::OK,
    );
    assert_eq!(replay["id"], first["id"]);
    let mut different = body.clone();
    different["instructions"] = "different".into();
    expect(
        call_with_key(&f, "/pt/v1/responses", different, &key).await,
        StatusCode::CONFLICT,
    );
    f.upstream.stream_release.notify_one();
    let completed = wait_state(&f, &format!("/pt/v1/responses/{id}"), "completed").await;
    assert_eq!(completed["id"], first["id"]);
    assert_eq!(completed["usage"]["total_tokens"], 10);
    let calls = f.upstream.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_ne!(calls[0].body["background"], true);
    assert_eq!(calls[0].body["store"], false);
    let rows = keycompute_db::models::usage_log::UserUsageScope::new(f.user.scope())
        .list(&f.db, None, None, 10, 0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn cancelling_background_keeps_terminal_state_after_a_late_result() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    body["background"] = true.into();
    body["hold_test"] = true.into();
    let initial = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    let id = initial["id"].as_str().unwrap();
    let path = format!("/pt/v1/responses/{id}");
    wait_calls(&f, 1).await;
    let cancelled = expect(
        f.request(Method::POST, &format!("{path}/cancel"), Some(json!({})))
            .await,
        StatusCode::OK,
    );
    assert_eq!(cancelled["status"], "cancelled");
    let again = expect(
        f.request(Method::POST, &format!("{path}/cancel"), Some(json!({})))
            .await,
        StatusCode::OK,
    );
    assert_eq!(again["status"], "cancelled");
    f.upstream.stream_release.notify_one();
    tokio::time::timeout(Duration::from_secs(10),async {
        loop {
            let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT COUNT(*)::BIGINT n FROM balance_reservations WHERE user_id=$1 AND status='active'",[f.user.id.into()])).await.unwrap().unwrap();
            if row.try_get::<i64>("","n").unwrap()==0 {break;}
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }).await.expect("cancelled generation leaked its reservation");
    let saved = expect(f.request(Method::GET, &path, None).await, StatusCode::OK);
    assert_eq!(saved["status"], "cancelled");
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn native_node_stored_response_is_platform_owned_without_worker_state() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    let worker = f.worker(200);
    let result = expect(
        f.request(Method::POST, "/nt/v1/responses", Some(body.clone()))
            .await,
        StatusCode::OK,
    );
    let task = worker.await.unwrap();
    let native = task.payload.native.unwrap();
    assert_eq!(native.body["store"], false);
    assert_eq!(native.body["tools"], body["tools"]);
    let id = result["id"].as_str().unwrap();
    assert_ne!(id, "resp-native");
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE nodes SET status='offline' WHERE id=$1",
        [f.node.id.into()],
    ))
    .await
    .unwrap();
    let stored = expect(
        f.request(Method::GET, &format!("/nt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(stored, result);
    expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.tasks().await, 1);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn invalid_state_controls_fail_before_inference_or_storage() {
    let mut f = Fixture::new().await;
    for controls in [
        json!({"store":"true"}),
        json!({"background":1}),
        json!({"previous_response_id":"missing","conversation":"missing"}),
        json!({"conversation":{"wrong":"x"}}),
    ] {
        let mut body = f.body(Op::Responses);
        body.as_object_mut()
            .unwrap()
            .extend(controls.as_object().unwrap().clone());
        expect(
            f.request(Method::POST, "/pt/v1/responses", Some(body))
                .await,
            StatusCode::BAD_REQUEST,
        );
    }
    let too_many: Vec<Value> = (0..21)
        .map(|_| json!({"role":"user","content":"x"}))
        .collect();
    expect(
        f.request(
            Method::POST,
            "/pt/v1/conversations",
            Some(json!({"items":too_many})),
        )
        .await,
        StatusCode::BAD_REQUEST,
    );
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    assert_eq!(f.tasks().await, 0);
    f.finish().await;
}

#[tokio::test]
async fn response_deletion_and_expiry_do_not_reexecute_the_model() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    let first = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body.clone()))
            .await,
        StatusCode::OK,
    );
    let id = first["id"].as_str().unwrap();
    let path = format!("/pt/v1/responses/{id}");
    let deleted = expect(f.request(Method::DELETE, &path, None).await, StatusCode::OK);
    assert_eq!(deleted["deleted"], true);
    expect(
        f.request(Method::GET, &path, None).await,
        StatusCode::NOT_FOUND,
    );
    expect(
        f.request(Method::GET, &format!("{path}/input_items"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    let second = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    let expired = second["id"].as_str().unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE scoped_responses SET expires_at=NOW()-INTERVAL '1 second' WHERE id=$1",
        [expired.into()],
    ))
    .await
    .unwrap();
    expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{expired}"), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn conversation_busy_fence_prevents_parallel_history_mutation() {
    let mut f = Fixture::new().await;
    let conv = expect(
        f.request(Method::POST, "/pt/v1/conversations", Some(json!({})))
            .await,
        StatusCode::OK,
    );
    let id = conv["id"].as_str().unwrap();
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    body["background"] = true.into();
    body["conversation"] = id.into();
    body["hold_test"] = true.into();
    let job = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body.clone()))
            .await,
        StatusCode::OK,
    );
    wait_calls(&f, 1).await;
    expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body))
            .await,
        StatusCode::CONFLICT,
    );
    expect(
        f.request(
            Method::POST,
            &format!("/pt/v1/conversations/{id}/items"),
            Some(json!({"items":[{"role":"user","content":"conflict"}]})),
        )
        .await,
        StatusCode::CONFLICT,
    );
    f.upstream.stream_release.notify_one();
    wait_state(
        &f,
        &format!("/pt/v1/responses/{}", job["id"].as_str().unwrap()),
        "completed",
    )
    .await;
    expect(
        f.request(
            Method::POST,
            &format!("/pt/v1/conversations/{id}/items"),
            Some(json!({"items":[{"role":"user","content":"after"}]})),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn managed_result_keeps_protocol_route_and_safe_delivery_metadata() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    let result = f
        .request(Method::POST, "/pt/v1/responses", Some(body))
        .await;
    assert!(result.headers.contains_key("x-request-id"));
    assert!(!result.headers.contains_key("set-cookie"));
    expect(result, StatusCode::OK);
    let calls = f.upstream.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].path, "openai/v1/responses");
    assert!(!calls[0].headers.is_empty());
    f.finish().await;
}

#[tokio::test]
async fn stored_stream_ids_match_the_retrievable_final_response() {
    use http_body_util::BodyExt;
    let mut f = Fixture::new().await;
    let mut payload = f.body(Op::Responses);
    payload["store"] = true.into();
    payload["stream"] = true.into();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pt/v1/responses")
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), f.app.clone().oneshot(request))
        .await
        .expect("stream must open before terminal")
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
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
    let mut bytes = first.to_vec();
    bytes.extend_from_slice(&tail);
    let text = String::from_utf8(bytes).unwrap();
    let data: Vec<Value> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let completed = data
        .iter()
        .find(|v| v["type"] == "response.completed")
        .unwrap();
    let id = completed["response"]["id"].as_str().unwrap();
    assert!(id.starts_with("resp_"));
    assert_ne!(id, "resp-native");
    for event in &data {
        if let Some(value) = event.pointer("/response/id") {
            assert_eq!(value, id);
        }
    }
    let stored = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(stored, completed["response"]);
    assert!(text.contains("vendor_extension") && text.contains("终端"));
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn background_stream_survives_initial_disconnect_and_resumes_without_inference() {
    use http_body_util::BodyExt;
    let mut f = Fixture::new().await;
    let mut payload = f.body(Op::Responses);
    payload["store"] = true.into();
    payload["background"] = true.into();
    payload["stream"] = true.into();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pt/v1/responses")
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), f.app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body();
    let id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = stream.frame().await.unwrap().unwrap();
            if let Some(bytes) = frame.data_ref() {
                for line in std::str::from_utf8(bytes).unwrap().lines() {
                    if let Some(data) = line.strip_prefix("data: ")
                        && let Ok(event) = serde_json::from_str::<Value>(data)
                        && let Some(id) = event.pointer("/response/id").and_then(Value::as_str)
                    {
                        return id.to_owned();
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    drop(stream);
    wait_calls(&f, 1).await;
    f.upstream.stream_release.notify_one();
    let completed = wait_state(&f, &format!("/pt/v1/responses/{id}"), "completed").await;
    assert_eq!(completed["id"], id);
    let replay = expect(
        f.request(
            Method::GET,
            &format!("/pt/v1/responses/{id}?stream=true&starting_after=0"),
            None,
        )
        .await,
        StatusCode::OK,
    );
    let events = replay
        .as_str()
        .expect("resource stream must be native SSE, not a JSON wrapper");
    assert!(events.contains("response.completed") && events.contains(&id));
    assert!(events.contains("vendor_extension"));
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn recovery_reuses_durable_result_without_a_second_inference_or_charge() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    let original = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    let id = original["id"].as_str().unwrap();
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT request_id,owner_id FROM scoped_responses WHERE id=$1",
            [id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let request_id: Uuid = row.try_get("", "request_id").unwrap();
    let owner: Uuid = row.try_get("", "owner_id").unwrap();
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE scoped_responses SET status='in_progress',response_json=NULL,output_json='[]'::jsonb,heartbeat_at=NOW()-INTERVAL '2 minutes',execution_json=execution_json||'{\"accounting_pending\":true}'::jsonb WHERE id=$1",[id.into()])).await.unwrap();
    assert!(
        keycompute_server::maintain_scoped_responses_once(&f.state)
            .await
            .unwrap()
            >= 1
    );
    let saved = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(saved, original);
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT owner_id,(SELECT COUNT(*)::BIGINT FROM usage_logs WHERE request_id=$2) AS charges FROM scoped_responses WHERE id=$1",[id.into(),request_id.into()])).await.unwrap().unwrap();
    assert_ne!(row.try_get::<Uuid>("", "owner_id").unwrap(), owner);
    assert_eq!(row.try_get::<i64>("", "charges").unwrap(), 1);
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    keycompute_server::maintain_scoped_responses_once(&f.state)
        .await
        .unwrap();
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn background_temporary_retention_expires_without_reexecution() {
    let mut f = Fixture::new().await;
    let mut body = f.body(Op::Responses);
    body["background"] = true.into();
    body["store"] = false.into();
    let queued = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    let id = queued["id"].as_str().unwrap();
    let path = format!("/pt/v1/responses/{id}");
    let complete = wait_state(&f, &path, "completed").await;
    assert_eq!(complete["store"], false);
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT EXTRACT(EPOCH FROM expires_at-NOW())::BIGINT AS ttl FROM scoped_responses WHERE id=$1",[id.into()])).await.unwrap().unwrap();
    assert!((580..=600).contains(&row.try_get::<i64>("", "ttl").unwrap()));
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE scoped_responses SET expires_at=NOW()-INTERVAL '1 second' WHERE id=$1",
        [id.into()],
    ))
    .await
    .unwrap();
    expect(
        f.request(Method::GET, &path, None).await,
        StatusCode::NOT_FOUND,
    );
    keycompute_server::maintain_scoped_responses_once(&f.state)
        .await
        .unwrap();
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT AS n FROM scoped_responses WHERE id=$1",
            [id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn managed_node_responses_require_control_capability_but_stateless_does_not() {
    let mut f = Fixture::new().await;
    let profiles = vec![
        NativeModelProfile::for_operation(f.model.clone(), Op::Responses)
            .with_features(vec![NativeFeature::Tools]),
    ];
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE node_sessions SET native_profiles_json=$2 WHERE id=$1",
        [f.session.id.into(), json!(profiles).into()],
    ))
    .await
    .unwrap();
    let managed = expect(
        f.request(
            Method::GET,
            "/nt/v1/models?capability=responses&managed=true",
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert!(
        !managed["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == f.model)
    );
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    expect(
        f.request(Method::POST, "/nt/v1/responses", Some(body))
            .await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert_eq!(f.tasks().await, 0);
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT n FROM balance_reservations WHERE user_id=$1 AND status='active'",[f.user.id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    let worker = f.worker(200);
    expect(
        f.request(
            Method::POST,
            "/nt/v1/responses",
            Some(f.body(Op::Responses)),
        )
        .await,
        StatusCode::OK,
    );
    worker.await.unwrap();
    assert_eq!(f.tasks().await, 1);
    f.finish().await;
}

#[tokio::test]
async fn pt_only_grant_still_authorizes_stored_resources_and_continuations() {
    let mut f = Fixture::new().await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE passthrough_bindings SET pool_enabled=FALSE WHERE account_id=$1",
        [f.accounts[0].id.into()],
    ))
    .await
    .unwrap();
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    body["metadata"] = json!({"purpose":"stored-pt"});
    let first = expect(
        f.request(Method::POST, "/pt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    assert_eq!(first["metadata"], json!({"purpose":"stored-pt"}));
    assert_eq!(first["instructions"], "Keep semantics");
    assert_eq!(first["tools"], f.body(Op::Responses)["tools"]);
    let id = first["id"].as_str().unwrap();
    let stored = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(stored, first);
    let mut next = f.body(Op::Responses);
    next["previous_response_id"] = id.into();
    next["store"] = true.into();
    expect(
        f.request(Method::POST, "/pt/v1/responses", Some(next))
            .await,
        StatusCode::OK,
    );
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 2);
    f.finish().await;
}

#[tokio::test]
async fn deleting_an_active_conversation_commits_cancellation_and_its_stream_event() {
    let mut f = Fixture::with_sse(true).await;
    let conversation = expect(
        f.request(Method::POST, "/pt/v1/conversations", Some(json!({})))
            .await,
        StatusCode::OK,
    );
    let conversation_id = conversation["id"].as_str().unwrap();
    let mut body = f.body(Op::Responses);
    body["store"] = true.into();
    body["stream"] = true.into();
    body["background"] = true.into();
    body["hold_test"] = true.into();
    body["conversation"] = conversation_id.into();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/pt/v1/responses")
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let initial = f.app.clone().oneshot(req).await.unwrap();
    assert_eq!(initial.status(), StatusCode::OK);
    drop(initial);
    wait_calls(&f, 1).await;
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM scoped_responses WHERE conversation_id=$1 AND user_id=$2",
            [conversation_id.into(), f.user.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let id = row.try_get::<String>("", "id").unwrap();
    expect(
        f.request(
            Method::DELETE,
            &format!("/pt/v1/conversations/{conversation_id}"),
            None,
        )
        .await,
        StatusCode::OK,
    );
    let saved = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(saved["status"], "cancelled");
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT frame FROM scoped_response_events WHERE response_id=$1 ORDER BY seq DESC LIMIT 1",[id.clone().into()])).await.unwrap().unwrap();
    assert!(
        row.try_get::<String>("", "frame")
            .unwrap()
            .contains("response_cancelled")
    );
    f.upstream.stream_release.notify_waiters();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let again = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(again["status"], "cancelled");
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn deleting_an_active_conversation_persists_a_native_cancellation_event() {
    use http_body_util::BodyExt;
    let mut f = Fixture::new().await;
    let conversation = expect(
        f.request(Method::POST, "/pt/v1/conversations", Some(json!({})))
            .await,
        StatusCode::OK,
    );
    let conversation_id = conversation["id"].as_str().unwrap();
    let mut payload = f.body(Op::Responses);
    payload["conversation"] = conversation_id.into();
    payload["store"] = true.into();
    payload["background"] = true.into();
    payload["stream"] = true.into();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/pt/v1/responses")
        .header("authorization", format!("Bearer {}", f.key))
        .header("content-type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(5), f.app.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body();
    let first = tokio::time::timeout(Duration::from_secs(5), stream.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = std::str::from_utf8(first.data_ref().unwrap()).unwrap();
    let event: Value =
        serde_json::from_str(text.lines().find_map(|l| l.strip_prefix("data: ")).unwrap()).unwrap();
    let id = event["response"]["id"].as_str().unwrap().to_owned();
    drop(stream);
    wait_calls(&f, 1).await;
    expect(
        f.request(
            Method::DELETE,
            &format!("/pt/v1/conversations/{conversation_id}"),
            None,
        )
        .await,
        StatusCode::OK,
    );
    let cancelled = wait_state(&f, &format!("/pt/v1/responses/{id}"), "cancelled").await;
    assert_eq!(cancelled["id"], id);
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT frame FROM scoped_response_events WHERE response_id=$1 ORDER BY seq DESC LIMIT 1",[id.clone().into()])).await.unwrap().unwrap();
    let terminal = row.try_get::<String>("", "frame").unwrap();
    assert!(terminal.contains("response_cancelled") && terminal.contains(&id));
    assert!(!terminal.contains("response.completed"));
    f.upstream.stream_release.notify_one();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let stable = expect(
        f.request(Method::GET, &format!("/pt/v1/responses/{id}"), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(stable["status"], "cancelled");
    assert_eq!(f.upstream.calls.lock().unwrap().len(), 1);
    f.finish().await;
}

// An established replay socket is not a perpetual grant. Revocation must not
// cancel the original accepted request's mandatory accounting or change owner.
async fn replay_revocation_case(change: &str) {
    use http_body_util::BodyExt;
    let mut f = Fixture::new().await;
    let mut payload = f.body(Op::Responses);
    payload["background"] = true.into();
    payload["stream"] = true.into();
    payload["store"] = true.into();
    payload["late_delta_after_revoke"] = (change == "user").into();
    payload["jwt_replay_probe"] = (change == "jwt_expiry").into();
    let response = f
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/pt/v1/responses")
                .header("authorization", format!("Bearer {}", f.key))
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let id = tokio::time::timeout(Duration::from_secs(5), async {
        'outer: loop {
            let frame = body
                .frame()
                .await
                .expect("stream ended before response ID")
                .expect("initial stream failed");
            if let Some(bytes) = frame.data_ref() {
                for line in std::str::from_utf8(bytes).unwrap().lines() {
                    if let Some(data) = line.strip_prefix("data: ")
                        && let Ok(value) = serde_json::from_str::<Value>(data)
                        && let Some(id) = value.pointer("/response/id").and_then(Value::as_str)
                    {
                        break 'outer id.to_owned();
                    }
                }
            }
        }
    })
    .await
    .unwrap();
    wait_calls(&f, 1).await;
    // Receipt by the mock server is not yet upstream acceptance. Wait for the
    // actual durable checkpoint before testing the accepted-work drain rule.
    tokio::time::timeout(Duration::from_secs(5),async{loop{
        let accepted=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT 1 FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND execution_json->>'upstream_accepted'='true'",
            [f.user.tenant_id.into(),f.user.id.into(),id.clone().into()])).await.unwrap();
        if accepted.is_some(){break;}
        tokio::time::sleep(Duration::from_millis(10)).await;
    }}).await.expect("upstream acceptance must be durably observed before revocation");
    if change == "jwt_expiry" {
        drop(body);
        let user = keycompute_db::User::find_by_id(&f.db, f.user.id)
            .await
            .unwrap()
            .unwrap();
        let tenant = keycompute_db::Tenant::find_by_id(&f.db, f.user.tenant_id)
            .await
            .unwrap()
            .unwrap();
        let member = keycompute_db::TenantMembership::find(&f.db, tenant.id, user.id)
            .await
            .unwrap()
            .unwrap();
        let token = f
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_identity_token(
                user.id,
                Some(tenant.id),
                user.token_version,
                Some(tenant.authz_version),
                Some(member.authz_version),
                5,
            )
            .unwrap();
        let signed_expiry = f
            .state
            .auth
            .get_jwt_validator()
            .unwrap()
            .validate_claims(&token)
            .unwrap()
            .exp;
        let replay = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/pt/v1/responses/{id}?stream=true"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            replay.status(),
            StatusCode::OK,
            "valid owner JWT must retain inference-resource access"
        );
        body = replay.into_body();
        tokio::time::timeout(Duration::from_secs(3), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // Make the previously scheduler-dependent pending event deterministic:
        // it is produced only after the first authenticated replay batch.
        f.upstream.jwt_replay_probe.notify_one();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                    "SELECT 1 FROM scoped_response_events e JOIN scoped_responses r ON r.id=e.response_id WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.id=$3 AND r.status IN ('queued','in_progress') AND e.frame LIKE '%live-jwt-boundary-probe%'",
                    [f.user.tenant_id.into(),f.user.id.into(),id.clone().into()])).await.unwrap();
                if row.is_some(){break;}
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("live JWT probe must be durably pending before expiry");
        assert!(
            Utc::now().timestamp() < signed_expiry,
            "fixture setup must not consume JWT lifetime"
        );
        // A body poll is not an expiry clock: the next batch may legally
        // contain a late upstream event while this five-second JWT is valid.
        // Keep the durable probe unread and wait for the actual signed exp.
        tokio::time::timeout(Duration::from_secs(7), async {
            while Utc::now().timestamp() < signed_expiry {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("original signed JWT expiry must be reached");
        assert!(Utc::now().timestamp() >= signed_expiry);
        let active = f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM scoped_responses WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND status IN ('queued','in_progress')",
            [f.user.tenant_id.into(), f.user.id.into(), id.clone().into()],
        )).await.unwrap();
        assert!(
            active.is_some(),
            "expiry must stop a live replay, not an already-completed response"
        );
    }

    let statement = match change {
        "jwt_expiry" => Statement::from_string(DbBackend::Postgres, "SELECT 1"),
        "key_expiry" => Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE produce_ai_keys SET expires_at=statement_timestamp()-interval '1 second' WHERE tenant_id=$1 AND user_id=$2",
            [f.user.tenant_id.into(), f.user.id.into()],
        ),
        "key" => Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE produce_ai_keys SET revoked=TRUE,revoked_at=NOW() WHERE tenant_id=$1 AND user_id=$2",
            [f.user.tenant_id.into(), f.user.id.into()],
        ),
        "user" => Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET status='suspended' WHERE id=$1",
            [f.user.id.into()],
        ),
        "token" => Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE users SET token_version=token_version+1 WHERE id=$1",
            [f.user.id.into()],
        ),
        "membership" => Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenant_memberships SET tenant_role='admin' WHERE tenant_id=$1 AND user_id=$2",
            [f.user.tenant_id.into(), f.user.id.into()],
        ),
        "tenant" => Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET authz_version=authz_version+1 WHERE id=$1",
            [f.user.tenant_id.into()],
        ),
        _ => unreachable!(),
    };
    f.db.execute(statement).await.unwrap();
    let next = tokio::time::timeout(
        Duration::from_secs(if change == "jwt_expiry" { 7 } else { 4 }),
        body.frame(),
    )
    .await;
    let stopped = matches!(next, Ok(None) | Ok(Some(Err(_))));
    drop(body);
    f.upstream.stream_release.notify_one();
    // Poll the immutable owner scope directly: invalidated credentials are not
    // used as a workaround to retrieve the result after revocation.
    let settled=tokio::time::timeout(Duration::from_secs(10),async{loop{
        let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT r.user_id,r.request_id,r.status,(SELECT count(*) FROM usage_logs u WHERE u.tenant_id=r.tenant_id AND u.user_id=r.user_id AND u.request_id=r.request_id)::bigint AS charges FROM scoped_responses r WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.id=$3",
            [f.user.tenant_id.into(),f.user.id.into(),id.clone().into()])).await.unwrap().unwrap();
        if row.try_get::<String>("","status").unwrap()=="completed" && row.try_get::<i64>("","charges").unwrap()==1 {break row;}
        tokio::time::sleep(Duration::from_millis(25)).await;
    }}).await;
    let calls = f.upstream.calls.lock().unwrap().len();
    let diagnostic = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT r.status,r.response_json->'error'->>'code' AS error_code,r.execution_json->>'accounting_pending' AS accounting_pending,(SELECT COUNT(*)::bigint FROM usage_logs u WHERE u.tenant_id=r.tenant_id AND u.user_id=r.user_id AND u.request_id=r.request_id) AS charges FROM scoped_responses r WHERE r.tenant_id=$1 AND r.user_id=$2 AND r.id=$3",
        [f.user.tenant_id.into(),f.user.id.into(),id.into()])).await.unwrap().unwrap();
    let diagnostic = (
        diagnostic.try_get::<String>("", "status").unwrap(),
        diagnostic
            .try_get::<Option<String>>("", "error_code")
            .unwrap(),
        diagnostic
            .try_get::<Option<String>>("", "accounting_pending")
            .unwrap(),
        diagnostic.try_get::<i64>("", "charges").unwrap(),
    );
    f.finish().await;
    assert!(
        stopped,
        "{change}: established replay continued after authority was invalidated"
    );
    let settled = settled
        .unwrap_or_else(|_| panic!("{change}: accepted-work accounting state {diagnostic:?}"));
    assert_eq!(settled.try_get::<Uuid>("", "user_id").unwrap(), f.user.id);
    assert_eq!(
        calls, 1,
        "replay revocation must not start another inference"
    );
}
#[tokio::test]
async fn replay_connections_stop_after_key_revocation_or_global_user_suspension() {
    replay_revocation_case("key").await;
    replay_revocation_case("user").await;
}
#[tokio::test]
async fn replay_connections_compare_live_user_tenant_and_membership_versions() {
    for change in ["token", "membership", "tenant"] {
        replay_revocation_case(change).await;
    }
}

#[tokio::test]
async fn replay_connections_enforce_jwt_expiration_and_live_key_expiration() {
    replay_revocation_case("jwt_expiry").await;
    replay_revocation_case("key_expiry").await;
}

#[tokio::test]
async fn tenant_cancels_unleased_managed_node_task_without_charging_or_reexecuting() {
    let mut f = Fixture::new().await;
    let before = UserBalance::find_by_user(&f.db, f.user.tenant_id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    let tenant = keycompute_db::Tenant::find_by_id(&f.db, f.user.tenant_id)
        .await
        .unwrap()
        .unwrap();
    let owner = keycompute_db::User::find_by_id(&f.db, tenant.owner_user_id)
        .await
        .unwrap()
        .unwrap();
    let raw = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(owner.id, None, owner.token_version, None, None, 3600)
        .unwrap();
    let global = f.state.auth.verify_token(&raw).await.unwrap();
    let admin = f
        .state
        .auth
        .select_tenant(&global, Some(tenant.id))
        .await
        .unwrap()
        .access_token;
    let mut body = f.body(Op::Responses);
    body["background"] = true.into();
    body["store"] = true.into();
    let created = expect(
        f.request(Method::POST, "/nt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    let response_id = created["id"].as_str().unwrap();
    let task=tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let task=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT t.id,t.request_id,t.updated_at FROM node_tasks t JOIN scoped_responses r ON r.request_id=t.request_id AND r.tenant_id=t.tenant_id AND r.user_id=t.user_id WHERE r.id=$1 AND t.tenant_id=$2 AND t.user_id=$3 AND t.status='queued'",
                [response_id.into(),tenant.id.into(),f.user.id.into()])).await.unwrap();
            if let Some(task)=task {break task;}
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("managed task should be queued without a worker");
    let id: Uuid = task.try_get("", "id").unwrap();
    let request_id: Uuid = task.try_get("", "request_id").unwrap();
    let revision: chrono::DateTime<Utc> = task.try_get("", "updated_at").unwrap();
    let cancelled = expect(
        http(
            f.app.clone(),
            Method::POST,
            &format!("/api/v1/tenants/{}/tasks/{id}/cancel", tenant.id),
            Some(&admin),
            Some(json!({"expected_updated_at":revision,"reason":"cancel queued response fixture"})),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(cancelled["task"]["status"], "failed");
    assert_eq!(cancelled["task"]["user_id"], f.user.id.to_string());
    wait_state(&f, &format!("/nt/v1/responses/{response_id}"), "failed").await;
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let active:i64=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT COUNT(*)::bigint FROM balance_reservations WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 AND status='active'",
                [tenant.id.into(),f.user.id.into(),request_id.into()])).await.unwrap().unwrap().try_get_by_index(0).unwrap();
            if active==0 {break;}tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("cancelled unleased task releases its original reservation");
    let after = UserBalance::find_by_user(&f.db, tenant.id, f.user.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.available_balance, after.available_balance);
    let charged:i64=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint FROM usage_logs WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3 AND user_amount>0",
        [tenant.id.into(),f.user.id.into(),request_id.into()])).await.unwrap().unwrap().try_get_by_index(0).unwrap();
    assert_eq!(charged, 0);
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    assert!(
        f.state
            .node_gateway
            .as_ref()
            .unwrap()
            .store
            .claim_next_native_task(f.node.id, f.session.id)
            .await
            .unwrap()
            .is_none()
    );
    let retained = keycompute_db::models::node_task::NodeTask::find_by_id(&f.db, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(retained.user_id, f.user.id);
    assert_eq!(retained.tenant_id, tenant.id);
    assert_eq!(retained.request_id, request_id);
    assert!(retained.assigned_node_id.is_none());
    f.finish().await;
}

#[tokio::test]
async fn accepted_node_result_after_cancel_request_keeps_one_original_owner_charge() {
    let mut f = Fixture::new().await;
    let tenant = keycompute_db::Tenant::find_by_id(&f.db, f.user.tenant_id)
        .await
        .unwrap()
        .unwrap();
    let owner = keycompute_db::User::find_by_id(&f.db, tenant.owner_user_id)
        .await
        .unwrap()
        .unwrap();
    let raw = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_identity_token(owner.id, None, owner.token_version, None, None, 3600)
        .unwrap();
    let global = f.state.auth.verify_token(&raw).await.unwrap();
    let admin = f
        .state
        .auth
        .select_tenant(&global, Some(tenant.id))
        .await
        .unwrap()
        .access_token;
    let mut body = f.body(Op::Responses);
    body["background"] = true.into();
    body["store"] = true.into();
    let created = expect(
        f.request(Method::POST, "/nt/v1/responses", Some(body))
            .await,
        StatusCode::OK,
    );
    let id = created["id"].as_str().unwrap();
    let task=tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT t.id FROM node_tasks t JOIN scoped_responses r ON r.request_id=t.request_id AND r.tenant_id=t.tenant_id AND r.user_id=t.user_id WHERE r.id=$1 AND t.tenant_id=$2 AND t.user_id=$3 AND t.status='queued'",
                [id.into(),tenant.id.into(),f.user.id.into()])).await.unwrap();
            if let Some(row)=row {
                let task_id:Uuid=row.try_get_by_index(0).unwrap();
                if let Some((task,_))=f.state.node_gateway.as_ref().unwrap().store.claim_task(task_id,f.node.id,f.session.id).await.unwrap(){break task;}
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("managed node task must be claimed");
    let cancelled=expect(http(f.app.clone(),Method::POST,&format!("/api/v1/tenants/{}/tasks/{}/cancel",tenant.id,task.id),Some(&admin),Some(json!({"expected_updated_at":task.updated_at,"reason":"cancellation racing worker result"}))).await,StatusCode::OK);
    assert_eq!(cancelled["task"]["status"], "leased");
    let result = NodeTaskResult::NativeSucceeded {
        response: NodeNativeHttpResult {
            status: 200,
            headers: vec![],
            body: sample_response(Op::Responses, &f.model),
        },
    };
    // A worker may have completed just before seeing its cancellation signal.
    // The authentic original result must remain chargeable exactly once.
    for _ in 0..2 {
        f.state
            .node_gateway
            .as_ref()
            .unwrap()
            .complete_task(
                task.id,
                task.lease_id.unwrap(),
                f.node.id,
                f.session.id,
                result.clone(),
            )
            .await
            .unwrap();
    }
    wait_state(&f, &format!("/nt/v1/responses/{id}"), "completed").await;
    let rows=f.db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT tenant_id,user_id,input_tokens,output_tokens,user_amount FROM usage_logs WHERE request_id=$1",
        [task.request_id.into()])).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.try_get::<Uuid>("", "tenant_id").unwrap(), tenant.id);
    assert_eq!(row.try_get::<Uuid>("", "user_id").unwrap(), f.user.id);
    assert_ne!(
        f.user.id, owner.id,
        "administrator must not become billing owner"
    );
    assert_eq!(row.try_get::<i32>("", "input_tokens").unwrap(), 7);
    assert_eq!(row.try_get::<i32>("", "output_tokens").unwrap(), 3);
    let amount: bigdecimal::BigDecimal = row.try_get("", "user_amount").unwrap();
    assert!(amount > 0);
    let saved = keycompute_db::models::node_task::NodeTask::find_by_id(&f.db, task.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.user_id, task.user_id);
    assert_eq!(saved.tenant_id, task.tenant_id);
    assert_eq!(saved.lease_id, task.lease_id);
    assert!(saved.cancellation_requested_at.is_some());
    assert!(f.upstream.calls.lock().unwrap().is_empty());
    f.finish().await;
}
