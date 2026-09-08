//! Foreground Responses SSE delivery and settlement ownership.

use super::*;

pub(super) const SSE_SEND_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) struct ResponsesStreamRuntime {
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
    pub(super) initial_status:
        Option<tokio::sync::oneshot::Sender<crate::handlers::InitialStreamStatus>>,
}

/// Secure foreground stream settlement at most once. A scheduled background
/// job already owns settlement, so both that state and a successful foreground
/// attempt satisfy later terminal events without invoking `settle` again.
pub(super) async fn secure_responses_stream_settlement_once<F, Fut>(
    background_settlement_scheduled: bool,
    terminal_settlement_secured: &mut bool,
    settle: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    if background_settlement_scheduled || *terminal_settlement_secured {
        return true;
    }
    *terminal_settlement_secured = settle().await;
    *terminal_settlement_secured
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn secure_responses_stream_settlement(
    state: &AppState,
    billing: &keycompute_billing::BillingService,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    response_id: Option<&str>,
    model: Option<String>,
    status: &str,
) -> bool {
    let durable = persist_terminal_responses_outbox(
        state,
        ctx,
        provider,
        account_id,
        response_id,
        false,
        model,
        status,
    )
    .await;
    let finalized =
        finalize_responses_billing_logged(state, billing, ctx, provider, account_id, status).await;
    let secured = state.pool.is_none() || durable || finalized;
    if !secured {
        tracing::error!(
            request_id = %ctx.request_id,
            status,
            "Responses stream settlement was neither persisted nor finalized"
        );
    }
    secured
}

pub(super) fn create_responses_stream(
    mut rx: tokio::sync::mpsc::Receiver<llm_protocol_provider::StreamEvent>,
    mut initial_event: Option<llm_protocol_provider::StreamEvent>,
    runtime: ResponsesStreamRuntime,
) -> impl Stream<Item = std::result::Result<Event, Infallible>> {
    let ResponsesStreamRuntime {
        ctx,
        provider,
        account_id,
        billing,
        lifecycle,
        state,
        client_previous_response_id,
        mut persist_response_affinity,
        mut reservations,
        body_permit,
        mut initial_status,
    } = runtime;
    let (sse_tx, sse_rx) =
        mpsc::channel(llm_protocol_provider::LARGE_NATIVE_EVENT_CHANNEL_CAPACITY);
    tokio::spawn(async move {
        let _body_permit = body_permit;
        let mut normalized_done = false;
        let mut client_connected = true;
        let mut first_content_recorded = false;
        let mut terminal_type: Option<String> = None;
        let mut raw_error_forwarded = false;
        let mut response_id = None;
        let mut conversation_id = None;
        let mut affinity_saved = false;
        let mut conversation_affinity_saved = false;
        let mut background_settlement_scheduled = false;
        let mut terminal_settlement_secured = false;
        let mut billing_ctx = Arc::clone(&ctx);
        let mut actual_model = None;
        let mut retain_reservations = false;
        let is_background = ctx
            .native_openai_responses_request
            .as_deref()
            .and_then(|body| body.get("background"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        loop {
            tokio::select! {
                _ = sse_tx.closed(), if client_connected => {
                    client_connected = false;
                    ctx.mark_client_disconnected();
                }
                event = async {
                    if initial_event.is_some() {
                        initial_event.take()
                    } else {
                        rx.recv().await
                    }
                } => {
                    crate::handlers::report_initial_stream_status(
                        &mut initial_status,
                        event.as_ref(),
                    );
                    let Some(event) = event else { break };
                    match event {
                        llm_protocol_provider::StreamEvent::Native {
                            event:
                                NativeStreamEvent::OpenAiResponsesSse {
                                    event: event_name,
                                    data: mut body,
                                    admission,
                                },
                        } => {
                                if let Some(stored) = body
                                    .pointer("/response/store")
                                    .and_then(Value::as_bool)
                                {
                                    persist_response_affinity &= stored;
                                }
                                patch_client_previous_response_id(
                                    &mut body,
                                    client_previous_response_id.as_deref(),
                                );
                                let _ = sanitize_upstream_responses_error(
                                    Some(&event_name),
                                    &mut body,
                                );
                                let body_type = body.get("type").and_then(Value::as_str);
                                if actual_model.is_none() {
                                    actual_model = response_model(&body).map(str::to_string);
                                }
                                let mut persistence_error = None;
                                if billing_ctx.model.is_empty()
                                    && actual_model.is_some()
                                {
                                    match resolved_responses_billing_context(
                                        &state,
                                        &ctx,
                                        actual_model.as_deref(),
                                    )
                                    .await
                                    {
                                        Ok(resolved) => billing_ctx = resolved,
                                        Err(error) => persistence_error = Some(error),
                                    }
                                }
                                if response_id.is_none() {
                                    response_id = response_event_resource_id(&body)
                                        .map(str::to_string);
                                }
                                if conversation_id.is_none() {
                                    conversation_id = conversation_resource_id(&body)
                                        .map(str::to_string);
                                }
                                let affinity_model = effective_response_affinity_model(
                                    actual_model.as_deref(),
                                    &billing_ctx,
                                );
                                if is_background
                                    && body.get("response").is_some()
                                    && response_id.is_none()
                                {
                                    persistence_error = Some(ApiError::Provider(
                                        "Background Responses event is missing its resource ID"
                                            .to_string(),
                                    ));
                                }
                                if persistence_error.is_none()
                                    && !affinity_saved
                                    && let Some(response_id) = response_id.as_deref()
                                {
                                    let (actual_provider, actual_account_id) =
                                        billing_ctx.billing_target(&provider, account_id);
                                    let settlement = if is_background {
                                        match background_settlement_value(
                                            &billing_ctx,
                                            &provider,
                                            account_id,
                                        ) {
                                            Ok(settlement) => Some(settlement),
                                            Err(error) => {
                                                persistence_error = Some(error);
                                                None
                                            }
                                        }
                                    } else {
                                        None
                                    };
                                    if persistence_error.is_none() {
                                        match save_response_affinity_if_stored(
                                            &state,
                                            persist_response_affinity,
                                            response_id,
                                            ResponsesAffinityRoute {
                                                tenant_id: billing_ctx.tenant_id,
                                                provider: actual_provider.clone(),
                                                model: affinity_model.clone(),
                                                account_id: actual_account_id,
                                            },
                                            settlement,
                                        )
                                        .await
                                        {
                                            Ok(settlement_durable) => {
                                                affinity_saved = true;
                                                if is_background && !settlement_durable {
                                                    spawn_background_settlement(
                                                        state.clone(),
                                                        Arc::clone(&billing_ctx),
                                                        provider.clone(),
                                                        account_id,
                                                        Arc::clone(&billing),
                                                        response_id.to_string(),
                                                    );
                                                }
                                                if is_background {
                                                    background_settlement_scheduled = true;
                                                }
                                                if let Some(conversation_id) = conversation_id.as_deref() {
                                                    match save_response_affinity(
                                                        &state,
                                                        conversation_id,
                                                        ResponsesResourceKind::Conversation,
                                                        ResponsesAffinityRoute {
                                                            tenant_id: billing_ctx.tenant_id,
                                                            provider: actual_provider,
                                                            model: affinity_model.clone(),
                                                            account_id: actual_account_id,
                                                        },
                                                        None,
                                                    )
                                                    .await
                                                    {
                                                        Ok(_) => conversation_affinity_saved = true,
                                                        Err(error) => persistence_error = Some(error),
                                                    }
                                                }
                                            }
                                            Err(error) => {
                                                if is_background {
                                                    spawn_background_settlement(
                                                        state.clone(),
                                                        Arc::clone(&billing_ctx),
                                                        provider.clone(),
                                                        account_id,
                                                        Arc::clone(&billing),
                                                        response_id.to_string(),
                                                    );
                                                    background_settlement_scheduled = true;
                                                }
                                                persistence_error = Some(error);
                                            }
                                        }
                                    }
                                }
                                if persistence_error.is_none()
                                    && affinity_saved
                                    && !conversation_affinity_saved
                                    && let Some(conversation_id) = conversation_id.as_deref()
                                {
                                    let (actual_provider, actual_account_id) =
                                        billing_ctx.billing_target(&provider, account_id);
                                    match save_response_affinity(
                                        &state,
                                        conversation_id,
                                        ResponsesResourceKind::Conversation,
                                        ResponsesAffinityRoute {
                                            tenant_id: billing_ctx.tenant_id,
                                            provider: actual_provider,
                                            model: affinity_model.clone(),
                                            account_id: actual_account_id,
                                        },
                                        None,
                                    )
                                    .await
                                    {
                                        Ok(_) => conversation_affinity_saved = true,
                                        Err(error) => persistence_error = Some(error),
                                    }
                                }
                                if persistence_error.is_some() {
                                    retain_reservations = response_id.is_some();
                                    normalized_done = true;
                                    if !background_settlement_scheduled {
                                        let _ = secure_responses_stream_settlement(
                                            &state,
                                            &billing,
                                            &billing_ctx,
                                            &provider,
                                            account_id,
                                            response_id.as_deref(),
                                            actual_model.clone(),
                                            "incomplete",
                                        )
                                        .await;
                                    }
                                    let _ = forward_sse_event(
                                        &sse_tx,
                                        &ctx,
                                        &mut client_connected,
                                        responses_error_event(
                                            "Responses state could not be durably persisted",
                                        ),
                                    )
                                    .await;
                                    crate::handlers::finish_client_response_trace(
                                        &lifecycle,
                                        &ctx,
                                        ClientResponseOutcome::ResponseFailed,
                                    )
                                    .await;
                                    break;
                                }
                                raw_error_forwarded |= event_name == "error" || body_type == Some("error");
                                if is_terminal_responses_event(&event_name, &body) {
                                    apply_response_event_usage(&billing_ctx, &body);
                                    terminal_type = body_type.map(str::to_string).or(Some(event_name.clone()));
                                    let status = terminal_type
                                        .as_deref()
                                        .map(terminal_billing_status)
                                        .unwrap_or("success");
                                    if !secure_responses_stream_settlement_once(
                                        background_settlement_scheduled,
                                        &mut terminal_settlement_secured,
                                        || secure_responses_stream_settlement(
                                            &state,
                                            &billing,
                                            &billing_ctx,
                                            &provider,
                                            account_id,
                                            response_id.as_deref(),
                                            actual_model.clone(),
                                            status,
                                        ),
                                    )
                                    .await
                                    {
                                        retain_reservations = response_id.is_some();
                                        normalized_done = true;
                                        let _ = forward_sse_event(
                                            &sse_tx,
                                            &ctx,
                                            &mut client_connected,
                                            responses_error_event(
                                                "Responses billing state could not be durably persisted",
                                            ),
                                        )
                                        .await;
                                        crate::handlers::finish_client_response_trace(
                                            &lifecycle,
                                            &ctx,
                                            ClientResponseOutcome::ResponseFailed,
                                        )
                                        .await;
                                        break;
                                    }
                                }
                                let sent = forward_admitted_sse_event(
                                    &sse_tx,
                                    &ctx,
                                    &mut client_connected,
                                    Event::default().event(event_name).data(body.to_string()),
                                    admission,
                                ).await;
                                if sent && !first_content_recorded {
                                    if let Err(error) = lifecycle
                                        .record_client_first_content(ctx.request_id, chrono::Utc::now())
                                        .await
                                    {
                                        tracing::warn!(request_id = %ctx.request_id, %error, "failed to record client first content");
                                    }
                                    first_content_recorded = true;
                                }
                        }
                        llm_protocol_provider::StreamEvent::Done => {
                            normalized_done = true;
                            let status = terminal_type
                                .as_deref()
                                .map(terminal_billing_status)
                                .unwrap_or("success");
                            // A durable background job is the sole settlement
                            // owner once it has been scheduled. Finalizing here
                            // as well would race the poller and could attempt a
                            // second balance deduction for the same request.
                            if !secure_responses_stream_settlement_once(
                                background_settlement_scheduled,
                                &mut terminal_settlement_secured,
                                || secure_responses_stream_settlement(
                                    &state,
                                    &billing,
                                    &billing_ctx,
                                    &provider,
                                    account_id,
                                    response_id.as_deref(),
                                    actual_model.clone(),
                                    status,
                                ),
                            )
                            .await
                            {
                                retain_reservations = response_id.is_some();
                                let _ = forward_sse_event(
                                    &sse_tx,
                                    &ctx,
                                    &mut client_connected,
                                    responses_error_event(
                                        "Responses billing state could not be durably persisted",
                                    ),
                                )
                                .await;
                                crate::handlers::finish_client_response_trace(
                                    &lifecycle,
                                    &ctx,
                                    ClientResponseOutcome::ResponseFailed,
                                )
                                .await;
                                break;
                            }
                            let outcome = if status == "success" {
                                ClientResponseOutcome::Succeeded
                            } else {
                                ClientResponseOutcome::ResponseFailed
                            };
                            crate::handlers::finish_client_response_trace(&lifecycle, &ctx, outcome)
                                .await;
                            break;
                        }
                        llm_protocol_provider::StreamEvent::Error { .. } => {
                            normalized_done = true;
                            if !secure_responses_stream_settlement_once(
                                background_settlement_scheduled,
                                &mut terminal_settlement_secured,
                                || secure_responses_stream_settlement(
                                    &state,
                                    &billing,
                                    &billing_ctx,
                                    &provider,
                                    account_id,
                                    response_id.as_deref(),
                                    actual_model.clone(),
                                    "error",
                                ),
                            )
                            .await
                            {
                                retain_reservations = response_id.is_some();
                                let _ = forward_sse_event(
                                    &sse_tx,
                                    &ctx,
                                    &mut client_connected,
                                    responses_error_event(
                                        "Responses billing state could not be durably persisted",
                                    ),
                                )
                                .await;
                                crate::handlers::finish_client_response_trace(
                                    &lifecycle,
                                    &ctx,
                                    ClientResponseOutcome::ResponseFailed,
                                )
                                .await;
                                break;
                            }
                            if !raw_error_forwarded {
                                let _ = forward_sse_event(
                                    &sse_tx,
                                    &ctx,
                                    &mut client_connected,
                                    responses_error_event("Upstream request failed"),
                                ).await;
                            }
                            crate::handlers::finish_client_response_trace(
                                &lifecycle,
                                &ctx,
                                ClientResponseOutcome::ResponseFailed,
                            ).await;
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

        if !normalized_done {
            let settlement_secured = background_settlement_scheduled
                || secure_responses_stream_settlement(
                    &state,
                    &billing,
                    &billing_ctx,
                    &provider,
                    account_id,
                    response_id.as_deref(),
                    actual_model.clone(),
                    "incomplete",
                )
                .await;
            if !settlement_secured {
                retain_reservations = response_id.is_some();
            }
            let _ = forward_sse_event(
                &sse_tx,
                &ctx,
                &mut client_connected,
                responses_error_event(if settlement_secured {
                    "Upstream stream ended before a terminal Responses event"
                } else {
                    "Responses billing state could not be durably persisted"
                }),
            )
            .await;
            crate::handlers::finish_client_response_trace(
                &lifecycle,
                &ctx,
                ClientResponseOutcome::ResponseFailed,
            )
            .await;
        }
        if !retain_reservations {
            reservations.release().await;
        } else if !reservations.is_empty() {
            tracing::warn!(
                request_id = %ctx.request_id,
                "retaining Responses account reservation after a persistence failure"
            );
            reservations.retain_until_expiry();
        }
    });
    futures::stream::unfold(
        (sse_rx, None::<LargeBodyPermit>),
        |(mut rx, previous_admission)| async move {
            // The next poll occurs only after Axum has consumed the previous
            // event. Retaining its permit in the stream state therefore keeps
            // the global slot while the encoded event remains resident.
            drop(previous_admission);
            rx.recv()
                .await
                .map(|admitted| (Ok(admitted.event), (rx, admitted.admission)))
        },
    )
}

pub(super) struct AdmittedSseEvent {
    pub(super) event: Event,
    pub(super) admission: Option<LargeBodyPermit>,
}

pub(super) async fn forward_sse_event(
    tx: &mpsc::Sender<AdmittedSseEvent>,
    ctx: &RequestContext,
    connected: &mut bool,
    event: Event,
) -> bool {
    forward_admitted_sse_event(tx, ctx, connected, event, None).await
}

pub(super) async fn forward_admitted_sse_event(
    tx: &mpsc::Sender<AdmittedSseEvent>,
    ctx: &RequestContext,
    connected: &mut bool,
    event: Event,
    admission: Option<LargeBodyPermit>,
) -> bool {
    if !*connected {
        return false;
    }
    let sent = tokio::time::timeout(
        SSE_SEND_TIMEOUT,
        tx.send(AdmittedSseEvent { event, admission }),
    )
    .await
    .map(|result| result.is_ok())
    .unwrap_or(false);
    if !sent {
        *connected = false;
        ctx.mark_client_disconnected();
    }
    sent
}
