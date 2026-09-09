//! Durable background polling, usage capture and settlement replay.

use super::*;

pub(crate) const BACKGROUND_SETTLEMENT_MAX: Duration = Duration::from_secs(24 * 60 * 60);

/// Startup must finish reconstructing every durable in-flight TPM prediction
/// before generation routes open. Keep this below the backend's one-minute
/// reservation horizon: a lease restored at the beginning of a successful
/// scan then remains live long enough for the normal maintenance loop to take
/// ownership immediately afterwards.
pub(super) const STARTUP_TPM_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_TPM_RECOVERY_PAGE_SIZE: usize = 256;
const STARTUP_TPM_RECOVERY_CONCURRENCY: usize = RESPONSES_SETTLEMENT_CONCURRENCY;

// A crashed worker's database claim must become reclaimable before its last
// TPM reservation can disappear. With a renewal attempted every 10 seconds,
// half a TPM window also keeps a renewal that finishes at the last valid
// instant of the old claim inside the pre-renewed TPM lease, with ten seconds
// of margin (30 + 30 < 10 + 60).
pub(super) const BACKGROUND_SETTLEMENT_LEASE_TTL: Duration =
    Duration::from_secs(keycompute_ratelimit::WINDOW_SECS / 2);
pub(super) const BACKGROUND_SETTLEMENT_LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(10);
pub(super) const BACKGROUND_SETTLEMENT_CLAIM_BATCH_SIZE: usize = RESPONSES_SETTLEMENT_CONCURRENCY;
pub(super) const BACKGROUND_SETTLEMENT_INITIALIZATION_CONCURRENCY: usize =
    RESPONSES_SETTLEMENT_CONCURRENCY;
// Processing work can hold writer connections across multi-step settlement.
// Stay below the default ten-connection writer pool so the claim cursor and
// foreground traffic retain headroom even when every worker is stalled.
pub(super) const BACKGROUND_SETTLEMENT_PROCESSING_CONCURRENCY: usize =
    RESPONSES_SETTLEMENT_CONCURRENCY / 2;

const BACKGROUND_TPM_LEASE_INITIALIZING: u8 = 0;
const BACKGROUND_TPM_LEASE_ACTIVE: u8 = 1;
const BACKGROUND_TPM_LEASE_NOT_REQUIRED: u8 = 2;
const BACKGROUND_TPM_LEASE_SETTLING: u8 = 3;

pub(super) struct BackgroundSettlementLease {
    current_lease_until: tokio::sync::Mutex<chrono::DateTime<chrono::Utc>>,
    renewal_round: tokio::sync::Mutex<()>,
    local_deadline: std::sync::Mutex<tokio::time::Instant>,
}

