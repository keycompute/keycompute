//! Durable background polling, usage capture and settlement replay.

use super::*;

pub(crate) const BACKGROUND_SETTLEMENT_MAX: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BackgroundSettlement {
    pub(super) request_id: uuid::Uuid,
    #[serde(default)]
    pub(super) billing_request_id: Option<uuid::Uuid>,
    pub(super) tenant_id: uuid::Uuid,
    pub(super) user_id: uuid::Uuid,
    pub(super) produce_ai_key_id: uuid::Uuid,
    pub(super) model: String,
    pub(super) provider: String,
    pub(super) account_id: uuid::Uuid,
    pub(super) pricing_snapshot: keycompute_types::PricingSnapshot,
    pub(super) started_at: chrono::DateTime<chrono::Utc>,
    pub(super) input_tokens: u32,
    pub(super) output_tokens: u32,
    #[serde(default)]
    pub(super) input_tokens_finalized: bool,
    #[serde(default)]
    pub(super) output_tokens_finalized: bool,
    #[serde(default)]
    pub(super) openai_beta: Option<String>,
    #[serde(default)]
    pub(super) terminal_status: Option<String>,
    // A terminal status without a terminal timestamp is the durable marker for
    // a pending response that exhausted its settlement deadline. Its usage is
    // still billed, but must not be shifted into the current TPM window.
    #[serde(default)]
    pub(super) terminal_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) deadline_at: chrono::DateTime<chrono::Utc>,
    pub(super) attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResponsesTpmTiming {
    LedgerFinishedAt,
    TerminalAt(chrono::DateTime<chrono::Utc>),
    Skip,
}

pub(super) fn background_settlement_tpm_timing(
    settlement: &BackgroundSettlement,
    now: chrono::DateTime<chrono::Utc>,
) -> ResponsesTpmTiming {
    if let Some(terminal_at) = settlement.terminal_at {
        return ResponsesTpmTiming::TerminalAt(terminal_at);
    }
    if settlement.terminal_status.is_some() || now >= settlement.deadline_at {
        return ResponsesTpmTiming::Skip;
    }
    ResponsesTpmTiming::LedgerFinishedAt
}

pub(super) enum BackgroundPollOutcome {
    Response {
        body: Value,
        billing_status: Option<&'static str>,
        admission: Option<LargeBodyPermit>,
    },
    Retry,
    TerminalHttpError(u16),
}

pub(super) fn background_settlement_value(
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
) -> Result<Value> {
    let (provider, account_id) = ctx.billing_target(provider, account_id);
    let (input_tokens, output_tokens) = ctx.usage_snapshot();
    let settlement = BackgroundSettlement {
        request_id: ctx.request_id,
        billing_request_id: Some(ctx.billing_request_id),
        tenant_id: ctx.tenant_id,
        user_id: ctx.user_id,
        produce_ai_key_id: ctx.produce_ai_key_id,
        model: ctx.model.clone(),
        provider,
        account_id,
        pricing_snapshot: ctx.pricing_snapshot.clone(),
        started_at: ctx.started_at,
        input_tokens,
        output_tokens,
        input_tokens_finalized: ctx.is_input_finalized(),
        output_tokens_finalized: ctx.is_output_finalized(),
        openai_beta: ctx
            .native_openai_responses_headers
            .get("openai-beta")
            .cloned(),
        terminal_status: None,
        terminal_at: None,
        deadline_at: chrono::Utc::now()
            + chrono::Duration::from_std(BACKGROUND_SETTLEMENT_MAX)
                .unwrap_or(chrono::Duration::hours(24)),
        attempt: 0,
    };
    serde_json::to_value(settlement).map_err(|error| {
        ApiError::Internal(format!(
            "Failed to serialize background Responses settlement: {error}"
        ))
    })
}

pub(super) fn terminal_settlement_value(
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    status: &str,
) -> Result<Value> {
    terminal_settlement_value_with_tpm_timing(
        ctx,
        provider,
        account_id,
        status,
        ResponsesTpmTiming::LedgerFinishedAt,
    )
}

