use super::*;
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
};
use futures::{StreamExt, stream};
use tokio::sync::oneshot;
use uuid::Uuid;
struct Running {
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<crate::Result<()>>>,
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl Running {
    async fn start(state: AppState, router: Router<AppState>, budget: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = router
            .route("/ready", get(readiness))
            .route("/health", get(|| async { "alive" }))
            .layer(from_fn_with_state(state.clone(), middleware))
            .with_state(state.clone());
        let (stop, signal) = oneshot::channel();
        let task = tokio::spawn(serve_router_with_shutdown(
            listener,
            router,
            state,
            async {
                let _ = signal.await;
            },
            budget,
        ));
        Self {
            address,
            stop: Some(stop),
            task: Some(task),
        }
    }
    fn signal(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
    }
    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }
    async fn finish(mut self) -> crate::Result<()> {
        tokio::time::timeout(Duration::from_secs(4), self.task.as_mut().unwrap())
            .await
            .expect("server exceeded bounded drain")
            .unwrap()
    }
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("drain state did not converge");
}
fn chunk(text: &'static str) -> Result<bytes::Bytes, io::Error> {
    Ok(bytes::Bytes::from_static(text.as_bytes()))
}
#[tokio::test]
async fn graceful_drain_keeps_new_node_completion_connections_live_until_stream_finishes() {
    let state = AppState::new();
    let completed = Arc::new(Notify::new());
    let gate = completed.clone();
    let completion = completed.clone();
    let router = Router::new()
        .route(
            "/work",
            post(move |State(state): State<AppState>| {
                let gate = gate.clone();
                async move {
                    let permit = state
                        .generation_admission
                        .requests
                        .acquire(Uuid::new_v4())
                        .await
                        .unwrap();
                    let stream = stream::once(async { chunk("data: first\n\n") }).chain(
                        stream::once(async move {
                            gate.notified().await;
                            chunk("data: [DONE]\n\n")
                        }),
                    );
                    crate::admission::retain_response(
                        Response::new(Body::from_stream(stream)),
                        Some(permit),
                    )
                }
            }),
        )
        .route(
            "/node/v1/tasks/{id}/complete",
            post(move || {
                let signal = completion.clone();
                async move {
                    signal.notify_one();
                    "accepted"
                }
            }),
        );
    let mut running = Running::start(state.clone(), router, Duration::from_secs(2)).await;
    let http = client();
    assert_eq!(
        http.get(running.url("/ready"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut stream = http
        .post(running.url("/work"))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    assert_eq!(stream.next().await.unwrap().unwrap(), "data: first\n\n");
    running.signal();
    state.shutdown.draining().await;
    assert_eq!(
        http.get(running.url("/ready"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let rejected = http.post(running.url("/work")).send().await.unwrap();
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers()["retry-after"], "1");
    assert_eq!(
        http.post(running.url(&format!("/node/v1/tasks/{}/complete", Uuid::new_v4())))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        output.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(output, b"data: [DONE]\n\n");
    running.finish().await.unwrap();
    assert!(!state.shutdown.is_forced());
    until(|| state.shutdown.snapshot().open_transports == 0).await;
    assert_eq!(state.shutdown.snapshot().http_inflight, 0);
    assert_eq!(state.generation_admission.requests.status().active, 0);
}
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[tokio::test]
async fn graceful_deadline_cancels_a_pending_body_and_transport() {
    let state = AppState::new();
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = dropped.clone();
    let router = Router::new().route(
        "/work",
        post(move || {
            let marker = marker.clone();
            async move {
                let stream =
                    stream::once(async { chunk("first") }).chain(stream::once(async move {
                        let _drop = Dropped(marker);
                        std::future::pending::<Result<bytes::Bytes, io::Error>>().await
                    }));
                Response::new(Body::from_stream(stream))
            }
        }),
    );
    let mut running = Running::start(state.clone(), router, Duration::from_millis(100)).await;
    let mut body = client()
        .post(running.url("/work"))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    assert_eq!(body.next().await.unwrap().unwrap(), "first");
    running.signal();
    assert!(running.finish().await.is_err());
    assert!(state.shutdown.is_forced());
    assert!(body.next().await.is_none_or(|value| value.is_err()));
    until(|| dropped.load(Ordering::SeqCst) && state.shutdown.snapshot().open_transports == 0)
        .await;
    assert_eq!(state.shutdown.snapshot().http_inflight, 0);
}
#[tokio::test]
async fn graceful_deadline_cancels_handler_before_response_headers() {
    let state = AppState::new();
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let signal = entered.clone();
    let marker = dropped.clone();
    let router = Router::new().route(
        "/work",
        post(move || {
            let signal = signal.clone();
            let marker = marker.clone();
            async move {
                let _drop = Dropped(marker);
                signal.notify_one();
                std::future::pending::<Response>().await
            }
        }),
    );
    let mut running = Running::start(state.clone(), router, Duration::from_millis(100)).await;
    let url = running.url("/work");
    let request = tokio::spawn(async move { client().post(url).send().await });
    entered.notified().await;
    running.signal();
    assert!(running.finish().await.is_err());
    let _ = request.await;
    until(|| dropped.load(Ordering::SeqCst) && state.shutdown.snapshot().http_inflight == 0).await;
}
#[tokio::test]
async fn graceful_drain_waits_for_detached_context_capacity() {
    let state = AppState::new();
    let permit = state
        .generation_admission
        .requests
        .acquire(Uuid::new_v4())
        .await
        .unwrap();
    let mut running = Running::start(state.clone(), Router::new(), Duration::from_secs(2)).await;
    running.signal();
    state.shutdown.draining().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!running.task.as_ref().unwrap().is_finished());
    drop(permit);
    running.finish().await.unwrap();
    assert!(!state.shutdown.is_forced());
}
#[tokio::test]
async fn cancelled_serving_future_closes_its_spawned_connection_drivers() {
    let state = AppState::new();
    let router = Router::new().route(
        "/work",
        post(|| async {
            Response::new(Body::from_stream(
                stream::once(async { chunk("first") })
                    .chain(stream::pending::<Result<bytes::Bytes, io::Error>>()),
            ))
        }),
    );
    let mut running = Running::start(state.clone(), router, Duration::from_secs(2)).await;
    let mut response = client()
        .post(running.url("/work"))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    assert_eq!(response.next().await.unwrap().unwrap(), "first");
    let task = running.task.take().unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(state.shutdown.is_forced());
    let _ = response.next().await;
    until(|| {
        state.shutdown.snapshot().open_transports == 0
            && state.shutdown.snapshot().http_inflight == 0
    })
    .await;
}
#[test]
fn drain_exception_is_exact_and_cannot_admit_new_node_tasks() {
    let id = Uuid::new_v4();
    assert!(completion_during_drain(
        &Method::POST,
        &format!("/node/v1/tasks/{id}/complete")
    ));
    assert!(completion_during_drain(&Method::POST, "/node/v1/heartbeat"));
    for path in [
        "/node/v1/register",
        "/node/v1/tasks/poll",
        "/node/v1/tasks/not-a-uuid/complete",
        "/node/v1/tasks/a/b/complete",
    ] {
        assert!(!completion_during_drain(&Method::POST, path));
    }
    assert!(!completion_during_drain(&Method::GET, "/node/v1/heartbeat"));
}
#[tokio::test]
async fn draining_does_not_bypass_node_completion_authentication() {
    use tower::ServiceExt;
    let state = AppState::new();
    state.begin_draining();
    let request = axum::http::Request::builder()
        .method(Method::POST)
        .uri(format!("/node/v1/tasks/{}/complete", Uuid::new_v4()))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let response = crate::create_router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn invalid_budget_does_not_start_serving() {
    let state = AppState::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    assert!(
        serve_router_with_shutdown(
            listener,
            Router::new(),
            state.clone(),
            std::future::pending(),
            Duration::ZERO
        )
        .await
        .is_err()
    );
    assert!(!state.shutdown.is_draining());
    assert_eq!(state.shutdown.snapshot().open_transports, 0);
}
#[tokio::test]
async fn drain_diagnostics_remain_available_only_through_existing_admin_auth() {
    use tower::ServiceExt;
    let state = AppState::new();
    state.begin_draining();
    assert!(completion_during_drain(
        &Method::GET,
        "/api/v1/admin/monitoring/capacity"
    ));
    assert!(!completion_during_drain(
        &Method::POST,
        "/api/v1/admin/monitoring/capacity"
    ));
    let request = axum::http::Request::builder()
        .uri("/api/v1/admin/monitoring/capacity")
        .body(Body::empty())
        .unwrap();
    let response = crate::create_router(state).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
