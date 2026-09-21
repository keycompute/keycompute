//! Actual-router ownership regressions against an isolated DB and loopback upstream.
//! No production identities, credentials, or provider endpoints are used.
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{Duration as ChronoDuration, Utc};
use futures::{SinkExt, StreamExt};
use integration_tests::{
    common::generate_test_id,
    db::{TenantActor, TestDataGuard, create_test_pool, create_test_tenant, create_test_user},
};
use keycompute_auth::ProduceAiKeyValidator;
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, DbRouter, ProduceAiKey,
    ResponseAffinity, UserBalance,
};
use keycompute_server::{AppState, create_router};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Once,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tower::ServiceExt;

#[derive(Default)]
struct Upstream {
    calls: Mutex<Vec<(String, HeaderMap, Value)>>,
    responses: Mutex<HashMap<String, Value>>,
    sequence: AtomicUsize,
}
async fn create_upstream(
    State(state): State<Arc<Upstream>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    let n = state.sequence.fetch_add(1, Ordering::SeqCst);
    let id = format!("resp_fixture_{n}");
    let conversation = body
        .get("conversation")
        .cloned()
        .unwrap_or_else(|| json!({"id":format!("conv_fixture_{n}")}));
    let value = json!({
        "id": id, "object":"response", "created_at":1, "model":body["model"],
        "status":"completed", "error":null, "conversation":conversation,
        "output":[{"id":format!("msg_{n}"),"type":"message","role":"assistant",
            "status":"completed","content":[{"type":"output_text","text":"private result","annotations":[]}]}],
        "usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7}
    });
    state
        .calls
        .lock()
        .unwrap()
        .push(("create".into(), headers, body));
    state.responses.lock().unwrap().insert(id, value.clone());
    Json(value)
}
async fn resource_upstream(
    State(state): State<Arc<Upstream>>,
    Path(id): Path<String>,
    method: Method,
) -> Response {
    state
        .calls
        .lock()
        .unwrap()
        .push((format!("{method}/{id}"), HeaderMap::new(), Value::Null));
    if method == Method::DELETE {
        state.responses.lock().unwrap().remove(&id);
        return Json(json!({"id":id,"object":"response","deleted":true})).into_response();
    }
    match state.responses.lock().unwrap().get(&id).cloned() {
        Some(value) => Json(value).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":{"message":"not found"}})),
        )
            .into_response(),
    }
}
async fn unexpected_upstream(
    State(state): State<Arc<Upstream>>,
    method: Method,
    uri: axum::http::Uri,
) -> Response {
    state.calls.lock().unwrap().push((
        format!("unexpected/{method}/{uri}"),
        HeaderMap::new(),
        Value::Null,
    ));
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":{"message":"unexpected upstream request"}})),
    )
        .into_response()
}
async fn key(db: &DatabaseConnection, user: &TenantActor) -> String {
    let raw = ProduceAiKeyValidator::generate_key();
    ProduceAiKey::create(
        db,
        &CreateProduceAiKeyRequest {
            tenant_id: user.tenant_id,
            user_id: user.id,
            name: "ownership fixture".into(),
            produce_ai_key_hash: ProduceAiKeyValidator::hash_key(&raw),
            produce_ai_key_preview: "sk-test****".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    raw
}
async fn jwt(state: &AppState, user: &TenantActor) -> String {
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
struct Fixture {
    run_id: String,
    db: DatabaseConnection,
    cleanup: TestDataGuard,
    state: AppState,
    owner: TenantActor,
    peer: TenantActor,
    key: String,
    rotated: String,
    peer_key: String,
    foreign_key: String,
    model: String,
    account: Account,
    upstream: Arc<Upstream>,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("warn")
            .with_test_writer()
            .try_init();
        static CRYPTO: Once = Once::new();
        CRYPTO.call_once(|| {
            keycompute_runtime::set_global_crypto(&keycompute_runtime::ApiKeyCrypto::generate_key())
                .unwrap()
        });
        let db = create_test_pool().await;
        let run = generate_test_id();
        let cleanup = TestDataGuard::new(db.clone(), run.clone());
        let tenant = create_test_tenant(&db, "response-owner", &run).await;
        let foreign_tenant = create_test_tenant(&db, "response-foreign", &run).await;
        // This suite tests authorization, not conservative opaque-context quotas.
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET default_rpm_limit=1000,default_tpm_limit=10000000 WHERE id=$1",
            [tenant.id.into()],
        ))
        .await
        .unwrap();
        let owner = create_test_user(&db, tenant.id, "response-owner", &run).await;
        let peer = create_test_user(&db, tenant.id, "response-peer", &run).await;
        let foreign = create_test_user(&db, foreign_tenant.id, "response-foreign", &run).await;
        for user in [&owner, &peer] {
            UserBalance::recharge(
                &db,
                user.tenant_id,
                user.id,
                Decimal::from(1000),
                None,
                Some("isolated ownership test credit"),
            )
            .await
            .unwrap();
        }
        let upstream = Arc::new(Upstream::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/responses", post(create_upstream))
            .route(
                "/v1/responses/{id}",
                get(resource_upstream).delete(resource_upstream),
            )
            .fallback(unexpected_upstream)
            .with_state(upstream.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let model = format!("ownership-model-{run}");
        let account = Account::create(
            &db,
            &CreateAccountRequest {
                tenant_id: tenant.id,
                provider: "openai".into(),
                name: format!("ownership-{run}"),
                endpoint: format!("http://{address}/v1"),
                upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(
                    "fixture-upstream-secret",
                )
                .unwrap()
                .into_inner(),
                upstream_api_key_preview: "fixture****".into(),
                rpm_limit: Some(1000),
                tpm_limit: Some(10_000_000),
                priority: Some(0),
                models_supported: vec![model.clone()],
                api_capabilities: vec!["responses".into()],
                pool_enabled: Some(true),
                visibility: Some("tenant".into()),
            },
        )
        .await
        .unwrap();
        let key = key(&db, &owner).await;
        let rotated = crate::key(&db, &owner).await;
        let peer_key = crate::key(&db, &peer).await;
        let foreign_key = crate::key(&db, &foreign).await;
        let state = AppState::with_pool(DbRouter::single(db.clone()));
        Self {
            run_id: run,
            db,
            cleanup,
            state,
            owner,
            peer,
            key,
            rotated,
            peer_key,
            foreign_key,
            model,
            account,
            upstream,
            tasks: vec![task],
        }
    }
    fn body(&self) -> Value {
        json!({"model":self.model,"input":"private input","max_output_tokens":16})
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        token: &str,
        body: Option<Value>,
        idem: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json");
        if let Some(idem) = idem {
            request = request.header("idempotency-key", idem);
        }
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            create_router(self.state.clone()).oneshot(
                request
                    .body(
                        body.map(|v| Body::from(v.to_string()))
                            .unwrap_or_else(Body::empty),
                    )
                    .unwrap(),
            ),
        )
        .await
        .expect("router timeout")
        .unwrap();
        let status = response.status();
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            to_bytes(response.into_body(), 2 << 20),
        )
        .await
        .expect("body timeout")
        .unwrap();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, body)
    }
    fn calls(&self) -> usize {
        self.upstream.calls.lock().unwrap().len()
    }
    async fn ws_url(&mut self) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = create_router(self.state.clone());
        self.tasks.push(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        format!("ws://{addr}/v1/responses")
    }
    async fn finish(&mut self) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "DELETE FROM response_affinities WHERE tenant_id=$1",
                [self.owner.tenant_id.into()],
            ))
            .await
            .unwrap();
        self.cleanup.cleanup().await.unwrap();
    }
}
#[track_caller]
fn expect(result: (StatusCode, Value), status: StatusCode) -> Value {
    assert_eq!(result.0, status, "{}", result.1);
    result.1
}