pub(super) fn terminal_settlement_value_with_tpm_timing(
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    status: &str,
    tpm_timing: ResponsesTpmTiming,
) -> Result<Value> {
    let mut value = background_settlement_value(ctx, provider, account_id)?;
    value["terminal_status"] = Value::String(status.to_string());
    let terminal_at = match tpm_timing {
        ResponsesTpmTiming::LedgerFinishedAt => chrono::Utc::now(),
        ResponsesTpmTiming::TerminalAt(terminal_at) => terminal_at,
        ResponsesTpmTiming::Skip => return Ok(value),
    };
    value["terminal_at"] = serde_json::to_value(terminal_at).map_err(|error| {
        ApiError::Internal(format!(
            "Failed to serialize Responses terminal timestamp: {error}"
        ))
    })?;
    Ok(value)
}

pub(super) fn settlement_billing_request_id(settlement: &BackgroundSettlement) -> uuid::Uuid {
    settlement
        .billing_request_id
        .unwrap_or(settlement.request_id)
}

pub(super) fn settlement_affinity_account_id(
    settlement: &BackgroundSettlement,
) -> Option<uuid::Uuid> {
    (!settlement.account_id.is_nil()).then_some(settlement.account_id)
}

pub(super) fn background_billing_context(
    settlement: &BackgroundSettlement,
    input_tokens: u32,
    output_tokens: u32,
) -> RequestContext {
    let mut ctx = RequestContext::new(
        settlement.request_id,
        settlement.user_id,
        settlement.tenant_id,
        settlement.produce_ai_key_id,
        settlement.model.clone(),
        Vec::new(),
        false,
        settlement.pricing_snapshot.clone(),
    );
    ctx.started_at = settlement.started_at;
    ctx.billing_request_id = settlement
        .billing_request_id
        .unwrap_or(settlement.request_id);
    if settlement.input_tokens_finalized {
        ctx.set_input_tokens(input_tokens);
    } else {
        ctx.set_input_tokens_estimate(input_tokens);
    }
    if settlement.output_tokens_finalized {
        ctx.set_output_tokens(output_tokens);
    } else {
        ctx.set_output_tokens_estimate(output_tokens);
    }
    ctx
}

pub(super) fn spawn_background_settlement(
    state: AppState,
    ctx: Arc<RequestContext>,
    provider: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
    response_id: String,
) {
    // Bind polling to the account that actually executed this request. Never
    // resolve it through the returned resource ID: a persistence collision can
    // mean that ID is already owned by a different account.
    let (provider, account_id) = ctx.billing_target(&provider, account_id);
    // The poller needs shared usage/identity state but never the original
    // request payload. Strip large native bodies before a background job can
    // retain them for its 24-hour retry window.
    let ctx = background_settlement_context(&ctx);
    let account = match background_account_snapshot(&ctx, &provider, account_id) {
        Ok(account) => account,
        Err(error) => {
            tracing::error!(request_id = %ctx.request_id, %error, "failed to snapshot background Responses account");
            tokio::spawn(async move {
                finalize_responses_billing_logged(
                    &state,
                    &billing,
                    &ctx,
                    &provider,
                    account_id,
                    "incomplete",
                )
                .await;
            });
            return;
        }
    };
    tokio::spawn(async move {
        settle_background_response(
            state,
            ctx,
            provider,
            account_id,
            billing,
            response_id,
            account,
        )
        .await;
    });
}

pub(super) fn background_account_snapshot(
    ctx: &RequestContext,
    expected_provider: &str,
    account_id: uuid::Uuid,
) -> Result<ResolvedResponsesAccount> {
    let Some(ExecutionTarget::ProviderAccount {
        provider,
        account_id: accepted_account_id,
        endpoint,
        upstream_api_key,
    }) = ctx.accepted_execution_target()
    else {
        return Err(ApiError::Internal(
            "The background Responses execution target was not retained".to_string(),
        ));
    };
    if accepted_account_id != account_id
        || !provider.eq_ignore_ascii_case(expected_provider)
        || !provider.eq_ignore_ascii_case("openai")
    {
        return Err(ApiError::Conflict(
            "The background Responses execution target does not match its billing account"
                .to_string(),
        ));
    }
    Ok(ResolvedResponsesAccount {
        provider,
        model: None,
        account_id,
        endpoint,
        api_key: upstream_api_key.expose().to_string(),
    })
}

