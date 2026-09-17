//! Resource safety is separate from authorization and billable rate limiting.
use crate::{ApiError, extractors::AuthExtractor, state::AppState};
use axum::{body::Body, response::Response};
use keycompute_config::gateway::GenerationAdmissionConfig;
use keycompute_runtime::admission::{AdmissionLimits, AdmissionPermit, BoundedAdmission};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

#[derive(Debug)]
pub struct GenerationAdmission {
    /// Bounds authentication/body/pre-dispatch work before tenant identity is known.
    pub ingress: Arc<BoundedAdmission>,
    pub requests: Arc<BoundedAdmission>,
    pub accounts: Arc<BoundedAdmission>,
}
impl GenerationAdmission {
    pub fn new(config: &GenerationAdmissionConfig) -> Result<Self, ApiError> {
        config
            .validate()
            .map_err(|message| ApiError::Config(message.to_string()))?;
        let make = |per_key, queue_per_key| {
            BoundedAdmission::new(AdmissionLimits {
                total: config.global_limit,
                per_key,
                queue: config.global_queue,
                queue_per_key,
                wait: Duration::from_millis(config.queue_timeout_ms),
            })
            .map_err(|message| ApiError::Config(message.to_string()))
        };
        Ok(Self {
            ingress: make(config.global_limit, config.global_queue)?,
            requests: make(config.tenant_limit, config.tenant_queue)?,
            accounts: make(config.account_limit, config.account_queue)?,
        })
    }
}

/// Runs outside credential lookup. This budget covers request setup only;
/// the tenant/global execution permit then follows the response and workers.
pub async fn ingress_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::{http::Method, response::IntoResponse};
    let generation = request.method() == Method::POST
        && matches!(
            request.uri().path(),
            "/v1/chat/completions"
                | "/v1/messages"
                | "/v1/responses"
                | "/v1/responses/compact"
                | "/v1/responses/input_tokens"
        );
    if !generation {
        return next.run(request).await;
    }
    let Ok(_permit) = state
        .generation_admission
        .ingress
        .acquire(uuid::Uuid::nil())
        .await
    else {
        let mut response =
            ApiError::ServiceUnavailable("Generation ingress capacity is exhausted".into())
                .into_response();
        response
            .headers_mut()
            .insert("retry-after", axum::http::HeaderValue::from_static("1"));
        return response;
    };
    next.run(request).await
}

pub async fn ensure_generation(state: &AppState, auth: &mut AuthExtractor) -> Result<(), ApiError> {
    if auth.generation_permit.is_none() {
        let permit = state.generation_admission.requests.acquire(auth.tenant_id).await
            .map_err(|error| {
                tracing::debug!(tenant_id=%auth.tenant_id, %error, "generation resource admission rejected");
                ApiError::ServiceUnavailable("Generation capacity is exhausted. Please retry later.".to_string())
            })?;
        auth.generation_permit = Some(permit);
    }
    Ok(())
}

pub fn bind_context(auth: &AuthExtractor, ctx: &mut keycompute_types::RequestContext) {
    ctx.resource_guard = auth
        .generation_permit
        .clone()
        .map(|permit| Arc::new(permit) as Arc<dyn std::any::Any + Send + Sync>);
}

/// Preserve frames/trailers and retain admission through actual body delivery,
/// not merely until the handler returns its HTTP response headers.
struct AdmittedBody {
    inner: Body,
    permit: Option<AdmissionPermit>,
}
impl http_body::Body for AdmittedBody {
    type Data = bytes::Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let polled = Pin::new(&mut self.inner).poll_frame(cx);
        if matches!(polled, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self.permit = None;
        }
        polled
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}
pub fn retain_response(response: Response, permit: Option<AdmissionPermit>) -> Response {
    let (parts, inner) = response.into_parts();
    Response::from_parts(parts, Body::new(AdmittedBody { inner, permit }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::Request, middleware::from_fn_with_state, routing::post};
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use tower::ServiceExt;
    use uuid::Uuid;
    fn state() -> AppState {
        let mut config = crate::AppStateConfig::default();
        config.gateway.admission.global_limit = 1;
        config.gateway.admission.tenant_limit = 1;
        config.gateway.admission.account_limit = 1;
        config.gateway.admission.global_queue = 0;
        config.gateway.admission.tenant_queue = 0;
        config.gateway.admission.account_queue = 0;
        AppState::with_config(config)
    }
    #[tokio::test]
    async fn generation_admission_holds_slot_through_body_delivery_and_context_clones() {
        let state = state();
        let mut auth = AuthExtractor::new(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), "user");
        ensure_generation(&state, &mut auth).await.unwrap();
        ensure_generation(&state, &mut auth).await.unwrap(); // no double admission
        let response = retain_response(
            Response::new(Body::from("hello")),
            auth.generation_permit.take(),
        );
        assert_eq!(state.generation_admission.requests.status().active, 1);
        let (head, body) = response.into_parts();
        drop(head);
        assert_eq!(state.generation_admission.requests.status().active, 1);
        drop(body);
        assert_eq!(state.generation_admission.requests.status().active, 0);
        ensure_generation(&state, &mut auth).await.unwrap();
        let mut ctx = keycompute_types::RequestContext::new(
            Uuid::new_v4(),
            auth.user_id,
            auth.tenant_id,
            auth.produce_ai_key_id,
            "model",
            vec![],
            true,
            keycompute_types::PricingSnapshot::default(),
        );
        bind_context(&auth, &mut ctx);
        let settlement = ctx.clone_without_request_payloads();
        drop((auth, ctx));
        assert_eq!(state.generation_admission.requests.status().active, 1);
        drop(settlement);
        assert_eq!(state.generation_admission.requests.status().active, 0);
    }
    #[tokio::test]
    async fn generation_admission_rejects_before_reading_body_or_entering_handler() {
        let state = state();
        let held = state
            .generation_admission
            .requests
            .acquire(Uuid::new_v4())
            .await
            .unwrap();
        let polls = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(move || {
                    c.fetch_add(1, Ordering::Relaxed);
                    async { "should not run" }
                }),
            )
            .layer(from_fn_with_state(
                state.clone(),
                crate::middleware::generation_http_body_admission_middleware,
            ));
        let p = polls.clone();
        let body = Body::from_stream(futures::stream::poll_fn(move |_| {
            p.fetch_add(1, Ordering::Relaxed);
            Poll::<Option<Result<bytes::Bytes, Infallible>>>::Pending
        }));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .extension(AuthExtractor::new(
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        Uuid::new_v4(),
                        "user",
                    ))
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(polls.load(Ordering::Relaxed), 0);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(state.generation_admission.requests.status().active, 1);
        drop(held);
    }
    #[tokio::test]
    async fn generation_ingress_bounds_work_before_authentication() {
        let state = state();
        let held = state
            .generation_admission
            .ingress
            .acquire(Uuid::nil())
            .await
            .unwrap();
        let app = crate::create_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("authorization", "Bearer invalid-token")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            503,
            "must reject before credential lookup returns 401"
        );
        assert_eq!(state.generation_admission.requests.status().active, 0);
        drop(held);
    }
}
