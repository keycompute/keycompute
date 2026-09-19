//! PassthroughBinding contracts against PostgreSQL, the real Axum router and loopback
//! HTTP upstreams. No real provider, production credential or fixed port is used.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{Duration as ChronoDuration, Utc};
use integration_tests::db::{TestDataGuard, create_test_pool, create_test_tenant};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, CreateUserRequest, DbRouter,
    ProduceAiKey, UpdateAccountRequest, User, UserBalance,
    models::passthrough_binding::{
        AccountModelHealth, AccountModelHealthProbe, CreatePassthroughBindingRequest,
        PassthroughBinding,
    },
};
use keycompute_ratelimit::RateLimitKey;
use keycompute_server::{AppState, create_router, state::AppStateConfig};
use keycompute_types::UserRole;
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinHandle};
use tower::ServiceExt;
use uuid::Uuid;

const PT: &str = "/pt/v1/chat/completions";
const ADMIN: &str = "/api/v1/admin/passthrough-bindings";

#[derive(Clone, Debug)]
struct Call {
    channel: String,
    body: Value,
    authorization: String,
}
struct MockState {
    calls: Mutex<Vec<Call>>,
    modes: Mutex<HashMap<String, u16>>,
    entered: Semaphore,
    release: Semaphore,
}
impl MockState {
    fn mode(&self, model: &str, status: u16) {
        self.modes.lock().unwrap().insert(model.to_string(), status);
    }
    fn calls_for(&self, body: &Value) -> Vec<Call> {
        let id = &body["metadata"]["test_id"];
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.body["metadata"]["test_id"] == *id)
            .cloned()
            .collect()
    }
}
async fn upstream(
    State(state): State<Arc<MockState>>,
    Path(channel): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let model = body["model"].as_str().unwrap_or_default().to_string();
    state.calls.lock().unwrap().push(Call {
        channel,
        authorization: headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string(),
        body: body.clone(),
    });
    // Capture outcome at acceptance; a newer failed probe can finish while
    // this earlier success is held, reproducing stale-generation races.
    let status = state
        .modes
        .lock()
        .unwrap()
        .get(&model)
        .copied()
        .unwrap_or(200);
    if body.get("hold_for_test").and_then(Value::as_bool) == Some(true) {
        state.entered.add_permits(1);
        state.release.acquire().await.unwrap().forget();
    }
    if status != 200 {
        return (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({"error": {
                "message": "isolated upstream rejection", "type": "invalid_request_error",
                "code": if status == 404 {"model_not_found"} else {"test_upstream_rejection"},
                "param": if status == 404 {Some("model")} else {None},
            }})),
        )
            .into_response();
    }
    if body["stream"].as_bool() == Some(true) {
        let chunk = json!({"id":"chatcmpl-binding-test","object":"chat.completion.chunk",
            "created":1,"model":model,"choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":null}]});
        let end = json!({"id":"chatcmpl-binding-test","object":"chat.completion.chunk",
            "created":1,"model":model,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
        return (
            [("content-type", "text/event-stream")],
            format!("data: {chunk}\n\ndata: {end}\n\ndata: [DONE]\n\n"),
        )
            .into_response();
    }
    Json(json!({"id":"chatcmpl-binding-test","object":"chat.completion","created":1,
        "model":model,"choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":3,"completion_tokens":1,"total_tokens":4}})).into_response()
}

async fn unexpected_upstream(
    State(state): State<Arc<MockState>>,
    request: Request<Body>,
) -> Response {
    if request.method() == Method::GET && request.uri().path().ends_with("/models") {
        let models: Vec<Value> = state
            .modes
            .lock()
            .unwrap()
            .keys()
            .map(|model| json!({"id":model,"object":"model"}))
            .collect();
        return Json(json!({"object":"list","data":models})).into_response();
    }

    state.calls.lock().unwrap().push(Call {
        channel: "unexpected-resource".into(),
        body: json!({"path":request.uri().path(),"method":request.method().as_str()}),
        authorization: String::new(),
    });
    (
        StatusCode::IM_A_TEAPOT,
        Json(json!({"error":{"message":"resource fixture contacted"}})),
    )
        .into_response()
}