pub(super) fn background_settlement_context(ctx: &RequestContext) -> Arc<RequestContext> {
    Arc::new(ctx.clone_without_request_payloads())
}

/// Start replica-safe Responses maintenance. Durable settlement jobs are
/// leased with `SKIP LOCKED`; expired affinity rows are removed periodically.
pub(super) fn run_responses_maintenance(state: AppState) {
    let Some(pool) = state.pool.clone() else {
        return;
    };
    tokio::spawn(async move {
        let settlement_capacity = Arc::new(Semaphore::new(RESPONSES_SETTLEMENT_CONCURRENCY));
        let mut cleanup_tick = tokio::time::interval(Duration::from_secs(5 * 60));
        let mut settlement_tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = cleanup_tick.tick() => {
                    let now = chrono::Utc::now().timestamp();
                    state
                        .responses_affinity
                        .write()
                        .await
                        .retain(|_, affinity| affinity.expires_at_unix > now);
                    match ResponseAffinity::delete_expired(pool.as_ref()).await {
                        Ok(count) if count > 0 => tracing::info!(count, "deleted expired Responses affinities"),
                        Ok(_) => {}
                        Err(error) => tracing::warn!(%error, "failed to delete expired Responses affinities"),
                    }
                    match ResponsesIdempotencyClaim::expire_completed_responses(pool.as_ref()).await {
                        Ok(count) if count > 0 => tracing::info!(count, "expired cached Responses idempotency results"),
                        Ok(_) => {}
                        Err(error) => tracing::warn!(%error, "failed to expire Responses idempotency results"),
                    }
                }
                _ = settlement_tick.tick() => {
                    let permits = take_available_settlement_permits(
                        &settlement_capacity,
                        RESPONSES_SETTLEMENT_CONCURRENCY,
                    );
                    if permits.is_empty() {
                        continue;
                    }
                    let lease_until = chrono::Utc::now() + chrono::Duration::minutes(5);
                    match ResponseAffinity::claim_due_settlements(
                        pool.as_ref(),
                        permits.len() as u64,
                        lease_until,
                    ).await {
                        Ok(jobs) => {
                            for (job, permit) in jobs.into_iter().zip(permits) {
                                let worker_state = state.clone();
                                tokio::spawn(async move {
                                    let _permit = permit;
                                    settle_background_job(&worker_state, job).await;
                                });
                            }
                        }
                        Err(error) => tracing::warn!(%error, "failed to lease background Responses settlements"),
                    }
                }
            }
        }
    });
}

pub(super) fn take_available_settlement_permits(
    semaphore: &Arc<Semaphore>,
    limit: usize,
) -> Vec<OwnedSemaphorePermit> {
    let mut permits = Vec::with_capacity(limit.min(semaphore.available_permits()));
    while permits.len() < limit {
        let Ok(permit) = Arc::clone(semaphore).try_acquire_owned() else {
            break;
        };
        permits.push(permit);
    }
    permits
}

