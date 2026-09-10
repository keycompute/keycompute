//! 处理器模块
//
//! 处理各种 HTTP 请求

// 管理功能（拆分为多个模块）
pub mod admin_account;
pub mod admin_monitoring;
pub mod admin_node_gateway;
pub mod admin_pricing;
pub mod admin_settings;
pub mod admin_user;
pub mod anthropic;

pub mod auth;
pub mod billing;
pub mod distribution;
pub mod gateway;
pub mod health;
pub mod node;
pub mod node_gateway_token;
pub mod node_tips;
pub mod openai;
pub(crate) mod pagination;
pub mod payment;
pub mod pricing;
pub mod requirement;
pub mod responses;
pub mod responses_websocket;
pub mod routing;
pub mod user;

// 认证相关
pub use auth::{
    complete_registration_handler, forgot_password_handler, login_handler, refresh_token_handler,
    register_handler, reset_password_handler, verify_reset_token_handler,
};

// 需求收集
pub use requirement::submit_requirement_handler;

// OpenAI 兼容 API (统一入口)
pub use openai::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ListModelsResponse, Model,
    chat_completions, list_models, retrieve_model,
};

// OpenAI Responses 兼容入口
pub use responses::{
    cancel_response, compact_response, count_response_input_tokens, delete_response,
    list_response_input_items, responses, retrieve_response,
};
pub use responses_websocket::responses_websocket;

// Anthropic Messages 兼容入口
pub use anthropic::messages;

// Distribution 分销管理
pub use distribution::{
    create_distribution_rule, delete_distribution_rule, generate_invite_link,
    get_distribution_stats, get_my_distribution_earnings, get_my_referral_code, get_my_referrals,
    list_distribution_records, list_distribution_rules, update_distribution_rule,
};

// 用户自服务
pub use user::{
    change_password, create_api_key, delete_api_key, get_current_user, get_my_usage,
    get_my_usage_stats, list_my_api_keys, update_profile,
};

// 用户管理（admin_user）
pub use admin_user::{
    AdminUserInfo, TenantListQueryParams, TenantListResponse, UpdateUserRequest,
    UserListQueryParams, UserListResponse, delete_user, freeze_user_balance, get_user_by_id,
    list_all_api_keys, list_all_users, list_tenants, unfreeze_user_balance, update_user,
    update_user_balance,
};

// 账号管理（admin_account）
pub use admin_account::{
    AccountInfo, AccountListQueryParams, AccountListResponse, CreateAccountRequest,
    UpdateAccountRequest, create_account, delete_account, get_default_endpoint, list_accounts,
    refresh_account, test_account, update_account,
};

// Node Gateway 管理
pub use admin_node_gateway::{
    delete_node, exclude_node, get_node_gateway_overview, list_node_gateway_nodes,
    list_node_gateway_tasks, recover_node, revoke_node_token,
};

// 监控追踪
pub use admin_account::{probe_account_for_monitoring, probe_enabled_account_for_monitoring};
pub use admin_monitoring::{
    get_monitoring_overview, get_monitoring_request, get_monitoring_summary,
    get_monitoring_target_health, list_monitoring_requests, probe_monitoring_targets,
};

// 定价管理（admin_pricing）
pub use admin_pricing::{
    CreatePricingAdminRequest, PricingInfo, UpdatePricingAdminRequest, create_pricing,
    delete_pricing, list_pricing, make_pricing_default, update_pricing,
};

// 系统设置（admin_settings）
pub use admin_settings::{
    AdminSystemSettings, get_public_settings, get_system_setting_by_key, get_system_settings,
    update_system_setting_by_key, update_system_settings,
};

// 定价和账单
pub use billing::{calculate_cost, get_billing_stats, list_billing_records};
pub use pricing::{calculate_cost as get_pricing_cost, get_pricing};

// 调试接口
pub use gateway::{check_provider_health, get_execution_stats, get_gateway_status};
pub use routing::{debug_routing, get_provider_health, reset_health, set_account_cooldown};

// 健康检查
pub use health::health_check;

// 节点网关
pub use node::{node_complete, node_heartbeat, node_poll, node_register};

// 用户节点网关 token 管理
pub use node_gateway_token::{
    admin_approve_token, admin_list_pending_tokens, create_my_node_gateway_token,
    delete_my_node_gateway_token, get_my_node_gateway_token, list_my_node_gateway_tokens,
};

// 节点租赁小费管理
pub use node_tips::{
    admin_approve_withdrawal, admin_complete_withdrawal, admin_get_tip_ratio,
    admin_list_pending_withdrawals, admin_update_tip_ratio, create_tip_withdrawal,
    get_my_tips_history, get_my_tips_summary, get_my_withdrawals,
};

// 支付相关
pub use payment::{
    admin_list_payment_orders, admin_payment_providers, admin_verify_payment_provider,
    alipay_notify, create_payment_order, get_my_balance, get_payment_order, list_my_payment_orders,
    list_payment_methods, sync_payment_order, wechatpay_notify,
};

/// Services used together when an immediate generation request reaches its
/// terminal point. Its billing and TPM effects share the durable settlement
/// outbox and worker used by Responses.
#[derive(Clone)]
pub(crate) struct ImmediateSettlementServices {
    pub(crate) billing: std::sync::Arc<keycompute_billing::BillingService>,
    pub(crate) rate_limiter: std::sync::Arc<keycompute_ratelimit::RateLimitService>,
    /// State needed to persist and acknowledge the shared terminal-settlement
    /// outbox. Tests and database-less development settle synchronously.
    pub(crate) durable_state: Option<crate::state::AppState>,
}