impl BackgroundSettlementLease {
    pub(super) fn new(current_lease_until: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            current_lease_until: tokio::sync::Mutex::new(current_lease_until),
            renewal_round: tokio::sync::Mutex::new(()),
            local_deadline: std::sync::Mutex::new(
                tokio::time::Instant::now() + BACKGROUND_SETTLEMENT_LEASE_TTL,
            ),
        }
    }

    async fn renew(
        &self,
        pool: &keycompute_db::DbRouter,
        affinity: &ResponseAffinity,
    ) -> std::result::Result<bool, keycompute_db::DbError> {
        // Keep the guard across the CAS so acknowledgement cannot read the old
        // generation while a renewal installs a new one.
        let mut current = self.current_lease_until.lock().await;
        let local_deadline = tokio::time::Instant::now() + BACKGROUND_SETTLEMENT_LEASE_TTL;
        let renewed = ResponseAffinity::renew_claimed_settlement(
            pool,
            affinity.tenant_id,
            &affinity.response_id,
            current.to_owned(),
            BACKGROUND_SETTLEMENT_LEASE_TTL,
        )
        .await?;
        let Some(renewed) = renewed else {
            return Ok(false);
        };
        *current = renewed;
        *self
            .local_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = local_deadline;
        Ok(true)
    }

    pub(super) fn locally_expired(&self, now: tokio::time::Instant) -> bool {
        now >= *self
            .local_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn local_deadline(&self) -> tokio::time::Instant {
        *self
            .local_deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn relinquish(
        &self,
        pool: &keycompute_db::DbRouter,
        affinity: &ResponseAffinity,
    ) -> std::result::Result<bool, keycompute_db::DbError> {
        let current = self.current_lease_until.lock().await;
        Ok(ResponseAffinity::relinquish_claimed_settlement(
            pool,
            affinity.tenant_id,
            &affinity.response_id,
            current.to_owned(),
        )
        .await?
            > 0)
    }
}

struct BackgroundTpmLeaseState(std::sync::atomic::AtomicU8);

impl BackgroundTpmLeaseState {
    fn new(needs_tpm_lease: bool) -> Self {
        Self(std::sync::atomic::AtomicU8::new(if needs_tpm_lease {
            BACKGROUND_TPM_LEASE_INITIALIZING
        } else {
            BACKGROUND_TPM_LEASE_NOT_REQUIRED
        }))
    }

    fn active(&self) {
        self.0.store(
            BACKGROUND_TPM_LEASE_ACTIVE,
            std::sync::atomic::Ordering::Release,
        );
    }

    fn settling(&self) {
        self.0.store(
            BACKGROUND_TPM_LEASE_SETTLING,
            std::sync::atomic::Ordering::Release,
        );
    }

    fn not_required(&self) {
        self.0.store(
            BACKGROUND_TPM_LEASE_NOT_REQUIRED,
            std::sync::atomic::Ordering::Release,
        );
    }

    fn load(&self) -> u8 {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub(super) fn background_worker_claim_deadline_applies(tpm_restore_in_flight: bool) -> bool {
    !tpm_restore_in_flight
}

/// One durable worker always occupies exactly one bounded resident slot. It
/// starts in the initialization pool and atomically transfers to the processing
/// pool before releasing initialization capacity. The processing permit remains
/// in this scope through the writer fence, provider request, terminal billing,
/// ledger replay and acknowledgement.
pub(super) struct BackgroundResidentPermit {
    initialization: Option<OwnedSemaphorePermit>,
    _processing: Option<OwnedSemaphorePermit>,
}

impl BackgroundResidentPermit {
    pub(super) fn initializing(permit: OwnedSemaphorePermit) -> Self {
        Self {
            initialization: Some(permit),
            _processing: None,
        }
    }

    pub(super) fn try_transfer_to_processing(
        &mut self,
        processing_capacity: &Arc<Semaphore>,
    ) -> std::result::Result<bool, tokio::sync::TryAcquireError> {
        let permit = match Arc::clone(processing_capacity).try_acquire_owned() {
            Ok(permit) => permit,
            Err(tokio::sync::TryAcquireError::NoPermits) => return Ok(false),
            Err(error @ tokio::sync::TryAcquireError::Closed) => return Err(error),
        };
        self._processing = Some(permit);
        self.initialization.take();
        Ok(true)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct BackgroundSettlement {
    pub(super) request_id: uuid::Uuid,
    #[serde(default)]
    pub(super) billing_request_id: Option<uuid::Uuid>,
    /// Compare-and-swap generation for the balance reservation. Persisting it
    /// in the durable settlement outbox prevents a replay after process restart
    /// from consuming a newer retry's frozen funds.
    #[serde(default)]
    pub(super) balance_reservation_owner_token: Option<uuid::Uuid>,
    /// Exact prediction admitted under `request_id`. Durable replay must restore
    /// this value rather than recompute it from mutable limits or request data.
    pub(super) tpm_reserved_tokens: u32,
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
        // New provider observations are clamped once before persistence. A
        // future value already present in durable state must not be dynamically
        // mapped to each replay's `now`: after the terminal tombstone expires,
        // that would repeatedly add the same usage to a new TPM window. Fail
        // safe by releasing instead.
        return if terminal_at > now {
            ResponsesTpmTiming::Skip
        } else {
            ResponsesTpmTiming::TerminalAt(terminal_at)
        };
    }
    if settlement.terminal_status.is_some() || now >= settlement.deadline_at {
        return ResponsesTpmTiming::Skip;
    }
    ResponsesTpmTiming::LedgerFinishedAt
}

pub(super) fn background_settlement_needs_tpm_lease(
    settlement: &BackgroundSettlement,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    match background_settlement_tpm_timing(settlement, now) {
        ResponsesTpmTiming::LedgerFinishedAt => true,
        ResponsesTpmTiming::TerminalAt(terminal_at) => {
            let window = chrono::Duration::seconds(
                i64::try_from(keycompute_ratelimit::WINDOW_SECS).unwrap_or(i64::MAX),
            );
            now.signed_duration_since(terminal_at) < window
        }
        ResponsesTpmTiming::Skip => false,
    }
}

pub(super) async fn restore_background_tpm_lease_if_needed(
    state: &AppState,
    settlement: &BackgroundSettlement,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Arc<RequestContext>>> {
    if !background_settlement_needs_tpm_lease(settlement, now) {
        return Ok(None);
    }
    let ctx = Arc::new(background_billing_context(
        settlement,
        settlement.input_tokens,
        settlement.output_tokens,
    ));
    let active = crate::handlers::restore_generation_tpm_lease(state, &ctx).await?;
    Ok(active.then_some(ctx))
}

pub(super) async fn restore_startup_tpm_affinity(
    state: &AppState,
    affinity: SettlementRecoveryRow,
) -> Result<bool> {
    let settlement: BackgroundSettlement =
        serde_json::from_value(affinity.settlement).map_err(|error| {
            ApiError::Internal(format!(
                "Invalid durable settlement {} during startup TPM recovery: {error}",
                affinity.response_id
            ))
        })?;
    if settlement.tenant_id != affinity.tenant_id
        || settlement_affinity_account_id(&settlement) != affinity.account_id
        || settlement.provider != affinity.provider
    {
        return Err(ApiError::Internal(format!(
            "Durable settlement {} has an ownership mismatch during startup TPM recovery",
            affinity.response_id
        )));
    }
    if !background_settlement_needs_tpm_lease(&settlement, chrono::Utc::now()) {
        return Ok(false);
    }
    let ctx = Arc::new(background_billing_context(
        &settlement,
        settlement.input_tokens,
        settlement.output_tokens,
    ));
    crate::handlers::restore_generation_tpm_lease(state, &ctx).await
}

/// Rebuild every still-relevant TPM reservation represented by durable
/// settlement state. This is a read-only keyset scan: retry delays and worker
/// leases affect monetary processing, but must never hide already-admitted
/// capacity from a freshly started rate limiter.
///
/// The caller must await this function, under
/// [`STARTUP_TPM_RECOVERY_TIMEOUT`], before opening HTTP generation routes.
/// Ordinary settlement polling is intentionally not performed here.
pub(super) async fn restore_durable_tpm_before_serving(state: &AppState) -> Result<usize> {
    let Some(pool) = state.pool.as_deref() else {
        return Ok(0);
    };
    let snapshot_cutoff = ResponseAffinity::settlement_claim_cutoff(pool)
        .await
        .map_err(|error| {
            ApiError::Internal(format!(
                "Failed to read the startup TPM recovery cutoff: {error}"
            ))
        })?;
    let mut cursor = None;
    let mut restored = 0usize;
    loop {
        let (affinities, next_cursor) = ResponseAffinity::scan_settlements_for_tpm_recovery(
            pool,
            STARTUP_TPM_RECOVERY_PAGE_SIZE as u64,
            snapshot_cutoff,
            cursor.as_ref(),
        )
        .await
        .map_err(|error| {
            ApiError::Internal(format!(
                "Failed to scan durable settlements for startup TPM recovery: {error}"
            ))
        })?;
        let count = affinities.len();
        let results = futures::stream::iter(affinities)
            .map(|affinity| restore_startup_tpm_affinity(state, affinity))
            .buffer_unordered(STARTUP_TPM_RECOVERY_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for result in results {
            if result? {
                restored = restored.saturating_add(1);
            }
        }
        if count < STARTUP_TPM_RECOVERY_PAGE_SIZE {
            return Ok(restored);
        }
        cursor = Some(next_cursor.ok_or_else(|| {
            ApiError::Internal(
                "A full startup TPM recovery page did not return a keyset cursor".to_string(),
            )
        })?);
    }
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
    let tpm_reserved_tokens = ctx.tpm_reservation_tokens().ok_or_else(|| {
        ApiError::Internal(
            "TPM reservation prediction was not bound before settlement persistence".to_string(),
        )
    })?;
    let settlement = BackgroundSettlement {
        request_id: ctx.request_id,
        billing_request_id: Some(ctx.billing_request_id),
        balance_reservation_owner_token: ctx.balance_reservation_owner_token(),
        tpm_reserved_tokens,
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
        ResponsesTpmTiming::TerminalAt(terminal_at) => terminal_at.min(chrono::Utc::now()),
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
    if let Some(owner_token) = settlement.balance_reservation_owner_token {
        ctx.set_balance_reservation_owner_token(owner_token);
    }
    ctx.set_tpm_reservation_tokens(settlement.tpm_reserved_tokens);
    update_background_billing_context_usage(&ctx, settlement, input_tokens, output_tokens);
    ctx
}

pub(super) fn update_background_billing_context_usage(
    ctx: &RequestContext,
    settlement: &BackgroundSettlement,
    input_tokens: u32,
    output_tokens: u32,
) {
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
}

#[derive(Debug)]
pub(super) enum BackgroundLedgerSettlementError {
    Tpm(keycompute_types::KeyComputeError),
    Lease(keycompute_db::DbError),
    LeaseSuperseded,
    PostLedger(keycompute_types::KeyComputeError),
}

/// Preserve the safety-critical ordering once an immutable usage ledger exists:
/// TPM first, then independently idempotent monetary effects.
#[cfg(test)]
pub(super) async fn run_tpm_before_post_ledger_effects<Tpm, TpmFuture, Effects, EffectsFuture>(
    record_tpm: Tpm,
    replay_effects: Effects,
) -> std::result::Result<(), BackgroundLedgerSettlementError>
where
    Tpm: FnOnce() -> TpmFuture,
    TpmFuture: std::future::Future<Output = keycompute_types::Result<()>>,
    Effects: FnOnce() -> EffectsFuture,
    EffectsFuture: std::future::Future<Output = keycompute_types::Result<()>>,
{
    record_tpm()
        .await
        .map_err(BackgroundLedgerSettlementError::Tpm)?;
    replay_effects()
        .await
        .map_err(BackgroundLedgerSettlementError::PostLedger)
}

struct BackgroundSettlementWorker<'a> {
    state: &'a AppState,
    pool: &'a keycompute_db::DbRouter,
    affinity: &'a ResponseAffinity,
    lease: &'a BackgroundSettlementLease,
    tpm_lease_state: &'a BackgroundTpmLeaseState,
}

impl BackgroundSettlementWorker<'_> {
    async fn apply_committed_ledger(
        &self,
        settlement: &BackgroundSettlement,
        usage_log: &keycompute_db::UsageLog,
        ctx: &RequestContext,
    ) -> std::result::Result<(), BackgroundLedgerSettlementError> {
        let (input_tokens, output_tokens) = authoritative_responses_token_counts(
            usage_log.input_tokens,
            usage_log.output_tokens,
            settlement.input_tokens,
            settlement.output_tokens,
        );
        update_background_billing_context_usage(ctx, settlement, input_tokens, output_tokens);
        let tpm_timing = background_settlement_tpm_timing(settlement, chrono::Utc::now());
        let renewal_round = self.lease.renewal_round.lock().await;
        // While the terminal Redis transition is in flight, do not let the lease
        // ticker race it with a strict renewal. The short current DB claim remains
        // the fence; after TPM succeeds we immediately prove ownership again.
        self.tpm_lease_state.settling();
        record_responses_token_usage_for_timing(
            self.state,
            ctx,
            input_tokens.saturating_add(output_tokens),
            tpm_timing,
            usage_log.finished_at,
        )
        .await
        .map_err(BackgroundLedgerSettlementError::Tpm)?;
        self.tpm_lease_state.not_required();
        match self
            .lease
            .renew(self.pool, self.affinity)
            .await
            .map_err(BackgroundLedgerSettlementError::Lease)?
        {
            true => {}
            false => return Err(BackgroundLedgerSettlementError::LeaseSuperseded),
        }
        drop(renewal_round);
        self.state
            .billing
            .replay_saved_usage_effects(ctx, usage_log, settlement.user_id)
            .await
            .map_err(BackgroundLedgerSettlementError::PostLedger)
    }

    async fn settle_committed_ledger(
        &self,
        settlement: BackgroundSettlement,
        usage_log: keycompute_db::UsageLog,
        ctx: Arc<RequestContext>,
    ) {
        match self
            .apply_committed_ledger(&settlement, &usage_log, &ctx)
            .await
        {
            Ok(()) => clear_claimed_background_job(self.pool, self.affinity, self.lease).await,
            Err(BackgroundLedgerSettlementError::Tpm(error)) => {
                tracing::error!(%error, request_id = %settlement.request_id, "failed to record background Responses TPM usage");
                reschedule_background_job(self.pool, self.affinity, self.lease, settlement).await;
            }
            Err(BackgroundLedgerSettlementError::Lease(error)) => {
                tracing::warn!(%error, request_id = %settlement.request_id, "failed to renew background settlement claim after TPM reconciliation");
            }
            Err(BackgroundLedgerSettlementError::LeaseSuperseded) => {
                tracing::debug!(request_id = %settlement.request_id, "background settlement claim was superseded after TPM reconciliation");
            }
            Err(BackgroundLedgerSettlementError::PostLedger(error)) => {
                tracing::error!(%error, request_id = %settlement.request_id, "failed to replay background Responses post-ledger settlement");
                reschedule_background_job(self.pool, self.affinity, self.lease, settlement).await;
            }
        }
    }
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

    // Cleanup must not wait behind a large settlement-claim drain. The two
    // loops operate on independent rows/conditions and remain replica-safe.
    let cleanup_state = state.clone();
    let cleanup_pool = Arc::clone(&pool);
    tokio::spawn(async move {
        let mut cleanup_tick = tokio::time::interval(Duration::from_secs(5 * 60));
        cleanup_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            cleanup_tick.tick().await;
            let now = chrono::Utc::now().timestamp();
            cleanup_state
                .responses_affinity
                .write()
                .await
                .retain(|_, affinity| affinity.expires_at_unix > now);
            match ResponseAffinity::delete_expired(cleanup_pool.as_ref()).await {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "deleted expired Responses affinities")
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to delete expired Responses affinities")
                }
            }
            match ResponsesIdempotencyClaim::expire_completed_responses(cleanup_pool.as_ref()).await
            {
                Ok(count) if count > 0 => {
                    tracing::info!(count, "expired cached Responses idempotency results")
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "failed to expire Responses idempotency results")
                }
            }
        }
    });

    tokio::spawn(async move {
        // Keep recovery work bounded independently from long processing. A job
        // owns one initialization slot from claim through TPM restoration and
        // then either transfers to a processing slot or exits immediately. The
        // processing slot covers writer, provider, billing and acknowledgement
        // awaits, so no downstream stall can grow the resident task set.
        let initialization_capacity = Arc::new(Semaphore::new(
            BACKGROUND_SETTLEMENT_INITIALIZATION_CONCURRENCY,
        ));
        let processing_capacity =
            Arc::new(Semaphore::new(BACKGROUND_SETTLEMENT_PROCESSING_CONCURRENCY));
        let mut settlement_tick = tokio::time::interval(Duration::from_secs(1));
        settlement_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            settlement_tick.tick().await;
            drain_due_background_settlements(
                &state,
                &pool,
                &initialization_capacity,
                &processing_capacity,
            )
            .await;
        }
    });
}