pub(super) async fn settle_background_job(state: &AppState, affinity: ResponseAffinity) {
    let Some(pool) = state.pool.as_deref() else {
        return;
    };
    let Some(value) = affinity.settlement.clone() else {
        return;
    };
    let mut settlement: BackgroundSettlement = match serde_json::from_value(value) {
        Ok(settlement) => settlement,
        Err(error) => {
            tracing::error!(%error, response_id = %affinity.response_id, "invalid background Responses settlement");
            // Never discard unrecognized durable billing work. Keeping the
            // settlement also preserves the account FK guard and lets an
            // operator repair/replay it after a deployment or data issue. The
            // current lease bounds the retry/logging cadence.
            return;
        }
    };
    if settlement.tenant_id != affinity.tenant_id
        || settlement_affinity_account_id(&settlement) != affinity.account_id
        || settlement.provider != affinity.provider
    {
        tracing::error!(response_id = %affinity.response_id, "background Responses settlement ownership mismatch");
        // An ownership mismatch indicates corrupted or unexpectedly rewritten
        // durable work. Clearing it would silently lose billing and remove the
        // account deletion guard, so leave it leased for operator recovery.
        return;
    }
    match keycompute_db::UsageLog::find_by_billing_request_id_on_writer(
        pool,
        settlement_billing_request_id(&settlement),
    )
    .await
    {
        Ok(Some(usage_log)) => {
            // The immutable ledger can commit before balance/distribution/tips
            // finish. Replay those idempotent effects before acknowledging the
            // durable job; otherwise a crash in that interval loses money.
            let (input_tokens, output_tokens) = authoritative_responses_token_counts(
                usage_log.input_tokens,
                usage_log.output_tokens,
                settlement.input_tokens,
                settlement.output_tokens,
            );
            let ctx = background_billing_context(&settlement, input_tokens, output_tokens);
            match state
                .billing
                .replay_saved_usage_effects(&ctx, &usage_log, settlement.user_id)
                .await
            {
                Ok(()) => {
                    let tpm_timing =
                        background_settlement_tpm_timing(&settlement, chrono::Utc::now());
                    if let Err(error) = record_responses_token_usage_for_timing(
                        state,
                        &ctx,
                        input_tokens.saturating_add(output_tokens),
                        tpm_timing,
                        usage_log.finished_at,
                    )
                    .await
                    {
                        tracing::error!(%error, request_id = %settlement.request_id, "failed to record background Responses TPM usage");
                        reschedule_background_job(pool, &affinity, settlement).await;
                        return;
                    }
                    clear_claimed_background_job(pool, &affinity).await;
                }
                Err(error) => {
                    tracing::error!(%error, request_id = %settlement.request_id, "failed to replay background Responses post-ledger settlement");
                    reschedule_background_job(pool, &affinity, settlement).await;
                }
            }
            return;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, request_id = %settlement.request_id, "failed to check background settlement ledger");
            reschedule_background_job(pool, &affinity, settlement).await;
            return;
        }
    }

    let now = chrono::Utc::now();
    let mut terminal_status = settlement.terminal_status.clone();
    let mut tpm_timing = background_settlement_tpm_timing(&settlement, now);
    if terminal_status.is_some() {
        // Terminal synchronous requests use this row as a durable ledger/TPM
        // outbox and already carry their final usage. A terminal status without
        // a timestamp is a rescheduled deadline-exhausted background job.
    } else if now >= settlement.deadline_at {
        terminal_status = Some("incomplete".to_string());
        tpm_timing = ResponsesTpmTiming::Skip;
    } else {
        match background_poll_account(state, &affinity, settlement.openai_beta.as_deref()).await {
            Ok(BackgroundPollOutcome::Response {
                body,
                billing_status,
                admission: _admission,
            }) => {
                let usage = response_usage_update(&body);
                if let Some(input_tokens) = usage.input_tokens {
                    settlement.input_tokens = input_tokens;
                    settlement.input_tokens_finalized = true;
                }
                if let Some(output_tokens) = usage.output_tokens {
                    settlement.output_tokens = output_tokens;
                    settlement.output_tokens_finalized = !usage.output_is_estimate;
                }
                terminal_status = billing_status.map(str::to_string);
                if terminal_status.is_some() {
                    // A worker may observe this response long after it actually
                    // finished. Preserve the provider timestamp so stale usage
                    // is not shifted into the current TPM window.
                    settlement.terminal_at = background_response_terminal_at(&body);
                    tpm_timing = settlement
                        .terminal_at
                        .map(ResponsesTpmTiming::TerminalAt)
                        .unwrap_or(ResponsesTpmTiming::LedgerFinishedAt);
                }
            }
            Ok(BackgroundPollOutcome::Retry) => {}
            Ok(BackgroundPollOutcome::TerminalHttpError(status)) => {
                tracing::warn!(
                    response_id = %affinity.response_id,
                    status,
                    "background Responses poll returned a permanent HTTP error"
                );
                terminal_status = Some("error".to_string());
                tpm_timing = ResponsesTpmTiming::LedgerFinishedAt;
            }
            Err(error) => {
                tracing::warn!(%error, response_id = %affinity.response_id, "background Responses poll failed")
            }
        }
    }

    let Some(status) = terminal_status.as_deref() else {
        reschedule_background_job(pool, &affinity, settlement).await;
        return;
    };
    settlement.terminal_status = Some(status.to_string());
    if tpm_timing == ResponsesTpmTiming::LedgerFinishedAt {
        let terminal_at = *settlement.terminal_at.get_or_insert_with(chrono::Utc::now);
        tpm_timing = ResponsesTpmTiming::TerminalAt(terminal_at);
    }
    let ctx = background_billing_context(
        &settlement,
        settlement.input_tokens,
        settlement.output_tokens,
    );
    let usage_log = match state
        .billing
        .finalize_and_save(&ctx, &settlement.provider, settlement.account_id, status)
        .await
    {
        Ok(usage_log) => usage_log,
        Err(error) => {
            tracing::error!(%error, request_id = %settlement.request_id, "background Responses billing failed");
            reschedule_background_job(pool, &affinity, settlement).await;
            return;
        }
    };
    // `finalize_and_save` can return an earlier idempotent ledger row. Use that
    // immutable row, not the possibly stale settlement snapshot, for TPM.
    let (ledger_input_tokens, ledger_output_tokens) = authoritative_responses_token_counts(
        usage_log.input_tokens,
        usage_log.output_tokens,
        settlement.input_tokens,
        settlement.output_tokens,
    );
    if let Err(error) = state
        .billing
        .replay_saved_usage_effects(&ctx, &usage_log, settlement.user_id)
        .await
    {
        tracing::error!(%error, request_id = %settlement.request_id, "background Responses post-ledger settlement failed");
        reschedule_background_job(pool, &affinity, settlement).await;
        return;
    }
    if let Err(error) = record_responses_token_usage_for_timing(
        state,
        &ctx,
        ledger_input_tokens.saturating_add(ledger_output_tokens),
        tpm_timing,
        usage_log.finished_at,
    )
    .await
    {
        tracing::error!(%error, request_id = %settlement.request_id, "failed to record background Responses TPM usage");
        reschedule_background_job(pool, &affinity, settlement).await;
        return;
    }
    clear_claimed_background_job(pool, &affinity).await;
}