impl ImmediateSettlementServices {
    pub(crate) fn from_state(state: &crate::state::AppState) -> Self {
        Self {
            billing: std::sync::Arc::clone(&state.billing),
            rate_limiter: std::sync::Arc::clone(&state.rate_limiter),
            durable_state: Some(state.clone()),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(billing: std::sync::Arc<keycompute_billing::BillingService>) -> Self {
        Self {
            billing,
            rate_limiter: std::sync::Arc::new(
                keycompute_ratelimit::RateLimitService::default_memory(),
            ),
            durable_state: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InitialStreamStatus {
    Ready,
    Failed,
}

pub(crate) fn report_initial_stream_status(
    status: &mut Option<tokio::sync::oneshot::Sender<InitialStreamStatus>>,
    event: Option<&llm_protocol_provider::StreamEvent>,
) {
    let Some(status) = status.take() else {
        return;
    };
    let outcome = if matches!(
        event,
        Some(llm_protocol_provider::StreamEvent::Error { .. }) | None
    ) {
        InitialStreamStatus::Failed
    } else {
        InitialStreamStatus::Ready
    };
    let _ = status.send(outcome);
}

/// Wait until the upstream HTTP request is accepted or the stream worker
/// proves that no successful response can be committed. Successful acceptance
/// is deliberately independent of the first protocol event so SSE keepalives
/// can reach the client during long model startup.
pub(crate) async fn await_initial_stream_status(
    ctx: &keycompute_types::RequestContext,
    status: tokio::sync::oneshot::Receiver<InitialStreamStatus>,
) -> InitialStreamStatus {
    tokio::select! {
        biased;
        _ = ctx.wait_for_upstream_response_accepted() => InitialStreamStatus::Ready,
        status = status => status.unwrap_or(InitialStreamStatus::Failed),
    }
}

/// Reserve the estimated maximum user charge before an upstream request is
/// dispatched. If the client omitted an output limit, this step applies an
/// internal bounded risk budget without changing the upstream request body.
pub(crate) struct GenerationBalanceReservation {
    owner: Option<(keycompute_billing::BalanceService, uuid::Uuid)>,
    billing_request_id: uuid::Uuid,
}

impl GenerationBalanceReservation {
    /// Release this exact ownership generation and disarm Drop cleanup.
    pub(crate) async fn release(&mut self) {
        // Keep the owner armed across the await. If this future is cancelled
        // while the database operation is in flight, Drop can still schedule
        // a best-effort retry instead of leaving the reservation frozen until
        // its expiry sweep.
        let Some((balance, owner_token)) = self.owner.clone() else {
            return;
        };
        match balance
            .release_request_reservation(self.billing_request_id, owner_token)
            .await
        {
            Ok(_) => self.owner = None,
            Err(error) => {
                // The durable row still has an expiry fallback, and the
                // ownership token makes Drop's retry safe if a newer attempt
                // has already reclaimed this request ID.
                tracing::error!(billing_request_id = %self.billing_request_id, %error, "failed to release balance reservation");
            }
        }
    }

    /// The durable usage settlement path now owns this reservation.
    pub(crate) fn transfer_to_settlement(&mut self) {
        self.owner = None;
    }
}

impl Drop for GenerationBalanceReservation {
    fn drop(&mut self) {
        let Some((balance, owner_token)) = self.owner.take() else {
            return;
        };
        let billing_request_id = self.billing_request_id;
        // Handler cancellation cannot await cleanup. The reservation remains
        // durable and has an expiry fallback if runtime shutdown prevents this
        // best-effort release task from completing.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = balance
                    .release_request_reservation(billing_request_id, owner_token)
                    .await
                {
                    tracing::error!(%billing_request_id, %error, "failed to release cancelled request balance reservation");
                }
            });
        }
    }
}

/// Own one physical attempt's in-flight TPM prediction until terminal
/// settlement takes responsibility for reconciling it to actual usage.
const TPM_RESERVATION_HEARTBEAT_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(keycompute_ratelimit::WINDOW_SECS / 3);

#[derive(Debug, Default)]
struct TpmReservationHeartbeatCancellation {
    cancelled: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl TpmReservationHeartbeatCancellation {
    fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        // There is exactly one heartbeat waiter. `notify_one` retains a permit
        // if cancellation races the first poll of `notified()`; `notify_waiters`
        // would lose that wake and strand the task until another branch fires.
        self.notify.notify_one();
    }

    async fn cancelled(&self) {
        loop {
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            let notified = self.notify.notified();
            if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

async fn release_stopped_tpm_heartbeat(
    rate_limiter: &keycompute_ratelimit::RateLimitService,
    rate_key: &keycompute_ratelimit::RateLimitKey,
    reservation_id: uuid::Uuid,
) {
    // This second, idempotent release closes the race where an in-flight
    // missing-lease restore lands after the explicit release that stopped the
    // heartbeat. Terminal records are immutable, so it is also safe after
    // reconciliation has already won.
    if let Err(error) = rate_limiter
        .release_token_reservation(rate_key, reservation_id)
        .await
    {
        tracing::warn!(
            %reservation_id,
            %error,
            "failed to release stopped TPM heartbeat reservation"
        );
    }
}

pub(crate) struct GenerationTpmReservation {
    owner: Option<(
        std::sync::Arc<keycompute_ratelimit::RateLimitService>,
        keycompute_ratelimit::RateLimitKey,
    )>,
    reservation_id: uuid::Uuid,
    terminal_id: uuid::Uuid,
    predicted_tokens: u32,
    heartbeat_cancel: Option<std::sync::Arc<TpmReservationHeartbeatCancellation>>,
}

impl GenerationTpmReservation {
    fn armed(
        rate_limiter: std::sync::Arc<keycompute_ratelimit::RateLimitService>,
        rate_key: keycompute_ratelimit::RateLimitKey,
        reservation_id: uuid::Uuid,
        terminal_id: uuid::Uuid,
        predicted_tokens: u32,
    ) -> Self {
        Self {
            owner: Some((rate_limiter, rate_key)),
            reservation_id,
            terminal_id,
            predicted_tokens,
            heartbeat_cancel: None,
        }
    }

    fn start_heartbeat(
        &mut self,
        request_lifetime: std::sync::Weak<()>,
        settlement_stopped: std::pin::Pin<
            Box<dyn std::future::Future<Output = ()> + Send + 'static>,
        >,
    ) {
        self.start_heartbeat_with_interval(
            request_lifetime,
            settlement_stopped,
            TPM_RESERVATION_HEARTBEAT_INTERVAL,
        );
    }

    fn start_heartbeat_with_interval(
        &mut self,
        request_lifetime: std::sync::Weak<()>,
        mut settlement_stopped: std::pin::Pin<
            Box<dyn std::future::Future<Output = ()> + Send + 'static>,
        >,
        interval: std::time::Duration,
    ) {
        let Some((rate_limiter, rate_key)) = self.owner.clone() else {
            return;
        };
        debug_assert!(self.heartbeat_cancel.is_none());
        let reservation_id = self.reservation_id;
        let terminal_id = self.terminal_id;
        let predicted_tokens = self.predicted_tokens;
        let cancellation = std::sync::Arc::new(TpmReservationHeartbeatCancellation::default());
        self.heartbeat_cancel = Some(std::sync::Arc::clone(&cancellation));

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // `interval` ticks immediately once. The original admission already
            // established a full lease, so wait for the first real heartbeat.
            tick.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => {
                        release_stopped_tpm_heartbeat(
                            rate_limiter.as_ref(),
                            &rate_key,
                            reservation_id,
                        ).await;
                        break;
                    }
                    _ = settlement_stopped.as_mut() => {
                        release_stopped_tpm_heartbeat(
                            rate_limiter.as_ref(),
                            &rate_key,
                            reservation_id,
                        ).await;
                        break;
                    }
                    _ = tick.tick() => {
                        // A strong reference is held only while the backend call
                        // is in flight. The heartbeat can never retain an
                        // abandoned request or settlement worker by itself.
                        let Some(_request_lifetime) = request_lifetime.upgrade() else {
                            break;
                        };
                        match rate_limiter
                            .renew_token_reservation(
                                &rate_key,
                                reservation_id,
                                terminal_id,
                                predicted_tokens,
                            )
                            .await
                        {
                            Ok(true) => {}
                            Ok(false) => {
                                // Strict renewal deliberately cannot distinguish
                                // an expired/missing lease from a terminal fence.
                                // Restore is the safe discriminator: it recreates
                                // only missing capacity for this already-admitted
                                // request and returns false when terminal usage won.
                                match rate_limiter
                                    .restore_token_reservation(
                                        &rate_key,
                                        reservation_id,
                                        terminal_id,
                                        predicted_tokens,
                                    )
                                    .await
                                {
                                    Ok(true) => {
                                        tracing::warn!(
                                            %reservation_id,
                                            %terminal_id,
                                            predicted_tokens,
                                            "restored missing active TPM reservation lease"
                                        );
                                    }
                                    Ok(false) => {
                                        // The logical terminal fence won. It may
                                        // coexist with another physical attempt,
                                        // so explicitly remove this attempt's
                                        // prediction before ending its heartbeat.
                                        release_stopped_tpm_heartbeat(
                                            rate_limiter.as_ref(),
                                            &rate_key,
                                            reservation_id,
                                        )
                                        .await;
                                        break;
                                    }
                                    Err(error) => {
                                        // Keep retrying while the request or its
                                        // durable settlement remains alive. A
                                        // recovered backend can then restore the
                                        // missing prediction before admitting
                                        // further work without accounting for it.
                                        tracing::warn!(
                                            %reservation_id,
                                            %terminal_id,
                                            predicted_tokens,
                                            %error,
                                            "failed to restore missing active TPM reservation lease"
                                        );
                                    }
                                }
                            }
                            Err(error) => {
                                // A transient backend failure must not end the
                                // heartbeat permanently. A later successful
                                // renewal, or the fenced restore path above after
                                // expiry, repairs the active prediction.
                                tracing::warn!(
                                    %reservation_id,
                                    %terminal_id,
                                    predicted_tokens,
                                    %error,
                                    "failed to renew active TPM reservation lease"
                                );
                            }
                        }
                    }
                }
            }
        });
    }

    fn stop_heartbeat(&mut self) {
        if let Some(cancellation) = self.heartbeat_cancel.take() {
            cancellation.cancel();
        }
    }

    /// Release this physical attempt's prediction and disarm Drop cleanup.
    pub(crate) async fn release(&mut self) {
        self.stop_heartbeat();
        // Preserve ownership until Redis/memory confirms the release. A task
        // cancelled during this await must leave the Drop fallback armed.
        let Some((rate_limiter, rate_key)) = self.owner.clone() else {
            return;
        };
        match rate_limiter
            .release_token_reservation(&rate_key, self.reservation_id)
            .await
        {
            Ok(()) => self.owner = None,
            Err(error) => {
                tracing::error!(
                    reservation_id = %self.reservation_id,
                    %error,
                    "failed to release TPM reservation"
                );
            }
        }
    }

    /// Terminal settlement (inline or durable replay) now owns reconciliation.
    pub(crate) fn transfer_to_settlement(&mut self) {
        self.owner = None;
        // Do not cancel: detached protocol settlement clones share the request
        // lifetime sentinel and still need the lease. Once all such clones are
        // gone, the weak heartbeat exits and the last 60-second lease provides
        // the bounded crash-orphan cleanup.
        self.heartbeat_cancel = None;
    }
}

impl Drop for GenerationTpmReservation {
    fn drop(&mut self) {
        self.stop_heartbeat();
        let Some((rate_limiter, rate_key)) = self.owner.take() else {
            return;
        };
        let reservation_id = self.reservation_id;
        // Cancellation cannot await cleanup. Backend records also expire at the
        // TPM window boundary if runtime shutdown prevents this best-effort task.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Err(error) = rate_limiter
                    .release_token_reservation(&rate_key, reservation_id)
                    .await
                {
                    tracing::error!(%reservation_id, %error, "failed to release cancelled TPM reservation");
                }
            });
        }
    }
}

/// Atomically admit one generation attempt against settled and in-flight TPM.
/// Known request bounds reserve their full prediction. An omitted output bound
/// uses the shared internal fallback, while input that cannot be bounded still
/// occupies the tenant's whole configured window.
pub(crate) async fn reserve_generation_tpm(
    state: &crate::state::AppState,
    ctx: &keycompute_types::RequestContext,
) -> crate::error::Result<GenerationTpmReservation> {
    let config = crate::middleware::authenticated_rate_limit_config(state, ctx.tenant_id).await?;
    let predicted_tokens = generation_tpm_reservation_tokens(ctx, config.tpm_limit);
    ctx.set_tpm_reservation_tokens(predicted_tokens);
    let request_lifetime = ctx.tpm_reservation_lifetime();
    let settlement_stopped = ctx.tpm_reservation_heartbeat_stopped();
    let rate_key =
        keycompute_ratelimit::RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
    let request_id = ctx.request_id;
    let billing_request_id = ctx.billing_request_id;
    let rate_limiter = std::sync::Arc::clone(&state.rate_limiter);
    let (handoff_tx, handoff_rx) = tokio::sync::oneshot::channel();

    // Detach the backend operation from the request future. If cancellation
    // races with an already-sent EVAL, this worker still observes its terminal
    // result. The armed guard itself is handed through the channel: failed
    // send, an unread receiver, and a received-then-cancelled handler all drop
    // the same owner and therefore compensate safely.
    tokio::spawn(async move {
        let mut reservation = GenerationTpmReservation::armed(
            std::sync::Arc::clone(&rate_limiter),
            rate_key.clone(),
            request_id,
            billing_request_id,
            predicted_tokens,
        );
        let result = rate_limiter
            .reserve_token_usage(
                &rate_key,
                request_id,
                billing_request_id,
                predicted_tokens,
                &config,
            )
            .await
            .map(|()| {
                reservation.start_heartbeat(request_lifetime, settlement_stopped);
                reservation
            });
        let _ = handoff_tx.send(result);
    });

    let result = handoff_rx.await.map_err(|error| {
        tracing::error!(%request_id, %billing_request_id, %error, "TPM reservation worker stopped before handoff");
        crate::error::ApiError::ServiceUnavailable(
            "Rate limit check failed. Please try again later.".to_string(),
        )
    })?;
    result.map_err(|error| match error {
        keycompute_types::KeyComputeError::RateLimitExceeded(_) => {
            crate::error::ApiError::RateLimit(
                "Rate limit exceeded. Please try again later.".to_string(),
            )
        }
        other => {
            tracing::error!(
                request_id = %ctx.request_id,
                billing_request_id = %ctx.billing_request_id,
                %other,
                "TPM reservation failed closed"
            );
            crate::error::ApiError::ServiceUnavailable(
                "Rate limit check failed. Please try again later.".to_string(),
            )
        }
    })
}

