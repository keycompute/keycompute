//! One dedicated test binary configures a 128 MiB process payload budget.
//! Real WebSocket handshake + HTTP buffering + native SSE parsing share it.
//! No database, upstream credentials or production services are contacted.
use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::get,
};
use futures::StreamExt;
use http_body_util::BodyExt;
use keycompute_server::{AppState, AppStateConfig, extractors::AuthExtractor};
use keycompute_types::{CredentialKind, memory::process_memory_budget};
use serde_json::{Value, json};
use std::time::Duration;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn websocket_http_and_native_streams_share_bytes_and_release_after_cancellation() {
    let mut config = AppStateConfig::default();
    config.gateway.managed_memory_mib = 128;
    let state = AppState::with_config(config);
    let app = Router::new()
        .route(
            "/v1/responses",
            get(keycompute_server::handlers::responses_websocket).post(
                |Json(_body): Json<Value>| async { (StatusCode::OK, "retained").into_response() },
            ),
        )
        .layer(axum::extract::DefaultBodyLimit::max(80 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            keycompute_server::middleware::generation_http_body_admission_middleware,
        ))
        .layer(middleware::from_fn(
            |mut request: Request<Body>, next: middleware::Next| async move {
                request.extensions_mut().insert(AuthExtractor::new(
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    Uuid::new_v4(),
                    CredentialKind::Jwt,
                ));
                next.run(request).await
            },
        ))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let serving = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, serving)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://{address}/v1/responses"))
            .await
            .unwrap();
    assert!(process_memory_budget().status().used >= 80 * 1024 * 1024);
    let body = json!({"input":"x".repeat(6 * 1024 * 1024)}).to_string();
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("content-type", "application/json")
            .header("content-length", body.len())
            .body(Body::from(body.clone()))
            .unwrap()
    };
    let held_http = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(held_http.status(), StatusCode::OK);
    let event = json!({"id":"chatcmpl-memory", "object":"chat.completion.chunk", "created":1, "model":"test",
        "choices":[{"index":0,"delta":{"content":"y".repeat(2 * 1024 * 1024)},"finish_reason":null}]});
    let source = futures::stream::iter(vec![
        Ok(bytes::Bytes::from(format!("data: {event}\n\n"))),
        Ok(bytes::Bytes::from_static(b"data: [DONE]\n\n")),
    ]);
    let mut stream =
        llm_protocol_openai::stream::parse_native_openai_chat_stream(Box::pin(source), false);
    let held_event = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        held_event,
        llm_protocol_provider::StreamEvent::Native { .. }
    ));
    let rejected = app.clone().oneshot(request()).await.unwrap();
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(rejected);
    let status = process_memory_budget().status();
    assert!(status.used <= status.limit && status.peak <= status.limit);
    println!(
        "mixed_memory_limit={} used={} peak={}",
        status.limit, status.used, status.peak
    );
    drop(held_http);
    let recovered = app.oneshot(request()).await.unwrap();
    assert_eq!(recovered.status(), StatusCode::OK);
    let mut body = recovered.into_body();
    let frame = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let frame_copy = frame.clone();
    drop((body, held_event, stream, frame));
    websocket.close(None).await.unwrap();
    drop(websocket);
    tokio::time::timeout(Duration::from_secs(3), async {
        while process_memory_budget().status().used != frame_copy.len() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("payload memory leaked after all owners were cancelled/dropped");
    assert_eq!(frame_copy.as_ref(), b"retained");
    drop(frame_copy);
    assert_eq!(process_memory_budget().status().used, 0);
    stop.send(()).unwrap();
    server.await.unwrap();
}