pub(super) fn authoritative_responses_token_counts(
    ledger_input_tokens: i32,
    ledger_output_tokens: i32,
    fallback_input_tokens: u32,
    fallback_output_tokens: u32,
) -> (u32, u32) {
    (
        u32::try_from(ledger_input_tokens).unwrap_or(fallback_input_tokens),
        u32::try_from(ledger_output_tokens).unwrap_or(fallback_output_tokens),
    )
}

pub(super) async fn background_poll_account(
    state: &AppState,
    affinity: &ResponseAffinity,
    openai_beta: Option<&str>,
) -> Result<BackgroundPollOutcome> {
    let pool = state
        .pool
        .as_deref()
        .ok_or_else(|| ApiError::ServiceUnavailable("Database not configured".into()))?;
    let account_id = affinity.account_id.ok_or_else(|| {
        ApiError::Internal("Background Responses settlement has no owning account".into())
    })?;
    let account = Account::find_by_id_for_key_share(pool, account_id)
        .await
        .map_err(|error| ApiError::Internal(format!("Failed to load Responses account: {error}")))?
        .ok_or_else(|| ApiError::NotFound("Responses account not found".into()))?;
    let endpoint = if account.endpoint.is_empty() {
        ProtocolType::parse(&account.provider)
            .map(|protocol| protocol.default_endpoint().to_string())
            .ok_or_else(|| ApiError::Conflict("Invalid Responses account protocol".into()))?
    } else {
        account.endpoint
    };
    let api_key = crate::handlers::admin_account::decrypt_account_api_key(
        &account.upstream_api_key_encrypted,
    )?;
    let client = state
        .http_proxy
        .client_for_provider_and_account(&account.provider, Some(account.id));
    let mut headers = vec![("Authorization".to_string(), format!("Bearer {api_key}"))];
    if let Some(openai_beta) = openai_beta {
        headers.push(("openai-beta".to_string(), openai_beta.to_string()));
    }
    let response = client
        .request_json_passthrough(
            JsonRequestMethod::Get,
            &upstream_resource_url(&endpoint, "responses", &affinity.response_id, ""),
            headers,
            None,
            false,
        )
        .await
        .map_err(crate::error::map_execution_error)?;
    if !(200..300).contains(&response.meta.status) {
        return Ok(
            if background_poll_status_is_retryable(response.meta.status) {
                BackgroundPollOutcome::Retry
            } else {
                BackgroundPollOutcome::TerminalHttpError(response.meta.status)
            },
        );
    }
    let PassthroughBody::Full(body) = response.body else {
        return Ok(BackgroundPollOutcome::Retry);
    };
    let (body, mut admission) = body.into_parts();
    admit_responses_json_parse(&body, &mut admission)?;
    let body = serde_json::from_str(&body).map_err(|error| {
        ApiError::Provider(format!("Invalid background Responses body: {error}"))
    })?;
    let billing_status = validate_background_response(&affinity.response_id, &body)?;
    Ok(BackgroundPollOutcome::Response {
        body,
        billing_status,
        admission,
    })
}