/// Restore a lease represented by the durable settlement outbox. This path is
/// deliberately separate from admission: the generation was already admitted
/// before dispatch, so forgetting it after restart would let fresh work bypass
/// the configured TPM cap. The backend transition remains terminal-fenced and
/// validates the exact physical ID and prediction.
pub(crate) async fn restore_generation_tpm_lease(
    state: &crate::state::AppState,
    ctx: &keycompute_types::RequestContext,
) -> crate::error::Result<bool> {
    let predicted_tokens = ctx.tpm_reservation_tokens().ok_or_else(|| {
        crate::error::ApiError::Internal(
            "Durable settlement is missing its TPM reservation prediction".to_string(),
        )
    })?;
    let rate_key =
        keycompute_ratelimit::RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
    let request_id = ctx.request_id;
    let billing_request_id = ctx.billing_request_id;
    let request_lifetime = ctx.tpm_reservation_lifetime();
    let settlement_stopped = ctx.tpm_reservation_heartbeat_stopped();
    let rate_limiter = std::sync::Arc::clone(&state.rate_limiter);
    let (handoff_tx, handoff_rx) = tokio::sync::oneshot::channel();

    // As with initial admission, drive a possibly-sent Redis EVAL to a known
    // result outside the caller future. If handoff is cancelled, dropping the
    // armed guard releases any restored physical reservation.
    tokio::spawn(async move {
        let mut reservation = GenerationTpmReservation::armed(
            std::sync::Arc::clone(&rate_limiter),
            rate_key.clone(),
            request_id,
            billing_request_id,
            predicted_tokens,
        );
        let result = rate_limiter
            .restore_token_reservation(&rate_key, request_id, billing_request_id, predicted_tokens)
            .await
            .map(|active| {
                active.then(|| {
                    reservation.start_heartbeat(request_lifetime, settlement_stopped);
                    reservation
                })
            });
        let _ = handoff_tx.send(result);
    });

    let result = handoff_rx.await.map_err(|error| {
        tracing::error!(%request_id, %billing_request_id, %error, "TPM lease restore worker stopped before handoff");
        crate::error::ApiError::ServiceUnavailable(
            "Rate limit recovery failed. Please try again later.".to_string(),
        )
    })?;
    let mut reservation = result.map_err(|error| {
        tracing::error!(%request_id, %billing_request_id, %error, "durable TPM lease restore failed closed");
        crate::error::ApiError::ServiceUnavailable(
            "Rate limit recovery failed. Please try again later.".to_string(),
        )
    })?;
    let active = reservation.is_some();
    if let Some(reservation) = reservation.as_mut() {
        reservation.transfer_to_settlement();
    }
    Ok(active)
}

pub(crate) async fn reserve_generation_balance(
    state: &crate::state::AppState,
    ctx: &keycompute_types::RequestContext,
    lifetime: GenerationBalanceReservationLifetime,
) -> crate::error::Result<GenerationBalanceReservation> {
    let Some(balance) = state.billing.balance_service() else {
        return Ok(GenerationBalanceReservation {
            owner: None,
            billing_request_id: ctx.billing_request_id,
        });
    };
    let balance = balance.clone();
    let amount = maximum_generation_reservation_amount(ctx);
    // Generate and bind ownership before PostgreSQL starts so every durable
    // settlement snapshot carries the exact owner used by the reservation.
    let owner_token = uuid::Uuid::new_v4();
    ctx.set_balance_reservation_owner_token(owner_token);
    let user_id = ctx.user_id;
    let tenant_id = ctx.tenant_id;
    let billing_request_id = ctx.billing_request_id;
    let reservation_ttl = generation_balance_reservation_ttl(&state.gateway_config, lifetime);
    let (handoff_tx, handoff_rx) = tokio::sync::oneshot::channel();

    // COMMIT has an ambiguous cancellation window: the database may commit
    // after its caller was dropped but before a compensating query can see the
    // row. Let a detached worker drive COMMIT to a known result and transfer
    // the armed owner guard as the channel value. There is then no ownership
    // gap at any point in the handoff.
    tokio::spawn(async move {
        let reservation_guard = GenerationBalanceReservation {
            owner: Some((balance.clone(), owner_token)),
            billing_request_id,
        };
        let result = balance
            .reserve_request_with_owner_token(
                user_id,
                tenant_id,
                billing_request_id,
                owner_token,
                amount,
                reservation_ttl,
            )
            .await
            .map(|reservation| {
                debug_assert_eq!(reservation.owner_token, owner_token);
                reservation_guard
            });
        let _ = handoff_tx.send(result);
    });

    handoff_rx
        .await
        .map_err(|error| {
            tracing::error!(%billing_request_id, %error, "balance reservation worker stopped before handoff");
            crate::error::ApiError::ServiceUnavailable(
                "Balance reservation failed. Please try again later.".to_string(),
            )
        })?
        .map_err(crate::error::ApiError::from)
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum GenerationBalanceReservationLifetime {
    Gateway,
    Node(std::time::Duration),
    Responses,
}

const GENERATION_BALANCE_RESERVATION_HANDOFF_MARGIN: std::time::Duration =
    std::time::Duration::from_secs(2 * 60 * 60);
/// 客户端未提供生成上限时使用的内部预留回退值。
///
/// 余额和 TPM 预留共用该值。它不会写入上游请求，也不是模型输出的硬限制；
/// 请求结束后仍以上游返回的实际 usage 为准结算。
const DEFAULT_OUTPUT_RESERVATION_TOKENS: u32 = 4_096;
/// 无法从请求 JSON 精确估算媒体、托管文件或历史响应输入时使用的风险下限。
const UNBOUNDED_INPUT_RESERVATION_TOKENS: u32 = 1_000_000;

fn unknown_input_reservation_tokens(serialized_request_size_bound: u32) -> u32 {
    UNBOUNDED_INPUT_RESERVATION_TOKENS.max(serialized_request_size_bound)
}

fn generation_balance_reservation_ttl(
    config: &keycompute_config::GatewayConfig,
    lifetime: GenerationBalanceReservationLifetime,
) -> std::time::Duration {
    let gateway_max = std::time::Duration::from_secs(
        config
            .timeout_secs
            .max(config.request_timeout_secs)
            .max(config.stream_timeout_secs),
    );
    let execution_max = match lifetime {
        GenerationBalanceReservationLifetime::Gateway => gateway_max,
        GenerationBalanceReservationLifetime::Node(task_deadline) => gateway_max.max(task_deadline),
        GenerationBalanceReservationLifetime::Responses => {
            gateway_max.max(responses::BACKGROUND_SETTLEMENT_MAX)
        }
    };
    execution_max.saturating_add(GENERATION_BALANCE_RESERVATION_HANDOFF_MARGIN)
}

fn maximum_generation_reservation_amount(
    ctx: &keycompute_types::RequestContext,
) -> rust_decimal::Decimal {
    let budget = conservative_generation_token_budget(ctx);
    let input_tokens = if ctx.pricing_snapshot.input_price_per_1k == rust_decimal::Decimal::ZERO {
        0
    } else {
        budget.input_tokens.unwrap_or_else(|| {
            // Never let the risk budget undercut request data already present
            // in memory (for example a large inline base64 image). Hosted or
            // referenced content may still be larger, hence the fixed risk
            // floor.
            unknown_input_reservation_tokens(serialized_generation_request_size_bound(ctx))
        })
    };
    let output_tokens = if ctx.pricing_snapshot.output_price_per_1k == rust_decimal::Decimal::ZERO {
        0
    } else {
        budget.output_tokens
    };
    // Unknown media/file/history input and omitted output limits are bounded by
    // fixed internal reservation risk budgets. These values are never injected
    // into the upstream protocol. Actual usage remains authoritative at
    // settlement time and may turn available balance negative.
    keycompute_billing::calculate_amount(input_tokens, output_tokens, &ctx.pricing_snapshot)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConservativeGenerationTokenBudget {
    input_tokens: Option<u32>,
    output_tokens: u32,
}

fn generation_tpm_reservation_tokens(
    ctx: &keycompute_types::RequestContext,
    tpm_limit: u32,
) -> u32 {
    // Omitted output limits use the same shared prediction as balance
    // reservation. It is an admission estimate rather than an upstream output
    // ceiling; terminal settlement replaces it with authoritative actual usage.
    let budget = conservative_generation_token_budget(ctx);
    budget
        .input_tokens
        .map(|input_tokens| input_tokens.saturating_add(budget.output_tokens))
        .unwrap_or(tpm_limit)
}

fn conservative_generation_token_budget(
    ctx: &keycompute_types::RequestContext,
) -> ConservativeGenerationTokenBudget {
    let output_tokens =
        conservative_generation_output_tokens(ctx).unwrap_or(DEFAULT_OUTPUT_RESERVATION_TOKENS);
    let choices = ctx
        .native_openai_chat_request
        .as_deref()
        .and_then(|body| body.get("n"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| u32::try_from(value).map_or(u32::MAX, |value| value.max(1)))
        .unwrap_or(1);
    ConservativeGenerationTokenBudget {
        input_tokens: conservative_generation_input_tokens(ctx),
        output_tokens: output_tokens.saturating_mul(choices),
    }
}

fn conservative_generation_output_tokens(ctx: &keycompute_types::RequestContext) -> Option<u32> {
    let native_chat_limits = ctx
        .native_openai_chat_request
        .as_deref()
        .into_iter()
        .flat_map(|body| [body.get("max_tokens"), body.get("max_completion_tokens")])
        .flatten()
        .filter_map(serde_json::Value::as_u64)
        .filter_map(|value| u32::try_from(value).ok());

    ctx.max_tokens.into_iter().chain(native_chat_limits).max()
}

/// Return a conservative request-side upper bound for billable input tokens.
///
/// Native protocol bodies are deliberately richer than the lightweight
/// routing projection. For text/JSON-only requests, every tokenizer token
/// consumes at least one serialized UTF-8 byte, so the serialized byte count
/// is a conservative bound and naturally includes tools, schemas, Anthropic
/// system blocks, and protocol-specific fields. Media and hosted files can be
/// metered from decoded content that is not bounded by their JSON reference;
/// those return `None` so the caller applies the configured input-risk budget.
fn conservative_generation_input_tokens(ctx: &keycompute_types::RequestContext) -> Option<u32> {
    let native_body = ctx
        .native_openai_chat_request
        .as_deref()
        .or(ctx.native_openai_responses_request.as_deref())
        .or(ctx.native_anthropic_request.as_deref());

    if let Some(body) = native_body {
        if contains_unbounded_metered_input(body) {
            return None;
        }
        let serialized_bytes = serialized_json_size_bound(body);
        return Some(serialized_bytes.max(
            llm_gateway::GatewayExecutor::estimate_context_input_tokens(ctx),
        ));
    }

    if ctx.messages.iter().any(|message| {
        matches!(
            &message.content,
            keycompute_types::MessageContent::Parts(parts)
                if parts.iter().any(|part| matches!(part, keycompute_types::ContentPart::ImageUrl { .. }))
        )
    }) {
        return None;
    }
    Some(llm_gateway::GatewayExecutor::estimate_context_input_tokens(
        ctx,
    ))
}

fn serialized_generation_request_size_bound(ctx: &keycompute_types::RequestContext) -> u32 {
    ctx.native_openai_chat_request
        .as_deref()
        .or(ctx.native_openai_responses_request.as_deref())
        .or(ctx.native_anthropic_request.as_deref())
        .map(serialized_json_size_bound)
        .unwrap_or_else(|| llm_gateway::GatewayExecutor::estimate_context_input_tokens(ctx))
}

fn contains_unbounded_metered_input(value: &serde_json::Value) -> bool {
    let Some(root) = value.as_object() else {
        return contains_unbounded_content_type(value);
    };

    // These fields can make the provider load billable context that is not
    // present in this request body.
    if root
        .get("web_search_options")
        .is_some_and(serde_json::Value::is_object)
        || ["previous_response_id", "conversation", "container"]
            .iter()
            .any(|name| root.get(*name).is_some_and(non_empty_json_value))
        || root.get("mcp_servers").is_some_and(non_empty_json_value)
        || root
            .get("prompt")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|prompt| prompt.get("id").is_some_and(non_empty_json_value))
    {
        return true;
    }

    if root
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                message.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                    && message
                        .pointer("/audio/id")
                        .is_some_and(non_empty_json_value)
            })
        })
    {
        return true;
    }

    // Function/custom tools only contribute their in-body schema. Hosted
    // tools may add search, file, MCP, code-execution, or computer context.
    if root
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| !matches!(kind, "function" | "custom"))
            })
        })
    {
        return true;
    }

    contains_unbounded_content_type(value)
}

