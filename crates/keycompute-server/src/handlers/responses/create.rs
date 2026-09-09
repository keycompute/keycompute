//! Responses create/compact orchestration and terminal JSON handling.

use super::*;

/// POST /v1/responses
pub async fn responses(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received_at: RequestReceivedAt,
    headers: HeaderMap,
    (body_permit, Json(body)): (Option<Extension<GenerationHttpBodyPermit>>, Json<Value>),
) -> Result<axum::response::Response> {
    responses_inner(
        state,
        auth,
        request_id,
        client_request_id,
        received_at,
        headers,
        body,
        body_permit.map(|Extension(permit)| permit),
        "/v1/responses",
        "/responses",
        true,
    )
    .await
}

/// POST /v1/responses/compact
pub async fn compact_response(
    State(state): State<AppState>,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received_at: RequestReceivedAt,
    headers: HeaderMap,
    (body_permit, Json(body)): (Option<Extension<GenerationHttpBodyPermit>>, Json<Value>),
) -> Result<axum::response::Response> {
    responses_inner(
        state,
        auth,
        request_id,
        client_request_id,
        received_at,
        headers,
        body,
        body_permit.map(|Extension(permit)| permit),
        "/v1/responses/compact",
        "/responses/compact",
        false,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(in crate::handlers) async fn responses_inner(
    state: AppState,
    auth: AuthExtractor,
    request_id: RequestId,
    client_request_id: ClientRequestId,
    received_at: RequestReceivedAt,
    headers: HeaderMap,
    mut body: Value,
    mut body_permit: Option<GenerationHttpBodyPermit>,
    request_path: &'static str,
    upstream_path: &'static str,
    supports_streaming: bool,
) -> Result<axum::response::Response> {
    let mut routing = ResponsesRoutingFields::parse(&body)?;
    let idempotency = responses_idempotency(&headers, request_path, auth.tenant_id, &body)?;
    let forwarded_headers = forwarded_responses_headers(&headers, auth.tenant_id)?;
    if routing.stream && idempotency.is_some() {
        return Err(ApiError::BadRequest(
            "Idempotency-Key is not supported for streaming Responses requests".to_string(),
        ));
    }
    if routing.stream && !supports_streaming {
        return Err(ApiError::BadRequest(format!(
            "stream is not supported by {request_path}"
        )));
    }
    let mut lifecycle: Arc<dyn RequestLifecycleRecorder> = Arc::clone(&state.lifecycle);
    let mut pre_execution_guard =
        crate::handlers::PreExecutionTraceGuard::new(Arc::clone(&lifecycle), request_id.0);
    if let Err(error) = lifecycle
        .start_request(RequestTraceStart {
            request_id: request_id.0,
            client_request_id: client_request_id.0,
            tenant_id: auth.tenant_id,
            user_id: auth.user_id,
            produce_ai_key_id: auth.produce_ai_key_id,
            protocol: "openai".to_string(),
            request_path: request_path.to_string(),
            requested_model: routing.model.clone(),
            is_stream: routing.stream,
            received_at: received_at.0,
        })
        .await
    {
        tracing::warn!(request_id=%request_id.0, %error, "request tracing disabled for this request");
        pre_execution_guard.disarm();
        lifecycle = Arc::new(NoopRequestLifecycleRecorder);
        pre_execution_guard =
            crate::handlers::PreExecutionTraceGuard::new(Arc::clone(&lifecycle), request_id.0);
    }

    if !auth.has_permission(&Permission::UseApi) {
        pre_execution_guard
            .finish_failed(
                ErrorOrigin::Client,
                TraceErrorCategory::Authorization,
                "permission_denied",
            )
            .await;
        return Err(ApiError::Forbidden(format!(
            "API-use permission is required for {request_path}"
        )));
    }
    if let Some(idempotency) = idempotency.as_ref() {
        match replay_completed_responses_idempotency(
            &state,
            auth.tenant_id,
            auth.user_id,
            auth.produce_ai_key_id,
            idempotency,
        )
        .await
        {
            Ok(Some(cached)) => {
                let response = cached_responses_idempotency_response(cached)?;
                let outcome = if response.status().is_success() {
                    ClientResponseOutcome::Succeeded
                } else {
                    ClientResponseOutcome::ResponseFailed
                };
                pre_execution_guard.finish_replayed(outcome).await;
                return Ok(response);
            }
            Ok(None) => {}
            Err(error) => {
                pre_execution_guard
                    .finish_failed(
                        ErrorOrigin::Client,
                        TraceErrorCategory::InvalidRequest,
                        "idempotency_lookup_failed",
                    )
                    .await;
                return Err(error);
            }
        }
    }
    let replayed_client_previous_response_id = match replay_stored_warmup_body(
        &state,
        auth.tenant_id,
        &mut body,
        &mut body_permit,
    )
    .await
    {
        Ok(previous_response_id) => previous_response_id,
        Err(error) => {
            pre_execution_guard
                .finish_failed(
                    ErrorOrigin::Client,
                    TraceErrorCategory::InvalidRequest,
                    "previous_response_replay_failed",
                )
                .await;
            return Err(error);
        }
    };

    // A local warmup may expand inherited request fields, including model and
    // input. Parse the effective body rather than retaining the pre-replay
    // routing projection. Do not synthesize `max_output_tokens`: an omitted or
    // explicit-null client limit must remain absent from the native upstream
    // body and unspecified in routing. Balance reservation applies its own
    // internal risk budget independently of the wire request.
    routing = ResponsesRoutingFields::parse(&body)?;
    let persist_response_affinity = response_affinity_storage_enabled(upstream_path, &body);
    // A root `generate:false` warmup never created upstream state. Its durable
    // row owns the local resource, but the account selected when the row was
    // stored is only a schema-level placeholder and must not constrain the
    // first real generation. Re-route the effective model normally. Warmups
    // chained from an upstream response keep that response's account affinity.
    let root_local_warmup =
        local_warmup_has_no_upstream_owner(replayed_client_previous_response_id.as_deref(), &body);
    let previous_response_id = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let conversation_id = conversation_resource_id(&body).map(str::to_string);
    let mut conversation_needs_discovery = false;
    let mut resolved_affinity_account =
        if let Some(previous_response_id) = previous_response_id.as_deref() {
            if root_local_warmup {
                None
            } else {
                match resolve_response_account(&state, previous_response_id, auth.tenant_id).await {
                    Ok(account) => Some(account),
                    Err(error) => {
                        pre_execution_guard
                            .finish_failed(
                                ErrorOrigin::Client,
                                TraceErrorCategory::InvalidRequest,
                                "previous_response_not_found",
                            )
                            .await;
                        return Err(error);
                    }
                }
            }
        } else if let Some(conversation_id) = conversation_id.as_deref() {
            match resolve_response_account(&state, conversation_id, auth.tenant_id).await {
                Ok(account) => Some(account),
                Err(ApiError::NotFound(_)) => {
                    conversation_needs_discovery = true;
                    None
                }
                Err(error) => {
                    pre_execution_guard
                        .finish_failed(
                            ErrorOrigin::Client,
                            TraceErrorCategory::InvalidRequest,
                            "conversation_lookup_failed",
                        )
                        .await;
                    return Err(error);
                }
            }
        } else {
            None
        };
    if conversation_needs_discovery && let Some(conversation_id) = conversation_id.as_deref() {
        resolved_affinity_account = match discover_conversation_account(
            &state,
            conversation_id,
            auth.tenant_id,
            headers
                .get("openai-beta")
                .and_then(|value| value.to_str().ok()),
        )
        .await
        {
            Ok(account) => Some(account),
            Err(error) => {
                pre_execution_guard
                    .finish_failed(
                        ErrorOrigin::Client,
                        TraceErrorCategory::InvalidRequest,
                        "conversation_not_found",
                    )
                    .await;
                return Err(error);
            }
        };
    }
    if let Err(error) = validate_idempotent_project_resource_owner(
        &body,
        idempotency.is_some(),
        resolved_affinity_account.is_some(),
    ) {
        pre_execution_guard
            .finish_failed(
                ErrorOrigin::Client,
                TraceErrorCategory::InvalidRequest,
                "idempotency_resource_owner_unresolved",
            )
            .await;
        return Err(error);
    }
    if routing.model.is_empty()
        && let Some(model) = resolved_affinity_account
            .as_ref()
            .and_then(|account| account.model.as_ref())
    {
        routing.model.clone_from(model);
    }
    if let Err(error) = validate_effective_responses_model(upstream_path, &routing.model) {
        pre_execution_guard
            .finish_failed(
                ErrorOrigin::Client,
                TraceErrorCategory::InvalidRequest,
                "compact_model_unresolved",
            )
            .await;
        return Err(error);
    }
    let reservation_constraint = if let Some(resource_id) = previous_response_id.as_ref() {
        Some(ResponsesReservationConstraint::Affinity {
            resource_id: resource_id.clone(),
        })
    } else if let Some(resource_id) = conversation_id.as_ref() {
        if conversation_needs_discovery {
            resolved_affinity_account
                .as_ref()
                .map(ResponsesReservationConstraint::discovered)
        } else {
            Some(ResponsesReservationConstraint::Affinity {
                resource_id: resource_id.clone(),
            })
        }
    } else {
        None
    };

    let provider = keycompute_pricing::resolve_pricing_provider(&routing.model);
    let pricing = match state
        .pricing
        .create_snapshot(&routing.model, &auth.tenant_id, Some(provider))
        .await
    {
        Ok(pricing) => pricing,
        Err(error) => {
            pre_execution_guard
                .finish_failed(
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::Internal,
                    "pricing_failed",
                )
                .await;
            return Err(ApiError::Internal(format!(
                "Failed to create pricing snapshot: {error}"
            )));
        }
    };

    let mut request_ctx = RequestContext::new(
        request_id.0,
        auth.user_id,
        auth.tenant_id,
        auth.produce_ai_key_id,
        routing.model.clone(),
        routing.messages,
        routing.stream,
        pricing,
    );
    request_ctx.max_tokens = routing.max_output_tokens;
    request_ctx.temperature = routing.temperature;
    request_ctx.top_p = routing.top_p;
    request_ctx.native_openai_responses_request = Some(Arc::new(body));
    request_ctx.native_openai_responses_path = Some(upstream_path.to_string());
    request_ctx.native_openai_responses_headers = forwarded_headers;
    let mut ctx = Arc::new(request_ctx);

    let mut plan = if let Some(account) = resolved_affinity_account.take() {
        ExecutionPlan::new(account.into_target())
    } else {
        match state.routing.route(&ctx).await {
            Ok(plan) => plan,
            Err(error) => {
                pre_execution_guard
                    .finish_failed(
                        ErrorOrigin::Gateway,
                        TraceErrorCategory::Internal,
                        "routing_failed",
                    )
                    .await;
                return Err(crate::error::map_routing_error(error, "openai responses"));
            }
        }
    };
    let (selected_provider, selected_account_id) = match &plan.primary {
        ExecutionTarget::ProviderAccount {
            provider,
            account_id,
            ..
        } if provider.eq_ignore_ascii_case("openai") => (provider.clone(), *account_id),
        ExecutionTarget::ProviderAccount { .. } => {
            pre_execution_guard
                .finish_failed(
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::InvalidRequest,
                    "incompatible_provider_route",
                )
                .await;
            return Err(ApiError::BadRequest(format!(
                "Model {} is not available through an OpenAI Responses-compatible provider",
                routing.model
            )));
        }
        ExecutionTarget::Node { .. } => {
            pre_execution_guard
                .finish_failed(
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::InvalidRequest,
                    "unsupported_node_route",
                )
                .await;
            return Err(ApiError::BadRequest(
                "Responses ingress cannot be routed to a node".to_string(),
            ));
        }
    };
    plan.fallback_chain.retain(|target| {
        matches!(
            target,
            ExecutionTarget::ProviderAccount { provider, .. }
                if provider.eq_ignore_ascii_case("openai")
        )
    });
    let routed_body = ctx
        .native_openai_responses_request
        .as_deref()
        .expect("Responses body initialized before routing");
    if let Err(error) = validate_project_resource_route(routed_body, &plan) {
        pre_execution_guard
            .finish_failed(
                ErrorOrigin::Client,
                TraceErrorCategory::InvalidRequest,
                "project_resource_owner_unresolved",
            )
            .await;
        return Err(error);
    }
    let (mut primary_provider, mut primary_account_id) = match &plan.primary {
        ExecutionTarget::ProviderAccount {
            provider,
            account_id,
            ..
        } => (provider.clone(), *account_id),
        ExecutionTarget::Node { .. } => unreachable!("Responses target validated above"),
    };
    if let Err(error) = lifecycle
        .set_route(
            request_id.0,
            RouteType::ProviderAccount,
            RequestStatus::Routing,
        )
        .await
    {
        tracing::warn!(request_id=%request_id.0, %error, "failed to record request route");
    }
    state
        .pricing
        .update_context_pricing(Arc::make_mut(&mut ctx), &primary_provider)
        .await;

    if idempotency.is_some() {
        // One durable key is bound to one upstream account. Cross-account
        // fallback would create a second idempotency namespace and make crash
        // recovery capable of executing the logical request twice.
        plan.fallback_chain.clear();
    }
    let mut reservations = match ResponsesExecutionReservations::acquire(
        &state,
        auth.tenant_id,
        request_id.0,
        &mut plan,
        reservation_constraint.as_ref(),
    )
    .await
    {
        Ok(reservations) => reservations,
        Err(error) => {
            pre_execution_guard
                .finish_failed(
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::Internal,
                    "responses_account_reservation_failed",
                )
                .await;
            return Err(error);
        }
    };
    let mut idempotency_execution = None;
    if let Some(idempotency) = idempotency.as_ref() {
        match bind_responses_idempotency(
            &state,
            auth.tenant_id,
            auth.user_id,
            auth.produce_ai_key_id,
            idempotency,
            &routing.model,
            &selected_provider,
            selected_account_id,
        )
        .await
        {
            Ok(ResponsesIdempotencyBinding::Execute { account, execution }) => {
                if account.account_id != primary_account_id
                    || !account.provider.eq_ignore_ascii_case(&primary_provider)
                {
                    reservations.release().await;
                    primary_provider.clone_from(&account.provider);
                    primary_account_id = account.account_id;
                    let claimed_account = account.clone();
                    plan = ExecutionPlan::new(account.into_target());
                    state
                        .pricing
                        .update_context_pricing(Arc::make_mut(&mut ctx), &primary_provider)
                        .await;
                    reservations = match ResponsesExecutionReservations::acquire(
                        &state,
                        auth.tenant_id,
                        request_id.0,
                        &mut plan,
                        reservation_constraint.as_ref(),
                    )
                    .await
                    {
                        Ok(mut reservations) => {
                            if let Err(error) =
                                validate_reserved_idempotency_connection(&plan, &claimed_account)
                            {
                                reservations.release().await;
                                abandon_unstarted_responses_idempotency_execution(
                                    &state, &execution,
                                )
                                .await;
                                pre_execution_guard
                                    .finish_failed(
                                        ErrorOrigin::Gateway,
                                        TraceErrorCategory::Internal,
                                        "idempotency_account_changed_before_reservation",
                                    )
                                    .await;
                                return Err(error);
                            }
                            reservations
                        }
                        Err(error) => {
                            abandon_unstarted_responses_idempotency_execution(&state, &execution)
                                .await;
                            pre_execution_guard
                                .finish_failed(
                                    ErrorOrigin::Gateway,
                                    TraceErrorCategory::Internal,
                                    "idempotency_account_reservation_failed",
                                )
                                .await;
                            return Err(error);
                        }
                    };
                }
                Arc::make_mut(&mut ctx).set_billing_request_id(idempotency.billing_request_id);
                idempotency_execution = Some(execution);
            }
            Ok(ResponsesIdempotencyBinding::Replay(cached)) => {
                reservations.release().await;
                let response = cached_responses_idempotency_response(cached)?;
                let outcome = if response.status().is_success() {
                    ClientResponseOutcome::Succeeded
                } else {
                    ClientResponseOutcome::ResponseFailed
                };
                pre_execution_guard.finish_replayed(outcome).await;
                return Ok(response);
            }
            Err(error) => {
                reservations.release().await;
                pre_execution_guard
                    .finish_failed(
                        ErrorOrigin::Client,
                        TraceErrorCategory::InvalidRequest,
                        "idempotency_binding_failed",
                    )
                    .await;
                return Err(error);
            }
        }
    }
    let mut tpm_reservation = match crate::handlers::reserve_generation_tpm(&state, &ctx).await {
        Ok(reservation) => reservation,
        Err(error) => {
            reservations.release().await;
            if let Some(execution) = idempotency_execution.as_ref() {
                abandon_unstarted_responses_idempotency_execution(&state, execution).await;
            }
            let (origin, category, code) = if matches!(error, ApiError::RateLimit(_)) {
                (
                    ErrorOrigin::Client,
                    TraceErrorCategory::RateLimit,
                    "tpm_limit_exceeded",
                )
            } else {
                (
                    ErrorOrigin::Gateway,
                    TraceErrorCategory::Internal,
                    "tpm_reservation_failed",
                )
            };
            pre_execution_guard
                .finish_failed(origin, category, code)
                .await;
            return Err(error);
        }
    };
    let mut balance_reservation = match crate::handlers::reserve_generation_balance(
        &state,
        &ctx,
        crate::handlers::GenerationBalanceReservationLifetime::Responses,
    )
    .await
    {
        Ok(reservation) => reservation,
        Err(error) => {
            tpm_reservation.release().await;
            reservations.release().await;
            if let Some(execution) = idempotency_execution.as_ref() {
                abandon_unstarted_responses_idempotency_execution(&state, execution).await;
            }
            pre_execution_guard
                .finish_failed(
                    ErrorOrigin::Client,
                    TraceErrorCategory::Balance,
                    "balance_reservation_failed",
                )
                .await;
            return Err(error);
        }
    };
    if let Some(execution) = idempotency_execution.as_ref()
        && let Err(error) = mark_responses_idempotency_dispatched(&state, execution).await
    {
        balance_reservation.release().await;
        tpm_reservation.release().await;
        reservations.release().await;
        abandon_unstarted_responses_idempotency_execution(&state, execution).await;
        pre_execution_guard
            .finish_failed(
                ErrorOrigin::Gateway,
                TraceErrorCategory::Internal,
                "idempotency_dispatch_fence_failed",
            )
            .await;
        return Err(error);
    }
    let timeout = Duration::from_secs(state.gateway_config.timeout_secs);
    let mut client_response_guard =
        crate::handlers::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
    pre_execution_guard.disarm();
    let rx = match tokio::time::timeout(
        timeout,
        state.gateway.execute_with_recorder(
            Arc::clone(&ctx),
            plan,
            Arc::clone(&state.account_states),
            Some(Arc::clone(&state.provider_health)),
            Arc::clone(&lifecycle),
        ),
    )
    .await
    {
        Ok(Ok(rx)) => rx,
        Ok(Err(error)) => {
            balance_reservation.release().await;
            tpm_reservation.release().await;
            reservations.release().await;
            expire_dispatched_responses_idempotency_execution(
                &state,
                idempotency_execution.as_ref(),
            )
            .await;
            client_response_guard
                .finish_with_outcome(ClientResponseOutcome::ResponseFailed)
                .await;
            return Err(crate::error::map_execution_error(error));
        }
        Err(_) => {
            balance_reservation.release().await;
            tpm_reservation.release().await;
            reservations.release().await;
            expire_dispatched_responses_idempotency_execution(
                &state,
                idempotency_execution.as_ref(),
            )
            .await;
            client_response_guard
                .finish_with_outcome(ClientResponseOutcome::TimedOut)
                .await;
            return Err(ApiError::Internal(format!(
                "Gateway execute timeout after {}s",
                state.gateway_config.timeout_secs
            )));
        }
    };
    tracing::info!(
        request_id = %request_id.0,
        model = %routing.model,
        stream = routing.stream,
        primary_provider = %primary_provider,
        "OpenAI Responses request"
    );

    let billing = Arc::clone(&state.billing);
    if routing.stream {
        let error_ctx = Arc::clone(&ctx);
        let (initial_status_tx, initial_status_rx) = tokio::sync::oneshot::channel();
        let stream = create_responses_stream(
            rx,
            None,
            ResponsesStreamRuntime {
                ctx,
                provider: primary_provider,
                account_id: primary_account_id,
                billing,
                lifecycle: Arc::clone(&lifecycle),
                state: state.clone(),
                client_previous_response_id: replayed_client_previous_response_id,
                persist_response_affinity,
                reservations,
                body_permit,
                initial_status: Some(initial_status_tx),
            },
        );
        balance_reservation.transfer_to_settlement();
        tpm_reservation.transfer_to_settlement();
        client_response_guard.disarm();
        if !matches!(
            initial_status_rx.await,
            Ok(crate::handlers::InitialStreamStatus::Ready)
        ) {
            drop(stream);
            if let Some(upstream) = error_ctx.client_upstream_response() {
                return client_upstream_response(upstream);
            }
            return Err(ApiError::Provider("Upstream request failed".to_string()));
        }
        let success_headers = error_ctx.client_upstream_response_headers();
        let mut response = Sse::new(stream).into_response();
        append_forwarded_upstream_response_headers(&mut response, success_headers);
        Ok(response)
    } else {
        client_response_guard.disarm();
        balance_reservation.transfer_to_settlement();
        tpm_reservation.transfer_to_settlement();
        let response = create_responses_json(
            rx,
            ResponsesJsonRuntime {
                ctx,
                provider: primary_provider,
                account_id: primary_account_id,
                billing,
                lifecycle: Arc::clone(&lifecycle),
                state: state.clone(),
                client_previous_response_id: replayed_client_previous_response_id,
                persist_response_affinity,
                reservations,
                body_permit,
                idempotency_execution,
            },
        )
        .await?;
        Ok(response)
    }
}

#[cfg(test)]
pub(super) fn initial_responses_stream_failure(
    initial_event: Option<&llm_protocol_provider::StreamEvent>,
) -> Option<ApiError> {
    match initial_event {
        Some(llm_protocol_provider::StreamEvent::Error { .. }) => {
            Some(ApiError::Provider("Upstream request failed".to_string()))
        }
        None => Some(ApiError::Internal(
            "Responses channel closed before the first event".to_string(),
        )),
        Some(_) => None,
    }
}

pub(super) struct ResponsesJsonRuntime {
    pub(super) ctx: Arc<RequestContext>,
    pub(super) provider: String,
    pub(super) account_id: uuid::Uuid,
    pub(super) billing: Arc<keycompute_billing::BillingService>,
    pub(super) lifecycle: Arc<dyn RequestLifecycleRecorder>,
    pub(super) state: AppState,
    pub(super) client_previous_response_id: Option<String>,
    pub(super) persist_response_affinity: bool,
    pub(super) reservations: ResponsesExecutionReservations,
    pub(super) body_permit: Option<GenerationHttpBodyPermit>,
    pub(super) idempotency_execution: Option<ResponsesIdempotencyExecution>,
}

pub(super) async fn create_responses_json(
    mut rx: tokio::sync::mpsc::Receiver<llm_protocol_provider::StreamEvent>,
    runtime: ResponsesJsonRuntime,
) -> Result<Response> {
    let ResponsesJsonRuntime {
        ctx,
        provider,
        account_id,
        billing,
        lifecycle,
        state,
        client_previous_response_id,
        persist_response_affinity,
        mut reservations,
        body_permit,
        idempotency_execution,
    } = runtime;
    let mut guard =
        crate::handlers::ClientResponseGuard::new(Arc::clone(&lifecycle), Arc::clone(&ctx));
    let (mut response_tx, response_rx) = tokio::sync::oneshot::channel();
    let worker_ctx = Arc::clone(&ctx);
    let abandoned_idempotency_execution = idempotency_execution.clone();
    let abandoned_state = state.clone();
    tokio::spawn(async move {
        let _body_permit = body_permit;
        let mut response = None;
        let mut response_admission = None;
        let mut completed = false;
        let mut handler_connected = true;
        let mut terminal_error = None;
        let mut billing_status = "success";
        let mut retain_reservations = false;

        loop {
            tokio::select! {
                biased;
                _ = response_tx.closed(), if handler_connected => {
                    handler_connected = false;
                    worker_ctx.mark_client_disconnected();
                }
                event = rx.recv() => {
                    let Some(event) = event else { break };
                    match event {
                        llm_protocol_provider::StreamEvent::Native {
                            event:
                                NativeStreamEvent::OpenAiResponsesJson {
                                    mut body,
                                    admission,
                                },
                        } => {
                            patch_client_previous_response_id(
                                &mut body,
                                client_previous_response_id.as_deref(),
                            );
                            let _ = sanitize_upstream_responses_error(None, &mut body);
                            billing_status = response_billing_status(&body);
                            response = Some(body);
                            response_admission = admission;
                        }
                        llm_protocol_provider::StreamEvent::Done => {
                            completed = true;
                            break;
                        }
                        llm_protocol_provider::StreamEvent::Error { .. } => {
                            billing_status = "error";
                            terminal_error = Some(ApiError::Provider(
                                "Upstream request failed".to_string(),
                            ));
                            break;
                        }
                        llm_protocol_provider::StreamEvent::Delta { .. }
                        | llm_protocol_provider::StreamEvent::Usage { .. }
                        | llm_protocol_provider::StreamEvent::InputUsage { .. }
                        | llm_protocol_provider::StreamEvent::Raw { .. }
                        | llm_protocol_provider::StreamEvent::Native { .. } => {}
                    }
                }
            }
        }
        if terminal_error.is_none() && !completed {
            billing_status = "incomplete";
            terminal_error = Some(ApiError::Internal(
                "Responses channel closed without a terminal event".to_string(),
            ));
        }
        if terminal_error.is_none() && response.is_none() {
            billing_status = "incomplete";
            terminal_error = Some(ApiError::Internal(
                "Responses JSON body missing after completion".to_string(),
            ));
        }
        let response_id = response
            .as_ref()
            .and_then(response_resource_id)
            .map(str::to_string);
        let conversation_id = response
            .as_ref()
            .and_then(conversation_resource_id)
            .map(str::to_string);
        let actual_model = response
            .as_ref()
            .and_then(response_model)
            .map(str::to_string);
        let settlement_ctx =
            match resolved_responses_billing_context(&state, &worker_ctx, actual_model.as_deref())
                .await
            {
                Ok(ctx) => ctx,
                Err(error) => {
                    billing_status = "incomplete";
                    terminal_error = Some(error);
                    retain_reservations = response_id.is_some();
                    Arc::clone(&worker_ctx)
                }
            };
        let affinity_model =
            effective_response_affinity_model(actual_model.as_deref(), &settlement_ctx);
        let background_pending = terminal_error.is_none()
            && response
                .as_ref()
                .is_some_and(response_is_background_pending);
        if background_pending && response_id.is_none() {
            terminal_error = Some(ApiError::Provider(
                "Background Responses response is missing its resource ID".to_string(),
            ));
            billing_status = "incomplete";
        }
        let settlement = if background_pending {
            match background_settlement_value(&settlement_ctx, &provider, account_id) {
                Ok(settlement) => Some(settlement),
                Err(error) => {
                    terminal_error = Some(error);
                    retain_reservations = response_id.is_some();
                    None
                }
            }
        } else if terminal_error.is_none() {
            match terminal_settlement_value(&settlement_ctx, &provider, account_id, billing_status)
            {
                Ok(settlement) => Some(settlement),
                Err(error) => {
                    terminal_error = Some(error);
                    retain_reservations = response_id.is_some();
                    None
                }
            }
        } else {
            None
        };
        let mut settlement_durable = false;
        let mut background_settlement_scheduled = false;
        let persist_response_affinity =
            persist_response_affinity && response.as_ref().is_none_or(response_store_enabled);
        if terminal_error.is_none()
            && let Some(response_id) = response_id.as_deref()
        {
            let (actual_provider, actual_account_id) =
                settlement_ctx.billing_target(&provider, account_id);
            match save_response_affinity_if_stored(
                &state,
                persist_response_affinity,
                response_id,
                ResponsesAffinityRoute {
                    tenant_id: settlement_ctx.tenant_id,
                    provider: actual_provider.clone(),
                    model: affinity_model.clone(),
                    account_id: actual_account_id,
                },
                settlement,
            )
            .await
            {
                Ok(durable) => {
                    settlement_durable = durable;
                    background_settlement_scheduled = background_pending && durable;
                    if let Some(conversation_id) = conversation_id.as_deref()
                        && let Err(error) = save_response_affinity(
                            &state,
                            conversation_id,
                            ResponsesResourceKind::Conversation,
                            ResponsesAffinityRoute {
                                tenant_id: settlement_ctx.tenant_id,
                                provider: actual_provider,
                                model: affinity_model.clone(),
                                account_id: actual_account_id,
                            },
                            None,
                        )
                        .await
                    {
                        if background_pending && !settlement_durable {
                            spawn_background_settlement(
                                state.clone(),
                                Arc::clone(&settlement_ctx),
                                provider.clone(),
                                account_id,
                                Arc::clone(&billing),
                                response_id.to_string(),
                            );
                            background_settlement_scheduled = true;
                        }
                        billing_status = "incomplete";
                        terminal_error = Some(error);
                        retain_reservations = true;
                    }
                }
                Err(error) => {
                    if background_pending {
                        spawn_background_settlement(
                            state.clone(),
                            Arc::clone(&settlement_ctx),
                            provider.clone(),
                            account_id,
                            Arc::clone(&billing),
                            response_id.to_string(),
                        );
                        background_settlement_scheduled = true;
                    }
                    billing_status = "incomplete";
                    terminal_error = Some(error);
                    retain_reservations = true;
                }
            }
        }
        if background_pending && terminal_error.is_none() {
            if let Some(response_id) = response_id.as_deref() {
                if !settlement_durable {
                    // Database-less development uses the in-process poller.
                    spawn_background_settlement(
                        state.clone(),
                        Arc::clone(&settlement_ctx),
                        provider.clone(),
                        account_id,
                        Arc::clone(&billing),
                        response_id.to_string(),
                    );
                }
            } else {
                finalize_responses_billing_logged(
                    &state,
                    &billing,
                    &settlement_ctx,
                    &provider,
                    account_id,
                    "incomplete",
                )
                .await;
            }
        } else if !background_settlement_scheduled {
            if !settlement_durable {
                persist_terminal_responses_outbox(
                    &state,
                    &settlement_ctx,
                    &provider,
                    account_id,
                    response_id.as_deref(),
                    persist_response_affinity,
                    actual_model.clone(),
                    billing_status,
                )
                .await;
            }
            finalize_responses_billing_logged(
                &state,
                &billing,
                &settlement_ctx,
                &provider,
                account_id,
                billing_status,
            )
            .await;
        }
        let mut result = terminal_error.map_or_else(
            || {
                response
                    .map(|body| (body, response_admission))
                    .ok_or_else(|| ApiError::Internal("Responses response missing".into()))
            },
            Err,
        );
        if let Some(execution) = idempotency_execution.as_ref() {
            let cached = match &result {
                Ok((body, _)) => serde_json::to_string(body)
                    .map(|body| ClientUpstreamResponse {
                        status: StatusCode::OK.as_u16(),
                        headers: cacheable_responses_success_headers(&worker_ctx),
                        body,
                    })
                    .map_err(|error| {
                        ApiError::Internal(format!(
                            "Failed to serialize an idempotent Responses result: {error}"
                        ))
                    }),
                Err(_) => match worker_ctx.client_upstream_response() {
                    Some(upstream) => cacheable_upstream_responses_error(upstream),
                    None => Err(ApiError::Internal(
                        "The idempotent Responses execution has no replayable HTTP result"
                            .to_string(),
                    )),
                },
            };
            match cached {
                Ok(cached) => {
                    if let Err(error) =
                        complete_responses_idempotency_execution(&state, execution, &cached).await
                    {
                        tracing::error!(
                            request_id = %worker_ctx.request_id,
                            %error,
                            "failed to persist idempotent Responses result"
                        );
                        expire_dispatched_responses_idempotency_execution(&state, Some(execution))
                            .await;
                        worker_ctx.clear_client_upstream_response();
                        result = Err(error);
                    }
                }
                Err(cache_error) => {
                    expire_dispatched_responses_idempotency_execution(&state, Some(execution))
                        .await;
                    worker_ctx.clear_client_upstream_response();
                    result = Err(cache_error);
                }
            }
        }
        if !retain_reservations {
            reservations.release().await;
        } else if !reservations.is_empty() {
            tracing::warn!(
                request_id = %worker_ctx.request_id,
                "retaining Responses account reservation after a persistence failure"
            );
            reservations.retain_until_expiry();
        }
        if handler_connected && response_tx.send(result).is_err() {
            worker_ctx.mark_client_disconnected();
        }
    });

    let (response, response_admission) = match response_rx.await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            crate::handlers::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
            guard.disarm();
            if let Some(upstream) = ctx.client_upstream_response() {
                return client_upstream_response(upstream);
            }
            return Err(error);
        }
        Err(_) => {
            expire_dispatched_responses_idempotency_execution(
                &abandoned_state,
                abandoned_idempotency_execution.as_ref(),
            )
            .await;
            crate::handlers::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
            guard.disarm();
            return Err(ApiError::Internal(
                "Responses worker stopped unexpectedly".to_string(),
            ));
        }
    };
    if let Err(error) =
        crate::handlers::record_final_client_first_content(&lifecycle, ctx.request_id).await
    {
        tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
    }
    crate::handlers::finish_client_response_trace(
        &lifecycle,
        &ctx,
        response_client_outcome(&response),
    )
    .await;
    guard.disarm();
    let mut response = Json(response).into_response();
    append_forwarded_upstream_response_headers(
        &mut response,
        ctx.client_upstream_response_headers(),
    );
    if let Some(admission) = response_admission {
        retain_response_body_guard(&mut response, admission);
    }
    Ok(response)
}