pub(super) fn background_poll_status_is_retryable(status: u16) -> bool {
    matches!(status, 404 | 408 | 409 | 429) || status >= 500
}

pub(super) fn background_poll_parse_error_is_retryable(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::ServiceUnavailable(message)
            if message == RESPONSES_JSON_PROCESSING_CAPACITY_MESSAGE
    )
}

pub(super) fn background_response_terminal_at(
    body: &Value,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let completed_at = body.get("completed_at")?.as_i64()?;
    chrono::DateTime::from_timestamp(completed_at, 0)
}

/// Validate a retrieved response before its usage can affect an immutable
/// billing settlement. A configured compatibility endpoint is still an
/// untrusted protocol peer: a successful HTTP status must not let it attach a
/// different response's usage to this job or turn arbitrary JSON into a
/// terminal result.
pub(super) fn validate_background_response(
    expected_response_id: &str,
    body: &Value,
) -> Result<Option<&'static str>> {
    let object = body.as_object().ok_or_else(|| {
        ApiError::Provider("Background Responses body must be a JSON object".to_string())
    })?;
    if object.get("object").and_then(Value::as_str) != Some("response") {
        return Err(ApiError::Provider(
            "Background Responses body has an invalid object type".to_string(),
        ));
    }
    if object.get("id").and_then(Value::as_str) != Some(expected_response_id) {
        return Err(ApiError::Provider(
            "Background Responses body does not match the requested resource".to_string(),
        ));
    }
    if !object.get("output").is_some_and(Value::is_array) {
        return Err(ApiError::Provider(
            "Background Responses body output must be an array".to_string(),
        ));
    }
    if let Some(usage) = object.get("usage").filter(|usage| !usage.is_null()) {
        let usage = usage.as_object().ok_or_else(|| {
            ApiError::Provider("Background Responses usage must be an object".to_string())
        })?;
        for field in ["input_tokens", "output_tokens"] {
            if usage
                .get(field)
                .and_then(Value::as_u64)
                .and_then(|tokens| u32::try_from(tokens).ok())
                .is_none()
            {
                return Err(ApiError::Provider(format!(
                    "Background Responses usage.{field} must be a u32"
                )));
            }
        }
    }
    match object.get("status").and_then(Value::as_str) {
        Some("queued" | "in_progress") => Ok(None),
        Some("completed") => Ok(Some("success")),
        Some("incomplete") => Ok(Some("incomplete")),
        Some("failed" | "cancelled") => Ok(Some("error")),
        _ => Err(ApiError::Provider(
            "Background Responses body has an invalid status".to_string(),
        )),
    }
}

