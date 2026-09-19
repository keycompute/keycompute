use client_api::{ApiClient, ClientConfig};
use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn client() -> (ApiClient, MockServer) {
    let server = MockServer::start().await;
    let client = ApiClient::new(
        ClientConfig::new(server.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap();
    (client, server)
}

#[tokio::test]
async fn concurrent_console_reads_deduplicate_and_session_switch_isolated() {
    let (client, server) = client().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/usage/stats"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(50))
                .set_body_json(json!({"requests": 3})),
        )
        .expect(2)
        .mount(&server)
        .await;

    let calls = (0..8).map(|_| {
        let client = client.clone();
        async move {
            client
                .get_json::<serde_json::Value>("/api/v1/usage/stats", Some("first-session"))
                .await
                .unwrap()
        }
    });
    let results = futures::future::join_all(calls).await;
    assert!(results.iter().all(|value| value == &json!({"requests": 3})));

    client.set_token("second-session");
    let _: serde_json::Value = client.get_json("/api/v1/usage/stats", None).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn invalidation_fences_an_inflight_display_response() {
    let (client, server) = client().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/payments/balance"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"balance": "10.00"})),
        )
        .mount(&server)
        .await;

    let pending = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .get_json::<serde_json::Value>("/api/v1/payments/balance", Some("session"))
                .await
        })
    };
    wait_for_request(&server, "GET", "/api/v1/payments/balance").await;
    client.invalidate_console_reads();
    let result = pending.await.unwrap();
    assert!(
        result.is_err(),
        "invalidated results must not reach display callers"
    );
}

#[test]
fn session_generation_cas_rejects_late_refresh_and_logout() {
    let client = ApiClient::new(ClientConfig::new("https://example.test")).unwrap();
    let (_, initial_generation) = client.session_snapshot();
    client.set_token("old-session");
    let (old_token, old_generation) = client.session_snapshot();
    assert_eq!(old_token.as_deref(), Some("old-session"));
    assert!(old_generation > initial_generation);

    client.set_token("new-session");
    assert!(!client.compare_and_set_session_token(
        Some("old-session"),
        old_generation,
        "late-refresh"
    ));
    assert_eq!(client.get_token().as_deref(), Some("new-session"));
    assert!(!client.clear_token_if_current(Some("old-session"), old_generation));
    assert!(client.clear_token_if_current(Some("new-session"), client.session_generation()));
    assert!(!client.is_authenticated());
}

async fn wait_for_request(server: &MockServer, method: &str, path: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .any(|r| r.method.as_str() == method && r.url.path() == path)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("mock server must observe the request before the state transition");
}

#[tokio::test]
async fn sdk_defaults_to_fresh_reads_and_opt_in_has_a_fresh_escape_hatch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/payments/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"balance":"10"})))
        .expect(4)
        .mount(&server)
        .await;
    let sdk = ApiClient::new(ClientConfig::new(server.uri()).with_no_proxy(true)).unwrap();
    for _ in 0..2 {
        let _: serde_json::Value = sdk
            .get_json("/api/v1/payments/balance", Some("A"))
            .await
            .unwrap();
    }
    let ui = ApiClient::new(
        ClientConfig::new(server.uri())
            .with_no_proxy(true)
            .with_console_display_cache(true),
    )
    .unwrap();
    for _ in 0..2 {
        let _: serde_json::Value = ui
            .get_json("/api/v1/payments/balance", Some("A"))
            .await
            .unwrap();
    }
    let _: serde_json::Value = ui
        .get_json_fresh("/api/v1/payments/balance", Some("A"))
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn different_rust_views_of_one_wire_response_share_a_request() {
    #[derive(serde::Deserialize)]
    struct Counts {
        count: u64,
    }
    #[derive(serde::Deserialize)]
    struct Amount {
        amount: String,
    }
    let (client, server) = client().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/usage/stats"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(40))
                .set_body_json(json!({"count":4,"amount":"1.000001"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (count, amount) = tokio::join!(
        client.get_json::<Counts>("/api/v1/usage/stats", Some("A")),
        client.get_json::<Amount>("/api/v1/usage/stats", Some("A"))
    );
    assert_eq!(count.unwrap().count, 4);
    assert_eq!(amount.unwrap().amount, "1.000001");
    server.verify().await;
}

#[tokio::test]
async fn cancelled_command_invalidates_reads_started_while_it_was_pending() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let (client, server) = client().await;
    let gets = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/api/v1/payments/balance"))
        .respond_with({
            let gets = gets.clone();
            move |_: &wiremock::Request| {
                let value = gets.fetch_add(1, Ordering::SeqCst) + 1;
                ResponseTemplate::new(200).set_body_json(json!({"value":value}))
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/payments/orders"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(3))
                .set_body_json(json!({"ok":true})),
        )
        .mount(&server)
        .await;
    let first: serde_json::Value = client
        .get_json("/api/v1/payments/balance", Some("A"))
        .await
        .unwrap();
    assert_eq!(first["value"], 1);
    let command = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .post_json::<serde_json::Value, _>("/api/v1/payments/orders", &json!({}), Some("A"))
                .await
        })
    };
    wait_for_request(&server, "POST", "/api/v1/payments/orders").await;
    let during: serde_json::Value = client
        .get_json("/api/v1/payments/balance", Some("A"))
        .await
        .unwrap();
    assert_eq!(during["value"], 2);
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    let after: serde_json::Value = client
        .get_json("/api/v1/payments/balance", Some("A"))
        .await
        .unwrap();
    assert_eq!(
        after["value"], 3,
        "RAII invalidation must execute when command future is dropped"
    );
}

#[tokio::test]
async fn dropping_one_waiter_does_not_abort_a_shared_read() {
    let (client, server) = client().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/usage/stats"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(200))
                .set_body_json(json!({"ok":true})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let first = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .get_json::<serde_json::Value>("/api/v1/usage/stats", Some("A"))
                .await
        })
    };
    wait_for_request(&server, "GET", "/api/v1/usage/stats").await;
    let second = client.get_json::<serde_json::Value>("/api/v1/usage/stats", Some("A"));
    tokio::pin!(second);
    assert!(futures::poll!(&mut second).is_pending());
    first.abort();
    let result = second.await.unwrap();
    assert_eq!(result["ok"], true);
    server.verify().await;
}