async fn take_background_initialization_permits(
    capacity: &Arc<Semaphore>,
    limit: usize,
) -> Vec<OwnedSemaphorePermit> {
    debug_assert!(limit > 0);
    let first = Arc::clone(capacity)
        .acquire_owned()
        .await
        .expect("background settlement initialization semaphore is never closed");
    let mut permits = Vec::with_capacity(limit);
    permits.push(first);
    while permits.len() < limit {
        let Ok(permit) = Arc::clone(capacity).try_acquire_owned() else {
            break;
        };
        permits.push(permit);
    }
    permits
}

pub(super) async fn drain_claim_batches_bounded<Job, Cursor, Error, Claim, ClaimFuture, Launch>(
    batch_size: usize,
    initialization_capacity: &Arc<Semaphore>,
    mut claim: Claim,
    mut launch: Launch,
) -> std::result::Result<usize, Error>
where
    Claim: FnMut(u64, Option<Cursor>) -> ClaimFuture,
    ClaimFuture:
        std::future::Future<Output = std::result::Result<(Vec<Job>, Option<Cursor>), Error>>,
    Launch: FnMut(Job, OwnedSemaphorePermit),
{
    debug_assert!(batch_size > 0);
    let mut total = 0usize;
    let mut cursor = None;
    loop {
        // Reserve task capacity before touching the database. Claimed work can
        // therefore never outnumber workers that are able to begin restoring
        // its admitted TPM reservation immediately.
        let permits =
            take_background_initialization_permits(initialization_capacity, batch_size).await;
        let claim_limit = permits.len();
        let (jobs, next_cursor) = claim(claim_limit as u64, cursor.take()).await?;
        let count = jobs.len();
        total = total.saturating_add(count);
        for (job, permit) in jobs.into_iter().zip(permits) {
            launch(job, permit);
        }
        if count < claim_limit {
            return Ok(total);
        }
        debug_assert!(
            next_cursor.is_some(),
            "a full background settlement claim batch must return a keyset cursor"
        );
        let Some(next_cursor) = next_cursor else {
            // Fail safe in release builds: the current claims still run, and a
            // fresh cutoff retries any omitted work on the next maintenance
            // tick instead of looping on an unusable cursor.
            tracing::error!(
                count,
                claim_limit,
                "full background settlement claim batch returned no keyset cursor"
            );
            return Ok(total);
        };
        cursor = Some(next_cursor);
        // A full batch means the finite cutoff snapshot may have more work.
        // Waiting for the next initialization permit is intentional backpressure.
        // A worker releases it immediately after TPM restore whether or not it
        // can transfer into the bounded processing pool.
        tokio::task::yield_now().await;
    }
}