pub(super) async fn reschedule_background_job(
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
    mut settlement: BackgroundSettlement,
) {
    let Some(expected_lease_until) = affinity.settlement_lease_until else {
        tracing::error!(response_id = %affinity.response_id, "claimed background Responses settlement has no lease");
        return;
    };
    settlement.attempt = settlement.attempt.saturating_add(1);
    let delay = 1_i64 << settlement.attempt.min(3);
    let value = match serde_json::to_value(&settlement) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to serialize background Responses retry");
            return;
        }
    };
    match ResponseAffinity::reschedule_claimed_settlement(
        pool,
        affinity.tenant_id,
        &affinity.response_id,
        value,
        chrono::Utc::now() + chrono::Duration::seconds(delay),
        expected_lease_until,
    )
    .await
    {
        Ok(0) => {
            tracing::debug!(response_id = %affinity.response_id, "background Responses settlement lease was superseded before reschedule")
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, response_id = %affinity.response_id, "failed to reschedule background Responses settlement")
        }
    }
}

pub(super) async fn clear_claimed_background_job(
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
) {
    let Some(expected_lease_until) = affinity.settlement_lease_until else {
        tracing::error!(response_id = %affinity.response_id, "claimed background Responses settlement has no lease");
        return;
    };
    match ResponseAffinity::clear_claimed_settlement(
        pool,
        affinity.tenant_id,
        &affinity.response_id,
        expected_lease_until,
    )
    .await
    {
        Ok(0) => {
            tracing::debug!(response_id = %affinity.response_id, "background Responses settlement lease was superseded before acknowledgement")
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, response_id = %affinity.response_id, "failed to acknowledge background Responses settlement")
        }
    }
}

/// A non-streaming `background: true` create call returns while generation is
/// still queued or in progress. Continue polling the owning account so the
/// eventual exact usage is charged even if the client never retrieves it.
pub(super) async fn settle_background_response(
    state: AppState,
    ctx: Arc<RequestContext>,
    provider: String,
    account_id: uuid::Uuid,
    billing: Arc<keycompute_billing::BillingService>,
    response_id: String,
    account: ResolvedResponsesAccount,
) {
    let deadline = tokio::time::Instant::now() + BACKGROUND_SETTLEMENT_MAX;
    let mut delay = Duration::from_secs(1);
    let mut final_status = "incomplete";
    let mut tpm_timing = ResponsesTpmTiming::Skip;

    let client = state
        .http_proxy
        .client_for_provider_and_account(&account.provider, Some(account.account_id));
    let url = upstream_resource_url(&account.endpoint, "responses", &response_id, "");

    while tokio::time::Instant::now() < deadline {
        let mut headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", account.api_key),
        )];
        if let Some(openai_beta) = ctx.native_openai_responses_headers.get("openai-beta") {
            headers.push(("openai-beta".to_string(), openai_beta.clone()));
        }
        let response = client
            .request_json_passthrough(JsonRequestMethod::Get, &url, headers, None, false)
            .await;
        match response {
            Ok(response) if (200..300).contains(&response.meta.status) => {
                let PassthroughBody::Full(body) = response.body else {
                    tracing::warn!(request_id = %ctx.request_id, "background Responses poll unexpectedly returned an SSE body");
                    tpm_timing = ResponsesTpmTiming::LedgerFinishedAt;
                    break;
                };
                let (body, mut admission) = body.into_parts();
                match admit_responses_json_parse(&body, &mut admission) {
                    Err(error) if background_poll_parse_error_is_retryable(&error) => {
                        tracing::warn!(request_id = %ctx.request_id, %error, "background Responses poll JSON processing capacity is temporarily exhausted");
                    }
                    Err(error) => {
                        tracing::warn!(request_id = %ctx.request_id, %error, "background Responses poll body exceeded JSON memory limits");
                        tpm_timing = ResponsesTpmTiming::LedgerFinishedAt;
                        break;
                    }
                    Ok(()) => {
                        let _admission = admission;
                        let body: Value = match serde_json::from_str(&body) {
                            Ok(body) => body,
                            Err(error) => {
                                tracing::warn!(request_id = %ctx.request_id, %error, "failed to parse background Responses poll body");
                                tpm_timing = ResponsesTpmTiming::LedgerFinishedAt;
                                break;
                            }
                        };
                        match validate_background_response(&response_id, &body) {
                            Ok(None) => {}
                            Ok(Some(status)) => {
                                final_status = status;
                                tpm_timing = background_response_terminal_at(&body)
                                    .map(ResponsesTpmTiming::TerminalAt)
                                    .unwrap_or(ResponsesTpmTiming::LedgerFinishedAt);
                                apply_response_usage(&ctx, &body);
                                break;
                            }
                            Err(error) => {
                                // A protocol-invalid 2xx response is not evidence that
                                // this resource completed. Keep polling until the
                                // bounded deadline instead of charging unrelated usage
                                // or prematurely acknowledging the request.
                                tracing::warn!(request_id = %ctx.request_id, %error, "background Responses poll returned an invalid resource");
                            }
                        }
                    }
                }
            }
            Ok(response)
                if response.meta.status == 404
                    || response.meta.status == 408
                    || response.meta.status == 409
                    || response.meta.status == 429
                    || response.meta.status >= 500 =>
            {
                // Background resources can be briefly unavailable immediately
                // after creation; retry transient status codes with a bounded
                // backoff, without ever reissuing the paid create request.
            }
            Ok(response) => {
                tracing::warn!(request_id = %ctx.request_id, status = response.meta.status, "background Responses poll failed");
                final_status = "error";
                tpm_timing = ResponsesTpmTiming::LedgerFinishedAt;
                break;
            }
            Err(error) => {
                tracing::warn!(request_id = %ctx.request_id, %error, "background Responses poll transport failure");
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(10));
    }
    finalize_responses_billing_logged_with_tpm_timing(
        &state,
        &billing,
        &ctx,
        &provider,
        account_id,
        final_status,
        tpm_timing,
    )
    .await;
}