#[tokio::test]
async fn pool_http_resources_and_idempotency_are_private_to_the_stable_user() {
    let mut f = Fixture::new().await;
    let body = f.body();
    let first = expect(
        f.request(
            Method::POST,
            "/v1/responses",
            &f.key,
            Some(body.clone()),
            Some("identical-client-key"),
        )
        .await,
        StatusCode::OK,
    );
    let id = first["id"].as_str().unwrap();
    let conv = first["conversation"]["id"].as_str().unwrap();
    assert_eq!(f.calls(), 1);
    let replay = expect(
        f.request(
            Method::POST,
            "/v1/responses",
            &f.rotated,
            Some(body.clone()),
            Some("identical-client-key"),
        )
        .await,
        StatusCode::OK,
    );
    assert_eq!(replay["id"], first["id"]);
    assert_eq!(
        f.calls(),
        1,
        "key rotation must not repeat accepted inference"
    );
    let second = expect(
        f.request(
            Method::POST,
            "/v1/responses",
            &f.peer_key,
            Some(body),
            Some("identical-client-key"),
        )
        .await,
        StatusCode::OK,
    );
    assert_ne!(first["id"], second["id"]);
    assert_eq!(
        f.calls(),
        2,
        "different users have independent idempotency keys"
    );
    {
        let calls = f.upstream.calls.lock().unwrap();
        assert_ne!(calls[0].1["idempotency-key"], calls[1].1["idempotency-key"]);
    }
    let owned = ResponseAffinity::find_active_for_user(&f.db, f.owner.tenant_id, f.owner.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owned.user_id, Some(f.owner.id));
    let admin = jwt(&f.state, &f.peer).await;
    for token in [&f.peer_key, &f.foreign_key, &admin] {
        let before = f.calls();
        for (method, path) in [
            (Method::GET, format!("/v1/responses/{id}")),
            (Method::GET, format!("/v1/responses/{id}?stream=true")),
            (Method::GET, format!("/v1/responses/{id}/input_items")),
            (Method::POST, format!("/v1/responses/{id}/cancel")),
            (Method::DELETE, format!("/v1/responses/{id}")),
        ] {
            expect(
                f.request(method, &path, token, None, None).await,
                StatusCode::NOT_FOUND,
            );
        }
        for reference in [
            json!({"previous_response_id":id}),
            json!({"conversation":conv}),
        ] {
            let mut continuation = f.body();
            continuation
                .as_object_mut()
                .unwrap()
                .extend(reference.as_object().unwrap().clone());
            for path in ["/v1/responses", "/v1/responses/input_tokens"] {
                expect(
                    f.request(Method::POST, path, token, Some(continuation.clone()), None)
                        .await,
                    StatusCode::NOT_FOUND,
                );
            }
        }
        assert_eq!(
            f.calls(),
            before,
            "denied requests must not touch the shared upstream"
        );
    }
    let before = f.calls();
    let mut unknown = f.body();
    unknown["conversation"] = json!("conv_not_registered");
    expect(
        f.request(Method::POST, "/v1/responses", &f.key, Some(unknown), None)
            .await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(
        f.calls(),
        before,
        "unknown conversations must not trigger discovery"
    );
    expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}"),
            &f.rotated,
            None,
            None,
        )
        .await,
        StatusCode::OK,
    );
    for prefix in ["/pt", "/nt"] {
        expect(
            f.request(
                Method::GET,
                &format!("{prefix}/v1/responses/{id}"),
                &f.rotated,
                None,
                None,
            )
            .await,
            StatusCode::NOT_FOUND,
        );
    }
    expect(
        f.request(
            Method::DELETE,
            &format!("/v1/responses/{id}"),
            &f.rotated,
            None,
            None,
        )
        .await,
        StatusCode::OK,
    );
    expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}"),
            &f.key,
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    f.finish().await;
}