fn client_upstream_response(
    upstream: keycompute_types::ClientUpstreamResponse,
) -> Result<Response> {
    let status = StatusCode::from_u16(upstream.status)
        .map_err(|_| ApiError::Internal("Upstream returned an invalid status".to_string()))?;
    let mut builder = Response::builder().status(status);
    for (name, value) in upstream.headers {
        if !forwarded_upstream_response_header(&name) {
            continue;
        }
        let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value))
        else {
            continue;
        };
        builder = builder.header(name, value);
    }
    builder
        .body(Body::from(upstream.body))
        .map_err(|error| ApiError::Internal(format!("Failed to build upstream error: {error}")))
}

fn cached_responses_idempotency_response(
    cached: CachedResponsesIdempotencyResult,
) -> Result<Response> {
    let mut response = client_upstream_response(cached.response)?;
    if let Some(admission) = cached.admission {
        retain_response_body_guard(&mut response, admission);
    }
    Ok(response)
}

pub(super) fn cacheable_upstream_responses_error(
    mut upstream: ClientUpstreamResponse,
) -> Result<ClientUpstreamResponse> {
    let status = StatusCode::from_u16(upstream.status)
        .map_err(|_| ApiError::Internal("Upstream returned an invalid status".to_string()))?;
    upstream.body = crate::middleware::normalize_openai_responses_upstream_error(
        status,
        upstream.body.as_bytes(),
    );
    Ok(upstream)
}