async fn drain_due_background_settlements(
    state: &AppState,
    pool: &Arc<keycompute_db::DbRouter>,
    initialization_capacity: &Arc<Semaphore>,
    processing_capacity: &Arc<Semaphore>,
) {
    let cutoff = match ResponseAffinity::settlement_claim_cutoff(pool.as_ref()).await {
        Ok(cutoff) => cutoff,
        Err(error) => {
            tracing::warn!(%error, "failed to read the background settlement claim cutoff");
            return;
        }
    };
    let result = drain_claim_batches_bounded(
        BACKGROUND_SETTLEMENT_CLAIM_BATCH_SIZE,
        initialization_capacity,
        |limit, after| {
            let pool = Arc::clone(pool);
            async move {
                ResponseAffinity::claim_due_settlements_before_for(
                    pool.as_ref(),
                    limit,
                    BACKGROUND_SETTLEMENT_LEASE_TTL,
                    cutoff,
                    after.as_ref(),
                )
                .await
            }
        },
        |job, initialization_permit| {
            let worker_state = state.clone();
            let worker_processing_capacity = Arc::clone(processing_capacity);
            tokio::spawn(async move {
                settle_background_job(
                    &worker_state,
                    job,
                    worker_processing_capacity,
                    initialization_permit,
                )
                .await;
            });
        },
    )
    .await;
    match result {
        Ok(count) if count > 0 => {
            tracing::debug!(count, "claimed due background Responses settlements")
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "failed to lease background Responses settlements")
        }
    }
}

