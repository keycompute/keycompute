//! Resource affinity, account reservations and durable settlement state.

use super::*;

pub(super) fn response_resource_id(body: &Value) -> Option<&str> {
    body.get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_response_id(id))
}

pub(super) fn response_event_resource_id(body: &Value) -> Option<&str> {
    body.pointer("/response/id")
        .and_then(Value::as_str)
        .filter(|id| valid_response_id(id))
}

pub(super) fn conversation_resource_id(body: &Value) -> Option<&str> {
    let conversation = body
        .pointer("/response/conversation")
        .or_else(|| body.get("conversation"))?;
    conversation
        .as_str()
        .or_else(|| conversation.get("id").and_then(Value::as_str))
        .filter(|id| valid_conversation_id(id))
}

pub(super) fn validate_idempotent_project_resource_owner(
    body: &Value,
    has_idempotency_key: bool,
    account_owner_resolved: bool,
) -> Result<()> {
    if !has_idempotency_key || account_owner_resolved {
        return Ok(());
    }
    let Some(resource_path) = project_scoped_responses_resource(body) else {
        return Ok(());
    };
    Err(ApiError::BadRequest(format!(
        "Idempotency-Key requests that reference {resource_path} require previous_response_id or conversation to resolve the upstream account"
    )))
}

/// Opaque Responses resources belong to one OpenAI project. They can be sent
/// safely only when every runnable route uses the same OpenAI account or an
/// affinity branch above has already reduced the plan to the known owner.
pub(super) fn validate_project_resource_route(body: &Value, plan: &ExecutionPlan) -> Result<()> {
    let Some(resource_path) = project_scoped_responses_resource(body) else {
        return Ok(());
    };
    let account_ids = plan
        .all_targets()
        .filter_map(|target| match target {
            ExecutionTarget::ProviderAccount {
                provider,
                account_id,
                ..
            } if provider.eq_ignore_ascii_case("openai") => Some(*account_id),
            _ => None,
        })
        .collect::<HashSet<_>>();
    if account_ids.len() <= 1 {
        return Ok(());
    }
    Err(ApiError::BadRequest(format!(
        "Requests that reference {resource_path} require previous_response_id or conversation to resolve the upstream account"
    )))
}

/// Return the first Responses field whose opaque ID is only usable by the
/// OpenAI project that owns it. Restrict recursive inspection to `input` and
/// known hosted-tool fields so similarly named custom-tool schema properties
/// do not accidentally constrain routing.
pub(super) fn project_scoped_responses_resource(body: &Value) -> Option<&'static str> {
    if body
        .pointer("/prompt/id")
        .and_then(non_empty_string)
        .is_some()
    {
        return Some("prompt.id");
    }
    if body
        .pointer("/prompt_cache_options/comparison_response_id")
        .and_then(non_empty_string)
        .is_some()
    {
        return Some("prompt_cache_options.comparison_response_id");
    }
    if let Some(resource) = body.get("input").and_then(project_scoped_input_resource) {
        return Some(resource);
    }
    body.get("tools")
        .and_then(Value::as_array)
        .and_then(|tools| tools.iter().find_map(project_scoped_tool_resource))
}

pub(super) fn project_scoped_input_resource(value: &Value) -> Option<&'static str> {
    match value {
        Value::Array(values) => values.iter().find_map(project_scoped_input_resource),
        Value::Object(object) => {
            let resource = match object.get("type").and_then(Value::as_str) {
                Some("input_file")
                    if object.get("file_id").and_then(non_empty_string).is_some() =>
                {
                    Some("input_file.file_id")
                }
                Some("input_image")
                    if object.get("file_id").and_then(non_empty_string).is_some() =>
                {
                    Some("input_image.file_id")
                }
                Some("computer_screenshot")
                    if object.get("file_id").and_then(non_empty_string).is_some() =>
                {
                    Some("computer_screenshot.file_id")
                }
                Some("container_reference")
                    if object
                        .get("container_id")
                        .and_then(non_empty_string)
                        .is_some() =>
                {
                    Some("input container_id")
                }
                Some("item_reference") if object.get("id").and_then(non_empty_string).is_some() => {
                    Some("input item_reference.id")
                }
                Some("code_interpreter_call")
                    if object
                        .get("container_id")
                        .and_then(non_empty_string)
                        .is_some() =>
                {
                    Some("input code_interpreter_call.container_id")
                }
                _ => None,
            };
            resource.or_else(|| object.values().find_map(project_scoped_input_resource))
        }
        _ => None,
    }
}