fn non_empty_json_value(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::String(value) => !value.is_empty(),
        serde_json::Value::Array(value) => !value.is_empty(),
        serde_json::Value::Object(value) => !value.is_empty(),
        _ => true,
    }
}

fn contains_unbounded_content_type(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(contains_unbounded_content_type),
        serde_json::Value::Object(object) => {
            if object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| {
                    let kind = kind.to_ascii_lowercase();
                    ["image", "audio", "video", "file", "document", "screenshot"]
                        .iter()
                        .any(|marker| kind.contains(marker))
                        || kind == "item_reference"
                })
            {
                return true;
            }
            object.values().any(contains_unbounded_content_type)
        }
        _ => false,
    }
}

#[derive(Default)]
struct JsonSizeCounter(u64);

impl std::io::Write for JsonSizeCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buffer.len() as u64);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized_json_size_bound(value: &serde_json::Value) -> u32 {
    let mut counter = JsonSizeCounter::default();
    // Serializing a Value into an infallible counter cannot fail. Counting via
    // a writer avoids allocating another copy of a potentially 96 MiB body.
    serde_json::to_writer(&mut counter, value).expect("JSON Value serialization must succeed");
    u32::try_from(counter.0).unwrap_or(u32::MAX)
}

/// Idempotently add one request's terminal usage to the shared TPM window.
///
/// Every generation protocol uses the same tenant/user/API-key key. Keeping
/// the write primitive here prevents one protocol from checking a shared TPM
/// budget without contributing its own completed usage. Callers with durable
/// or delayed settlement must pass the original terminal time so late retries
/// cannot extend the request's quota window.
pub(crate) async fn record_terminal_token_usage_at(
    rate_limiter: &keycompute_ratelimit::RateLimitService,
    ctx: &keycompute_types::RequestContext,
    total_tokens: u32,
    occurred_at: std::time::SystemTime,
) -> keycompute_types::Result<()> {
    let rate_key =
        keycompute_ratelimit::RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
    rate_limiter
        .reconcile_token_usage_once_at(
            &rate_key,
            ctx.request_id,
            ctx.billing_request_id,
            total_tokens,
            occurred_at,
        )
        .await?;
    ctx.stop_tpm_reservation_heartbeat();
    Ok(())
}

pub(crate) async fn release_terminal_token_reservation(
    rate_limiter: &keycompute_ratelimit::RateLimitService,
    ctx: &keycompute_types::RequestContext,
) -> keycompute_types::Result<()> {
    // Stop first: the backend delete is cancellation-safe and idempotent, but a
    // heartbeat already inside the missing-lease restore path can land after
    // that delete. Its stop branch performs a final release, so every ordering
    // converges on no active physical reservation even if this caller is
    // cancelled after the first delete commits.
    ctx.stop_tpm_reservation_heartbeat();
    let rate_key =
        keycompute_ratelimit::RateLimitKey::new(ctx.tenant_id, ctx.user_id, ctx.produce_ai_key_id);
    rate_limiter
        .release_token_reservation(&rate_key, ctx.request_id)
        .await?;
    Ok(())
}

/// Record the current usage snapshot at the terminal point of an immediate
/// request lifecycle such as Chat Completions or Messages.
#[cfg(test)]
pub(crate) async fn record_terminal_token_usage(
    rate_limiter: &keycompute_ratelimit::RateLimitService,
    ctx: &keycompute_types::RequestContext,
) -> keycompute_types::Result<()> {
    let (input_tokens, output_tokens) = ctx.usage_snapshot();
    record_terminal_token_usage_at(
        rate_limiter,
        ctx,
        input_tokens.saturating_add(output_tokens),
        std::time::SystemTime::now(),
    )
    .await
}

/// Finalize a non-Responses generation through the same durable replay path
/// used by Responses. The outbox is installed before either side effect so a
/// crash or backend outage cannot permanently lose billing or TPM accounting.
///
/// Returns `true` when every effect completed inline or unfinished work was
/// durably deferred. A `false` result is logged as an operator-visible failure;
/// this can only occur without a database or when the outbox write also fails.
pub(crate) async fn finalize_immediate_settlement_logged(
    settlement: &ImmediateSettlementServices,
    ctx: &keycompute_types::RequestContext,
    primary_provider: &str,
    primary_account_id: uuid::Uuid,
    status: &str,
    protocol: &'static str,
) -> bool {
    let terminal_at = chrono::Utc::now();
    let durable = if let Some(state) = settlement.durable_state.as_ref() {
        responses::persist_immediate_terminal_settlement_outbox(
            state,
            ctx,
            primary_provider,
            primary_account_id,
            status,
            terminal_at,
        )
        .await
    } else {
        false
    };
    let (provider, account_id) = ctx.billing_target(primary_provider, primary_account_id);
    let (input_tokens, output_tokens) = ctx.usage_snapshot();
    let occurred_at = std::time::SystemTime::from(terminal_at);
    let (billing_result, tpm_result) = tokio::join!(
        settlement.billing.finalize_and_trigger_distribution(
            ctx,
            &provider,
            account_id,
            status,
            ctx.user_id,
        ),
        record_terminal_token_usage_at(
            settlement.rate_limiter.as_ref(),
            ctx,
            input_tokens.saturating_add(output_tokens),
            occurred_at,
        ),
    );

    let billing_complete = match billing_result {
        Ok(_) => true,
        Err(error) => {
            tracing::error!(
                request_id = %ctx.request_id,
                %protocol,
                %error,
                durable,
                "failed to finalize immediate billing"
            );
            false
        }
    };
    let tpm_complete = match tpm_result {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                request_id = %ctx.request_id,
                %protocol,
                %error,
                durable,
                "failed to record immediate token usage for TPM limiting"
            );
            false
        }
    };

    if billing_complete && tpm_complete {
        if let Some(state) = settlement.durable_state.as_ref() {
            responses::acknowledge_terminal_settlement_outbox(state, ctx).await;
        }
        return true;
    }
    if !durable {
        tracing::error!(
            request_id = %ctx.request_id,
            %protocol,
            "immediate settlement failed and could not be durably deferred"
        );
    }
    durable
}

/// Persist the first client-facing outcome selected by the handler.
///
/// Billing and protocol delivery happen before successful callers reach this
/// helper. `RequestContext` keeps the first outcome so a concurrent body drop
/// cannot overwrite a response that was already handed off, or vice versa.
pub(crate) async fn finish_client_response_trace(
    lifecycle: &std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
    ctx: &keycompute_types::RequestContext,
    outcome: keycompute_types::ClientResponseOutcome,
) {
    use keycompute_types::ClientResponseOutcome;

    match outcome {
        ClientResponseOutcome::Succeeded => ctx.mark_client_response_succeeded(),
        ClientResponseOutcome::ClientDisconnected => ctx.mark_client_disconnected(),
        ClientResponseOutcome::ResponseFailed => ctx.mark_client_response_failed(),
        ClientResponseOutcome::TimedOut => ctx.mark_client_response_timed_out(),
    }
    let effective_outcome = ctx.client_response_outcome().unwrap_or(outcome);
    let finish = keycompute_types::client_response_trace_finish_with_failure(
        ctx.request_id,
        effective_outcome,
        ctx.execution_failure(),
    );
    if let Err(error) = lifecycle.finish_request_without_attempt(finish).await {
        tracing::warn!(
            request_id = %ctx.request_id,
            ?effective_outcome,
            %error,
            "failed to finish request trace after client response"
        );
    }
}

/// Owns terminalization from the moment a request trace is created until a
/// `RequestContext`-aware response guard takes over.
///
/// Axum may drop a handler future when the client disconnects while the handler
/// is still awaiting balance, pricing, or routing. Those phases do not yet have
/// a `RequestContext`, but their process-local metrics and database trace still
/// need a terminal outcome.
pub(crate) struct PreExecutionTraceGuard {
    lifecycle: std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
    request_id: uuid::Uuid,
    armed: bool,
}