async fn ensure_background_tpm_lease(
    state: &AppState,
    settlement: &BackgroundSettlement,
) -> keycompute_types::Result<bool> {
    let rate_key = keycompute_ratelimit::RateLimitKey::new(
        settlement.tenant_id,
        settlement.user_id,
        settlement.produce_ai_key_id,
    );
    match state
        .rate_limiter
        .renew_token_reservation(
            &rate_key,
            settlement.request_id,
            settlement_billing_request_id(settlement),
            settlement.tpm_reserved_tokens,
        )
        .await
    {
        Ok(true) => return Ok(true),
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(
                %error,
                request_id = %settlement.request_id,
                "strict durable TPM renewal failed; attempting terminal-fenced restore"
            );
        }
    }
    state
        .rate_limiter
        .restore_token_reservation(
            &rate_key,
            settlement.request_id,
            settlement_billing_request_id(settlement),
            settlement.tpm_reserved_tokens,
        )
        .await
}

async fn relinquish_background_worker_lease(
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
    settlement: &BackgroundSettlement,
    lease: &BackgroundSettlementLease,
) {
    match lease.relinquish(pool, affinity).await {
        Ok(true) => {}
        Ok(false) => tracing::debug!(
            request_id = %settlement.request_id,
            response_id = %affinity.response_id,
            "background Responses settlement lease was superseded before relinquish"
        ),
        Err(error) => tracing::warn!(
            %error,
            request_id = %settlement.request_id,
            response_id = %affinity.response_id,
            "failed to relinquish DB lease after TPM renewal failure"
        ),
    }
}