pub(super) fn project_scoped_tool_resource(tool: &Value) -> Option<&'static str> {
    let tool = tool.as_object()?;
    match tool.get("type").and_then(Value::as_str) {
        Some("file_search")
            if tool
                .get("vector_store_ids")
                .is_some_and(contains_non_empty_string) =>
        {
            Some("file_search.vector_store_ids")
        }
        Some("code_interpreter") => {
            let container = tool.get("container")?;
            if non_empty_string(container).is_some() {
                return Some("code_interpreter.container");
            }
            let container_object = container.as_object()?;
            if container_object
                .get("file_ids")
                .is_some_and(contains_non_empty_string)
            {
                return Some("code_interpreter.container.file_ids");
            }
            project_scoped_input_resource(container)
        }
        Some("shell") => tool
            .get("environment")
            .and_then(project_scoped_input_resource),
        _ => None,
    }
}

pub(super) fn non_empty_string(value: &Value) -> Option<&str> {
    value.as_str().filter(|value| !value.trim().is_empty())
}

pub(super) fn contains_non_empty_string(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|values| values.iter().any(|value| non_empty_string(value).is_some()))
}

pub(super) fn patch_client_previous_response_id(
    body: &mut Value,
    previous_response_id: Option<&str>,
) {
    let Some(previous_response_id) = previous_response_id else {
        return;
    };
    let response = if body.get("object").and_then(Value::as_str) == Some("response") {
        body.as_object_mut()
    } else {
        body.get_mut("response").and_then(Value::as_object_mut)
    };
    if let Some(response) = response {
        response.insert(
            "previous_response_id".to_string(),
            Value::String(previous_response_id.to_string()),
        );
    }
}

pub(super) fn local_warmup_has_no_upstream_owner(
    replayed_client_previous_response_id: Option<&str>,
    effective_body: &Value,
) -> bool {
    replayed_client_previous_response_id.is_some()
        && effective_body
            .get("previous_response_id")
            .is_none_or(Value::is_null)
        && conversation_resource_id(effective_body).is_none()
}

pub(super) fn valid_response_id(id: &str) -> bool {
    llm_protocol_openai::responses_stream::valid_openai_resource_id(id)
}

pub(super) fn valid_conversation_id(id: &str) -> bool {
    llm_protocol_openai::responses_stream::valid_openai_resource_id(id)
}

pub(super) fn valid_affinity_resource_id(id: &str) -> bool {
    valid_response_id(id) || valid_conversation_id(id)
}

pub(super) fn upstream_resource_url(
    endpoint: &str,
    collection: &str,
    resource_id: &str,
    suffix: &str,
) -> String {
    let encoded_id = utf8_percent_encode(resource_id, NON_ALPHANUMERIC);
    format!(
        "{}/{collection}/{encoded_id}{suffix}",
        endpoint.trim_end_matches('/')
    )
}

