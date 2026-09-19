//! Opt-in, open-loop HTTP load profile. This is NOT a mock-only component test.
//! Requires an explicitly isolated local database and marked test Redis.
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
};
use futures::StreamExt;
use integration_tests::db::{
    TestDataGuard, create_test_tenant, create_test_user, initialize_test_schema,
};
use keycompute_billing::balance::BalanceService;
use keycompute_db::{
    Account, CreateAccountRequest, CreateProduceAiKeyRequest, DbRouter, ProduceAiKey,
};
use keycompute_server::{AppState, AppStateConfig, create_router, state::RateLimitBackendConfig};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{task::JoinSet, time::Instant};
use uuid::Uuid;

#[derive(Clone)]
struct Mock {
    completed: Arc<AtomicU64>,
    stream_ms: u64,
}
async fn upstream(State(mock): State<Mock>, Json(body): Json<Value>) -> Response {
    let id = format!("chatcmpl-{}", Uuid::new_v4());
    if body.get("stream").and_then(Value::as_bool) != Some(true) {
        mock.completed.fetch_add(1, Ordering::Relaxed);
        return Json(json!({"id":id,"object":"chat.completion","created":1,"model":"gpt-4o",
            "choices":[{"index":0,"message":{"role":"assistant","content":"Hello from the load fixture."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":8,"completion_tokens":8,"total_tokens":16}})).into_response();
    }
    let stream = futures::stream::unfold((0_u8, mock, id), |(n, mock, id)| async move {
        if n > 11 {
            return None;
        }
        let data = if n < 10 {
            tokio::time::sleep(Duration::from_millis(mock.stream_ms / 10)).await;
            format!(
                "data: {}\n\n",
                json!({"id":id,"object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{"content":"hello "},"finish_reason":null}]})
            )
        } else if n == 10 {
            format!(
                "data: {}\n\n",
                json!({"id":id,"object":"chat.completion.chunk","created":1,"model":"gpt-4o",
                "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":8,"completion_tokens":8,"total_tokens":16}})
            )
        } else {
            mock.completed.fetch_add(1, Ordering::Relaxed);
            "data: [DONE]\n\n".to_string()
        };
        Some((
            Ok::<_, std::convert::Infallible>(bytes::Bytes::from(data)),
            (n + 1, mock, id),
        ))
    });
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}
struct Server {
    address: std::net::SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl Server {
    async fn start(app: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, signal) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = signal.await;
                })
                .await
                .unwrap();
        });
        Self {
            address,
            stop: Some(stop),
            task: Some(task),
        }
    }
    async fn close(mut self) {
        let _ = self.stop.take().unwrap().send(());
        if let Some(mut task) = self.task.take() {
            match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
                Ok(result) => result.expect("load server task failed"),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    panic!("load server did not drain");
                }
            }
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
#[derive(Debug, Serialize)]
struct Sample {
    status: u16,
    complete: bool,
    header_ms: f64,
    first_content_ms: Option<f64>,
    elapsed_ms: f64,
    schedule_lag_ms: f64,
}
async fn request(
    client: reqwest::Client,
    url: String,
    key: String,
    stream: bool,
    lag: f64,
) -> Sample {
    let start = Instant::now();
    let mut sample = Sample {
        status: 0,
        complete: false,
        header_ms: 0.0,
        first_content_ms: None,
        elapsed_ms: 0.0,
        schedule_lag_ms: lag,
    };
    let response=client.post(url).bearer_auth(key).json(&json!({"model":"gpt-4o","messages":[{"role":"user","content":"Hello"}],"max_tokens":32,"stream":stream,"stream_options":if stream {json!({"include_usage":true})}else{Value::Null}})).send().await;
    let Ok(response) = response else {
        sample.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return sample;
    };
    sample.status = response.status().as_u16();
    sample.header_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut chunks = response.bytes_stream();
    let mut pending = Vec::new();
    let mut total = 0_usize;
    let mut transport_ok = true;
    let mut usage_seen = false;
    let mut protocol_error = false;
    while let Some(chunk) = chunks.next().await {
        let Ok(chunk) = chunk else {
            transport_ok = false;
            break;
        };
        total += chunk.len();
        if total > 4 * 1024 * 1024 {
            transport_ok = false;
            break;
        }
        pending.extend_from_slice(&chunk);
        if stream && sample.status == 200 {
            while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<_> = pending.drain(..=index).collect();
                let text = String::from_utf8_lossy(&line);
                if let Some(data) = text.trim().strip_prefix("data:") {
                    let data = data.trim();
                    if data == "[DONE]" {
                        sample.complete = true;
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(data) {
                        usage_seen |= value.pointer("/usage/total_tokens").and_then(Value::as_u64)
                            == Some(16);
                        protocol_error |= value.get("error").is_some();
                        if value
                            .pointer("/choices/0/delta/content")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                            && sample.first_content_ms.is_none()
                        {
                            sample.first_content_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
            }
        }
    }
    if stream {
        sample.complete &=
            transport_ok && usage_seen && !protocol_error && sample.first_content_ms.is_some();
    }
    if !stream
        && transport_ok
        && sample.status == 200
        && let Ok(body) = serde_json::from_slice::<Value>(&pending)
    {
        sample.complete = body
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .is_some()
            && body.pointer("/usage/total_tokens").and_then(Value::as_u64) == Some(16);
        if sample.complete {
            sample.first_content_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    sample.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
    sample
}
fn setting(name: &str, default: u64, min: u64, max: u64) -> u64 {
    let value = std::env::var(name)
        .map(|s| s.parse::<u64>().expect("load setting must be an integer"))
        .unwrap_or(default);
    assert!((min..=max).contains(&value), "{name} is out of range");
    value
}
fn percentiles(mut values: Vec<f64>) -> Value {
    values.sort_by(f64::total_cmp);
    if values.is_empty() {
        return Value::Null;
    }
    let p = |q: f64| values[((values.len() - 1) as f64 * q).ceil() as usize];
    json!({"p50":p(0.5),"p95":p(0.95),"p99":p(0.99),"max":values[values.len()-1]})
}
fn rss_bytes() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|v| v * 1024)
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in sustained load; requires an isolated local kc_load_* database and a marked Redis"]
async fn sustained_real_http_database_redis_and_billing_profile() {
    assert_eq!(
        std::env::var("KC_LOAD_ACK_ISOLATED").as_deref(),
        Ok("1"),
        "explicit test-environment acknowledgement required"
    );
    let database_url = integration_tests::common::resolve_database_url();
    let parsed = url::Url::parse(&database_url).unwrap();
    assert!(
        matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            && parsed.path().starts_with("/kc_load_"),
        "load profiling requires a dedicated loopback kc_load_* database"
    );
    let redis_url = integration_tests::common::resolve_redis_url();
    let redis_endpoint = url::Url::parse(&redis_url).unwrap();
    assert!(
        matches!(
            redis_endpoint.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]")
        ),
        "load Redis must be local"
    );
    let redis_client = deadpool_redis::redis::Client::open(redis_url.clone()).unwrap();
    let mut redis = tokio::time::timeout(
        Duration::from_secs(3),
        redis_client.get_multiplexed_async_connection(),
    )
    .await
    .unwrap()
    .unwrap();
    redis.set_response_timeout(Duration::from_secs(3));
    let marker: Option<String> = deadpool_redis::redis::cmd("GET")
        .arg("keycompute:test:load-instance")
        .query_async(&mut redis)
        .await
        .unwrap();
    assert_eq!(
        marker.as_deref(),
        Some("disposable-load-v1"),
        "Redis must be explicitly marked for load tests, never production"
    );
    let seconds = setting("KC_LOAD_SECONDS", 10, 1, 120);
    let rate = setting("KC_LOAD_RATE", 20, 1, 1000);
    let tenants = setting("KC_LOAD_TENANTS", 4, 1, 32);
    let users = setting("KC_LOAD_USERS_PER_TENANT", 1, 1, 16);
    let accounts = setting("KC_LOAD_ACCOUNTS", 8, 1, 128);
    let stream_ms = setting("KC_LOAD_STREAM_MS", 1000, 10, 10_000);
    let writer_connections = setting("KC_LOAD_DB_CONNECTIONS", 10, 2, 64);
    let mode = std::env::var("KC_LOAD_MODE").unwrap_or_else(|_| "json".into());
    assert!(mode == "json" || mode == "sse");
    let stream = mode == "sse";
    let planned = seconds * rate;
    assert!(planned <= 100_000);
    let mut options = ConnectOptions::new(database_url);
    options
        .max_connections(writer_connections as u32)
        .min_connections(2)
        .acquire_timeout(Duration::from_secs(10))
        .sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    initialize_test_schema(&db).await.unwrap();
    let run = Uuid::new_v4().to_string();
    let mut cleanup = TestDataGuard::new(db.clone(), &run);
    keycompute_runtime::set_global_crypto(&keycompute_runtime::ApiKeyCrypto::generate_key())
        .unwrap();
    let mock = Mock {
        completed: Arc::new(AtomicU64::new(0)),
        stream_ms,
    };
    let upstream = Server::start(
        Router::new()
            .route("/v1/chat/completions", post(upstream))
            .with_state(mock.clone()),
    )
    .await;
    let balance = BalanceService::new(DbRouter::single(db.clone()));
    let mut tenant_ids = Vec::new();
    let mut keys = Vec::new();
    for t in 0..tenants {
        let tenant = create_test_tenant(&db, &format!("load-{t}"), &run).await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE tenants SET default_rpm_limit=1000000,default_tpm_limit=1000000000 WHERE id=$1",
            [tenant.id.into()],
        ))
        .await
        .unwrap();
        tenant_ids.push(tenant.id);
        for u in 0..users {
            let user = create_test_user(&db, tenant.id, &format!("load-{t}-{u}"), &run).await;
            balance
                .recharge(
                    user.id,
                    tenant.id,
                    rust_decimal::Decimal::from(100000),
                    None,
                    None,
                )
                .await
                .unwrap();
            let key = format!(
                "sk-{}{}",
                Uuid::new_v4().simple(),
                &Uuid::new_v4().simple().to_string()[..16]
            );
            ProduceAiKey::create(
                &db,
                &CreateProduceAiKeyRequest {
                    tenant_id: tenant.id,
                    user_id: user.id,
                    name: "load fixture".into(),
                    produce_ai_key_hash: hex::encode(Sha256::digest(key.as_bytes())),
                    produce_ai_key_preview: "sk-load***".into(),
                    expires_at: None,
                },
            )
            .await
            .unwrap();
            keys.push(key);
        }
    }
    for n in 0..accounts {
        Account::create(
            &db,
            &CreateAccountRequest {
                tenant_id: tenant_ids[0],
                provider: "openai".into(),
                name: format!("load-{n}"),
                endpoint: format!("http://{}/v1", upstream.address),
                upstream_api_key_encrypted: keycompute_runtime::encrypt_api_key(
                    "test-upstream-only",
                )
                .unwrap()
                .into_inner(),
                upstream_api_key_preview: "test***".into(),
                rpm_limit: Some(1000000),
                tpm_limit: Some(1000000000),
                priority: Some(0),
                models_supported: vec!["gpt-4o".into()],
                api_capabilities: vec!["chat_completions".into()],
                pool_enabled: None,
                visibility: Some("global".into()),
            },
        )
        .await
        .unwrap();
    }
    let state = AppState::try_with_pool_and_config(
        DbRouter::single(db.clone()),
        AppStateConfig {
            rate_limit: RateLimitBackendConfig::Redis(keycompute_config::RedisConfig {
                url: redis_url,
                cache_url: std::env::var("CACHE_REDIS_URL").ok(),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let server = Server::start(create_router(state.clone())).await;
    let url = format!("http://{}/v1/chat/completions", server.address);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    for key in &keys {
        let sample = request(client.clone(), url.clone(), key.clone(), stream, 0.0).await;
        assert!(
            sample.status == 200 && sample.complete,
            "warmup failed: {sample:?}"
        );
    }
    let before = keycompute_server::handlers::admin_capacity::process_snapshot(&state);
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    let mut samples = Vec::new();
    let client_slots = Arc::new(tokio::sync::Semaphore::new(512));
    let mut generator_drops = 0_u64;
    let mut rss_peak = rss_bytes().unwrap_or(0);
    for n in 0..planned {
        let scheduled = started + Duration::from_secs_f64(n as f64 / rate as f64);
        tokio::time::sleep_until(scheduled).await;
        let lag = Instant::now()
            .saturating_duration_since(scheduled)
            .as_secs_f64()
            * 1000.0;
        if let Ok(permit) = client_slots.clone().try_acquire_owned() {
            let client = client.clone();
            let url = url.clone();
            let key = keys[n as usize % keys.len()].clone();
            tasks.spawn(async move {
                let _permit = permit;
                request(client, url, key, stream, lag).await
            });
        } else {
            generator_drops += 1;
        }
        while let Some(sample) = tasks.try_join_next() {
            samples.push(sample.unwrap());
        }
        rss_peak = rss_peak.max(rss_bytes().unwrap_or(0));
    }
    while let Some(sample) = tasks.join_next().await {
        samples.push(sample.unwrap());
        rss_peak = rss_peak.max(rss_bytes().unwrap_or(0));
    }
    // The denominator includes the complete offered-load window even when the
    // last short request finishes before its final scheduling interval elapses.
    tokio::time::sleep_until(started + Duration::from_secs(seconds)).await;
    let elapsed = started.elapsed().as_secs_f64();
    let mut ledger = 0_i64;
    let mut active = 0_i64;
    let mut settled = 0_i64;
    let drain=tokio::time::timeout(Duration::from_secs(30),async {
        loop {
            let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT (SELECT COUNT(*) FROM usage_logs WHERE tenant_id=ANY($1))::BIGINT AS ledger, (SELECT COUNT(*) FROM balance_reservations WHERE tenant_id=ANY($1) AND status='active')::BIGINT AS active, (SELECT COUNT(*) FROM balance_reservations WHERE tenant_id=ANY($1) AND status='settled')::BIGINT AS settled",
                [tenant_ids.clone().into()])).await.unwrap().unwrap();
            ledger=row.try_get("","ledger").unwrap();active=row.try_get("","active").unwrap();settled=row.try_get("","settled").unwrap();
            if state.generation_admission.requests.status().active==0 && active==0 && ledger==mock.completed.load(Ordering::Relaxed) as i64 && settled==ledger {break;}
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await;
    let success = samples
        .iter()
        .filter(|s| s.status == 200 && s.complete)
        .count();
    let mut statuses = BTreeMap::new();
    for s in &samples {
        *statuses.entry(s.status).or_insert(0_u64) += 1;
    }
    let good = samples
        .iter()
        .filter(|s| s.status == 200 && s.complete)
        .collect::<Vec<_>>();
    let monetary = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::BIGINT AS inconsistent FROM user_balances WHERE tenant_id=ANY($1) AND (available_balance+frozen_balance+total_consumed<>total_recharged OR frozen_balance<>0)",
        [tenant_ids.clone().into()])).await.unwrap().unwrap().try_get::<i64>("","inconsistent").unwrap();
    let report = json!({"schema":"keycompute-load-v1","generated_at":chrono::Utc::now(),"profile":if cfg!(debug_assertions){"debug"}else{"release-size-optimized"},
        "scope":"real TCP gateway + PostgreSQL + Redis + billing; local synthetic model; no TLS or Nginx; client and gateway share one process",
        "settings":{"seconds":seconds,"offered_rps":rate,"tenants":tenants,"users_per_tenant":users,"accounts":accounts,"mode":mode,"stream_ms":stream_ms,"writer_connections":writer_connections,"distributed_cache_configured":state.cache.is_available()},
        "planned":planned,"sent":samples.len(),"generator_dropped":generator_drops,"status_counts":statuses,"completed_success":success,
        "successful_rps_including_tail":success as f64/elapsed,"elapsed_seconds":elapsed,
        "success_header_ms":percentiles(good.iter().map(|s|s.header_ms).collect()),"success_first_content_ms":percentiles(good.iter().filter_map(|s|s.first_content_ms).collect()),
        "success_complete_ms":percentiles(good.iter().map(|s|s.elapsed_ms).collect()),"schedule_lag_ms":percentiles(samples.iter().map(|s|s.schedule_lag_ms).collect()),
        "rss_observed_peak_bytes":rss_peak,"rss_scope":"shared harness/gateway process, sampled; not a proven server-only RSS ceiling",
        "upstream_completed_including_warmup":mock.completed.load(Ordering::Relaxed),"ledger_including_warmup":ledger,"active_balance_reservations":active,"settled_balance_reservations":settled,"drained":drain.is_ok(),"inconsistent_fixture_balances":monetary,
        "before":before,"after":keycompute_server::handlers::admin_capacity::process_snapshot(&state)});
    println!("KC_LOAD_REPORT={report}");
    if let Ok(path) = std::env::var("KC_LOAD_REPORT") {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("report path must be new");
        file.write_all(serde_json::to_string_pretty(&report).unwrap().as_bytes())
            .unwrap();
    }
    server.close().await;
    upstream.close().await;
    cleanup.cleanup().await.unwrap();
    assert!(drain.is_ok(), "billing/resource drain did not reconcile");
    assert_eq!(monetary, 0, "fixture balance conservation failed");
    assert_eq!(
        generator_drops, 0,
        "load generator saturated; profile is not valid"
    );
    assert!(
        success as f64 / planned as f64 >= 0.99,
        "offered rate exceeds this run's successful service capacity; report retained"
    );
}

#[tokio::test]
async fn load_reader_does_not_count_done_only_or_protocol_error_as_success() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            (
                [("content-type", "text/event-stream")],
                "data: {\"error\":{\"message\":\"failed\"}}\n\ndata: [DONE]\n\n",
            )
        }),
    );
    let server = Server::start(app).await;
    let sample = request(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        format!("http://{}/v1/chat/completions", server.address),
        "test-reader-only".into(),
        true,
        0.0,
    )
    .await;
    assert_eq!(sample.status, 200);
    assert!(!sample.complete);
    assert!(sample.first_content_ms.is_none());
    server.close().await;
}