async fn renew_background_worker_leases(
    state: &AppState,
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
    settlement: &BackgroundSettlement,
    lease: &BackgroundSettlementLease,
    tpm_lease_state: &BackgroundTpmLeaseState,
) -> bool {
    if matches!(
        tpm_lease_state.load(),
        BACKGROUND_TPM_LEASE_INITIALIZING | BACKGROUND_TPM_LEASE_SETTLING
    ) {
        return true;
    }
    // Serialize the complete TPM -> DB -> TPM sequence. The initial ownership
    // fence and the periodic ticker can otherwise interleave and let one
    // failed round relinquish a lease just renewed by the other.
    let _renewal_round = lease.renewal_round.lock().await;
    let tpm_is_active = match tpm_lease_state.load() {
        BACKGROUND_TPM_LEASE_INITIALIZING | BACKGROUND_TPM_LEASE_SETTLING => {
            // Do not extend a DB claim while its terminal-fenced Redis restore
            // is still in flight. A crash in that interval must remain bounded
            // by the original short claim.
            return true;
        }
        BACKGROUND_TPM_LEASE_ACTIVE => match ensure_background_tpm_lease(state, settlement).await {
            Ok(true) => true,
            Ok(false) => {
                // A terminal tombstone is authoritative: no pending prediction
                // remains to protect, but the durable monetary replay must run.
                tpm_lease_state.not_required();
                false
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    request_id = %settlement.request_id,
                    response_id = %affinity.response_id,
                    "failed to keep durable TPM reservation active"
                );
                relinquish_background_worker_lease(pool, affinity, settlement, lease).await;
                return false;
            }
        },
        BACKGROUND_TPM_LEASE_NOT_REQUIRED => false,
        _ => {
            tracing::error!(
                request_id = %settlement.request_id,
                response_id = %affinity.response_id,
                "invalid background TPM lease state"
            );
            relinquish_background_worker_lease(pool, affinity, settlement, lease).await;
            return false;
        }
    };

    let renewed = match lease.renew(pool, affinity).await {
        Ok(true) => true,
        Ok(false) => {
            tracing::debug!(
                request_id = %settlement.request_id,
                response_id = %affinity.response_id,
                "background Responses settlement lease was superseded during renewal"
            );
            false
        }
        Err(error) => {
            tracing::warn!(
                %error,
                request_id = %settlement.request_id,
                response_id = %affinity.response_id,
                "failed to renew background Responses settlement lease"
            );
            false
        }
    };
    if !renewed || !tpm_is_active {
        return renewed;
    }

    // Confirm the TPM state after the DB await. Missing capacity is restored;
    // a terminal fence switches the remaining replay to DB-only maintenance.
    match ensure_background_tpm_lease(state, settlement).await {
        Ok(true) => true,
        Ok(false) => {
            tpm_lease_state.not_required();
            true
        }
        Err(error) => {
            tracing::warn!(
                %error,
                request_id = %settlement.request_id,
                response_id = %affinity.response_id,
                "failed to confirm durable TPM reservation after DB lease renewal"
            );
            relinquish_background_worker_lease(pool, affinity, settlement, lease).await;
            false
        }
    }
}