async fn ws_create(url: &str, token: &str, body: Value) -> Value {
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .expect("websocket handshake timeout")
    .unwrap();
    socket
        .send(Message::Text(body.to_string().into()))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(message) = socket.next().await {
            match message.unwrap() {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if matches!(
                        value["type"].as_str(),
                        Some("response.completed" | "error" | "response.failed")
                    ) {
                        return value;
                    }
                }
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.unwrap(),
                _ => {}
            }
        }
        panic!("websocket closed before terminal event")
    })
    .await
    .expect("websocket terminal timeout");
    let _ = socket.close(None).await;
    event
}

#[tokio::test]
async fn websocket_warmups_retain_user_ownership_across_connections_and_keys() {
    let mut f = Fixture::new().await;
    let url = f.ws_url().await;
    let mut body = f.body();
    body["type"] = json!("response.create");
    body["generate"] = json!(false);
    body["store"] = json!(true);
    let first = ws_create(&url, &f.key, body.clone()).await;
    assert_eq!(first["type"], "response.completed", "{first}");
    let id = first["response"]["id"].as_str().unwrap();
    assert_eq!(f.calls(), 0, "root warmups do not need upstream inference");
    let row = ResponseAffinity::find_active_for_user(&f.db, f.owner.tenant_id, f.owner.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.user_id, Some(f.owner.id));
    expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}"),
            &f.rotated,
            None,
            None,
        )
        .await,
        StatusCode::OK,
    );
    let items = expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}/input_items"),
            &f.rotated,
            None,
            None,
        )
        .await,
        StatusCode::OK,
    );
    assert!(items.to_string().contains("private input"));
    for token in [&f.peer_key, &f.foreign_key] {
        for (method, path) in [
            (Method::GET, format!("/v1/responses/{id}")),
            (Method::GET, format!("/v1/responses/{id}/input_items")),
            (Method::DELETE, format!("/v1/responses/{id}")),
            (Method::POST, format!("/v1/responses/{id}/cancel")),
        ] {
            expect(
                f.request(method, &path, token, None, None).await,
                StatusCode::NOT_FOUND,
            );
        }
    }
    body["previous_response_id"] = json!(id);
    let denied = ws_create(&url, &f.peer_key, body.clone()).await;
    assert_eq!(denied["type"], "error", "{denied}");
    assert!(!denied.to_string().contains("private input"));
    let continued = ws_create(&url, &f.rotated, body).await;
    assert_eq!(continued["type"], "response.completed", "{continued}");
    assert_ne!(continued["response"]["id"], id);
    assert_eq!(f.calls(), 0);
    expect(
        f.request(
            Method::DELETE,
            &format!("/v1/responses/{id}"),
            &f.rotated,
            None,
            None,
        )
        .await,
        StatusCode::OK,
    );
    expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}"),
            &f.key,
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    f.finish().await;
}