pub(super) fn response_is_background_pending(body: &Value) -> bool {
    matches!(
        body.get("status").and_then(Value::as_str),
        Some("queued" | "in_progress")
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResponseUsageUpdate {
    pub(super) input_tokens: Option<u32>,
    pub(super) output_tokens: Option<u32>,
    pub(super) output_is_estimate: bool,
}

pub(super) fn response_usage_update(body: &Value) -> ResponseUsageUpdate {
    let usage = body.get("usage").and_then(Value::as_object);
    let input_tokens = usage
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        // Preserve the request-side estimate for compatibility gateways that
        // use zero to represent an unavailable input count.
        .filter(|tokens| *tokens > 0);
    let exact_output = usage
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let estimated_output = exact_output
        .is_none()
        .then(|| llm_gateway::estimate_responses_output_tokens(body));
    let estimated_output = estimated_output.filter(|tokens| *tokens > 0);
    ResponseUsageUpdate {
        input_tokens,
        output_tokens: exact_output.or(estimated_output),
        output_is_estimate: exact_output.is_none() && estimated_output.is_some(),
    }
}

pub(super) fn apply_response_usage(ctx: &RequestContext, body: &Value) {
    let usage = response_usage_update(body);
    if let Some(input_tokens) = usage.input_tokens {
        ctx.set_input_tokens(input_tokens);
    }
    if let Some(output_tokens) = usage.output_tokens {
        if usage.output_is_estimate {
            // Replace any per-delta estimate with the complete terminal body.
            // Exact Provider usage, when available, always takes precedence.
            ctx.set_output_tokens_estimate(output_tokens);
        } else {
            ctx.set_output_tokens(output_tokens);
        }
    }
}

pub(super) fn apply_response_event_usage(ctx: &RequestContext, body: &Value) {
    apply_response_usage(ctx, body.get("response").unwrap_or(body));
}