#[derive(Debug)]
struct HttpResult {
    status: StatusCode,
    body: Value,
    headers: HeaderMap,
}
async fn api(
    app: Router,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> HttpResult {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-real-ip", "127.0.0.1");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = tokio::time::timeout(
        Duration::from_secs(15),
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
    .expect("HTTP request did not finish")
    .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = tokio::time::timeout(
        Duration::from_secs(15),
        to_bytes(response.into_body(), 2 << 20),
    )
    .await
    .expect("response body did not finish")
    .unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    HttpResult {
        status,
        body,
        headers,
    }
}
fn expect(result: HttpResult, status: StatusCode) -> Value {
    assert_eq!(
        result.status, status,
        "unexpected response: {}",
        result.body
    );
    result.body
}

struct Fixture {
    db: DatabaseConnection,
    cleanup: TestDataGuard,
    state: AppState,
    app: Router,
    admin: String,
    key: String,
    key_id: Uuid,
    user: User,
    run: String,
    models: [String; 2],
    accounts: Vec<Account>,
    mock: Arc<MockState>,
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
        let tenant = create_test_tenant(&db, "binding", &run).await;
        let user = User::create(
            &db,
            &CreateUserRequest {
                tenant_id: tenant.id,
                email: format!("binding-{run}@example.test"),
                name: Some("Binding fixture admin".into()),
                role: Some(UserRole::Admin),
            },
        )
        .await
        .unwrap();
        UserBalance::recharge(
            &db,
            user.id,
            tenant.id,
            Decimal::from(1000),
            None,
            Some("isolated binding regression credit"),
        )
        .await
        .unwrap();
        keycompute_runtime::set_global_crypto(&keycompute_runtime::ApiKeyCrypto::generate_key())
            .unwrap();
        let models = [format!("mb-m-{run}"), format!("mb-n-{run}")];
        let mock = Arc::new(MockState {
            calls: Mutex::new(Vec::new()),
            modes: Mutex::new(HashMap::new()),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let upstream_app = Router::new()
            .route("/{channel}/v1/chat/completions", post(upstream))
            .fallback(unexpected_upstream)
            .with_state(Arc::clone(&mock));
        let server = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });
        let mut accounts = Vec::new();
        for (channel, priority) in [("A", 0), ("B", 10), ("C", 9)] {
            accounts.push(
                Account::create(
                    &db,
                    &CreateAccountRequest {
                        tenant_id: tenant.id,
                        provider: "openai".into(),
                        name: format!("binding-{channel}-{run}"),
                        endpoint: format!("http://{address}/{channel}/v1"),
                        upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(&format!(
                            "sk-binding-{channel}"
                        ))
                        .unwrap()
                        .into_inner(),
                        upstream_api_key_preview: "sk-binding****".into(),
                        rpm_limit: Some(1000),
                        tpm_limit: Some(1_000_000),
                        priority: Some(priority),
                        models_supported: models.to_vec(),
                        api_capabilities: vec!["chat_completions".into()],
                        pool_enabled: None,
                        visibility: Some("tenant".into()),
                    },
                )
                .await
                .unwrap(),
            );
        }
        let mut config = AppStateConfig::default();
        config.gateway.admission.account_limit = 1;
        config.gateway.admission.queue_timeout_ms = 5_000;
        let state = AppState::try_with_pool_and_config(DbRouter::single(db.clone()), config)
            .await
            .unwrap();
        let admin = state
            .auth
            .get_jwt_validator()
            .unwrap()
            .generate_token_with_version(user.id, tenant.id, &user.role, user.token_version)
            .unwrap();
        let key = ProduceAiKeyValidator::generate_key();
        let key_id = ProduceAiKey::create(
            &db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: user.id,
                name: "binding-test".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "sk-****".into(),
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
            admin,
            key,
            key_id,
            user,
            run,
            models,
            accounts,
            mock,
            server,
        }
    }
    fn body(&self, index: usize) -> Value {
        json!({"model":self.models[index],"messages":[{"role":"user","content":"hello"}],
            "max_tokens":8,"metadata":{"test_id":Uuid::new_v4().to_string()},
            "tools":[{"type":"function","function":{"name":"noop","parameters":{"type":"object","properties":{}}}}],
            "vendor_extension":{"preserve":true}})
    }
    async fn admin(&self, method: Method, path: &str, body: Option<Value>) -> HttpResult {
        api(self.app.clone(), method, path, Some(&self.admin), body).await
    }
    async fn chat(&self, path: &str, body: Value) -> HttpResult {
        api(
            self.app.clone(),
            Method::POST,
            path,
            Some(&self.key),
            Some(body),
        )
        .await
    }
    async fn bind(&self, pool_enabled: bool) -> Value {
        expect(
            self.admin(
                Method::POST,
                ADMIN,
                Some(json!({
                    "tenant_id": self.user.tenant_id, "account_id": self.accounts[0].id,
                    "is_global": false, "pool_enabled": pool_enabled
                })),
            )
            .await,
            StatusCode::OK,
        )
    }
    async fn probe(&self, binding: &Value) -> Value {
        expect(
            self.admin(
                Method::POST,
                &format!("{ADMIN}/{}/probe", binding["id"].as_str().unwrap()),
                Some(json!({"model": self.models[0], "timeout_ms": 5000})),
            )
            .await,
            StatusCode::OK,
        )
    }
    async fn listed(&self) -> Vec<String> {
        let response = api(
            self.app.clone(),
            Method::GET,
            "/pt/v1/models",
            Some(&self.key),
            None,
        )
        .await;
        expect(response, StatusCode::OK)["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect()
    }
    async fn health_update(&self, clause: &str) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "UPDATE account_model_health SET {clause} WHERE account_id=$1 AND model=$2"
                ),
                [self.accounts[0].id.into(), self.models[0].clone().into()],
            ))
            .await
            .unwrap();
    }
    async fn finish(&mut self) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM response_affinities WHERE tenant_id=$1",
                [self.user.tenant_id.into()],
            ))
            .await
            .unwrap();
        self.cleanup.cleanup().await.unwrap();
    }
}