#[tokio::test]
async fn immutable_ownership_prevents_takeover_and_preserves_internal_settlements() {
    let mut f = Fixture::new().await;
    let id = "resp_owned_pending";
    let expiry = Utc::now() + ChronoDuration::hours(1);
    ResponseAffinity::upsert_route_with_settlement_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        id,
        "openai",
        Some(&f.model),
        f.account.id,
        expiry,
        json!({"test_pending":true}),
        Utc::now(),
    )
    .await
    .unwrap();
    let collision = ResponseAffinity::upsert_route_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        f.peer.id,
        id,
        "openai",
        Some(&f.model),
        f.account.id,
        expiry,
    )
    .await;
    assert!(
        collision.is_err(),
        "same-tenant user cannot rebind an existing opaque resource"
    );
    expect(
        f.request(
            Method::DELETE,
            &format!("/v1/responses/{id}"),
            &f.peer_key,
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    expect(
        f.request(
            Method::DELETE,
            &format!("/v1/responses/{id}"),
            &f.key,
            None,
            None,
        )
        .await,
        StatusCode::CONFLICT,
    );
    // A user moving does not move previously accepted resources or billing work.
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='revoked' WHERE tenant_id=$2 AND user_id=$1",
        [f.owner.id.into(), f.owner.tenant_id.into()],
    ))
    .await
    .unwrap();
    assert!(f.state.auth.verify_token(&f.key).await.is_err());
    let destination = create_test_tenant(&f.db, "response-moved", &f.run_id).await;
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO tenant_memberships(tenant_id,user_id,role,status) VALUES($1,$2,'member','active')",
        [destination.id.into(), f.owner.id.into()],
    )).await.unwrap();
    let mut moved = f.owner.clone();
    moved.tenant_id = destination.id;
    let moved_token = jwt(&f.state, &moved).await;
    expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}"),
            &moved_token,
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tenant_memberships SET status='revoked' WHERE tenant_id=$1 AND user_id=$2",
        [f.owner.tenant_id.into(), f.owner.id.into()],
    ))
    .await
    .unwrap();
    let pending = ResponseAffinity::find_active_for_user(&f.db, f.owner.tenant_id, f.owner.id, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.user_id, Some(f.owner.id));
    assert!(pending.settlement.is_some());
    // Delayed completion can still create its hidden outbox after user removal.
    ResponseAffinity::upsert_hidden_settlement_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        Some(f.owner.id),
        "resp_removed_owner_outbox",
        "openai",
        Some(&f.model),
        Some(f.account.id),
        expiry,
        json!({"test_pending":true}),
        Utc::now(),
    )
    .await
    .unwrap();
    expect(
        f.request(
            Method::GET,
            "/v1/responses/resp_removed_owner_outbox",
            &f.peer_key,
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.calls(), 0);
    f.finish().await;
}

