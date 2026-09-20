//! Protocol-native scoped generation. Admission and settlement remain common;
//! only target selection and transport differ between account grants and nodes.
use crate::{
    error::{ApiError, Result},
    extractors::{AuthExtractor, ClientRequestId, RequestId, RequestReceivedAt},
    state::{AppState, GenerationHttpBodyPermit},
};
use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use keycompute_auth::Permission;
use keycompute_types::{
    AccountApiCapability, AccountModelHealthObserver, ClientResponseOutcome, ExecutionPlan,
    ExecutionTarget, ModelAccessMode, RequestContext, RequestLifecycleRecorder, RequestStatus,
    RequestTraceStart, RouteType,
    node::NodeTaskPayload,
    node_native::{NodeNativeHttpResult, NodeNativeOperation as Op, NodeNativeRequest},
};
use llm_protocol_provider::{LargeBodyPermit, NativeStreamEvent, StreamEvent};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;
macro_rules! endpoint {
    ($name:ident,$mode:ident,$op:ident) => {
        pub async fn $name(
            State(state): State<AppState>,
            auth: AuthExtractor,
            request_id: RequestId,
            client_request_id: ClientRequestId,
            received: RequestReceivedAt,
            headers: HeaderMap,
            (permit, Json(body)): (Option<Extension<GenerationHttpBodyPermit>>, Json<Value>),
        ) -> Result<Response> {
            generate(
                state,
                auth,
                request_id,
                client_request_id,
                received,
                headers,
                permit.map(|v| v.0),
                body,
                ModelAccessMode::$mode,
                Op::$op,
            )
            .await
        }
    };
}
endpoint!(passthrough_binding_messages, Passthrough, Messages);
endpoint!(passthrough_binding_responses, Passthrough, Responses);
endpoint!(node_dispatch_messages, NodeDispatch, Messages);
endpoint!(node_dispatch_responses, NodeDispatch, Responses);
fn capability(op: Op) -> AccountApiCapability {
    match op {
        Op::Chat => AccountApiCapability::ChatCompletions,
        Op::Messages => AccountApiCapability::Messages,
        Op::Responses => AccountApiCapability::Responses,
    }
}
fn request_path(mode: ModelAccessMode, op: Op) -> String {
    format!(
        "{}{}",
        if mode == ModelAccessMode::Passthrough {
            "/pt"
        } else {
            "/nt"
        },
        op.local_path()
    )
}
pub(crate) fn validate_managed_responses_body(body: &Value) -> Result<()> {
    super::responses::request::validate_responses_request(body)
}