pub(super) const RESPONSES_AFFINITY_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub(super) const RESPONSES_AFFINITY_LOCAL_MAX: usize = 100_000;
pub(super) const RESPONSES_EXECUTION_RESERVATION_MIN_TTL: Duration =
    Duration::from_secs(2 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ResponsesResourceKind {
    Response,
    Conversation,
}

pub(super) struct ResponsesAffinityRoute {
    pub(super) tenant_id: uuid::Uuid,
    pub(super) provider: String,
    pub(super) model: Option<String>,
    pub(super) account_id: uuid::Uuid,
}

pub(super) fn responses_execution_reservation_ttl(
    config: &keycompute_config::GatewayConfig,
) -> Duration {
    Duration::from_secs(
        config
            .timeout_secs
            .max(config.request_timeout_secs)
            .max(config.stream_timeout_secs),
    )
    .saturating_add(Duration::from_secs(60))
    .max(RESPONSES_EXECUTION_RESERVATION_MIN_TTL)
}

#[async_trait::async_trait]
pub(super) trait ResponsesReservationCleanup: Send + Sync {
    async fn delete(
        &self,
        tenant_id: uuid::Uuid,
        reservation_id: &str,
    ) -> std::result::Result<(), String>;
}

pub(super) struct DatabaseResponsesReservationCleanup {
    pub(super) pool: Arc<keycompute_db::DbRouter>,
}

#[async_trait::async_trait]
impl ResponsesReservationCleanup for DatabaseResponsesReservationCleanup {
    async fn delete(
        &self,
        tenant_id: uuid::Uuid,
        reservation_id: &str,
    ) -> std::result::Result<(), String> {
        ResponseAffinity::delete_reservation(self.pool.as_ref(), tenant_id, reservation_id)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[derive(Default)]
pub(super) struct ResponsesExecutionReservations {
    pub(super) tenant_id: uuid::Uuid,
    pub(super) reservation_ids: Vec<String>,
    pub(super) cleanup: Option<Arc<dyn ResponsesReservationCleanup>>,
}

impl Drop for ResponsesExecutionReservations {
    fn drop(&mut self) {
        let Some(cleanup) = self.cleanup.clone() else {
            return;
        };
        let reservation_ids = std::mem::take(&mut self.reservation_ids);
        if reservation_ids.is_empty() {
            return;
        }
        let tenant_id = self.tenant_id;
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                count = reservation_ids.len(),
                %tenant_id,
                "Responses reservations dropped without an active runtime; expiry cleanup will release them"
            );
            return;
        };
        runtime.spawn(async move {
            for reservation_id in reservation_ids {
                if let Err(error) = cleanup.delete(tenant_id, &reservation_id).await {
                    tracing::warn!(
                        %error,
                        %reservation_id,
                        "failed to release a cancelled Responses account reservation"
                    );
                }
            }
        });
    }
}

impl ResponsesExecutionReservations {
    pub(super) async fn acquire(
        state: &AppState,
        tenant_id: uuid::Uuid,
        request_id: uuid::Uuid,
        plan: &mut ExecutionPlan,
        constraint: Option<&ResponsesReservationConstraint>,
    ) -> Result<Self> {
        let Some(pool) = state.pool.clone() else {
            return Ok(Self::default());
        };
        let mut targets = Vec::new();
        for target in std::iter::once(&plan.primary).chain(plan.fallback_chain.iter()) {
            if let ExecutionTarget::ProviderAccount {
                provider,
                account_id,
                ..
            } = target
                && !targets
                    .iter()
                    .any(|(_, existing_id)| existing_id == account_id)
            {
                targets.push((provider.clone(), *account_id));
            }
        }
        let reservation_ttl = responses_execution_reservation_ttl(&state.gateway_config);
        let expires_at = chrono::Utc::now()
            + chrono::Duration::from_std(reservation_ttl).unwrap_or(chrono::Duration::hours(2));
        let mut reservations = Self {
            tenant_id,
            reservation_ids: Vec::with_capacity(targets.len()),
            cleanup: Some(Arc::new(DatabaseResponsesReservationCleanup {
                pool: Arc::clone(&pool),
            })),
        };
        let mut account_snapshots = Vec::with_capacity(targets.len());
        for (index, (provider, account_id)) in targets.into_iter().enumerate() {
            let reservation_id = format!("kc_reservation_{}_{index}", request_id.simple());
            // Register the ID before the database await. Cancellation can
            // otherwise land after COMMIT but before this future returns,
            // leaving Drop unaware of the durable reservation. Deleting an ID
            // whose insert failed or rolled back is intentionally idempotent.
            reservations.reservation_ids.push(reservation_id.clone());
            let snapshot = match reserve_responses_execution_target(
                pool.as_ref(),
                tenant_id,
                &reservation_id,
                &provider,
                account_id,
                expires_at,
                (index == 0).then_some(constraint).flatten(),
            )
            .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    reservations.release().await;
                    return Err(error);
                }
            };
            account_snapshots.push(snapshot);
        }
        if let Err(error) = apply_reserved_account_snapshots(plan, &account_snapshots) {
            reservations.release().await;
            return Err(error);
        }
        Ok(reservations)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.reservation_ids.is_empty()
    }

    pub(super) async fn release(&mut self) {
        let Some(cleanup) = self.cleanup.clone() else {
            self.reservation_ids.clear();
            return;
        };
        // Keep the current ID in the guard until its delete future completes.
        // If this task is cancelled mid-await, Drop will retry every remaining
        // reservation on a detached cleanup task.
        while let Some(reservation_id) = self.reservation_ids.last().cloned() {
            if let Err(error) = cleanup.delete(self.tenant_id, &reservation_id).await {
                tracing::warn!(%error, %reservation_id, "failed to release Responses account reservation");
            }
            self.reservation_ids.pop();
        }
    }

    /// Persistence failures intentionally retain the account lock until its
    /// bounded reservation TTL. Disarm automatic cancellation cleanup without
    /// deleting those durable guard rows.
    pub(super) fn retain_until_expiry(&mut self) {
        self.reservation_ids.clear();
    }
}