#[tokio::test]
async fn passthrough_create_contract_and_dynamic_models() {
    let mut f = Fixture::new().await;
    let b = f.bind(false).await;
    assert_eq!(b["is_global"], false);
    assert_eq!(b["pool_enabled"], false);
    assert!(b.get("model").is_none());
    assert!(b.get("enabled").is_none());
    assert_eq!(b["models_supported"].as_array().unwrap().len(), 2);
    let list = expect(
        f.admin(
            Method::GET,
            &format!("{ADMIN}?tenant_id={}", f.user.tenant_id),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(list["bindings"].as_array().unwrap().len(), 1);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_defaults_missing_health_and_probe_is_diagnostic_only() {
    let mut f = Fixture::new().await;
    let b = f.bind(false).await;
    let response = f.chat(PT, f.body(0)).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let p = f.probe(&b).await;
    assert_eq!(p["status"], "healthy");
    assert_eq!(p["scope"], "single_model_diagnostic");
    let after = expect(
        f.admin(
            Method::GET,
            &format!("{ADMIN}/{}", b["id"].as_str().unwrap()),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(after["id"], b["id"]);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_rejects_legacy_write_fields_and_stale_revision() {
    let mut f = Fixture::new().await;
    let bad=f.admin(Method::POST,ADMIN,Some(json!({"tenant_id":f.user.tenant_id,"account_id":f.accounts[0].id,"model":f.models[0]}))).await;
    assert!(
        bad.status == StatusCode::BAD_REQUEST || bad.status == StatusCode::UNPROCESSABLE_ENTITY
    );
    let b = f.bind(false).await;
    let stale = f
        .admin(
            Method::PUT,
            &format!("{ADMIN}/{}", b["id"].as_str().unwrap()),
            Some(json!({"pool_enabled":true,"expected_revision":0})),
        )
        .await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_single_attempt_preserves_native_json() {
    let mut f = Fixture::new().await;
    let _ = f.bind(false).await;
    let body = f.body(0);
    let id = body["metadata"]["test_id"].clone();
    let response = f.chat(PT, body).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.body);
    let calls = f
        .mock
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.body["metadata"]["test_id"] == id)
        .count();
    assert_eq!(calls, 1);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_delete_revokes_grant_without_reopening_pool() {
    let mut f = Fixture::new().await;
    let b = f.bind(true).await;
    let del = f
        .admin(
            Method::DELETE,
            &format!(
                "{ADMIN}/{}?expected_revision={}",
                b["id"].as_str().unwrap(),
                b["revision"]
            ),
            None,
        )
        .await;
    assert_eq!(del.status, StatusCode::OK);
    let account = Account::find_by_id(&f.db, f.accounts[0].id)
        .await
        .unwrap()
        .unwrap();
    assert!(!account.pool_enabled);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_health_bad_model_isolated() {
    let mut f = Fixture::new().await;
    let _ = f.bind(false).await;
    expect(f.chat(PT, f.body(0)).await, StatusCode::OK);
    f.health_update("status='unhealthy', reason_code='test_failure'")
        .await;
    let bad = f.chat(PT, f.body(0)).await;
    assert_eq!(bad.status, StatusCode::SERVICE_UNAVAILABLE, "{}", bad.body);
    let good = f.chat(PT, f.body(1)).await;
    assert_eq!(good.status, StatusCode::OK, "{}", good.body);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_global_grant_allows_cross_owner_anchor_and_deduplicates() {
    let mut f = Fixture::new().await;
    let other = create_test_tenant(&f.db, "other", &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET visibility='global' WHERE id=$1",
        [f.accounts[0].id.into()],
    ))
    .await
    .unwrap();
    let first = PassthroughBinding::create(
        &f.db,
        &CreatePassthroughBindingRequest {
            account_id: f.accounts[0].id,
            tenant_id: other.id,
            is_global: true,
            pool_enabled: false,
        },
    )
    .await
    .unwrap();
    assert!(first.is_global);
    let second = PassthroughBinding::create(
        &f.db,
        &CreatePassthroughBindingRequest {
            account_id: f.accounts[0].id,
            tenant_id: f.user.tenant_id,
            is_global: false,
            pool_enabled: true,
        },
    )
    .await
    .unwrap();
    assert_ne!(first.id, second.id);
    let rows = PassthroughBinding::find_all_filtered(&f.db, None, 20, 0)
        .await
        .unwrap();
    assert!(
        rows.iter()
            .filter(|r| r.account_id == f.accounts[0].id)
            .count()
            >= 2
    );
    f.finish().await;
}

#[tokio::test]
async fn passthrough_grant_account_models_are_dynamic_and_pool_flag_is_scoped() {
    let mut f = Fixture::new().await;
    let b = f.bind(true).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET models_supported = array_append(models_supported, $2) WHERE id=$1",
        [
            f.accounts[0].id.into(),
            format!("new-model-{}", f.run).into(),
        ],
    ))
    .await
    .unwrap();
    let account = Account::find_by_id(&f.db, f.accounts[0].id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        account
            .models_supported
            .iter()
            .any(|m| m.starts_with("new-model-"))
    );
    assert_eq!(b["pool_enabled"], true);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_options_are_bounded_and_model_display_only() {
    let mut f = Fixture::new().await;
    let response = expect(
        f.admin(
            Method::GET,
            "/api/v1/admin/passthrough-bindings/options?page=1&page_size=10",
            None,
        )
        .await,
        StatusCode::OK,
    );
    let option = &response["accounts"][0];
    assert!(option.get("id").is_some());
    assert!(option.get("models").is_some());
    assert!(option.get("endpoint").is_none());
    assert!(option.get("upstream_api_key").is_none());
    f.finish().await;
}

#[tokio::test]
async fn passthrough_ambiguous_accounts_fail_before_upstream_io() {
    let mut f = Fixture::new().await;
    let req = |account_id| CreatePassthroughBindingRequest {
        account_id,
        tenant_id: f.user.tenant_id,
        is_global: false,
        pool_enabled: false,
    };
    PassthroughBinding::create(&f.db, &req(f.accounts[0].id))
        .await
        .unwrap();
    assert!(
        PassthroughBinding::create(&f.db, &req(f.accounts[1].id))
            .await
            .is_err()
    );
    // Out-of-band configuration corruption must still fail closed at runtime.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO passthrough_bindings(account_id,tenant_id) VALUES($1,$2)",
        [f.accounts[1].id.into(), f.user.tenant_id.into()],
    ))
    .await
    .unwrap();
    let body = f.body(0);
    let test_id = body["metadata"]["test_id"].clone();
    let result = f.chat(PT, body).await;
    assert_eq!(result.status, StatusCode::CONFLICT);
    assert_eq!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.body["metadata"]["test_id"] == test_id)
            .count(),
        0
    );
    f.finish().await;
}

#[tokio::test]
async fn account_pool_checkbox_cannot_override_a_bound_grant() {
    let mut f = Fixture::new().await;
    let _ = f.bind(true).await;
    let err = f.accounts[0]
        .update(
            &f.db,
            &UpdateAccountRequest {
                tenant_id: None,
                name: None,
                endpoint: None,
                upstream_api_key_encrypted: None,
                upstream_api_key_preview: None,
                rpm_limit: None,
                tpm_limit: None,
                priority: None,
                enabled: None,
                models_supported: None,
                api_capabilities: None,
                visibility: None,
                pool_enabled: Some(false),
            },
        )
        .await;
    assert!(err.is_err());
    f.finish().await;
}

#[tokio::test]
async fn passthrough_shares_account_quota_and_provenance_with_normal_requests() {
    let mut f = Fixture::new().await;
    let _ = f.bind(true).await;
    let first = f.chat(PT, f.body(0)).await;
    let second = f.chat(PT, f.body(1)).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(second.status, StatusCode::OK, "{}", second.body);
    {
        let calls = f.mock.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .all(|call| call.authorization == "Bearer sk-binding-A" && call.channel == "A")
        );
    }
    f.finish().await;
}

#[tokio::test]
async fn passthrough_client_error_does_not_poison_sibling_model() {
    let mut f = Fixture::new().await;
    let _ = f.bind(false).await;
    f.mock.mode(&f.models[0], 400);
    let failed = f.chat(PT, f.body(0)).await;
    assert_eq!(failed.status, StatusCode::BAD_REQUEST, "{}", failed.body);
    f.mock.mode(&f.models[1], 200);
    let sibling = f.chat(PT, f.body(1)).await;
    assert_eq!(sibling.status, StatusCode::OK, "{}", sibling.body);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_stream_and_extensions_round_trip_without_retry() {
    let mut f = Fixture::new().await;
    let _ = f.bind(false).await;
    let mut body = f.body(0);
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage": true});
    let id = body["metadata"]["test_id"].clone();
    let result = f.chat(PT, body).await;
    assert_eq!(result.status, StatusCode::OK, "{}", result.body);
    assert!(
        f.mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.body["metadata"]["test_id"] == id)
            .count()
            == 1
    );
    f.finish().await;
}

#[tokio::test]
async fn passthrough_uses_only_a_preserves_native_json_and_records_real_account() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    assert_eq!(f.probe(&passthrough).await["status"], "healthy");
    let listed = f.listed().await;
    // Other concurrent fixtures may intentionally publish global grants.
    // Assert the complete model set owned by this fixture, not a globally
    // empty or fixed-size directory shared by all authenticated tenants.
    assert_eq!(listed.iter().filter(|m| f.models.contains(m)).count(), 2);
    assert!(f.models.iter().all(|m| listed.contains(m)));
    for stream in [
        None,
        Some(Value::Null),
        Some(json!(false)),
        Some(json!(true)),
    ] {
        let mut body = f.body(0);
        if let Some(stream) = stream {
            body["stream"] = stream;
        }
        if body["stream"] == true {
            body["stream_options"] = json!({"include_usage":false,"vendor":"keep"});
        }
        let result = f.chat(PT, body.clone()).await;
        let id = result
            .headers
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let response = expect(result, StatusCode::OK);
        if body["stream"] == true {
            assert!(response.as_str().unwrap().contains("[DONE]"));
        }
        let calls = f.mock.calls_for(&body);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].channel, "A");
        assert_eq!(calls[0].body, body);
        assert_eq!(calls[0].authorization, "Bearer sk-binding-A");
        let row =
            f.db.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT route_type,request_path FROM gateway_requests WHERE request_id=$1",
                [Uuid::parse_str(&id).unwrap().into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get::<String>("", "route_type").unwrap(),
            "passthrough_binding"
        );
        assert_eq!(row.try_get::<String>("", "request_path").unwrap(), PT);
        let ledger =
            f.db.query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT account_id FROM usage_logs WHERE request_id=$1",
                [Uuid::parse_str(&id).unwrap().into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            ledger.try_get::<Uuid>("", "account_id").unwrap(),
            f.accounts[0].id
        );
    }
    // One execution RPM per POST, not middleware plus handler double counting.
    let rpm = f
        .state
        .rate_limiter
        .get_rpm_count(&RateLimitKey::new(f.user.tenant_id, f.user.id, f.key_id))
        .await
        .unwrap();
    // GET /pt/v1/models is a separately rate-limited request.
    assert_eq!(rpm, 5);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_old_running_success_cannot_revive_a_newer_failed_probe() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    f.probe(&passthrough).await;
    let mut body = f.body(0);
    body["hold_for_test"] = true.into();
    let app = f.app.clone();
    let key = f.key.clone();
    let worker =
        tokio::spawn(async move { api(app, Method::POST, PT, Some(&key), Some(body)).await });
    tokio::time::timeout(Duration::from_secs(10), f.mock.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    f.mock.mode(&f.models[0], 404);
    assert_eq!(f.probe(&passthrough).await["status"], "unhealthy");
    f.mock.mode(&f.models[0], 200);
    f.mock.release.add_permits(1);
    expect(worker.await.unwrap(), StatusCode::OK);
    let body = f.body(0);
    expect(
        f.chat(PT, body.clone()).await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert!(f.mock.calls_for(&body).is_empty());
    let health =
        AccountModelHealth::find(&f.db, f.accounts[0].id, "chat_completions", &f.models[0])
            .await
            .unwrap()
            .unwrap();
    assert_eq!(health.status, "unhealthy");
    f.finish().await;
}

#[tokio::test]
async fn ordinary_model_failure_does_not_quarantine_sibling_bound_model() {
    let mut f = Fixture::new().await;
    let m = f.bind(true).await;
    f.probe(&m).await;
    for account in &f.accounts[1..] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE accounts SET enabled=FALSE WHERE id=$1",
            [account.id.into()],
        ))
        .await
        .unwrap();
    }
    f.mock.mode(&f.models[0], 404);
    let body = f.body(0);
    expect(
        f.chat("/v1/chat/completions", body.clone()).await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.mock.calls_for(&body).len(), 1);
    let account = Account::find_by_id(&f.db, f.accounts[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        f.state.provider_health.account_health_for(&account).status,
        "unhealthy"
    );
    let m_health =
        AccountModelHealth::find(&f.db, f.accounts[0].id, "chat_completions", &f.models[0])
            .await
            .unwrap()
            .unwrap();
    assert_eq!(m_health.status, "unhealthy");
    expect(f.chat(PT, f.body(1)).await, StatusCode::OK);
    let body = f.body(0);
    expect(
        f.chat(PT, body.clone()).await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert!(f.mock.calls_for(&body).is_empty());
    f.finish().await;
}

#[tokio::test]
async fn passthrough_retarget_while_queued_rejects_without_dispatch_or_fallback() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    f.probe(&passthrough).await;
    let permit = f
        .state
        .generation_admission
        .accounts
        .acquire(f.accounts[0].id)
        .await
        .unwrap();
    let body = f.body(0);
    let app = f.app.clone();
    let key = f.key.clone();
    let worker_body = body.clone();
    let worker =
        tokio::spawn(
            async move { api(app, Method::POST, PT, Some(&key), Some(worker_body)).await },
        );
    tokio::time::timeout(Duration::from_secs(4), async {
        while f.state.generation_admission.accounts.status().queued == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request must reach account queue");
    expect(
        f.admin(
            Method::PUT,
            &format!("{ADMIN}/{}", passthrough["id"].as_str().unwrap()),
            Some(json!({"expected_revision":1,"account_id":f.accounts[1].id})),
        )
        .await,
        StatusCode::OK,
    );
    drop(permit);
    expect(worker.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(f.mock.calls_for(&body).is_empty());
    assert_eq!(f.state.generation_admission.accounts.status().active, 0);
    let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
    "SELECT COUNT(*)::BIGINT AS n FROM balance_reservations WHERE user_id=$1 AND status='active'", [f.user.id.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
    f.finish().await;
}

#[tokio::test]
async fn passthrough_probe_cas_rejects_stale_generation_and_changed_account_config() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    f.probe(&passthrough).await;
    let before =
        AccountModelHealth::find(&f.db, f.accounts[0].id, "chat_completions", &f.models[0])
            .await
            .unwrap()
            .unwrap();
    let now = Utc::now();
    let stale_probe = AccountModelHealthProbe {
        account_id: before.account_id,
        api_capability: before.api_capability.clone(),
        model: before.model.clone(),
        status: "healthy".into(),
        reason_code: None,
        checked_at: now,
        expires_at: now + ChronoDuration::minutes(5),
        account_config_version: before.account_config_version,
        expected_generation: before.generation,
    };
    f.mock.mode(&f.models[0], 404);
    f.probe(&passthrough).await;
    assert!(
        AccountModelHealth::upsert_probe_if_current(&f.db, &stale_probe)
            .await
            .unwrap()
            .is_none()
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET upstream_config_version=upstream_config_version+INTERVAL '1 second' WHERE id=$1",
        [f.accounts[0].id.into()],
    ))
    .await
    .unwrap();
    let latest =
        AccountModelHealth::find(&f.db, f.accounts[0].id, "chat_completions", &f.models[0])
            .await
            .unwrap()
            .unwrap();
    let old_config = AccountModelHealthProbe {
        expected_generation: latest.generation,
        ..stale_probe
    };
    assert!(
        AccountModelHealth::upsert_probe_if_current(&f.db, &old_config)
            .await
            .unwrap()
            .is_none()
    );
    f.finish().await;
}

#[tokio::test]
async fn passthrough_global_account_owner_lifecycle_is_rechecked() {
    let mut f = Fixture::new().await;
    let owner = create_test_tenant(&f.db, "passthrough-owner", &f.run).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET tenant_id=$2,visibility='global' WHERE id=$1",
        [f.accounts[0].id.into(), owner.id.into()],
    ))
    .await
    .unwrap();
    let passthrough = f.bind(false).await;
    // An unrelated global offering remains visible after this account's
    // owner is deactivated. Make that coexistence deterministic rather than
    // relying on timing between independent parallel tests.
    let unrelated_model = format!("global-independent-{}", f.run);
    expect(
        f.admin(
            Method::PUT,
            &format!("/api/v1/accounts/{}", f.accounts[1].id),
            Some(json!({"models":[unrelated_model]})),
        )
        .await,
        StatusCode::OK,
    );
    expect(
        f.admin(
            Method::POST,
            ADMIN,
            Some(json!({
                "account_id": f.accounts[1].id, "tenant_id": f.user.tenant_id,
                "is_global": true, "pool_enabled": false,
            })),
        )
        .await,
        StatusCode::OK,
    );
    f.probe(&passthrough).await;
    expect(f.chat(PT, f.body(0)).await, StatusCode::OK);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenants SET status='inactive' WHERE id=$1",
        [owner.id.into()],
    ))
    .await
    .unwrap();
    let body = f.body(0);
    expect(
        f.chat(PT, body.clone()).await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert!(f.mock.calls_for(&body).is_empty());
    let listed = f.listed().await;
    assert!(f.models.iter().all(|model| !listed.contains(model)));
    assert!(
        listed.contains(&unrelated_model),
        "unrelated global grants must remain visible"
    );
    f.finish().await;
}