impl PreExecutionTraceGuard {
    pub(crate) fn new(
        lifecycle: std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
        request_id: uuid::Uuid,
    ) -> Self {
        Self {
            lifecycle,
            request_id,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(crate) async fn finish_failed(
        &mut self,
        origin: keycompute_types::ErrorOrigin,
        category: keycompute_types::TraceErrorCategory,
        code: &str,
    ) {
        let finish = keycompute_types::RequestTraceFinish {
            request_id: self.request_id,
            status: keycompute_types::RequestStatus::Failed,
            error: Some(keycompute_types::TraceErrorInfo {
                origin,
                category,
                code: code.to_string(),
                summary: None,
                retryable: Some(false),
            }),
            billing_status: keycompute_types::BillingStatus::NotApplicable,
            finished_at: chrono::Utc::now(),
        };
        if let Err(error) = self.lifecycle.finish_request_without_attempt(finish).await {
            tracing::warn!(
                request_id = %self.request_id,
                %error,
                "failed to finish pre-execution trace"
            );
        }
        self.disarm();
    }

    /// Finish a request served entirely from durable idempotency state.
    pub(crate) async fn finish_replayed(
        &mut self,
        outcome: keycompute_types::ClientResponseOutcome,
    ) {
        let mut finish = keycompute_types::client_response_trace_finish_with_failure(
            self.request_id,
            outcome,
            None,
        );
        finish.billing_status = keycompute_types::BillingStatus::NotApplicable;
        if let Err(error) = self.lifecycle.finish_request_without_attempt(finish).await {
            tracing::warn!(
                request_id = %self.request_id,
                %error,
                "failed to finish replayed request trace"
            );
        }
        self.disarm();
    }
}

impl Drop for PreExecutionTraceGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let lifecycle = std::sync::Arc::clone(&self.lifecycle);
        let request_id = self.request_id;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(%request_id, "unable to finish cancelled pre-execution request outside a Tokio runtime");
            return;
        };
        runtime.spawn(async move {
            let finish = keycompute_types::RequestTraceFinish {
                request_id,
                status: keycompute_types::RequestStatus::Cancelled,
                error: Some(keycompute_types::TraceErrorInfo {
                    origin: keycompute_types::ErrorOrigin::Gateway,
                    category: keycompute_types::TraceErrorCategory::ClientDisconnect,
                    code: "client_disconnected".to_string(),
                    summary: None,
                    retryable: Some(false),
                }),
                billing_status: keycompute_types::BillingStatus::NotApplicable,
                finished_at: chrono::Utc::now(),
            };
            if let Err(error) = lifecycle.finish_request_without_attempt(finish).await {
                tracing::warn!(%request_id, %error, "failed to finish cancelled pre-execution request");
            }
        });
    }
}

/// Marks and persists a response as disconnected if the HTTP handler is
/// cancelled before it records a terminal client outcome. This closes the
/// narrow race where a background response worker successfully hands its
/// result to a oneshot channel just before Axum drops the handler future.
pub(crate) struct ClientResponseGuard {
    lifecycle: std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
    ctx: std::sync::Arc<keycompute_types::RequestContext>,
    armed: bool,
}

impl ClientResponseGuard {
    pub(crate) fn new(
        lifecycle: std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
        ctx: std::sync::Arc<keycompute_types::RequestContext>,
    ) -> Self {
        Self {
            lifecycle,
            ctx,
            armed: true,
        }
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(crate) async fn finish_with_outcome(
        &mut self,
        outcome: keycompute_types::ClientResponseOutcome,
    ) {
        finish_client_response_trace(&self.lifecycle, &self.ctx, outcome).await;
        self.disarm();
    }
}

impl Drop for ClientResponseGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        self.ctx.mark_client_disconnected();
        let lifecycle = std::sync::Arc::clone(&self.lifecycle);
        let ctx = std::sync::Arc::clone(&self.ctx);
        let request_id = ctx.request_id;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(%request_id, "unable to finish cancelled request outside a Tokio runtime");
            return;
        };
        runtime.spawn(async move {
            finish_client_response_trace(
                &lifecycle,
                &ctx,
                keycompute_types::ClientResponseOutcome::ClientDisconnected,
            )
            .await;
        });
    }
}

/// Record the first client-visible content for a response whose execution may
/// already be terminal, then place a barrier behind the queued write. The
/// barrier must run even when enqueueing fails so request-scoped failure state
/// is cleared and the PostgreSQL recorder can downgrade the trace to partial.
pub(crate) async fn record_final_client_first_content(
    lifecycle: &std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
    request_id: uuid::Uuid,
) -> Result<(), keycompute_types::TraceWriteError> {
    let record_result = lifecycle
        .record_client_first_content(request_id, chrono::Utc::now())
        .await;
    let flush_result = lifecycle.flush_intermediate_updates(request_id).await;

    match (record_result, flush_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(record_error), Err(flush_error)) => Err(keycompute_types::TraceWriteError(format!(
            "{record_error}; final intermediate flush failed: {flush_error}"
        ))),
    }
}