pub(super) async fn reserve_responses_execution_target(
    pool: &keycompute_db::DbRouter,
    tenant_id: uuid::Uuid,
    reservation_id: &str,
    expected_provider: &str,
    account_id: uuid::Uuid,
    expires_at: chrono::DateTime<chrono::Utc>,
    constraint: Option<&ResponsesReservationConstraint>,
) -> Result<ResolvedResponsesAccount> {
    // Lock and snapshot the account in the same writer transaction that
    // installs the reservation. An admin connection-material update that wins
    // first is reflected in this snapshot; one that starts later observes the
    // reservation and is rejected until the request/settlement releases it.
    let txn = pool.begin().await.map_err(|error| {
        responses_state_unavailable("begin a Responses account reservation", error)
    })?;
    let account = match Account::find_by_id_for_key_share(&txn, account_id).await {
        Ok(Some(account)) => account,
        Ok(None) => {
            let _ = txn.rollback().await;
            return Err(ApiError::ServiceUnavailable(
                "The selected Responses account is no longer available".to_string(),
            ));
        }
        Err(error) => {
            let _ = txn.rollback().await;
            return Err(responses_state_unavailable(
                "snapshot a Responses provider account",
                error,
            ));
        }
    };
    let require_responses_capability = !matches!(
        constraint,
        Some(ResponsesReservationConstraint::Affinity { .. })
    );
    let snapshot = match reserved_responses_account_snapshot(
        account,
        expected_provider,
        tenant_id,
        require_responses_capability,
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let _ = txn.rollback().await;
            return Err(error);
        }
    };
    if let Err(error) =
        validate_responses_reservation_constraint(&txn, tenant_id, &snapshot, constraint).await
    {
        let _ = txn.rollback().await;
        return Err(error);
    }
    if let Err(error) = ResponseAffinity::reserve_account(
        &txn,
        tenant_id,
        reservation_id,
        expected_provider,
        account_id,
        expires_at,
    )
    .await
    {
        let _ = txn.rollback().await;
        return Err(responses_state_unavailable(
            "reserve a Responses provider account",
            error,
        ));
    }
    txn.commit().await.map_err(|error| {
        responses_state_unavailable("commit a Responses account reservation", error)
    })?;
    Ok(snapshot)
}

pub(super) async fn validate_responses_reservation_constraint(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: uuid::Uuid,
    snapshot: &ResolvedResponsesAccount,
    constraint: Option<&ResponsesReservationConstraint>,
) -> Result<()> {
    match constraint {
        None => Ok(()),
        Some(ResponsesReservationConstraint::Affinity { resource_id }) => {
            let affinity = ResponseAffinity::find_active_for_key_share(txn, tenant_id, resource_id)
                .await
                .map_err(|error| {
                    responses_state_unavailable("revalidate a Responses resource route", error)
                })?
                .ok_or_else(|| {
                    ApiError::Conflict(
                        "The Responses resource route changed before execution".to_string(),
                    )
                })?;
            if affinity.account_id != Some(snapshot.account_id)
                || !affinity.provider.eq_ignore_ascii_case(&snapshot.provider)
            {
                return Err(ApiError::Conflict(
                    "The Responses resource owner changed before execution".to_string(),
                ));
            }
            Ok(())
        }
        Some(ResponsesReservationConstraint::ConnectionSnapshot {
            account_id,
            endpoint,
            api_key,
        }) if *account_id == snapshot.account_id
            && endpoint == &snapshot.endpoint
            && api_key == &snapshot.api_key =>
        {
            Ok(())
        }
        Some(ResponsesReservationConstraint::ConnectionSnapshot { .. }) => Err(ApiError::Conflict(
            "The discovered Responses connection changed before execution".to_string(),
        )),
    }
}

pub(super) fn reserved_responses_account_snapshot(
    account: Account,
    expected_provider: &str,
    tenant_id: uuid::Uuid,
    require_responses_capability: bool,
) -> Result<ResolvedResponsesAccount> {
    if !account.provider.eq_ignore_ascii_case(expected_provider)
        || !account.provider.eq_ignore_ascii_case("openai")
        || !responses_account_is_visible_to_tenant(&account, tenant_id)
    {
        return Err(ApiError::ServiceUnavailable(
            "The selected Responses account changed before execution".to_string(),
        ));
    }
    if !reserved_responses_account_is_available(&account, require_responses_capability) {
        return Err(ApiError::ServiceUnavailable(
            "The selected account no longer supports the Responses API".to_string(),
        ));
    }
    let protocol = ProtocolType::parse(&account.provider).ok_or_else(|| {
        ApiError::ServiceUnavailable("The selected Responses account is invalid".to_string())
    })?;
    let endpoint = if account.endpoint.is_empty() {
        protocol.default_endpoint().to_string()
    } else {
        account.endpoint
    };
    Ok(ResolvedResponsesAccount {
        provider: account.provider,
        model: None,
        account_id: account.id,
        endpoint,
        api_key: crate::handlers::admin_account::decrypt_account_api_key(
            &account.upstream_api_key_encrypted,
        )?,
    })
}