async fn maintain_background_worker_leases(
    state: &AppState,
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
    settlement: &BackgroundSettlement,
    lease: &BackgroundSettlementLease,
    tpm_lease_state: &BackgroundTpmLeaseState,
) {
    let mut renewal_tick = tokio::time::interval(BACKGROUND_SETTLEMENT_LEASE_RENEW_INTERVAL);
    renewal_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` ticks immediately. The claim already has a full lease.
    renewal_tick.tick().await;
    loop {
        renewal_tick.tick().await;
        let phase = tpm_lease_state.load();
        if !background_worker_claim_deadline_applies(phase == BACKGROUND_TPM_LEASE_INITIALIZING) {
            // The restore operation deliberately survives request-future
            // cancellation so an already-sent Redis mutation always reaches a
            // known outcome. Keep the worker (and its initialization permit)
            // resident until that operation finishes; otherwise every expired
            // DB claim could start another detached restore and grow without
            // bound while Redis is stalled. No writer/provider/monetary work
            // can run before the worker fences the claim after restoration.
            continue;
        }
        if lease.locally_expired(tokio::time::Instant::now()) {
            tracing::warn!(
                request_id = %settlement.request_id,
                response_id = %affinity.response_id,
                "background Responses worker outlived its last confirmed DB claim"
            );
            return;
        }
        if phase == BACKGROUND_TPM_LEASE_SETTLING {
            continue;
        }
        let renewal = renew_background_worker_leases(
            state,
            pool,
            affinity,
            settlement,
            lease,
            tpm_lease_state,
        );
        match complete_before_background_claim_deadline(lease.local_deadline(), renewal).await {
            Some(true) => {}
            Some(false) | None => return,
        }
    }
}

pub(super) async fn complete_before_background_claim_deadline<F, Output>(
    deadline: tokio::time::Instant,
    future: F,
) -> Option<Output>
where
    F: std::future::Future<Output = Output>,
{
    tokio::time::timeout_at(deadline, future).await.ok()
}

pub(super) async fn run_background_worker_with_maintenance<Worker, Maintenance>(
    worker: Worker,
    maintenance: Maintenance,
) where
    Worker: std::future::Future<Output = ()>,
    Maintenance: std::future::Future<Output = ()>,
{
    tokio::pin!(worker);
    tokio::pin!(maintenance);
    // Poll both futures throughout their awaits. In particular, maintenance
    // may wait for the worker's lease mutex while the worker holds it across a
    // DB CAS; nesting that await inside a selected tick branch would deadlock.
    tokio::select! {
        biased;
        _ = &mut worker => {}
        _ = &mut maintenance => {}
    }
}

pub(super) async fn settle_background_job(
    state: &AppState,
    affinity: ResponseAffinity,
    processing_capacity: Arc<Semaphore>,
    initialization_permit: OwnedSemaphorePermit,
) {
    let Some(pool) = state.pool.as_deref() else {
        return;
    };
    let Some(value) = affinity.settlement.clone() else {
        return;
    };
    let settlement: BackgroundSettlement = match serde_json::from_value(value) {
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
    let Some(current_lease_until) = affinity.settlement_lease_until else {
        tracing::error!(response_id = %affinity.response_id, "claimed background Responses settlement has no lease");
        return;
    };
    let lease = BackgroundSettlementLease::new(current_lease_until);
    let tpm_lease_state = BackgroundTpmLeaseState::new(background_settlement_needs_tpm_lease(
        &settlement,
        chrono::Utc::now(),
    ));
    let worker = settle_background_job_under_lease(
        state,
        &affinity,
        settlement.clone(),
        &lease,
        &tpm_lease_state,
        &processing_capacity,
        initialization_permit,
    );
    let maintenance = maintain_background_worker_leases(
        state,
        pool,
        &affinity,
        &settlement,
        &lease,
        &tpm_lease_state,
    );
    // Whichever side completes cancels the other in this same task. Lease loss
    // therefore cancels the worker, and worker completion cannot leak a
    // detached DB renewal loop.
    run_background_worker_with_maintenance(worker, maintenance).await;
}

async fn settle_background_job_under_lease(
    state: &AppState,
    affinity: &ResponseAffinity,
    mut settlement: BackgroundSettlement,
    lease: &BackgroundSettlementLease,
    tpm_lease_state: &BackgroundTpmLeaseState,
    processing_capacity: &Arc<Semaphore>,
    initialization_permit: OwnedSemaphorePermit,
) {
    let Some(pool) = state.pool.as_deref() else {
        return;
    };
    let worker = BackgroundSettlementWorker {
        state,
        pool,
        affinity,
        lease,
        tpm_lease_state,
    };
    let mut resident_permit = BackgroundResidentPermit::initializing(initialization_permit);
    let restore_started_at = chrono::Utc::now();
    // The durable outbox represents work that was already admitted. Restore
    // its exact, terminal-fenced prediction before any writer-pool wait. This
    // keeps every claimed job protected even when the writer is saturated; if
    // an immutable ledger already won, the Redis terminal fence returns false
    // and the writer lookup below replays only its idempotent effects.
    let mut active_tpm_context = match restore_background_tpm_lease_if_needed(
        state,
        &settlement,
        restore_started_at,
    )
    .await
    {
        Ok(ctx) => {
            if ctx.is_some() {
                tpm_lease_state.active();
            } else {
                tpm_lease_state.not_required();
            }
            ctx
        }
        Err(error) => {
            tracing::warn!(%error, request_id = %settlement.request_id, "failed to restore durable TPM reservation lease");
            // Do not wait on the writer while holding scarce initialization
            // capacity. The short claim expires naturally; meanwhile a shared
            // rate-limit backend failure also makes new admission fail closed.
            return;
        }
    };

    // TPM restoration is the only work allowed to occupy an initialization
    // slot. Never let a saturated writer/provider/billing path prevent the
    // finite cursor sweep from refreshing the tail of the durable backlog.
    // A job that cannot transfer leaves its 30-second DB claim to expire; its
    // just-restored 60-second TPM lease safely covers the next recovery sweep.
    match resident_permit.try_transfer_to_processing(processing_capacity) {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            tracing::error!(%error, response_id = %affinity.response_id, "background Responses processing semaphore closed");
            return;
        }
    }

    // The potentially slow TPM restore above has completed. Re-fence ownership
    // before issuing a writer lookup, upstream poll or any monetary side effect.
    // A worker whose short initial claim expired cannot continue merely because
    // it eventually restored the shared TPM reservation.
    if !renew_background_worker_leases(state, pool, affinity, &settlement, lease, tpm_lease_state)
        .await
    {
        return;
    }

    match keycompute_db::UsageLog::find_by_billing_request_id_on_writer(
        pool,
        settlement_billing_request_id(&settlement),
    )
    .await
    {
        Ok(Some(usage_log)) => {
            let ctx = active_tpm_context.take().unwrap_or_else(|| {
                Arc::new(background_billing_context(
                    &settlement,
                    settlement.input_tokens,
                    settlement.output_tokens,
                ))
            });
            worker
                .settle_committed_ledger(settlement, usage_log, ctx)
                .await;
            return;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, request_id = %settlement.request_id, "failed to check background settlement ledger after TPM restore");
            reschedule_background_job(pool, affinity, lease, settlement).await;
            return;
        }
    }

    // The restore and writer read are external awaits. Re-evaluate both the
    // 24-hour settlement deadline and the terminal TPM horizon afterwards. If
    // either elapsed during the wait, remove the just-restored prediction now
    // rather than carrying it into a retry or billing failure.
    let mut now = chrono::Utc::now();
    if active_tpm_context.is_some() && !background_settlement_needs_tpm_lease(&settlement, now) {
        let ctx = active_tpm_context
            .take()
            .expect("restored TPM context was checked above");
        if let Err(error) =
            crate::handlers::release_terminal_token_reservation(state.rate_limiter.as_ref(), &ctx)
                .await
        {
            tracing::warn!(%error, request_id = %settlement.request_id, "failed to release expired durable TPM reservation lease");
            reschedule_background_job(pool, affinity, lease, settlement).await;
            return;
        }
        tpm_lease_state.not_required();
    }

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
        // The processing permit acquired immediately after TPM restoration is
        // retained through this provider request and every later side effect.
        now = chrono::Utc::now();
        if now >= settlement.deadline_at {
            terminal_status = Some("incomplete".to_string());
            tpm_timing = ResponsesTpmTiming::Skip;
        } else {
            match background_poll_account(state, affinity, settlement.openai_beta.as_deref()).await
            {
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
    }

    let Some(status) = terminal_status.as_deref() else {
        reschedule_background_job(pool, affinity, lease, settlement).await;
        return;
    };
    settlement.terminal_status = Some(status.to_string());
    if tpm_timing == ResponsesTpmTiming::LedgerFinishedAt {
        settlement.terminal_at.get_or_insert_with(chrono::Utc::now);
    }
    // Reuse the exact context whose stop signal owns the recovered heartbeat.
    // Refresh its usage snapshot after polling so successful reconciliation
    // wakes that task immediately.
    let ctx = active_tpm_context.unwrap_or_else(|| {
        Arc::new(background_billing_context(
            &settlement,
            settlement.input_tokens,
            settlement.output_tokens,
        ))
    });
    update_background_billing_context_usage(
        &ctx,
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
            reschedule_background_job(pool, affinity, lease, settlement).await;
            return;
        }
    };
    // `finalize_and_save` may return an earlier idempotent row. From this point
    // on the immutable ledger is authoritative, so use the same TPM-before-
    // post-effects ordering as both writer-ledger replay paths.
    worker
        .settle_committed_ledger(settlement, usage_log, ctx)
        .await;
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
    background_response_terminal_at_observed(body, chrono::Utc::now())
}

pub(super) fn background_response_terminal_at_observed(
    body: &Value,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let completed_at = body.get("completed_at")?.as_i64()?;
    chrono::DateTime::from_timestamp(completed_at, 0).map(|terminal_at| {
        // Provider clocks and compatibility endpoints are untrusted. Clamp
        // exactly once to the observation instant and persist that fixed value.
        terminal_at.min(observed_at)
    })
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

async fn reschedule_background_job(
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
    lease: &BackgroundSettlementLease,
    mut settlement: BackgroundSettlement,
) {
    settlement.attempt = settlement.attempt.saturating_add(1);
    let delay = 1_i64 << settlement.attempt.min(3);
    let value = match serde_json::to_value(&settlement) {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "failed to serialize background Responses retry");
            return;
        }
    };
    // Serialize the final CAS with renewal so it always uses the latest lease
    // generation and no detached renew can race after acknowledgement.
    let current_lease_until = lease.current_lease_until.lock().await;
    match ResponseAffinity::reschedule_claimed_settlement(
        pool,
        affinity.tenant_id,
        &affinity.response_id,
        value,
        chrono::Utc::now() + chrono::Duration::seconds(delay),
        current_lease_until.to_owned(),
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

async fn clear_claimed_background_job(
    pool: &keycompute_db::DbRouter,
    affinity: &ResponseAffinity,
    lease: &BackgroundSettlementLease,
) {
    let current_lease_until = lease.current_lease_until.lock().await;
    match ResponseAffinity::clear_claimed_settlement(
        pool,
        affinity.tenant_id,
        &affinity.response_id,
        current_lease_until.to_owned(),
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
