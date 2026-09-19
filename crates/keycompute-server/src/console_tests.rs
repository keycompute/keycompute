use super::*;
use crate::state::AppStateConfig;
use axum::{
    Router,
    body::Body,
    http::Request as HttpRequest,
    middleware::from_fn_with_state,
    routing::{get, post},
};
use tower::ServiceExt;

fn app(state: &AppState) -> Router {
    Router::new()
        .route("/api/v1/test/read", get(|| async { StatusCode::OK }))
        .route("/api/v1/test/stats", get(|| async { StatusCode::OK }))
        .route("/api/v1/test/write", post(|| async { StatusCode::OK }))
        .layer(from_fn_with_state(state.clone(), middleware))
}
fn state(config: ConsoleConfig) -> AppState {
    AppState::with_config(AppStateConfig {
        console: config,
        ..Default::default()
    })
}
fn request(path: &str, tenant: Uuid, user: Uuid, key: Uuid) -> HttpRequest<Body> {
    let mut req = HttpRequest::builder()
        .method(if path.ends_with("write") {
            "POST"
        } else {
            "GET"
        })
        .uri(path)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(AuthExtractor::new(user, tenant, key, "user"));
    req
}
#[tokio::test]
async fn aggregate_budget_cannot_be_bypassed_by_walking_classes_or_identities() {
    let s = state(ConsoleConfig {
        aggregate_rpm: 3,
        ..Default::default()
    });
    let a = app(&s);
    for path in [
        "/api/v1/test/read",
        "/api/v1/test/stats",
        "/api/v1/test/write",
    ] {
        assert_eq!(
            a.clone()
                .oneshot(request(
                    path,
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4()
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    let r = a
        .oneshot(request(
            "/api/v1/test/read",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert!(r.headers().contains_key("retry-after"));
}
#[tokio::test]
async fn rotating_api_keys_does_not_reset_user_budget_and_tenants_are_isolated() {
    let s = state(ConsoleConfig {
        user_rpm: 2,
        tenant_rpm: 3,
        ..Default::default()
    });
    let a = app(&s);
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    for p in ["/api/v1/test/read", "/api/v1/test/stats"] {
        assert_eq!(
            a.clone()
                .oneshot(request(p, t, u, Uuid::new_v4()))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        a.clone()
            .oneshot(request("/api/v1/test/write", t, u, Uuid::new_v4()))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        a.clone()
            .oneshot(request(
                "/api/v1/test/read",
                t,
                Uuid::new_v4(),
                Uuid::new_v4()
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        a.oneshot(request(
            "/api/v1/test/read",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4()
        ))
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn class_limit_is_independent_of_mutations_and_generation() {
    let s = state(ConsoleConfig {
        read_rpm: 1,
        ..Default::default()
    });
    let a = app(&s);
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    let k = Uuid::new_v4();
    assert_eq!(
        a.clone()
            .oneshot(request("/api/v1/test/read", t, u, k))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let r = a
        .clone()
        .oneshot(request("/api/v1/test/read", t, u, k))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers()["x-ratelimit-scope"], "console_read");
    assert_eq!(
        a.oneshot(request("/api/v1/test/write", t, u, k))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let rate = RateLimitKey::new(t, u, k);
    let cfg = RateLimitConfig::new(1, 1000);
    assert!(
        s.rate_limiter
            .check_and_record_with_config(&rate, &cfg)
            .await
            .is_ok()
    );
    assert!(
        s.rate_limiter
            .check_and_record_with_config(&rate, &cfg)
            .await
            .is_err()
    );
    assert!(
        !s.console_admission
            .metrics()
            .to_string()
            .contains(&u.to_string())
    );
}
#[tokio::test]
async fn read_exhaustion_cannot_hold_write_capacity_and_dropped_lease_releases_slot() {
    let s = state(ConsoleConfig {
        read_concurrency: 1,
        origin_concurrency: 1,
        queue_limit: 0,
        ..Default::default()
    });
    let a = app(&s);
    let permit = s.console_admission.read.acquire(Uuid::nil()).await.unwrap();
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    let r = a
        .clone()
        .oneshot(request("/api/v1/test/read", t, u, Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let independent_write = a
        .clone()
        .oneshot(request("/api/v1/test/write", t, u, Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(independent_write.status(), StatusCode::OK);
    drop(permit);
    let next_read = a
        .oneshot(request("/api/v1/test/read", t, u, Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(next_read.status(), StatusCode::OK);
    assert_eq!(s.console_admission.read.status().active, 0);
}
// These tests use only the in-process router and synthetic request extensions.
#[tokio::test]
async fn dropping_an_unfinished_router_future_releases_its_queue_ticket() {
    let s = state(ConsoleConfig {
        read_concurrency: 1,
        origin_concurrency: 1,
        queue_limit: 1,
        ..Default::default()
    });
    let permit = s.console_admission.read.acquire(Uuid::nil()).await.unwrap();
    let mut future = Box::pin(app(&s).oneshot(request(
        "/api/v1/test/read",
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::nil(),
    )));
    assert!(futures::poll!(&mut future).is_pending());
    assert_eq!(s.console_admission.read.status().queued, 1);
    drop(future);
    assert_eq!(s.console_admission.read.status().queued, 0);
    drop(permit);
    assert_eq!(s.console_admission.read.status().active, 0);
}
#[tokio::test]
async fn finite_queue_wait_returns_unavailable() {
    let s = state(ConsoleConfig {
        read_concurrency: 1,
        origin_concurrency: 1,
        queue_timeout_ms: 1,
        ..Default::default()
    });
    let permit = s.console_admission.read.acquire(Uuid::nil()).await.unwrap();
    let result = app(&s)
        .oneshot(request(
            "/api/v1/test/read",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::nil(),
        ))
        .await
        .unwrap();
    assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(s.console_admission.read.status().queued, 0);
    drop(permit);
}
#[cfg(feature = "redis")]
#[tokio::test]
async fn redis_parallel_requests_share_the_same_user_budget() {
    let url = std::env::var("REDIS_URL").expect("explicit isolated Redis URL required");
    let pool = deadpool_redis::Config::from_url(url)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .unwrap();
    let mut s = state(ConsoleConfig {
        user_rpm: 7,
        ..Default::default()
    });
    s.console_limiter = Arc::new(
        keycompute_ratelimit::RateLimitService::with_redis_pool_and_prefix(
            pool,
            format!("keycompute:console:test:{}", Uuid::new_v4()),
        ),
    );
    let a = app(&s);
    let t = Uuid::new_v4();
    let u = Uuid::new_v4();
    let calls = (0..16).map(|_| {
        let a = a.clone();
        async move {
            a.oneshot(request("/api/v1/test/read", t, u, Uuid::new_v4()))
                .await
                .unwrap()
                .status()
        }
    });
    let results = futures::future::join_all(calls).await;
    assert_eq!(results.iter().filter(|s| **s == StatusCode::OK).count(), 7);
    assert_eq!(
        results
            .iter()
            .filter(|s| **s == StatusCode::TOO_MANY_REQUESTS)
            .count(),
        9
    );
}
#[cfg(feature = "redis")]
#[tokio::test]
async fn unavailable_configured_console_redis_never_falls_back_to_memory() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = crate::state::AppStateConfig {
        rate_limit: crate::state::RateLimitBackendConfig::Redis(keycompute_config::RedisConfig {
            url: format!("redis://127.0.0.1:{port}"),
            connect_timeout_secs: 1,
            pool_wait_timeout_ms: 50,
            command_timeout_ms: 50,
            ..Default::default()
        }),
        ..Default::default()
    };
    let s = AppState::with_config(config);
    let r = app(&s)
        .oneshot(request(
            "/api/v1/test/read",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::nil(),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        s.console_limiter.backend(),
        keycompute_ratelimit::RateLimitBackend::Redis
    );
}

#[tokio::test]
async fn anonymous_commands_cannot_flush_authenticated_display_snapshots() {
    let s = state(ConsoleConfig::default());
    s.display_cache
        .read(
            s.cache.clone(),
            s.console_admission.origin.clone(),
            Uuid::nil(),
            "fence-probe".into(),
            async { Ok(serde_json::json!({"value": 7})) },
        )
        .await
        .unwrap();
    let denied = Router::new()
        .route(
            "/api/v1/test/write",
            post(|| async { StatusCode::UNAUTHORIZED }),
        )
        .layer(from_fn_with_state(s.clone(), middleware));
    let response = denied
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/test/write")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(s.display_cache.metrics()["entries"], 1);
    let cached = s
        .display_cache
        .read(
            s.cache.clone(),
            s.console_admission.origin.clone(),
            Uuid::nil(),
            "fence-probe".into(),
            async { panic!("anonymous request must not force an origin reload") },
        )
        .await
        .unwrap();
    assert_eq!(cached["value"], 7);
    // A quota-checked authenticated command still invalidates its display view.
    app(&s)
        .oneshot(request(
            "/api/v1/test/write",
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        ))
        .await
        .unwrap();
    assert_eq!(s.display_cache.metrics()["entries"], 0);
}