fn validate_request(body: &Value, headers: &HeaderMap, op: Op) -> Result<()> {
    if !body.is_object() {
        return Err(ApiError::BadRequest("Request must be a JSON object".into()));
    }
    if !matches!(
        body.get("stream"),
        None | Some(Value::Null) | Some(Value::Bool(false)) | Some(Value::Bool(true))
    ) {
        return Err(ApiError::BadRequest(
            "stream must be a boolean or null".into(),
        ));
    }
    if body.get("model").and_then(Value::as_str).is_none_or(|m| {
        m.trim().is_empty() || m.chars().count() > 255 || m.chars().any(char::is_control)
    }) {
        return Err(ApiError::BadRequest("A valid model is required".into()));
    }
    match op {
        Op::Messages => {
            super::anthropic::validate_anthropic_headers(headers)?;
            let mut validation = body.clone();
            if validation.get("stream") == Some(&Value::Null) {
                validation.as_object_mut().unwrap().remove("stream");
            }
            let parsed: super::anthropic::AnthropicMessagesRequest =
                serde_json::from_value(validation)
                    .map_err(|_| ApiError::BadRequest("Invalid Messages request".into()))?;
            parsed.validate()?;
        }
        Op::Responses => {
            super::responses::request::validate_responses_request(body)?;
            if ["previous_response_id", "conversation"]
                .iter()
                .any(|name| body.get(name).is_some_and(|v| !v.is_null()))
                || body.get("background").and_then(Value::as_bool) == Some(true)
                || body.get("store").and_then(Value::as_bool) == Some(true)
            {
                return Err(ApiError::BadRequest(
                    "This endpoint currently supports stateless Responses with store=false".into(),
                ));
            }
        }
        Op::Chat => {
            return Err(ApiError::BadRequest(
                "Use the Chat Completions handler".into(),
            ));
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate(
    state: AppState,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received: RequestReceivedAt,
    headers: HeaderMap,
    body_permit: Option<GenerationHttpBodyPermit>,
    body: Value,
    mode: ModelAccessMode,
    op: Op,
) -> Result<Response> {
    if op == Op::Responses {
        return crate::scoped_state::create(
            state,
            auth,
            request_id,
            client_request_id,
            received,
            headers,
            body_permit,
            body,
            mode,
        )
        .await;
    }
    generate_with_state(
        state,
        auth,
        request_id,
        client_request_id,
        received,
        headers,
        body_permit,
        body,
        mode,
        op,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn generate_with_state(
    state: AppState,
    mut auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received: RequestReceivedAt,
    headers: HeaderMap,
    body_permit: Option<GenerationHttpBodyPermit>,
    body: Value,
    mode: ModelAccessMode,
    op: Op,
    managed: Option<Arc<crate::scoped_state::execution::ManagedExecution>>,
) -> Result<Response> {
    crate::admission::ensure_generation(&state, &mut auth).await?;
    if !auth.has_permission(&Permission::UseApi) {
        return Err(ApiError::Forbidden("API-use permission is required".into()));
    }
    validate_request(&body, &headers, op)?;
    let model = body["model"].as_str().expect("validated model").to_owned();
    let protocol_headers = if op == Op::Messages {
        super::anthropic::forwarded_anthropic_headers(&headers)
    } else {
        Default::default()
    };
    let native = (mode == ModelAccessMode::NodeDispatch).then(|| NodeNativeRequest {
        operation: op,
        body: body.clone(),
        headers: protocol_headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    });
    if let Some(native) = &native {
        native
            .validate(&model)
            .map_err(|e| ApiError::BadRequest(e.into()))?;
        if state.node_gateway.is_none() {
            return Err(ApiError::ServiceUnavailable(
                "Node task service is unavailable".into(),
            ));
        }
    }
    let lifecycle = Arc::clone(&state.lifecycle);
    lifecycle
        .start_request(RequestTraceStart {
            request_id: request_id.0,
            client_request_id: client_request_id.0,
            tenant_id: auth.tenant_id,
            user_id: auth.user_id,
            produce_ai_key_id: auth.produce_ai_key_id,
            protocol: op.protocol().into(),
            request_path: request_path(mode, op),
            requested_model: model.clone(),
            is_stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
            received_at: received.0,
        })
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Request tracing is unavailable".into()))?;
    let mut pre = super::PreExecutionTraceGuard::new(lifecycle.clone(), request_id.0);
    let provider = keycompute_pricing::resolve_pricing_provider(mode);
    let pricing = state
        .pricing
        .create_snapshot(&model, &auth.tenant_id, Some(provider))
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Pricing is temporarily unavailable".into()))?;
    let mut ctx = RequestContext::new(
        request_id.0,
        auth.user_id,
        auth.tenant_id,
        auth.produce_ai_key_id,
        model.clone(),
        vec![],
        body.get("stream").and_then(Value::as_bool).unwrap_or(false),
        pricing,
    );
    ctx.messages = match op {
        Op::Messages => {
            let mut projection = body.clone();
            if projection.get("stream") == Some(&Value::Null) {
                projection.as_object_mut().unwrap().remove("stream");
            }
            serde_json::from_value::<super::anthropic::AnthropicMessagesRequest>(projection)
                .map_err(|_| ApiError::BadRequest("Invalid Messages projection".into()))?
                .context_messages()
        }
        Op::Responses => super::responses::context_messages(&body),
        Op::Chat => vec![],
    };
    ctx.access_mode = mode;
    crate::admission::bind_context(&auth, &mut ctx);
    ctx.max_tokens = body
        .get(if op == Op::Responses {
            "max_output_tokens"
        } else {
            "max_tokens"
        })
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok());
    ctx.temperature = body
        .get("temperature")
        .and_then(Value::as_f64)
        .map(|v| v as f32);
    ctx.top_p = body.get("top_p").and_then(Value::as_f64).map(|v| v as f32);
    match op {
        Op::Messages => {
            ctx.native_anthropic_headers = protocol_headers;
            ctx.native_anthropic_request = Some(Arc::new(body));
        }
        Op::Responses => {
            ctx.native_openai_responses_request = Some(Arc::new(body));
            ctx.native_openai_responses_path = Some("/responses".into());
        }
        Op::Chat => unreachable!("validated scoped operation"),
    }
    let plan = if mode == ModelAccessMode::Passthrough {
        let (plan, selection, version) =
            crate::passthrough_binding::resolve_passthrough_binding_plan_for(
                &state,
                auth.tenant_id,
                &model,
                capability(op),
            )
            .await?;
        let validator = Arc::new(
            crate::passthrough_binding::DbPassthroughBindingValidator::for_capability(
                &state,
                capability(op),
            )?,
        );
        ctx.set_passthrough_binding(selection);
        ctx.set_passthrough_binding_account_config_version(version);
        ctx.set_account_model_health_observer(
            validator.clone() as Arc<dyn AccountModelHealthObserver>
        );
        ctx.set_passthrough_binding_validator(validator);
        plan
    } else {
        state
            .routing
            .route(&ctx)
            .await
            .map_err(|e| crate::error::map_routing_error(e, op.protocol()))?
    };
    let (billing_provider, account_id) = match &plan.primary {
        ExecutionTarget::NodeDispatch { .. } => ("node".to_owned(), Uuid::nil()),
        ExecutionTarget::UpstreamAccount {
            provider,
            account_id,
            ..
        } => (provider.clone(), *account_id),
    };
    ctx.set_provider(&billing_provider);
    let ctx = Arc::new(ctx);
    lifecycle
        .set_route(
            ctx.request_id,
            if mode == ModelAccessMode::NodeDispatch {
                RouteType::Node
            } else {
                RouteType::PassthroughBinding
            },
            RequestStatus::Routing,
        )
        .await
        .map_err(|_| ApiError::ServiceUnavailable("Request tracing is unavailable".into()))?;
    let limits = crate::middleware::authenticated_rate_limit_config_for_target(
        &state,
        auth.tenant_id,
        &plan.primary,
    )
    .await?;
    if let Some(managed) = &managed {
        managed.prepare(ctx.clone(), account_id).await?;
    }
    let mut tpm = super::reserve_generation_tpm(&state, &ctx, limits.clone()).await?;
    let lifetime = if mode == ModelAccessMode::NodeDispatch {
        super::GenerationBalanceReservationLifetime::Node(
            state.node_gateway.as_ref().unwrap().task_deadline(),
        )
    } else {
        super::GenerationBalanceReservationLifetime::Gateway
    };
    let mut balance = match super::reserve_generation_balance(&state, &ctx, lifetime).await {
        Ok(v) => v,
        Err(e) => {
            tpm.release().await;
            return Err(e);
        }
    };
    if let Err(error) =
        crate::middleware::enforce_authenticated_rate_limit_with_config(&state, &auth, &limits)
            .await
    {
        balance.release().await;
        tpm.release().await;
        return Err(error);
    }
    if let Some(managed) = &managed
        && let Err(error) = managed.checkpoint(&ctx).await
    {
        balance.release().await;
        tpm.release().await;
        return Err(error);
    }
    let mut guard = super::ClientResponseGuard::new(lifecycle.clone(), ctx.clone());
    pre.disarm();
    if ctx.stream {
        return super::scoped_stream::serve(super::scoped_stream::Prepared {
            state,
            mode,
            op,
            model,
            ctx,
            plan,
            native,
            billing_provider,
            account_id,
            lifecycle,
            body_permit,
            balance,
            tpm,
            guard,
            managed,
        })
        .await;
    }
    let (mut sender, receiver) = tokio::sync::oneshot::channel();
    let worker_ctx = ctx.clone();
    let worker_lifecycle = lifecycle.clone();
    tokio::spawn(async move {
        let _body_permit = body_permit;
        let mut result = if mode == ModelAccessMode::NodeDispatch {
            let payload = NodeTaskPayload {
                request_id: worker_ctx.request_id,
                native,
                chat: None,
                image_generation: None,
                image_edit: None,
            };
            let gateway = state.node_gateway.as_ref().unwrap();
            let execution = if managed.is_some() {
                gateway
                    .enqueue_native_cancellable_and_wait(
                        worker_ctx.user_id,
                        model,
                        payload,
                        worker_ctx.clone(),
                    )
                    .await
            } else {
                gateway
                    .enqueue_native_and_wait(worker_ctx.user_id, model, payload)
                    .await
            };
            match execution {
                Ok(response) => match response.validate_for(op, &worker_ctx.model) {
                    Ok(Some((input, output))) => {
                        worker_ctx.set_input_tokens(input);
                        worker_ctx.set_output_tokens(output);
                        Ok(Delivery::from_native(response))
                    }
                    Ok(None) => Ok(Delivery::from_native(response)),
                    Err(_) => Err(ApiError::Provider("Invalid native node response".into())),
                },
                Err(error) => {
                    worker_ctx.set_execution_failure(error.request_failure());
                    Err(ApiError::from(error))
                }
            }
        } else {
            execute_account(
                &state,
                worker_ctx.clone(),
                plan,
                op,
                &mut sender,
                worker_lifecycle,
            )
            .await
        };
        let mut persisted = true;
        if let Some(managed) = &managed {
            persisted = match &result {
                Ok(response) => managed
                    .capture_http(
                        &worker_ctx,
                        response.status,
                        &response.headers,
                        &response.body,
                    )
                    .await
                    .is_ok(),
                Err(_) => managed.checkpoint(&worker_ctx).await.is_ok(),
            };
        }
        let mut secured = true;
        if mode == ModelAccessMode::Passthrough || result.as_ref().is_ok_and(|r| r.status == 200) {
            balance.transfer_to_settlement();
            tpm.transfer_to_settlement();
            secured = super::finalize_immediate_settlement_logged(
                &super::ImmediateSettlementServices::from_state(&state),
                &worker_ctx,
                &billing_provider,
                account_id,
                if result.as_ref().is_ok_and(|r| r.status == 200) {
                    "success"
                } else {
                    "error"
                },
                op.protocol(),
            )
            .await;
        } else {
            balance.release().await;
            tpm.release().await;
        }
        if let Some(managed) = &managed {
            let outcome = if result.as_ref().is_ok_and(|r| r.status == 200) {
                ClientResponseOutcome::Succeeded
            } else {
                ClientResponseOutcome::ResponseFailed
            };
            match managed.finish(outcome, secured && persisted).await {
                Ok((public, _)) => {
                    if let Ok(response) = &mut result
                        && response.status == 200
                    {
                        response.body = public;
                    }
                    if !persisted || !secured {
                        result = Err(ApiError::ServiceUnavailable(
                            "Managed response could not be durably completed".into(),
                        ));
                    }
                }
                Err(error) => {
                    result = Err(error);
                }
            }
        }
        if sender.send(result).is_err() {
            worker_ctx.mark_client_disconnected();
        }
    });
    let result = receiver
        .await
        .map_err(|_| ApiError::Internal("Native response worker stopped unexpectedly".into()))?;
    let success = result.as_ref().is_ok_and(|r| r.status == 200);
    if success {
        let _ = super::record_final_client_first_content(&lifecycle, ctx.request_id).await;
    }
    super::finish_client_response_trace(
        &lifecycle,
        &ctx,
        if success {
            ClientResponseOutcome::Succeeded
        } else {
            ClientResponseOutcome::ResponseFailed
        },
    )
    .await;
    guard.disarm();
    result?.into_response()
}
struct Delivery {
    status: u16,
    headers: Vec<(String, String)>,
    body: Value,
    admission: Option<LargeBodyPermit>,
}
impl Delivery {
    fn from_native(result: NodeNativeHttpResult) -> Self {
        Self {
            status: result.status,
            headers: result.headers,
            body: result.body,
            admission: None,
        }
    }
    fn into_response(self) -> Result<Response> {
        let mut response = crate::admission::json_with_admission(self.body, self.admission)?;
        *response.status_mut() = StatusCode::from_u16(self.status)
            .map_err(|_| ApiError::Internal("Invalid native response status".into()))?;
        for (name, value) in self.headers {
            if keycompute_types::node_native::native_response_header_allowed(&name, &value)
                && let (Ok(name), Ok(value)) = (
                    axum::http::HeaderName::from_bytes(name.as_bytes()),
                    axum::http::HeaderValue::from_str(&value),
                )
            {
                response.headers_mut().insert(name, value);
            }
        }
        Ok(response)
    }
}
fn account_error(ctx: &RequestContext, op: Op) -> ApiError {
    let error = crate::error::openai_client_failure(ctx, "Native upstream request failed");
    match error {
        ApiError::OpenAiUpstream(response) if op == Op::Messages => {
            ApiError::AnthropicUpstream(response)
        }
        other => other,
    }
}
async fn execute_account(
    state: &AppState,
    ctx: Arc<RequestContext>,
    plan: ExecutionPlan,
    op: Op,
    sender: &mut tokio::sync::oneshot::Sender<Result<Delivery>>,
    lifecycle: Arc<dyn RequestLifecycleRecorder>,
) -> Result<Delivery> {
    let mut receiver = tokio::time::timeout(
        Duration::from_secs(state.gateway_config.timeout_secs),
        state.gateway.execute_with_recorder(
            ctx.clone(),
            plan,
            state.account_states.clone(),
            Some(state.provider_health.clone()),
            lifecycle,
        ),
    )
    .await
    .map_err(|_| ApiError::ServiceUnavailable("Native upstream setup timed out".into()))?
    .map_err(|error| {
        match crate::error::map_openai_execution_error(error, ctx.client_upstream_response()) {
            ApiError::OpenAiUpstream(r) if op == Op::Messages => ApiError::AnthropicUpstream(r),
            other => other,
        }
    })?;
    let mut result: Option<Delivery> = None;
    let mut connected = true;
    loop {
        let event = tokio::select! {
            biased;
            _=sender.closed(),if connected=>{connected=false;ctx.mark_client_disconnected();continue;}
            event=receiver.recv()=>event,
        };
        match event {
            Some(StreamEvent::Native {
                event: NativeStreamEvent::OpenAiResponsesJson { body, admission },
            }) if op == Op::Responses => {
                if result.is_some() {
                    return Err(ApiError::Provider("Duplicate native response body".into()));
                }
                result = Some(Delivery {
                    status: 200,
                    headers: vec![],
                    body,
                    admission,
                });
            }
            Some(StreamEvent::Raw { data, admission }) if op == Op::Messages => {
                let mut envelope: Value = serde_json::from_str(&data)
                    .map_err(|_| ApiError::Provider("Invalid native Messages response".into()))?;
                if envelope["kind"] != "anthropic_message" || result.is_some() {
                    return Err(ApiError::Provider(
                        "Unexpected native Messages response".into(),
                    ));
                }
                let body = envelope
                    .get_mut("body")
                    .map(Value::take)
                    .ok_or_else(|| ApiError::Provider("Missing Messages response body".into()))?;
                result = Some(Delivery {
                    status: 200,
                    headers: vec![],
                    body,
                    admission,
                });
            }
            Some(StreamEvent::Done) => {
                if let Some(response) = &mut result {
                    response.headers = ctx
                        .client_upstream_response_headers()
                        .into_iter()
                        .filter(|(name, value)| {
                            keycompute_types::node_native::native_response_header_allowed(
                                name, value,
                            )
                        })
                        .collect();
                }
                return result
                    .ok_or_else(|| ApiError::Provider("Native response body missing".into()));
            }
            Some(StreamEvent::Error { .. }) => return Err(account_error(&ctx, op)),
            None => {
                return Err(ApiError::Provider(
                    "Native response ended without completion".into(),
                ));
            }
            _ => {}
        }
    }
}