fn normalize_public_base_url(base_url: &str) -> Option<String> {
    let normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub(crate) fn configured_public_base_url(configured_base_url: Option<&str>) -> Option<String> {
    configured_base_url.and_then(normalize_public_base_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct CancellationAwareLimiter {
        block_first_reserve: bool,
        block_first_release: bool,
        block_first_restore: bool,
        renew_inactive_once: std::sync::atomic::AtomicBool,
        renew_always_inactive: bool,
        restore_active: bool,
        reserve_calls: std::sync::atomic::AtomicUsize,
        renew_calls: std::sync::atomic::AtomicUsize,
        restore_calls: std::sync::atomic::AtomicUsize,
        restored_reservation: std::sync::Mutex<Option<(uuid::Uuid, uuid::Uuid, u32)>>,
        release_calls: std::sync::atomic::AtomicUsize,
        reserve_gate: tokio::sync::Semaphore,
        restore_gate: tokio::sync::Semaphore,
    }

    impl Default for CancellationAwareLimiter {
        fn default() -> Self {
            Self {
                block_first_reserve: false,
                block_first_release: false,
                block_first_restore: false,
                renew_inactive_once: std::sync::atomic::AtomicBool::new(false),
                renew_always_inactive: false,
                restore_active: true,
                reserve_calls: std::sync::atomic::AtomicUsize::new(0),
                renew_calls: std::sync::atomic::AtomicUsize::new(0),
                restore_calls: std::sync::atomic::AtomicUsize::new(0),
                restored_reservation: std::sync::Mutex::new(None),
                release_calls: std::sync::atomic::AtomicUsize::new(0),
                reserve_gate: tokio::sync::Semaphore::new(0),
                restore_gate: tokio::sync::Semaphore::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl keycompute_ratelimit::RateLimiter for CancellationAwareLimiter {
        async fn check(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
        ) -> keycompute_types::Result<bool> {
            Ok(true)
        }

        async fn check_with_config(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _config: &keycompute_ratelimit::RateLimitConfig,
        ) -> keycompute_types::Result<bool> {
            Ok(true)
        }

        async fn record(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
        ) -> keycompute_types::Result<()> {
            Ok(())
        }

        async fn renew_token_reservation(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _reservation_id: uuid::Uuid,
            _terminal_id: uuid::Uuid,
            _predicted_tokens: u32,
        ) -> keycompute_types::Result<bool> {
            self.renew_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(!(self.renew_always_inactive
                || self
                    .renew_inactive_once
                    .swap(false, std::sync::atomic::Ordering::SeqCst)))
        }

        async fn restore_token_reservation(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            reservation_id: uuid::Uuid,
            terminal_id: uuid::Uuid,
            predicted_tokens: u32,
        ) -> keycompute_types::Result<bool> {
            let call = self
                .restore_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.block_first_restore && call == 0 {
                self.restore_gate
                    .acquire()
                    .await
                    .expect("test restore gate must stay open")
                    .forget();
            }
            *self.restored_reservation.lock().unwrap() =
                Some((reservation_id, terminal_id, predicted_tokens));
            Ok(self.restore_active)
        }

        async fn record_tokens(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _tokens: u32,
        ) -> keycompute_types::Result<()> {
            Ok(())
        }

        async fn reserve_tokens(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _reservation_id: uuid::Uuid,
            _terminal_id: uuid::Uuid,
            _predicted_tokens: u32,
            _limit: u32,
        ) -> keycompute_types::Result<()> {
            let call = self
                .reserve_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.block_first_reserve && call == 0 {
                self.reserve_gate
                    .acquire()
                    .await
                    .expect("test reserve gate must stay open")
                    .forget();
            }
            Ok(())
        }

        async fn release_token_reservation(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _request_id: uuid::Uuid,
        ) -> keycompute_types::Result<()> {
            let call = self
                .release_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // Model the backend mutation committing before its reply reaches
            // the caller. A later restore can repopulate this slot while the
            // first release future is still waiting or has been cancelled.
            *self.restored_reservation.lock().unwrap() = None;
            if self.block_first_release && call == 0 {
                std::future::pending::<()>().await;
            }
            Ok(())
        }

        async fn reconcile_tokens_once_at(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
            _reservation_id: uuid::Uuid,
            _terminal_id: uuid::Uuid,
            _tokens: u32,
            _occurred_at: std::time::SystemTime,
        ) -> keycompute_types::Result<()> {
            Ok(())
        }

        async fn get_count(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
        ) -> keycompute_types::Result<u64> {
            Ok(0)
        }

        async fn get_token_count(
            &self,
            _key: &keycompute_ratelimit::RateLimitKey,
        ) -> keycompute_types::Result<u64> {
            Ok(0)
        }
    }

    #[tokio::test]
    async fn cancellation_during_tpm_release_keeps_drop_retry_armed() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter {
            block_first_release: true,
            ..CancellationAwareLimiter::default()
        });
        let rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let reservation_id = uuid::Uuid::new_v4();
        let mut reservation = GenerationTpmReservation::armed(
            rate_limiter,
            keycompute_ratelimit::RateLimitKey::new(
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
            ),
            reservation_id,
            reservation_id,
            1,
        );

        let release_task = tokio::spawn(async move {
            reservation.release().await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the explicit release should reach the backend");

        release_task.abort();
        assert!(release_task.await.unwrap_err().is_cancelled());

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Drop should retry a release cancelled during its await");
    }

    #[tokio::test]
    async fn cancellation_during_tpm_reserve_releases_an_ambiguous_backend_write() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter {
            block_first_reserve: true,
            ..CancellationAwareLimiter::default()
        });
        let mut state =
            crate::state::AppState::with_config(crate::state::AppStateConfig::default());
        state.rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = reservation_context(keycompute_types::PricingSnapshot::default());

        let reserve_task = tokio::spawn(async move { reserve_generation_tpm(&state, &ctx).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .reserve_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the TPM reservation should reach the backend");

        reserve_task.abort();
        assert!(matches!(reserve_task.await, Err(error) if error.is_cancelled()));
        backend.reserve_gate.add_permits(1);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Drop should compensate a reservation whose reply was cancelled");
    }

    #[tokio::test]
    async fn tpm_heartbeat_follows_shared_request_and_settlement_lifetime() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter::default());
        let rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = reservation_context(keycompute_types::PricingSnapshot::default());
        let mut reservation = GenerationTpmReservation::armed(
            std::sync::Arc::clone(&rate_limiter),
            keycompute_ratelimit::RateLimitKey::new(
                ctx.tenant_id,
                ctx.user_id,
                ctx.produce_ai_key_id,
            ),
            ctx.request_id,
            ctx.billing_request_id,
            42,
        );
        reservation.start_heartbeat_with_interval(
            ctx.tpm_reservation_lifetime(),
            ctx.tpm_reservation_heartbeat_stopped(),
            std::time::Duration::from_millis(10),
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .renew_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an active request should renew its TPM lease");

        reservation.transfer_to_settlement();
        drop(reservation);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .renew_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("settlement ownership transfer must keep the heartbeat alive");

        record_terminal_token_usage_at(
            rate_limiter.as_ref(),
            &ctx,
            7,
            std::time::SystemTime::now(),
        )
        .await
        .unwrap();
        // Allow one potentially in-flight renewal to finish, then prove that
        // successful reconciliation stops the heartbeat even while the shared
        // request context itself remains alive.
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let renewals_after_reconcile = backend
            .renew_calls
            .load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            backend
                .renew_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            renewals_after_reconcile
        );
    }

    #[tokio::test]
    async fn tpm_heartbeat_restores_a_missing_active_lease() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter {
            renew_inactive_once: std::sync::atomic::AtomicBool::new(true),
            ..CancellationAwareLimiter::default()
        });
        let rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = reservation_context(keycompute_types::PricingSnapshot::default());
        let mut reservation = GenerationTpmReservation::armed(
            rate_limiter,
            keycompute_ratelimit::RateLimitKey::new(
                ctx.tenant_id,
                ctx.user_id,
                ctx.produce_ai_key_id,
            ),
            ctx.request_id,
            ctx.billing_request_id,
            42,
        );
        reservation.start_heartbeat_with_interval(
            ctx.tpm_reservation_lifetime(),
            ctx.tpm_reservation_heartbeat_stopped(),
            std::time::Duration::from_millis(10),
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
                || backend
                    .renew_calls
                    .load(std::sync::atomic::Ordering::SeqCst)
                    < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a missing active lease should be restored and renewed again");
        assert_eq!(
            backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        reservation.release().await;
    }

    #[tokio::test]
    async fn stopped_tpm_heartbeat_releases_a_late_missing_lease_restore() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter {
            block_first_restore: true,
            renew_inactive_once: std::sync::atomic::AtomicBool::new(true),
            ..CancellationAwareLimiter::default()
        });
        let rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = reservation_context(keycompute_types::PricingSnapshot::default());
        let mut reservation = GenerationTpmReservation::armed(
            rate_limiter,
            keycompute_ratelimit::RateLimitKey::new(
                ctx.tenant_id,
                ctx.user_id,
                ctx.produce_ai_key_id,
            ),
            ctx.request_id,
            ctx.billing_request_id,
            42,
        );
        reservation.start_heartbeat_with_interval(
            ctx.tpm_reservation_lifetime(),
            ctx.tpm_reservation_heartbeat_stopped(),
            std::time::Duration::from_millis(10),
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the missing-lease restore should be in flight");

        // Explicit release deletes the original record while restore is still
        // blocked. Let restore land afterwards and prove the stopped heartbeat
        // issues a final idempotent release for the resurrected record.
        reservation.release().await;
        assert_eq!(
            backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        backend.restore_gate.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the stopped heartbeat should compensate the late restore");
    }

    #[tokio::test]
    async fn cancelled_terminal_release_cannot_leave_a_late_restore_active() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter {
            block_first_release: true,
            block_first_restore: true,
            renew_inactive_once: std::sync::atomic::AtomicBool::new(true),
            ..CancellationAwareLimiter::default()
        });
        let rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = std::sync::Arc::new(reservation_context(
            keycompute_types::PricingSnapshot::default(),
        ));
        let mut reservation = GenerationTpmReservation::armed(
            std::sync::Arc::clone(&rate_limiter),
            keycompute_ratelimit::RateLimitKey::new(
                ctx.tenant_id,
                ctx.user_id,
                ctx.produce_ai_key_id,
            ),
            ctx.request_id,
            ctx.billing_request_id,
            42,
        );
        reservation.start_heartbeat_with_interval(
            ctx.tpm_reservation_lifetime(),
            ctx.tpm_reservation_heartbeat_stopped(),
            std::time::Duration::from_millis(10),
        );
        reservation.transfer_to_settlement();
        drop(reservation);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the heartbeat restore should be in flight before terminal release");

        let release_rate_limiter = std::sync::Arc::clone(&rate_limiter);
        let release_ctx = std::sync::Arc::clone(&ctx);
        let release_task = tokio::spawn(async move {
            release_terminal_token_reservation(release_rate_limiter.as_ref(), &release_ctx).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the terminal release should commit before its reply is cancelled");

        release_task.abort();
        assert!(release_task.await.unwrap_err().is_cancelled());

        // The restore was selected before the terminal stop signal, so let it
        // land after the caller that issued the first release has disappeared.
        backend.restore_gate.add_permits(1);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                < 2
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the stopped heartbeat should release the late restored reservation");
        assert_eq!(
            *backend.restored_reservation.lock().unwrap(),
            None,
            "no physical TPM reservation may survive terminal release"
        );
    }

    #[tokio::test]
    async fn tpm_heartbeat_stops_when_restore_observes_a_terminal_fence() {
        let backend = std::sync::Arc::new(CancellationAwareLimiter {
            renew_always_inactive: true,
            restore_active: false,
            ..CancellationAwareLimiter::default()
        });
        let rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = reservation_context(keycompute_types::PricingSnapshot::default());
        let mut reservation = GenerationTpmReservation::armed(
            rate_limiter,
            keycompute_ratelimit::RateLimitKey::new(
                ctx.tenant_id,
                ctx.user_id,
                ctx.produce_ai_key_id,
            ),
            ctx.request_id,
            ctx.billing_request_id,
            42,
        );
        reservation.start_heartbeat_with_interval(
            ctx.tpm_reservation_lifetime(),
            ctx.tpm_reservation_heartbeat_stopped(),
            std::time::Duration::from_millis(10),
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the heartbeat should ask the terminal-fenced restore path");
        let renewals_after_terminal = backend
            .renew_calls
            .load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert_eq!(
            backend
                .renew_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            renewals_after_terminal,
            "terminal usage must stop the heartbeat instead of resurrecting the lease"
        );
        assert_eq!(
            backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            backend
                .release_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the terminal-fenced path must remove this physical prediction"
        );

        reservation.release().await;
    }

    #[tokio::test]
    async fn terminal_fenced_heartbeat_removes_only_its_physical_prediction() {
        let rate_limiter =
            std::sync::Arc::new(keycompute_ratelimit::RateLimitService::default_memory());
        let mut ctx = reservation_context(keycompute_types::PricingSnapshot::default());
        ctx.billing_request_id = uuid::Uuid::new_v4();
        let rate_key = keycompute_ratelimit::RateLimitKey::new(
            ctx.tenant_id,
            ctx.user_id,
            ctx.produce_ai_key_id,
        );
        let first_physical_id = uuid::Uuid::new_v4();
        let config = keycompute_ratelimit::RateLimitConfig::new(100, 100);

        rate_limiter
            .reserve_token_usage(
                &rate_key,
                first_physical_id,
                ctx.billing_request_id,
                20,
                &config,
            )
            .await
            .unwrap();
        rate_limiter
            .reserve_token_usage(
                &rate_key,
                ctx.request_id,
                ctx.billing_request_id,
                30,
                &config,
            )
            .await
            .unwrap();
        rate_limiter
            .reconcile_token_usage_once_at(
                &rate_key,
                first_physical_id,
                ctx.billing_request_id,
                7,
                std::time::SystemTime::now(),
            )
            .await
            .unwrap();
        assert_eq!(rate_limiter.get_tpm_count(&rate_key).await.unwrap(), 37);

        let mut reservation = GenerationTpmReservation::armed(
            std::sync::Arc::clone(&rate_limiter),
            rate_key.clone(),
            ctx.request_id,
            ctx.billing_request_id,
            30,
        );
        reservation.start_heartbeat_with_interval(
            ctx.tpm_reservation_lifetime(),
            ctx.tpm_reservation_heartbeat_stopped(),
            std::time::Duration::from_millis(10),
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while rate_limiter.get_tpm_count(&rate_key).await.unwrap() != 7 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the terminal fence should remove the other physical prediction");
        assert_eq!(
            rate_limiter.get_tpm_count(&rate_key).await.unwrap(),
            7,
            "the logical terminal usage itself must remain immutable"
        );

        reservation.release().await;
    }

    #[tokio::test]
    async fn durable_tpm_restore_reinstates_the_exact_persisted_prediction() {
        let mut state =
            crate::state::AppState::with_config(crate::state::AppStateConfig::default());
        let backend = std::sync::Arc::new(CancellationAwareLimiter::default());
        state.rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            backend.clone(),
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        let ctx = reservation_context(keycompute_types::PricingSnapshot::default());
        ctx.set_tpm_reservation_tokens(77);

        assert!(restore_generation_tpm_lease(&state, &ctx).await.unwrap());
        assert_eq!(
            backend
                .restore_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            *backend.restored_reservation.lock().unwrap(),
            Some((ctx.request_id, ctx.billing_request_id, 77))
        );

        let terminal_backend = std::sync::Arc::new(CancellationAwareLimiter {
            restore_active: false,
            ..CancellationAwareLimiter::default()
        });
        state.rate_limiter = std::sync::Arc::new(keycompute_ratelimit::RateLimitService::new(
            terminal_backend,
            keycompute_ratelimit::RateLimitBackend::Memory,
        ));
        assert!(
            !restore_generation_tpm_lease(&state, &ctx).await.unwrap(),
            "a logical terminal fence must be observable by durable workers"
        );
    }

    #[tokio::test]
    async fn initial_stream_gate_opens_on_http_acceptance_without_a_protocol_event() {
        let ctx = keycompute_types::RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        );
        let (_status_tx, status_rx) = tokio::sync::oneshot::channel();

        ctx.mark_upstream_response_accepted();

        assert_eq!(
            await_initial_stream_status(&ctx, status_rx).await,
            InitialStreamStatus::Ready
        );
    }

    #[tokio::test]
    async fn initial_stream_gate_preserves_pre_acceptance_failure() {
        let ctx = keycompute_types::RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "gpt-test",
            Vec::new(),
            true,
            keycompute_types::PricingSnapshot::default(),
        );
        let (status_tx, status_rx) = tokio::sync::oneshot::channel();
        status_tx.send(InitialStreamStatus::Failed).unwrap();

        assert_eq!(
            await_initial_stream_status(&ctx, status_rx).await,
            InitialStreamStatus::Failed
        );
    }

    fn reservation_context(
        pricing: keycompute_types::PricingSnapshot,
    ) -> keycompute_types::RequestContext {
        keycompute_types::RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "test-model",
            vec![keycompute_types::Message::user("short projection")],
            false,
            pricing,
        )
    }

    #[test]
    fn native_anthropic_tools_are_included_in_the_reservation_upper_bound() {
        let pricing = keycompute_types::PricingSnapshot::new(
            "claude-test",
            "CNY",
            rust_decimal::Decimal::ONE,
            rust_decimal::Decimal::from(2),
        );
        let mut ctx = reservation_context(pricing);
        let body = serde_json::json!({
            "model": "claude-test",
            "max_tokens": 100,
            "system": "Follow the supplied tool contract",
            "messages": [{"role": "user", "content": "run it"}],
            "tools": [{
                "name": "large_tool",
                "description": "detailed contract ".repeat(256),
                "input_schema": {"type": "object", "properties": {"query": {"type": "string"}}}
            }]
        });
        let serialized_bytes = serialized_json_size_bound(&body);
        ctx.native_anthropic_request = Some(std::sync::Arc::new(body));
        ctx.max_tokens = Some(100);

        let input_bound = conservative_generation_input_tokens(&ctx).unwrap();
        assert!(input_bound >= serialized_bytes);
        assert!(
            input_bound > llm_gateway::GatewayExecutor::estimate_context_input_tokens(&ctx),
            "the native tool schema must not collapse to the routing projection"
        );
        assert_eq!(
            maximum_generation_reservation_amount(&ctx),
            keycompute_billing::calculate_amount(input_bound, 100, &ctx.pricing_snapshot,)
        );
    }

    #[test]
    fn metered_media_uses_the_fixed_input_risk_bound() {
        let pricing = keycompute_types::PricingSnapshot::new(
            "gpt-test",
            "CNY",
            rust_decimal::Decimal::ONE,
            rust_decimal::Decimal::ONE,
        );
        let mut ctx = reservation_context(pricing);
        ctx.native_openai_responses_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-test",
            "input": [{
                "role": "user",
                "content": [{"type": "input_image", "image_url": "https://example.invalid/image.png"}]
            }],
            "max_output_tokens": 32
        })));
        ctx.max_tokens = Some(32);

        assert_eq!(conservative_generation_input_tokens(&ctx), None);
        assert_eq!(
            generation_tpm_reservation_tokens(&ctx, 100_000),
            100_000,
            "unknown input occupies one otherwise-empty TPM window instead of permanently exceeding it"
        );
        assert_eq!(
            maximum_generation_reservation_amount(&ctx),
            keycompute_billing::calculate_amount(
                UNBOUNDED_INPUT_RESERVATION_TOKENS,
                32,
                &ctx.pricing_snapshot,
            )
        );
    }

    #[test]
    fn unknown_input_fallback_never_undercuts_the_serialized_request() {
        assert_eq!(unknown_input_reservation_tokens(1), 1_000_000);
        assert_eq!(unknown_input_reservation_tokens(1_000_001), 1_000_001);
    }

    #[test]
    fn inherited_context_and_hosted_tools_require_the_unknown_input_risk_bound() {
        for body in [
            serde_json::json!({
                "model": "gpt-test",
                "previous_response_id": "resp_existing",
                "input": "continue"
            }),
            serde_json::json!({
                "model": "gpt-test",
                "input": "search",
                "tools": [{"type": "web_search_preview"}]
            }),
            serde_json::json!({
                "model": "gpt-test",
                "input": "use the saved prompt",
                "prompt": {"id": "pmpt_existing"}
            }),
        ] {
            assert!(contains_unbounded_metered_input(&body));
        }

        assert!(!contains_unbounded_metered_input(&serde_json::json!({
            "model": "gpt-test",
            "input": "call a client tool",
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": {"type": "object"}
            }]
        })));
    }

    #[test]
    fn chat_search_and_stored_audio_require_the_unknown_input_risk_bound() {
        for body in [
            serde_json::json!({
                "model": "gpt-test",
                "messages": [{"role": "user", "content": "search"}],
                "web_search_options": {}
            }),
            serde_json::json!({
                "model": "gpt-test",
                "messages": [{
                    "role": "assistant",
                    "content": null,
                    "audio": {"id": "audio_existing"}
                }]
            }),
        ] {
            assert!(contains_unbounded_metered_input(&body));
        }

        assert!(!contains_unbounded_metered_input(&serde_json::json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}],
            "audio": {"format": "wav", "voice": "alloy"}
        })));
    }

    #[test]
    fn reservation_uses_the_larger_chat_output_limit() {
        let pricing = keycompute_types::PricingSnapshot::new(
            "gpt-test",
            "CNY",
            rust_decimal::Decimal::ZERO,
            rust_decimal::Decimal::ONE,
        );
        let mut ctx = reservation_context(pricing);
        ctx.max_tokens = Some(100);
        ctx.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 100,
            "max_completion_tokens": 256,
            "n": 2
        })));

        assert_eq!(conservative_generation_output_tokens(&ctx), Some(256));
        let input_tokens = conservative_generation_input_tokens(&ctx).unwrap();
        assert_eq!(
            generation_tpm_reservation_tokens(&ctx, 100_000),
            input_tokens.saturating_add(512),
            "TPM prediction counts input even when its monetary price is zero"
        );
        assert_eq!(
            maximum_generation_reservation_amount(&ctx),
            keycompute_billing::calculate_amount(0, 512, &ctx.pricing_snapshot,)
        );
    }

    #[test]
    fn reservation_ttl_tracks_the_execution_lifecycle() {
        let mut config = keycompute_config::GatewayConfig::default();
        assert_eq!(
            generation_balance_reservation_ttl(
                &config,
                GenerationBalanceReservationLifetime::Gateway,
            ),
            std::time::Duration::from_secs(2 * 60 * 60 + 10 * 60)
        );
        assert_eq!(
            generation_balance_reservation_ttl(
                &config,
                GenerationBalanceReservationLifetime::Node(std::time::Duration::from_secs(
                    3 * 60 * 60,
                )),
            ),
            std::time::Duration::from_secs(5 * 60 * 60)
        );
        assert_eq!(
            generation_balance_reservation_ttl(
                &config,
                GenerationBalanceReservationLifetime::Responses,
            ),
            std::time::Duration::from_secs(26 * 60 * 60)
        );

        config.stream_timeout_secs = 30 * 60 * 60;
        assert_eq!(
            generation_balance_reservation_ttl(
                &config,
                GenerationBalanceReservationLifetime::Gateway,
            ),
            std::time::Duration::from_secs(32 * 60 * 60)
        );
    }

    #[test]
    fn omitted_or_null_billable_output_uses_the_fixed_reservation_fallback() {
        let pricing = keycompute_types::PricingSnapshot::new(
            "gpt-test",
            "CNY",
            rust_decimal::Decimal::ONE,
            rust_decimal::Decimal::ONE,
        );
        for body in [
            serde_json::json!({
                "model": "gpt-test",
                "messages": [{"role": "user", "content": "hello"}]
            }),
            serde_json::json!({
                "model": "gpt-test",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": null,
                "max_completion_tokens": null
            }),
        ] {
            let mut ctx = reservation_context(pricing.clone());
            ctx.native_openai_chat_request = Some(std::sync::Arc::new(body));

            let input_tokens = conservative_generation_input_tokens(&ctx).unwrap();
            assert_eq!(
                maximum_generation_reservation_amount(&ctx),
                keycompute_billing::calculate_amount(
                    input_tokens,
                    DEFAULT_OUTPUT_RESERVATION_TOKENS,
                    &ctx.pricing_snapshot,
                )
            );
            assert_eq!(
                generation_tpm_reservation_tokens(&ctx, 100_000),
                input_tokens.saturating_add(DEFAULT_OUTPUT_RESERVATION_TOKENS),
                "an omitted output limit must use the shared TPM reservation fallback"
            );
        }
    }

    #[test]
    fn omitted_output_limit_multiplies_the_shared_fallback_by_chat_choices() {
        let pricing = keycompute_types::PricingSnapshot::new(
            "gpt-test",
            "CNY",
            rust_decimal::Decimal::ONE,
            rust_decimal::Decimal::ONE,
        );
        let mut ctx = reservation_context(pricing);
        ctx.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}],
            "n": 3
        })));

        let input_tokens = conservative_generation_input_tokens(&ctx).unwrap();
        assert_eq!(
            generation_tpm_reservation_tokens(&ctx, 100_000),
            input_tokens.saturating_add(DEFAULT_OUTPUT_RESERVATION_TOKENS.saturating_mul(3))
        );
    }

    #[test]
    fn omitted_output_limit_bounds_chat_choices_to_minimum_one() {
        let pricing = keycompute_types::PricingSnapshot::new(
            "gpt-test",
            "CNY",
            rust_decimal::Decimal::ONE,
            rust_decimal::Decimal::ONE,
        );
        let mut ctx = reservation_context(pricing);
        ctx.native_openai_chat_request = Some(std::sync::Arc::new(serde_json::json!({
            "model": "gpt-test",
            "messages": [{"role": "user", "content": "hello"}],
            "n": 0
        })));

        let input_tokens = conservative_generation_input_tokens(&ctx).unwrap();
        assert_eq!(
            generation_tpm_reservation_tokens(&ctx, 100_000),
            input_tokens.saturating_add(DEFAULT_OUTPUT_RESERVATION_TOKENS)
        );
    }

    #[tokio::test]
    async fn generation_protocols_share_one_idempotent_tpm_window() {
        let tenant_id = uuid::Uuid::new_v4();
        let user_id = uuid::Uuid::new_v4();
        let api_key_id = uuid::Uuid::new_v4();
        let rate_limiter = keycompute_ratelimit::RateLimitService::default_memory();

        let chat = keycompute_types::RequestContext::new(
            uuid::Uuid::new_v4(),
            user_id,
            tenant_id,
            api_key_id,
            "gpt-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        chat.set_input_tokens(7);
        chat.set_output_tokens(5);
        record_terminal_token_usage(&rate_limiter, &chat)
            .await
            .unwrap();
        // A repeated terminalization of the same logical request must not
        // consume the shared quota twice.
        record_terminal_token_usage(&rate_limiter, &chat)
            .await
            .unwrap();

        let messages = keycompute_types::RequestContext::new(
            uuid::Uuid::new_v4(),
            user_id,
            tenant_id,
            api_key_id,
            "claude-test",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        );
        messages.set_input_tokens(11);
        messages.set_output_tokens(7);
        record_terminal_token_usage(&rate_limiter, &messages)
            .await
            .unwrap();

        let key = keycompute_ratelimit::RateLimitKey::new(tenant_id, user_id, api_key_id);
        assert_eq!(rate_limiter.get_tpm_count(&key).await.unwrap(), 30);
        assert!(
            !rate_limiter
                .check_tpm(&key, &keycompute_ratelimit::RateLimitConfig::new(100, 30))
                .await
                .unwrap(),
            "usage produced through either generation protocol must close the shared TPM budget"
        );
    }

    fn response_guard_context() -> std::sync::Arc<keycompute_types::RequestContext> {
        std::sync::Arc::new(keycompute_types::RequestContext::new(
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            "test-model",
            Vec::new(),
            false,
            keycompute_types::PricingSnapshot::default(),
        ))
    }

    #[tokio::test]
    async fn client_response_guard_only_finishes_an_armed_handler_as_disconnected() {
        let cancelled = response_guard_context();
        let cancelled_recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        drop(ClientResponseGuard::new(
            std::sync::Arc::clone(&cancelled_recorder)
                as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
            std::sync::Arc::clone(&cancelled),
        ));
        assert!(cancelled.is_client_disconnected());
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while cancelled_recorder.request_finishes().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("armed guard should persist its disconnect outcome");
        assert_eq!(
            cancelled_recorder.request_finishes()[0].status,
            keycompute_types::RequestStatus::Cancelled
        );

        let completed = response_guard_context();
        let completed_recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let mut guard = ClientResponseGuard::new(
            std::sync::Arc::clone(&completed_recorder)
                as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
            std::sync::Arc::clone(&completed),
        );
        guard.disarm();
        drop(guard);
        assert!(!completed.is_client_disconnected());
        tokio::task::yield_now().await;
        assert!(completed_recorder.request_finishes().is_empty());
    }

    #[tokio::test]
    async fn client_response_guard_explicit_outcome_disarms_disconnect_fallback() {
        for (outcome, expected_status) in [
            (
                keycompute_types::ClientResponseOutcome::ResponseFailed,
                keycompute_types::RequestStatus::Failed,
            ),
            (
                keycompute_types::ClientResponseOutcome::TimedOut,
                keycompute_types::RequestStatus::TimedOut,
            ),
        ] {
            let ctx = response_guard_context();
            let recorder =
                std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
            let mut guard = ClientResponseGuard::new(
                std::sync::Arc::clone(&recorder)
                    as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
                std::sync::Arc::clone(&ctx),
            );

            guard.finish_with_outcome(outcome).await;
            drop(guard);
            tokio::task::yield_now().await;

            assert_eq!(ctx.client_response_outcome(), Some(outcome));
            let finishes = recorder.request_finishes();
            assert_eq!(finishes.len(), 1);
            assert_eq!(finishes[0].status, expected_status);
        }
    }

    #[tokio::test]
    async fn pre_execution_guard_finishes_cancelled_requests_without_billing() {
        let request_id = uuid::Uuid::new_v4();
        let recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        drop(PreExecutionTraceGuard::new(
            std::sync::Arc::clone(&recorder)
                as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
            request_id,
        ));

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while recorder.request_finishes().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pre-execution cancellation should be persisted");
        let finish = &recorder.request_finishes()[0];
        assert_eq!(finish.request_id, request_id);
        assert_eq!(finish.status, keycompute_types::RequestStatus::Cancelled);
        assert_eq!(
            finish.billing_status,
            keycompute_types::BillingStatus::NotApplicable
        );
        assert_eq!(
            finish.error.as_ref().map(|error| error.category),
            Some(keycompute_types::TraceErrorCategory::ClientDisconnect)
        );
    }

    #[tokio::test]
    async fn pre_execution_guard_explicit_failure_disarms_disconnect_fallback() {
        let request_id = uuid::Uuid::new_v4();
        let recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let mut guard = PreExecutionTraceGuard::new(
            std::sync::Arc::clone(&recorder)
                as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>,
            request_id,
        );

        guard
            .finish_failed(
                keycompute_types::ErrorOrigin::Client,
                keycompute_types::TraceErrorCategory::InvalidRequest,
                "invalid_request",
            )
            .await;
        drop(guard);
        tokio::task::yield_now().await;

        let finishes = recorder.request_finishes();
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].request_id, request_id);
        assert_eq!(finishes[0].status, keycompute_types::RequestStatus::Failed);
        assert_eq!(
            finishes[0].billing_status,
            keycompute_types::BillingStatus::NotApplicable
        );
        assert_eq!(
            finishes[0].error.as_ref().map(|error| error.category),
            Some(keycompute_types::TraceErrorCategory::InvalidRequest)
        );
    }

    #[tokio::test]
    async fn client_response_trace_preserves_the_first_terminal_outcome() {
        let ctx = response_guard_context();
        let recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let lifecycle = std::sync::Arc::clone(&recorder)
            as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>;

        ctx.set_execution_failure(keycompute_types::RequestExecutionFailure {
            status: keycompute_types::RequestStatus::TimedOut,
            error: keycompute_types::TraceErrorInfo {
                origin: keycompute_types::ErrorOrigin::Node,
                category: keycompute_types::TraceErrorCategory::NodeExpired,
                code: "node_expired".to_string(),
                summary: None,
                retryable: Some(false),
            },
            billing_status: keycompute_types::BillingStatus::NotApplicable,
        });
        ctx.mark_client_disconnected();
        finish_client_response_trace(
            &lifecycle,
            &ctx,
            keycompute_types::ClientResponseOutcome::Succeeded,
        )
        .await;

        assert_eq!(
            ctx.client_response_outcome(),
            Some(keycompute_types::ClientResponseOutcome::ClientDisconnected)
        );
        assert_eq!(
            recorder.request_finishes()[0].status,
            keycompute_types::RequestStatus::Cancelled
        );
        assert_eq!(
            recorder.request_finishes()[0]
                .error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("client_disconnected")
        );
    }

    #[tokio::test]
    async fn client_response_trace_uses_the_execution_failure() {
        let ctx = response_guard_context();
        let recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let lifecycle = std::sync::Arc::clone(&recorder)
            as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>;
        ctx.set_execution_failure(keycompute_types::RequestExecutionFailure {
            status: keycompute_types::RequestStatus::Failed,
            error: keycompute_types::TraceErrorInfo {
                origin: keycompute_types::ErrorOrigin::Upstream,
                category: keycompute_types::TraceErrorCategory::Upstream5xx,
                code: "upstream_failed".to_string(),
                summary: None,
                retryable: Some(true),
            },
            billing_status: keycompute_types::BillingStatus::Pending,
        });

        finish_client_response_trace(
            &lifecycle,
            &ctx,
            keycompute_types::ClientResponseOutcome::ResponseFailed,
        )
        .await;

        let finishes = recorder.request_finishes();
        assert_eq!(finishes.len(), 1);
        assert_eq!(finishes[0].status, keycompute_types::RequestStatus::Failed);
        assert_eq!(
            finishes[0].error.as_ref().map(|error| error.code.as_str()),
            Some("upstream_failed")
        );
    }

    #[tokio::test]
    async fn final_client_content_is_followed_by_an_intermediate_barrier() {
        let request_id = uuid::Uuid::new_v4();
        let recorder =
            std::sync::Arc::new(keycompute_types::TestRequestLifecycleRecorder::default());
        let lifecycle = std::sync::Arc::clone(&recorder)
            as std::sync::Arc<dyn keycompute_types::RequestLifecycleRecorder>;

        record_final_client_first_content(&lifecycle, request_id)
            .await
            .unwrap();

        assert_eq!(
            recorder.events(),
            [
                format!("client_first_content:{request_id}"),
                format!("flush_intermediate:{request_id}"),
            ]
        );
    }

    #[test]
    fn test_configured_public_base_url_prefers_configured_value() {
        let base_url = configured_public_base_url(Some("https://configured.example.com/"));

        assert_eq!(base_url.as_deref(), Some("https://configured.example.com"));
    }

    #[test]
    fn test_configured_public_base_url_returns_none_when_missing() {
        let base_url = configured_public_base_url(None);

        assert!(base_url.is_none());
    }

    #[test]
    fn test_configured_public_base_url_ignores_blank_values() {
        let base_url = configured_public_base_url(Some("   "));

        assert!(base_url.is_none());
    }
}