pub(super) fn reserved_responses_account_is_available(
    account: &Account,
    require_responses_capability: bool,
) -> bool {
    account.enabled
        && (!require_responses_capability
            || account
                .api_capabilities
                .iter()
                .any(|capability| capability == AccountApiCapability::Responses.as_str()))
}

pub(super) fn apply_reserved_account_snapshots(
    plan: &mut ExecutionPlan,
    snapshots: &[ResolvedResponsesAccount],
) -> Result<()> {
    for target in std::iter::once(&mut plan.primary).chain(plan.fallback_chain.iter_mut()) {
        let ExecutionTarget::ProviderAccount {
            provider,
            account_id,
            ..
        } = target
        else {
            continue;
        };
        let snapshot = snapshots
            .iter()
            .find(|snapshot| {
                snapshot.account_id == *account_id
                    && snapshot.provider.eq_ignore_ascii_case(provider)
            })
            .ok_or_else(|| {
                ApiError::Internal(
                    "A reserved Responses execution target was not snapshotted".to_string(),
                )
            })?;
        *target = snapshot.clone().into_target();
    }
    Ok(())
}

pub(super) fn validate_reserved_idempotency_connection(
    plan: &ExecutionPlan,
    claimed: &ResolvedResponsesAccount,
) -> Result<()> {
    let ExecutionTarget::ProviderAccount {
        provider,
        account_id,
        endpoint,
        upstream_api_key,
    } = &plan.primary
    else {
        return Err(ApiError::Internal(
            "A reserved idempotent Responses target became a node route".to_string(),
        ));
    };
    if *account_id != claimed.account_id
        || !provider.eq_ignore_ascii_case(&claimed.provider)
        || endpoint != &claimed.endpoint
        || upstream_api_key.expose() != claimed.api_key
    {
        return Err(ApiError::Conflict(
            "The idempotent Responses account changed before it could be reserved".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn affinity_storage_key(tenant_id: uuid::Uuid, response_id: &str) -> String {
    format!("{tenant_id}:{response_id}")
}

pub(super) fn affinity_cache_key(tenant_id: uuid::Uuid, response_id: &str) -> String {
    format!(
        "responses:affinity:{}",
        affinity_storage_key(tenant_id, response_id)
    )
}

pub(super) const RESPONSES_STATE_UNAVAILABLE_MESSAGE: &str =
    "Responses state is temporarily unavailable";

pub(super) fn responses_state_unavailable(
    operation: &str,
    error: impl std::fmt::Display,
) -> ApiError {
    tracing::error!(%error, operation, "Responses state operation failed");
    ApiError::ServiceUnavailable(RESPONSES_STATE_UNAVAILABLE_MESSAGE.to_string())
}

pub(super) fn map_response_affinity_write_error(
    error: keycompute_db::DbError,
    operation: &str,
) -> ApiError {
    match error {
        keycompute_db::DbError::DuplicateKey { entity, .. }
            if entity == "response affinity ownership" =>
        {
            ApiError::Conflict(
                "The upstream Responses resource ID is already owned by another account"
                    .to_string(),
            )
        }
        keycompute_db::DbError::ResourceLimitExceeded { resource, limit }
            if resource == "stored Responses warmups" =>
        {
            tracing::warn!(%resource, %limit, "Responses warmup storage quota exceeded");
            ApiError::RateLimit(
                "Stored Responses warmup quota exceeded; delete an existing warmup or wait for it to expire"
                    .to_string(),
            )
        }
        error => responses_state_unavailable(operation, error),
    }
}

pub(super) fn map_responses_idempotency_bind_error(error: keycompute_db::DbError) -> ApiError {
    match error {
        keycompute_db::DbError::ResourceLimitExceeded { resource, limit }
            if resource == "Responses idempotency identities" =>
        {
            tracing::warn!(%resource, %limit, "Responses idempotency identity quota exceeded");
            ApiError::RateLimit(format!(
                "Responses Idempotency-Key quota exceeded ({RESPONSES_IDEMPOTENCY_MAX_IDENTITIES_PER_TENANT} identities per tenant); reuse an existing key or omit Idempotency-Key"
            ))
        }
        error => responses_state_unavailable("bind Responses idempotency key", error),
    }
}

pub(super) async fn cache_response_affinity_best_effort(
    state: &AppState,
    response_id: &str,
    affinity: ResponsesAffinity,
) {
    if !cache_response_affinity_locally(
        state,
        affinity_storage_key(affinity.tenant_id, response_id),
        affinity.clone(),
    )
    .await
    {
        tracing::warn!("Responses affinity local map is full; skipping local cache entry");
    }
    if let Err(error) = state
        .cache
        .set(
            &affinity_cache_key(affinity.tenant_id, response_id),
            &affinity,
            RESPONSES_AFFINITY_TTL,
        )
        .await
    {
        tracing::warn!(%error, "failed to persist Responses affinity in cache");
    }
}

pub(super) async fn save_response_affinity(
    state: &AppState,
    response_id: &str,
    resource_kind: ResponsesResourceKind,
    route: ResponsesAffinityRoute,
    settlement: Option<Value>,
) -> Result<bool> {
    if !valid_affinity_resource_id(response_id) {
        return Err(ApiError::Provider(
            "Upstream returned an invalid Responses resource ID".to_string(),
        ));
    }
    let affinity = ResponsesAffinity {
        tenant_id: route.tenant_id,
        provider: route.provider,
        model: route.model,
        account_id: route.account_id,
        expires_at_unix: affinity_expiry_unix(resource_kind, chrono::Utc::now().timestamp()),
    };
    let settlement_durable = if let Some(pool) = state.pool.as_deref() {
        let expires_at = chrono::DateTime::from_timestamp(affinity.expires_at_unix, 0)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
        if let Some(settlement) = settlement {
            let next_poll_at = settlement_next_poll_at(&settlement);
            ResponseAffinity::upsert_route_with_settlement(
                pool,
                affinity.tenant_id,
                response_id,
                &affinity.provider,
                affinity.model.as_deref(),
                affinity.account_id,
                expires_at,
                settlement,
                next_poll_at,
            )
            .await
            .map_err(|error| {
                map_response_affinity_write_error(
                    error,
                    "persist the Responses route and billing settlement",
                )
            })?;
            true
        } else {
            ResponseAffinity::upsert_route(
                pool,
                affinity.tenant_id,
                response_id,
                &affinity.provider,
                affinity.model.as_deref(),
                affinity.account_id,
                expires_at,
            )
            .await
            .map_err(|error| {
                map_response_affinity_write_error(error, "persist Responses resource routing")
            })?;
            false
        }
    } else {
        false
    };
    cache_response_affinity_best_effort(state, response_id, affinity).await;
    Ok(settlement_durable)
}

pub(super) async fn save_response_affinity_if_stored(
    state: &AppState,
    stored: bool,
    response_id: &str,
    route: ResponsesAffinityRoute,
    settlement: Option<Value>,
) -> Result<bool> {
    if !stored {
        let Some(settlement) = settlement else {
            return Ok(false);
        };
        if !valid_response_id(response_id) {
            return Err(ApiError::Provider(
                "Upstream returned an invalid Responses resource ID".to_string(),
            ));
        }
        let Some(pool) = state.pool.as_deref() else {
            return Ok(false);
        };
        let expires_at_unix = affinity_expiry_unix(
            ResponsesResourceKind::Response,
            chrono::Utc::now().timestamp(),
        );
        let expires_at = chrono::DateTime::from_timestamp(expires_at_unix, 0)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
        let next_poll_at = settlement_next_poll_at(&settlement);
        ResponseAffinity::upsert_hidden_settlement(
            pool,
            route.tenant_id,
            response_id,
            &route.provider,
            route.model.as_deref(),
            Some(route.account_id),
            expires_at,
            settlement,
            next_poll_at,
        )
        .await
        .map_err(|error| {
            map_response_affinity_write_error(
                error,
                "persist background Responses billing settlement",
            )
        })?;
        return Ok(true);
    }
    save_response_affinity(
        state,
        response_id,
        ResponsesResourceKind::Response,
        route,
        settlement,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_terminal_responses_outbox(
    state: &AppState,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    response_id: Option<&str>,
    stored: bool,
    model: Option<String>,
    status: &str,
) -> bool {
    persist_terminal_responses_outbox_with_tpm_timing(
        state,
        ctx,
        provider,
        account_id,
        response_id,
        stored,
        model,
        status,
        ResponsesTpmTiming::LedgerFinishedAt,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn persist_terminal_responses_outbox_with_tpm_timing(
    state: &AppState,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    response_id: Option<&str>,
    stored: bool,
    model: Option<String>,
    status: &str,
    tpm_timing: ResponsesTpmTiming,
) -> bool {
    if state.pool.is_none() {
        return false;
    }
    let model = effective_response_affinity_model(model.as_deref(), ctx);
    let settlement = match terminal_settlement_value_with_tpm_timing(
        ctx, provider, account_id, status, tpm_timing,
    ) {
        Ok(settlement) => settlement,
        Err(error) => {
            tracing::error!(request_id = %ctx.request_id, %error, "failed to create Responses terminal settlement outbox");
            return false;
        }
    };
    let (provider, account_id) = ctx.billing_target(provider, account_id);
    for (attempt, (target_id, target_stored)) in
        terminal_settlement_outbox_targets(response_id, stored, ctx.billing_request_id)
            .into_iter()
            .enumerate()
    {
        match save_response_affinity_if_stored(
            state,
            target_stored,
            &target_id,
            ResponsesAffinityRoute {
                tenant_id: ctx.tenant_id,
                provider: provider.clone(),
                model: model.clone(),
                account_id,
            },
            Some(settlement.clone()),
        )
        .await
        {
            Ok(durable) => return durable,
            Err(error) => {
                tracing::error!(
                    request_id = %ctx.request_id,
                    %error,
                    fallback = attempt > 0,
                    "failed to persist Responses terminal settlement outbox"
                );
            }
        }
    }
    false
}

/// Persist terminal settlement for protocols that do not own an addressable
/// Responses resource. The synthetic, tombstoned affinity is only an outbox
/// row; the existing settlement worker can replay billing and TPM effects for
/// Chat Completions, Messages, and accountless node executions as well.
pub(super) async fn persist_immediate_terminal_settlement_outbox_inner(
    state: &AppState,
    ctx: &RequestContext,
    provider: &str,
    account_id: uuid::Uuid,
    status: &str,
    terminal_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    let Some(pool) = state.pool.as_deref() else {
        return false;
    };
    let settlement = match terminal_settlement_value_with_tpm_timing(
        ctx,
        provider,
        account_id,
        status,
        ResponsesTpmTiming::TerminalAt(terminal_at),
    ) {
        Ok(settlement) => settlement,
        Err(error) => {
            tracing::error!(request_id = %ctx.request_id, %error, "failed to create immediate terminal settlement outbox");
            return false;
        }
    };
    let (provider, account_id) = ctx.billing_target(provider, account_id);
    let response_id = format!("resp_kc_settlement_{}", ctx.billing_request_id.simple());
    let expires_at_unix = affinity_expiry_unix(
        ResponsesResourceKind::Response,
        chrono::Utc::now().timestamp(),
    );
    let expires_at = chrono::DateTime::from_timestamp(expires_at_unix, 0)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
    match ResponseAffinity::upsert_hidden_settlement(
        pool,
        ctx.tenant_id,
        &response_id,
        &provider,
        effective_response_affinity_model(None, ctx).as_deref(),
        (!account_id.is_nil()).then_some(account_id),
        expires_at,
        settlement,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::error!(request_id = %ctx.request_id, %error, "failed to persist immediate terminal settlement outbox");
            false
        }
    }
}

pub(super) async fn acknowledge_terminal_settlement_outbox_inner(
    state: &AppState,
    ctx: &RequestContext,
) {
    if let Some(pool) = state.pool.as_deref()
        && let Err(error) = ResponseAffinity::clear_completed_settlements(
            pool,
            ctx.tenant_id,
            ctx.billing_request_id,
        )
        .await
    {
        // Billing and TPM are already complete. Leaving the row behind is safe:
        // the worker will replay both effects idempotently before clearing it.
        tracing::warn!(request_id = %ctx.request_id, %error, "failed to acknowledge terminal settlement outbox");
    }
}

pub(super) fn terminal_settlement_outbox_targets(
    response_id: Option<&str>,
    stored: bool,
    billing_request_id: uuid::Uuid,
) -> Vec<(String, bool)> {
    // Key the hidden fallback by the logical billing request so idempotent
    // client retries converge on the same durable settlement row. A provider
    // resource ID can collide with an existing affinity; the billing job does
    // not need to remain resource-visible, so retry on this internal ID.
    let synthetic_id = format!("resp_kc_settlement_{}", billing_request_id.simple());
    match response_id {
        Some(response_id) if response_id != synthetic_id => {
            vec![(response_id.to_string(), stored), (synthetic_id, false)]
        }
        _ => vec![(synthetic_id, false)],
    }
}

pub(super) fn settlement_next_poll_at(settlement: &Value) -> chrono::DateTime<chrono::Utc> {
    let delay = if settlement
        .get("terminal_status")
        .and_then(Value::as_str)
        .is_some()
    {
        chrono::Duration::seconds(5)
    } else {
        chrono::Duration::zero()
    };
    chrono::Utc::now() + delay
}

pub(super) fn affinity_expiry_unix(resource_kind: ResponsesResourceKind, now: i64) -> i64 {
    if resource_kind == ResponsesResourceKind::Conversation {
        // Conversations remain usable until explicitly deleted upstream. Keep
        // their durable tenant/account binding rather than applying the
        // shorter stored-Response retention window.
        chrono::DateTime::<chrono::Utc>::MAX_UTC.timestamp()
    } else {
        now.saturating_add(i64::try_from(RESPONSES_AFFINITY_TTL.as_secs()).unwrap_or(i64::MAX))
    }
}

pub(super) async fn response_affinity(
    state: &AppState,
    response_id: &str,
    tenant_id: uuid::Uuid,
) -> Result<ResponsesAffinity> {
    if !valid_affinity_resource_id(response_id) {
        return Err(ApiError::NotFound(format!(
            "Responses resource not found: {response_id}"
        )));
    }
    let now = chrono::Utc::now().timestamp();
    let storage_key = affinity_storage_key(tenant_id, response_id);
    let affinity = if let Some(pool) = state.pool.as_deref() {
        // The database is the ownership source of truth. Local/Redis entries
        // may outlive an administrative endpoint or credential replacement on
        // another replica; consulting them first could retarget an old opaque
        // resource ID to the account's new upstream connection.
        ResponseAffinity::find_active(pool, tenant_id, response_id)
            .await
            .map_err(|error| {
                ApiError::Internal(format!(
                    "Responses affinity database lookup failed: {error}"
                ))
            })?
            .and_then(|model| {
                model.account_id.map(|account_id| ResponsesAffinity {
                    tenant_id: model.tenant_id,
                    provider: model.provider,
                    model: model.model,
                    account_id,
                    expires_at_unix: model.expires_at.timestamp(),
                })
            })
            .ok_or_else(|| {
                ApiError::NotFound(format!("Responses resource not found: {response_id}"))
            })?
    } else {
        if let Some(affinity) = state
            .responses_affinity
            .read()
            .await
            .get(&storage_key)
            .cloned()
            && affinity.expires_at_unix > now
            && affinity.tenant_id == tenant_id
        {
            affinity
        } else {
            state
                .cache
                .get::<ResponsesAffinity>(&affinity_cache_key(tenant_id, response_id))
                .await
                .map_err(|error| {
                    ApiError::Internal(format!("Responses affinity lookup failed: {error}"))
                })?
                .filter(|affinity| affinity.expires_at_unix > now)
                .filter(|affinity| affinity.tenant_id == tenant_id)
                .ok_or_else(|| {
                    ApiError::NotFound(format!("Responses resource not found: {response_id}"))
                })?
        }
    };
    if !cache_response_affinity_locally(state, storage_key, affinity.clone()).await {
        tracing::warn!("Responses affinity local map is full; skipping local cache entry");
    }
    Ok(affinity)
}

pub(super) async fn cache_response_affinity_locally(
    state: &AppState,
    storage_key: String,
    affinity: ResponsesAffinity,
) -> bool {
    let mut local = state.responses_affinity.write().await;
    insert_response_affinity_with_limit(
        &mut local,
        storage_key,
        affinity,
        chrono::Utc::now().timestamp(),
        RESPONSES_AFFINITY_LOCAL_MAX,
    )
}

pub(super) fn insert_response_affinity_with_limit(
    local: &mut std::collections::HashMap<String, ResponsesAffinity>,
    storage_key: String,
    affinity: ResponsesAffinity,
    now: i64,
    max_entries: usize,
) -> bool {
    if local.len() >= max_entries {
        local.retain(|_, value| value.expires_at_unix > now);
    }
    if local.len() < max_entries || local.contains_key(&storage_key) {
        local.insert(storage_key, affinity);
        true
    } else {
        false
    }
}

pub(super) async fn delete_response_affinity(
    state: &AppState,
    response_id: &str,
    tenant_id: uuid::Uuid,
) -> Result<()> {
    let database_authoritative = if let Some(pool) = state.pool.as_deref() {
        ResponseAffinity::delete_route_preserving_settlement(pool, tenant_id, response_id)
            .await
            .map_err(|error| {
                responses_state_unavailable("delete Responses affinity from database", error)
            })?;
        true
    } else {
        false
    };
    state
        .responses_affinity
        .write()
        .await
        .remove(&affinity_storage_key(tenant_id, response_id));
    if let Err(error) = state
        .cache
        .delete(&affinity_cache_key(tenant_id, response_id))
        .await
    {
        if !database_authoritative {
            return Err(responses_state_unavailable(
                "delete Responses affinity from authoritative cache",
                error,
            ));
        }
        // A cache outage after the authoritative database delete committed
        // must not turn an otherwise successful DELETE into an unretryable
        // 503; database-backed reads will not trust the stale key.
        tracing::warn!(%error, %response_id, %tenant_id, "failed to evict deleted Responses affinity from cache");
    }
    Ok(())
}