// These positive continuation scenarios use separate fixtures because an opaque
// upstream history reserves the account's whole TPM budget. No limiter is reset
// or weakened; the fixture simply starts with an existing, owner-bound resource.
async fn owned_continuation(reference: &str, resource_id: &str) {
    let mut f = Fixture::new().await;
    ResponseAffinity::upsert_route_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        resource_id,
        "openai",
        Some(&f.model),
        f.account.id,
        Utc::now() + ChronoDuration::hours(1),
    )
    .await
    .unwrap();
    let mut body = f.body();
    body[reference] = json!(resource_id);
    let response = expect(
        f.request(Method::POST, "/v1/responses", &f.rotated, Some(body), None)
            .await,
        StatusCode::OK,
    );
    assert_eq!(f.calls(), 1);
    let result = ResponseAffinity::find_active_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        response["id"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.user_id, Some(f.owner.id));
    assert_eq!(
        f.upstream.calls.lock().unwrap()[0].2[reference],
        resource_id
    );
    f.finish().await;
}
#[tokio::test]
async fn owner_can_continue_a_previous_response_with_another_key() {
    owned_continuation("previous_response_id", "resp_preexisting_owner").await;
}
#[tokio::test]
async fn owner_can_continue_a_registered_conversation_with_another_key() {
    owned_continuation("conversation", "conv_preexisting_owner").await;
}

#[tokio::test]
async fn owner_scoped_late_settlement_never_resurrects_a_deleted_resource() {
    let mut f = Fixture::new().await;
    let id = "resp_owner_deleted_before_settlement";
    let expiry = Utc::now() + ChronoDuration::hours(1);
    ResponseAffinity::upsert_route_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        id,
        "openai",
        Some(&f.model),
        f.account.id,
        expiry,
    )
    .await
    .unwrap();
    let db = DbRouter::single(f.db.clone());
    assert_eq!(
        ResponseAffinity::delete_route_preserving_settlement_for_user(
            &db,
            f.owner.tenant_id,
            f.owner.id,
            id,
        )
        .await
        .unwrap(),
        1
    );
    let saved = ResponseAffinity::upsert_route_with_settlement_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        id,
        "openai",
        Some(&f.model),
        f.account.id,
        expiry,
        json!({"test_pending": true}),
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(
        saved.deleted_at.is_some(),
        "late billing must preserve the deletion tombstone"
    );
    assert!(
        saved.settlement.is_some(),
        "settlement still has to be retained"
    );
    let refreshed = ResponseAffinity::upsert_route_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        id,
        "openai",
        Some(&f.model),
        f.account.id,
        expiry,
    )
    .await
    .unwrap();
    assert!(
        refreshed.deleted_at.is_some(),
        "late route refresh must retain the tombstone"
    );
    let hidden = ResponseAffinity::upsert_hidden_settlement_in_tx_for_user(
        &f.db,
        f.owner.tenant_id,
        Some(f.owner.id),
        id,
        "openai",
        Some(&f.model),
        Some(f.account.id),
        expiry,
        json!({"test_pending": true}),
        Utc::now(),
    )
    .await
    .unwrap();
    assert!(hidden.deleted_at.is_some());
    let rewrite = ResponseAffinity::upsert_local_for_user(
        &f.db,
        f.owner.tenant_id,
        f.owner.id,
        id,
        "openai",
        Some(f.account.id),
        json!({"id":id,"model":f.model}),
        json!({"items":[]}),
        128,
        expiry,
    )
    .await;
    assert!(
        rewrite.is_err(),
        "late local state must not rewrite erased resource contents"
    );
    assert!(
        ResponseAffinity::find_active_for_user(&f.db, f.owner.tenant_id, f.owner.id, id,)
            .await
            .unwrap()
            .is_none()
    );
    expect(
        f.request(
            Method::GET,
            &format!("/v1/responses/{id}"),
            &f.key,
            None,
            None,
        )
        .await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(
        f.calls(),
        0,
        "a deleted resource must never be proxied upstream"
    );
    f.finish().await;
}