#[cfg(test)]
mod native_output_limit_tests {
    use super::*;

    #[test]
    fn omitted_and_null_responses_limits_remain_unspecified_and_native() {
        let omitted = json!({"model": "gpt-5", "input": "hello"});
        let omitted_before = omitted.clone();
        let routing = ResponsesRoutingFields::parse(&omitted).unwrap();
        assert_eq!(routing.max_output_tokens, None);
        assert_eq!(omitted, omitted_before);
        assert!(omitted.get("max_output_tokens").is_none());

        let nullable = json!({
            "model": "gpt-5",
            "input": "hello",
            "max_output_tokens": null
        });
        let nullable_before = nullable.clone();
        let routing = ResponsesRoutingFields::parse(&nullable).unwrap();
        assert_eq!(routing.max_output_tokens, None);
        assert_eq!(nullable, nullable_before);
        assert!(nullable["max_output_tokens"].is_null());
    }

    #[test]
    fn explicit_responses_limit_and_compact_body_are_preserved() {
        let explicit = json!({
            "model": "gpt-5",
            "input": "hello",
            "max_output_tokens": 123
        });
        let explicit_before = explicit.clone();
        let routing = ResponsesRoutingFields::parse(&explicit).unwrap();
        assert_eq!(routing.max_output_tokens, Some(123));
        assert_eq!(explicit, explicit_before);

        let compact = json!({"model": "gpt-5", "input": "hello"});
        let compact_before = compact.clone();
        let routing = ResponsesRoutingFields::parse(&compact).unwrap();
        assert_eq!(routing.max_output_tokens, None);
        assert_eq!(compact, compact_before);
        assert!(compact.get("max_output_tokens").is_none());

        let compact_null = json!({
            "model": "gpt-5",
            "input": "hello",
            "max_output_tokens": null
        });
        let compact_null_before = compact_null.clone();
        let routing = ResponsesRoutingFields::parse(&compact_null).unwrap();
        assert_eq!(routing.max_output_tokens, None);
        assert_eq!(compact_null, compact_null_before);
        assert!(compact_null["max_output_tokens"].is_null());
    }
}
