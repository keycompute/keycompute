use client_api::error::ClientError;
use client_api::{ApiClient, ClientConfig};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

async fn client() -> (ApiClient, MockServer) {
    let server = MockServer::start().await;
    let client = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    (client, server)
}

#[tokio::test]
async fn quota_returns_immediately_and_shared_cooldown_suppresses_followers() {
    let (client, server) = client().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "60")
                .insert_header("x-ratelimit-scope", "authenticated")
                .set_body_json(json!({"error":{"message":"slow down"}})),
        )
        .mount(&server)
        .await;
    let started = Instant::now();
    let result: Result<Value, _> = client
        .get_json("/api/v1/payments/balance", Some("first"))
        .await;
    match result.unwrap_err() {
        ClientError::RateLimited(info) => {
            assert_eq!(info.retry_after, Some(Duration::from_secs(60)));
            assert_eq!(info.scope.as_deref(), Some("authenticated"));
            assert_eq!(info.message, "slow down");
        }
        error => panic!("unexpected error: {error}"),
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    let followers = (0..16).map(|_| {
        let client = client.clone();
        async move {
            client
                .get_json::<Value>("/api/v1/usage/stats", Some("first"))
                .await
        }
    });
    for result in futures::future::join_all(followers).await {
        assert!(result.unwrap_err().is_rate_limited());
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let _: Result<Value, _> = client
        .get_json("/api/v1/payments/balance", Some("other"))
        .await;
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[derive(Clone)]
struct Recovering {
    calls: Arc<AtomicUsize>,
}
impl Respond for Recovering {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(502).set_body_json(json!({"error":"transient"}))
        } else {
            ResponseTemplate::new(200).set_body_json(json!({"ok":true}))
        }
    }
}

#[tokio::test]
async fn mutations_do_not_retry_without_explicit_idempotency_contract() {
    let (client, server) = client().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error":"uncertain"})))
        .expect(1)
        .mount(&server)
        .await;
    let result: Result<Value, _> = client
        .post_json(
            "/api/v1/payments/orders",
            &json!({"amount":"1.00"}),
            Some("token"),
        )
        .await;
    assert!(matches!(result, Err(ClientError::ServerError(_))));
    server.verify().await;
}

#[tokio::test]
async fn safe_read_retries_and_idempotent_post_keeps_key_and_body() {
    let (client, server) = client().await;
    let read_calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/safe"))
        .respond_with(Recovering {
            calls: read_calls.clone(),
        })
        .expect(2)
        .mount(&server)
        .await;
    assert_eq!(
        client.get_json::<Value>("/safe", None).await.unwrap(),
        json!({"ok":true})
    );
    assert_eq!(read_calls.load(Ordering::SeqCst), 2);
    let write_calls = Arc::new(AtomicUsize::new(0));
    let body = json!({"amount":"2.00","reason":"test"});
    Mock::given(method("POST"))
        .and(path("/api/v1/users/test/balance"))
        .and(header("idempotency-key", "same-command"))
        .and(body_json(body.clone()))
        .respond_with(Recovering {
            calls: write_calls.clone(),
        })
        .expect(2)
        .mount(&server)
        .await;
    let value: Value = client
        .post_json_with_idempotency_key(
            "/api/v1/users/test/balance",
            &body,
            "same-command",
            Some("token"),
        )
        .await
        .unwrap();
    assert_eq!(value, json!({"ok":true}));
    assert_eq!(write_calls.load(Ordering::SeqCst), 2);
    server.verify().await;
}

#[tokio::test]
async fn request_deadline_bounds_all_retries_and_debug_redacts_authentication() {
    let server = MockServer::start().await;
    let client = ApiClient::new(
        ClientConfig::new(server.uri())
            .with_no_proxy(true)
            .with_timeout(1)
            .with_max_retries(100),
    )
    .unwrap();
    client.set_token("test-identity-not-for-logging");
    assert!(!format!("{client:?}").contains("test-identity-not-for-logging"));
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(3))
                .set_body_json(json!({})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let started = Instant::now();
    assert!(matches!(
        client.get_json::<Value>("/safe", None).await,
        Err(ClientError::Network(_))
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    server.verify().await;
}