async fn redis_backed_state(db: &DatabaseConnection) -> AppState {
    use keycompute_server::state::{AppStateConfig, RateLimitBackendConfig};
    let url = std::env::var("REDIS_URL")
        .expect("REDIS_URL must explicitly identify an isolated test Redis");
    let state = AppState::try_with_pool_and_config(
        DbRouter::single(db.clone()),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(keycompute_config::RedisConfig {
                url,
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        state.runtime_state.is_available(),
        "this regression requires real Redis"
    );
    state
}

#[tokio::test]
async fn independent_instances_never_authorize_resources_from_stale_redis_routes() {
    let mut f = Fixture::new().await;
    f.state = redis_backed_state(&f.db).await;
    let first = expect(
        f.request(Method::POST, "/v1/responses", &f.key, Some(f.body()), None)
            .await,
        StatusCode::OK,
    );
    let id = first["id"].as_str().unwrap();
    let path = format!("/v1/responses/{id}");
    let cache_key = format!(
        "responses:affinity:{}:{}:{id}",
        f.owner.tenant_id, f.owner.id
    );
    let original: Value = f
        .state
        .runtime_state
        .get(&cache_key)
        .await
        .unwrap()
        .expect("the production create path must really write its Redis route");
    assert_eq!(original["user_id"], f.owner.id.to_string());
    let primary = f.state.clone();
    f.state = redis_backed_state(&f.db).await;
    expect(
        f.request(Method::GET, &path, &f.rotated, None, None).await,
        StatusCode::OK,
    );
    let replica = f.state.clone();

    // Deliberately inconsistent test-only cache content must never override
    // the authoritative owner in PostgreSQL, even on another server instance.
    let other_key = format!(
        "responses:affinity:{}:{}:{id}",
        f.owner.tenant_id, f.peer.id
    );
    let mut stale = original.clone();
    stale["user_id"] = json!(f.peer.id);
    primary
        .runtime_state
        .set(&other_key, &stale, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(
        replica
            .runtime_state
            .get::<Value>(&other_key)
            .await
            .unwrap(),
        Some(stale)
    );
    let before = f.calls();
    expect(
        f.request(Method::GET, &path, &f.peer_key, None, None).await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(f.calls(), before);

    f.state = primary;
    expect(
        f.request(Method::DELETE, &path, &f.key, None, None).await,
        StatusCode::OK,
    );
    f.state
        .runtime_state
        .set(&cache_key, &original, Duration::from_secs(60))
        .await
        .unwrap();
    f.state = replica;
    assert_eq!(
        f.state
            .runtime_state
            .get::<Value>(&cache_key)
            .await
            .unwrap(),
        Some(original)
    );
    let before = f.calls();
    expect(
        f.request(Method::GET, &path, &f.rotated, None, None).await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(
        f.calls(),
        before,
        "neither stale L1 nor L2 may undo database deletion"
    );
    f.state.runtime_state.delete(&cache_key).await.unwrap();
    f.state.runtime_state.delete(&other_key).await.unwrap();
    f.finish().await;
}