#[tokio::test]
async fn passthrough_private_model_is_not_visible_to_a_different_authenticated_tenant() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    f.probe(&passthrough).await;
    let other = create_test_tenant(&f.db, "passthrough-private-other", &f.run).await;
    let user =
        integration_tests::db::create_test_user(&f.db, other.id, "passthrough-private", &f.run)
            .await;
    let token = f
        .state
        .auth
        .get_jwt_validator()
        .unwrap()
        .generate_token_with_version(user.id, user.tenant_id, &user.role, user.token_version)
        .unwrap();
    let body = f.body(0);
    expect(
        api(
            f.app.clone(),
            Method::POST,
            PT,
            Some(&token),
            Some(body.clone()),
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    assert!(f.mock.calls_for(&body).is_empty());
    let listed = expect(
        api(
            f.app.clone(),
            Method::GET,
            "/pt/v1/models",
            Some(&token),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert!(
        listed["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| { !f.models.iter().any(|model| entry["id"] == *model) })
    );
    f.finish().await;
}

#[tokio::test]
async fn passthrough_admin_endpoint_change_during_queue_rejects_old_connection() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    f.probe(&passthrough).await;
    let permit = f
        .state
        .generation_admission
        .accounts
        .acquire(f.accounts[0].id)
        .await
        .unwrap();
    let body = f.body(0);
    let app = f.app.clone();
    let token = f.key.clone();
    let payload = body.clone();
    let worker =
        tokio::spawn(async move { api(app, Method::POST, PT, Some(&token), Some(payload)).await });
    tokio::time::timeout(Duration::from_secs(4), async {
        while f.state.generation_admission.accounts.status().queued == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    expect(
        f.admin(
            Method::PUT,
            &format!("/api/v1/accounts/{}", f.accounts[0].id),
            Some(json!({"api_base":f.accounts[1].endpoint})),
        )
        .await,
        StatusCode::OK,
    );
    let updated = Account::find_by_id(&f.db, f.accounts[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.endpoint, f.accounts[1].endpoint,
        "admin request must actually change the upstream connection"
    );
    drop(permit);
    let error = expect(worker.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        error["error"]["code"]
            .as_str()
            .unwrap()
            .starts_with("passthrough_binding_")
    );
    assert!(f.mock.calls_for(&body).is_empty());
    f.finish().await;
}

#[tokio::test]
async fn passthrough_client_cancellation_releases_resources_without_poisoning_model_health() {
    let mut f = Fixture::new().await;
    let passthrough = f.bind(false).await;
    f.probe(&passthrough).await;
    let before =
        AccountModelHealth::find(&f.db, f.accounts[0].id, "chat_completions", &f.models[0])
            .await
            .unwrap()
            .unwrap();
    let mut body = f.body(0);
    body["hold_for_test"] = true.into();
    let payload = body.clone();
    let app = f.app.clone();
    let token = f.key.clone();
    let worker =
        tokio::spawn(async move { api(app, Method::POST, PT, Some(&token), Some(payload)).await });
    tokio::time::timeout(Duration::from_secs(10), f.mock.entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(10),async {
    loop {
        let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*)::BIGINT AS n FROM balance_reservations WHERE user_id=$1 AND status='active'",
            [f.user.id.into()])).await.unwrap().unwrap();
        if f.state.generation_admission.accounts.status().active==0
            && f.state.generation_admission.requests.status().active==0
            && row.try_get::<i64>("","n").unwrap()==0 {break;}
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}).await.expect("cancelled passthrough request must release permits and settle its reservation");
    f.mock.release.add_permits(1);
    assert_eq!(f.mock.calls_for(&body).len(), 1);
    let after = AccountModelHealth::find(&f.db, f.accounts[0].id, "chat_completions", &f.models[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.status, "healthy");
    assert_eq!(after.generation, before.generation);
    f.finish().await;
}

#[tokio::test]
async fn exclusive_grant_blocks_existing_response_resources_and_continuations() {
    let mut f = Fixture::new().await;
    expect(
        f.admin(
            Method::PUT,
            &format!("/api/v1/accounts/{}", f.accounts[0].id),
            Some(json!({"api_capabilities":["chat_completions","responses"]})),
        )
        .await,
        StatusCode::OK,
    );
    let response_id = format!("resp_{}", Uuid::new_v4().simple());
    keycompute_db::ResponseAffinity::upsert_route(
        &f.db,
        f.user.tenant_id,
        &response_id,
        "openai",
        Some(&f.models[0]),
        f.accounts[0].id,
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .unwrap();
    let binding = f.bind(false).await;
    for deleted in [false, true] {
        if deleted {
            expect(
                f.admin(
                    Method::DELETE,
                    &format!(
                        "{ADMIN}/{}?expected_revision={}",
                        binding["id"].as_str().unwrap(),
                        binding["revision"]
                    ),
                    None,
                )
                .await,
                StatusCode::OK,
            );
        }
        let before = f.mock.calls.lock().unwrap().len();
        for (method, path, body) in [
            (Method::GET, format!("/v1/responses/{response_id}"), None),
            (
                Method::GET,
                format!("/v1/responses/{response_id}/input_items"),
                None,
            ),
            (Method::DELETE, format!("/v1/responses/{response_id}"), None),
            (
                Method::POST,
                format!("/v1/responses/{response_id}/cancel"),
                Some(json!({})),
            ),
            (
                Method::POST,
                "/v1/responses".into(),
                Some(
                    json!({"model":f.models[0],"input":"hello","previous_response_id":response_id}),
                ),
            ),
        ] {
            expect(
                api(f.app.clone(), method, &path, Some(&f.key), body).await,
                StatusCode::NOT_FOUND,
            );
        }
        assert_eq!(
            f.mock.calls.lock().unwrap().len(),
            before,
            "an existing affinity must not bypass a /pt-only grant"
        );
    }
    f.finish().await;
}

#[tokio::test]
async fn private_account_grants_apply_scope_and_pool_matrix_to_discovery_and_execution() {
    let mut f = Fixture::new().await;
    // Legacy visibility deliberately stays private: the grant must authorize
    // both the router and rate-limit/admission layers, without owner leakage.
    for account in &f.accounts[1..] {
        f.db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE accounts SET enabled=FALSE WHERE id=$1",
            [account.id.into()],
        ))
        .await
        .unwrap();
    }
    let mut consumers = Vec::new();
    for name in ["grant-target", "grant-outsider"] {
        let tenant = create_test_tenant(&f.db, name, &f.run).await;
        let user = integration_tests::db::create_test_user(&f.db, tenant.id, name, &f.run).await;
        UserBalance::recharge(
            &f.db,
            user.id,
            tenant.id,
            Decimal::from(1000),
            None,
            Some("isolated account grant matrix"),
        )
        .await
        .unwrap();
        let key = ProduceAiKeyValidator::generate_key();
        ProduceAiKey::create(
            &f.db,
            &CreateProduceAiKeyRequest {
                tenant_id: tenant.id,
                user_id: user.id,
                name: "grant-matrix".into(),
                produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&key),
                produce_ai_key_preview: "sk-test****".into(),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        consumers.push((tenant.id, key));
    }
    let mut binding = expect(
        f.admin(
            Method::POST,
            ADMIN,
            Some(json!({
                "account_id":f.accounts[0].id,"tenant_id":consumers[0].0,
            })),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(binding["is_global"], false);
    assert_eq!(binding["pool_enabled"], false);
    for (global, pool_enabled) in [(false, false), (false, true), (true, false), (true, true)] {
        binding = expect(f.admin(Method::PUT, &format!("{ADMIN}/{}",binding["id"].as_str().unwrap()),
            Some(json!({"expected_revision":binding["revision"],"is_global":global,"pool_enabled":pool_enabled}))).await, StatusCode::OK);
        for (key, scope_allows) in [
            (&consumers[0].1, true),
            (&consumers[1].1, global),
            (&f.key, global),
        ] {
            for (path, allowed) in [
                (PT, scope_allows),
                ("/v1/chat/completions", scope_allows && pool_enabled),
            ] {
                let body = f.body(1);
                expect(
                    api(
                        f.app.clone(),
                        Method::POST,
                        path,
                        Some(key),
                        Some(body.clone()),
                    )
                    .await,
                    if allowed {
                        StatusCode::OK
                    } else {
                        StatusCode::NOT_FOUND
                    },
                );
                let calls = f.mock.calls_for(&body);
                assert_eq!(
                    calls.len(),
                    usize::from(allowed),
                    "{path}: global={global}, pool={pool_enabled}"
                );
                if allowed {
                    assert_eq!(calls[0].channel, "A");
                }
            }
            for (path, allowed) in [
                ("/pt/v1/models", scope_allows),
                ("/v1/models", scope_allows && pool_enabled),
            ] {
                let list = expect(
                    api(f.app.clone(), Method::GET, path, Some(key), None).await,
                    StatusCode::OK,
                );
                for model in &f.models {
                    assert_eq!(
                        list["data"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|m| m["id"] == *model),
                        allowed,
                        "discovery {path}: global={global}, pool={pool_enabled}"
                    );
                }
            }
        }
    }
    let account = Account::find_by_id(&f.db, f.accounts[0].id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        !account.pool_enabled,
        "binding flags must not reopen legacy account exposure"
    );
    assert_eq!(account.visibility, "tenant");
    f.finish().await;
}

#[tokio::test]
async fn restricting_a_grant_remains_possible_during_namespace_conflict() {
    let mut f = Fixture::new().await;
    let first = f.bind(true).await;
    let other_model = format!("other-{}", f.run);
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET models_supported=ARRAY[$2]::TEXT[] WHERE id=$1",
        [f.accounts[1].id.into(), other_model.into()],
    ))
    .await
    .unwrap();
    expect(
        f.admin(
            Method::POST,
            ADMIN,
            Some(json!({"account_id":f.accounts[1].id,
        "tenant_id":f.user.tenant_id})),
        )
        .await,
        StatusCode::OK,
    );
    // Out-of-band edit is intentionally unsupported but must not block revoking
    // an already excessive grant while the runtime rejects the ambiguity.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE accounts SET models_supported=ARRAY[$2]::TEXT[] WHERE id=$1",
        [f.accounts[1].id.into(), f.models[0].clone().into()],
    ))
    .await
    .unwrap();
    let body = f.body(0);
    expect(f.chat(PT, body.clone()).await, StatusCode::CONFLICT);
    assert!(f.mock.calls_for(&body).is_empty());
    let reduced = expect(
        f.admin(
            Method::PUT,
            &format!("{ADMIN}/{}", first["id"].as_str().unwrap()),
            Some(json!({"expected_revision":first["revision"],"pool_enabled":false})),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(reduced["pool_enabled"], false);
    assert_eq!(reduced["revision"], 2);
    f.finish().await;
}

#[tokio::test]
async fn invalid_connection_is_not_advertised_as_ready_or_healthy() {
    let mut f = Fixture::new().await;
    let binding = f.bind(false).await;
    f.probe(&binding).await;
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE accounts SET health_status='healthy',upstream_api_key_encrypted='invalid-test-ciphertext' WHERE id=$1",
        [f.accounts[0].id.into()])).await.unwrap();
    let details = expect(
        f.admin(
            Method::GET,
            &format!("{ADMIN}/{}", binding["id"].as_str().unwrap()),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(details["health_status"], "unavailable");
    assert!(details.get("upstream_api_key_encrypted").is_none());
    assert!(details.get("endpoint").is_none());
    let page = expect(
        f.admin(
            Method::GET,
            &format!(
                "/api/v1/admin/model-catalog?mode=passthrough&tenant_id={}&q={}",
                f.user.tenant_id, f.run
            ),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(page["entries"].as_array().unwrap().len(), 2);
    assert!(
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["status"] == "unavailable" && v["eligible_targets"] == 0)
    );
    let listed = f.listed().await;
    assert!(f.models.iter().all(|model| !listed.contains(model)));
    let body = f.body(0);
    expect(
        f.chat(PT, body.clone()).await,
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert!(f.mock.calls_for(&body).is_empty());
    f.finish().await;
}

#[tokio::test]
async fn refreshing_account_models_rolls_back_scope_collisions() {
    let mut f = Fixture::new().await;
    f.bind(false).await;
    let other_model = format!("refresh-only-{}", f.run);
    expect(
        f.admin(
            Method::PUT,
            &format!("/api/v1/accounts/{}", f.accounts[1].id),
            Some(json!({"models":[other_model]})),
        )
        .await,
        StatusCode::OK,
    );
    expect(
        f.admin(
            Method::POST,
            ADMIN,
            Some(json!({"account_id":f.accounts[1].id,"tenant_id":f.user.tenant_id})),
        )
        .await,
        StatusCode::OK,
    );
    let before = Account::find_by_id(&f.db, f.accounts[1].id)
        .await
        .unwrap()
        .unwrap();
    f.mock.mode(&f.models[0], 200);
    expect(
        f.admin(
            Method::POST,
            &format!("/api/v1/accounts/{}/refresh", before.id),
            None,
        )
        .await,
        StatusCode::CONFLICT,
    );
    let after = Account::find_by_id(&f.db, before.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.models_supported, before.models_supported);
    assert_eq!(
        after.upstream_config_version,
        before.upstream_config_version
    );
    let body = f.body(0);
    expect(f.chat(PT, body.clone()).await, StatusCode::OK);
    assert_eq!(f.mock.calls_for(&body).len(), 1);
    assert_eq!(f.mock.calls_for(&body)[0].channel, "A");
    f.finish().await;
}

#[tokio::test]
async fn account_owned_local_warmups_obey_revocation_but_root_warmups_survive() {
    let mut f = Fixture::new().await;
    let owned = format!("resp_local_owned_{}", Uuid::new_v4().simple());
    let root = format!("resp_local_root_{}", Uuid::new_v4().simple());
    for (id, account_id) in [(&owned, Some(f.accounts[0].id)), (&root, None)] {
        keycompute_db::ResponseAffinity::upsert_local(&f.db,f.user.tenant_id,id,"openai",account_id,
            json!({"id":id,"object":"response","model":f.models[0],"status":"completed","output":[],"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}),
            json!({"upstream_previous_response_id":account_id.map(|_|"resp_parent"),"items":[{"id":"msg_root","type":"message","role":"user","content":[{"type":"input_text","text":"warmup"}]}]}),
            512,Utc::now()+ChronoDuration::hours(1)).await.unwrap();
    }
    // Control: account-backed local resources were accessible before revocation.
    expect(
        api(
            f.app.clone(),
            Method::GET,
            &format!("/v1/responses/{owned}"),
            Some(&f.key),
            None,
        )
        .await,
        StatusCode::OK,
    );
    f.bind(false).await;
    let before = f.mock.calls.lock().unwrap().len();
    for (method, path) in [
        (Method::GET, format!("/v1/responses/{owned}")),
        (Method::GET, format!("/v1/responses/{owned}?stream=true")),
        (Method::GET, format!("/v1/responses/{owned}/input_items")),
        (Method::POST, format!("/v1/responses/{owned}/cancel")),
        (Method::DELETE, format!("/v1/responses/{owned}")),
    ] {
        expect(
            api(f.app.clone(), method, &path, Some(&f.key), None).await,
            StatusCode::NOT_FOUND,
        );
    }
    expect(
        api(
            f.app.clone(),
            Method::POST,
            "/v1/responses",
            Some(&f.key),
            Some(json!({"model":f.models[0],"input":"continue","previous_response_id":owned})),
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    assert!(
        keycompute_db::ResponseAffinity::find_active_local_response(
            &f.db,
            f.user.tenant_id,
            &owned
        )
        .await
        .unwrap()
        .is_some(),
        "denied delete must not mutate the local resource"
    );
    expect(
        api(
            f.app.clone(),
            Method::GET,
            &format!("/v1/responses/{root}"),
            Some(&f.key),
            None,
        )
        .await,
        StatusCode::OK,
    );
    expect(
        api(
            f.app.clone(),
            Method::GET,
            &format!("/v1/responses/{root}/input_items"),
            Some(&f.key),
            None,
        )
        .await,
        StatusCode::OK,
    );
    expect(
        api(
            f.app.clone(),
            Method::DELETE,
            &format!("/v1/responses/{root}"),
            Some(&f.key),
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(
        f.mock.calls.lock().unwrap().len(),
        before,
        "local resources must never dispatch an upstream HTTP request"
    );
    f.finish().await;
}
